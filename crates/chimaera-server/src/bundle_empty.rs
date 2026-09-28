//! Missing native history is an error unless complete local evidence proves
//! that this structured session has never contained a conversation.
use super::*;
use chimaera_agent::{
    journal::SeqEvent,
    model::{AgentEvent, SessionUi},
};

fn startup_only(events: &[Arc<SeqEvent>], head: u64) -> bool {
    if head == 0 || head > 256 || events.len() as u64 != head {
        return false;
    }
    let mut initialized = false;
    for (index, event) in events.iter().enumerate() {
        if event.seq != index as u64 + 1 {
            return false;
        }
        match &event.ev {
            AgentEvent::Init { .. } => initialized = true,
            AgentEvent::BackgroundTasks { tasks, closed }
                if tasks.is_empty() && closed.is_empty() => {}
            AgentEvent::EffortState { .. }
            | AgentEvent::ModeChanged { .. }
            | AgentEvent::Exited { .. } => {}
            _ => return false,
        }
    }
    initialized
}

/// Called on the blocking lane, under the session lifecycle guard. The replay
/// joins the live journal ring so unwritten events cannot masquerade as empty.
pub(super) fn unstarted(state: &AppState, entry: &LedgerEntry) -> bool {
    let Some(agent) = &entry.agent else {
        return false;
    };
    if agent.ui != SessionUi::Chat
        || agent.kind != crate::agents::AgentKind::Claude
        || agent
            .resume
            .as_deref()
            .is_some_and(|id| !crate::codex_notify::valid_thread_id(id))
        || state.chat.has_submitted_input(&entry.id)
    {
        return false;
    }
    let safe_recipe = crate::lock(&state.chat_recipes)
        .get(&entry.id)
        .is_some_and(|r| {
            r.resume.is_none()
                && r.fork_at.is_none()
                && !r.fork_head
                && r.rollback_turns.is_none()
                && r.revert_before_turn.is_none()
                && r.portable_context.is_none()
        });
    if !safe_recipe {
        return false;
    }
    let empty_record = crate::lock(&state.agents).get(&entry.id).is_some_and(|r| {
        r.first_prompt.is_none()
            && r.files_touched.is_empty()
            && r.subagents.is_empty()
            && r.resumed_from.is_none()
    });
    if !empty_record {
        return false;
    }
    let empty_process = state.chat.carryover(&entry.id).is_some_and(|c| {
        !c.turn_in_flight && c.background.is_empty() && !c.remote_control && c.pickup_at_ms == 0
    });
    if !empty_process {
        return false;
    }
    let path = state.chat.journal_dir().join(format!("{}.jsonl", entry.id));
    // Replay tolerates corrupt historical lines for display. An omission proof
    // must be stricter: malformed, torn or pruned disk evidence is never empty.
    let Ok(bytes) = read_capped(&path, 256 * 1024) else {
        return false;
    };
    if bytes.last() != Some(&b'\n') {
        return false;
    }
    let Ok(disk): Result<Vec<_>, _> = bytes
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<SeqEvent>(line).map(Arc::new))
        .collect()
    else {
        return false;
    };
    if !startup_only(&disk, disk.last().map_or(0, |e| e.seq)) {
        return false;
    }
    let Ok(attached) = state.chat.attach(&entry.id, 0) else {
        return false;
    };
    attached.info.alive
        && !attached.info.pending_permission
        && attached.info.background_running == 0
        && startup_only(&attached.replay, attached.head_seq)
        && !state.chat.has_submitted_input(&entry.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agents::{AgentKind, AgentRecord},
        chat::{ChatRecipe, RemoteControlAtStart},
        lock,
    };
    use chimaera_agent::{
        driver::{AgentAdapter, DriverExit, DriverIo, SpawnSpec},
        model::AgentCommand,
    };
    const NATIVE: &str = "11111111-1111-4111-8111-111111111111";
    fn init() -> AgentEvent {
        serde_json::from_value(
            json!({"type":"init","native_session_id":NATIVE,"model":null,"current_mode":null}),
        )
        .unwrap()
    }
    fn entries(events: Vec<AgentEvent>) -> Vec<Arc<SeqEvent>> {
        events
            .into_iter()
            .enumerate()
            .map(|(n, ev)| {
                Arc::new(SeqEvent {
                    seq: n as u64 + 1,
                    ts: 1,
                    ev,
                })
            })
            .collect()
    }
    #[test]
    fn only_complete_startup_history_is_empty() {
        let events = entries(vec![
            AgentEvent::BackgroundTasks {
                tasks: vec![],
                closed: vec![],
            },
            init(),
            AgentEvent::Exited { status: Some(0) },
            init(),
        ]);
        assert!(
            startup_only(&events, 4),
            "a restarted empty conversation remains empty"
        );
        assert!(
            !startup_only(&events[1..], 4),
            "missing prefix is ambiguous"
        );
        assert!(
            !startup_only(&events, 5),
            "a pending journal event is not empty evidence"
        );
        for ev in [
            AgentEvent::TurnStarted {
                turn_id: "turn".into(),
            },
            AgentEvent::MessageChunk {
                turn_id: "turn".into(),
                text: "work".into(),
            },
            AgentEvent::Notice {
                text: "history".into(),
            },
            serde_json::from_value(json!({"type":"user_message","text":"keep me","queued":true}))
                .unwrap(),
        ] {
            assert!(!startup_only(&entries(vec![init(), ev]), 2));
        }
    }
    struct Quiet;
    impl AgentAdapter for Quiet {
        fn kind(&self) -> &'static str {
            "claude"
        }
        fn spawn(
            &self,
            _spec: SpawnSpec,
            mut io: DriverIo,
        ) -> Result<tokio::task::JoinHandle<DriverExit>> {
            Ok(tokio::spawn(async move {
                let commands = io.commands;
                let _ = io.events.send(init()).await;
                // Do not consume input: this exercises the accepted-before-echo fence.
                let _ = io.kill.changed().await;
                drop(commands);
                DriverExit::Killed
            }))
        }
    }
    async fn fixture() -> (PathBuf, Arc<AppState>) {
        let root = std::env::temp_dir().join(format!(
            "chimaera-empty-bundle-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let mut state = AppState::new(
            "fixture".into(),
            "fixture".into(),
            0,
            0,
            root.join("data"),
            root.join("config"),
        );
        state.claude_projects_dir = root.join("claude/projects");
        state.codex_config_path = root.join("codex/config.toml");
        let state = Arc::new(state);
        let workspace = lock(&state.workspaces).add(root.clone()).unwrap();
        lock(&state.session_workspaces).insert("s-empty".into(), workspace.id.clone());
        lock(&state.agents).insert(
            "s-empty".into(),
            AgentRecord::new("fixture".into(), AgentKind::Claude),
        );
        lock(&state.chat_recipes).insert(
            "s-empty".into(),
            ChatRecipe {
                workspace_root: root.clone(),
                workspace_id: workspace.id,
                kind: AgentKind::Claude,
                bin: "/bin/false".into(),
                version: None,
                settings: None,
                mcp_config: None,
                model: None,
                resume: None,
                fork_at: None,
                fork_head: false,
                rollback_turns: None,
                revert_before_turn: None,
                remote_control: RemoteControlAtStart::No,
                carry_ultracode: false,
                theme: "dark".into(),
                prelude: None,
                mastermind: None,
                portable_context: None,
                created_at_ms: None,
            },
        );
        state
            .chat
            .spawn(&Quiet, SpawnSpec::new("s-empty", vec![], root.clone()))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if state
                    .chat
                    .get("s-empty")
                    .is_some_and(|s| s.native_session_id.is_some())
                    && state.chat.journal_dir().join("s-empty.jsonl").is_file()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        (root, state)
    }
    async fn finish(root: PathBuf, state: Arc<AppState>) {
        state.chat.kill("s-empty");
        tokio::time::timeout(Duration::from_secs(3), async {
            while state.chat.get("s-empty").is_some_and(|s| s.alive) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn clean_empty_omission_fences_delayed_input_and_preserves_restart() {
        let _serial = super::super::TEST_SERIAL.lock().await;
        let (root, state) = fixture().await;
        // Models a WS send that passed its ownership check before handoff,
        // then finished preparing attachments after the empty proof.
        let pause = state.chat.pause_commands("s-empty").await.unwrap();
        let entry = crate::ledger::snapshot(&state)
            .0
            .into_iter()
            .find(|e| e.id == "s-empty")
            .unwrap();
        assert!(unstarted(&state, &entry));
        let command = serde_json::from_value::<AgentCommand>(
            json!({"type":"send","text":"delayed","blocks":[]}),
        )
        .unwrap();
        assert!(state
            .chat
            .command("s-empty", command.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("paused"));
        assert!(!state.chat.has_submitted_input("s-empty"));
        drop(pause); // A failed/canceled proof cannot strand the local session.
        let pause = state.chat.pause_commands("s-empty").await.unwrap();
        drop(pause);
        assert!(
            export_for_mirror(state.clone(), "s-empty", ExportMode::Stop)
                .await
                .unwrap()
                .is_none()
        );
        assert!(state.chat.get("s-empty").is_none());
        let deferred = lock(&state.deferred_sessions)
            .get("s-empty")
            .cloned()
            .unwrap();
        assert!(deferred.suspended);
        assert_eq!(deferred.agent.unwrap().resume.as_deref(), Some(NATIVE));
        assert!(state.chat.journal_dir().join("s-empty.jsonl").is_file());
        // A resumed process has a new ingress gate; the preserved public ID
        // and startup-only journal do not leave a permanent input lock.
        state
            .chat
            .spawn(&Quiet, SpawnSpec::new("s-empty", vec![], root.clone()))
            .unwrap();
        state.chat.command("s-empty", command).await.unwrap();
        assert!(state.chat.has_submitted_input("s-empty"));
        finish(root, state).await;
    }
    #[tokio::test]
    async fn empty_claude_does_not_block_completed_codex_but_missing_work_still_fails() {
        let _serial = super::super::TEST_SERIAL.lock().await;
        let (root, state) = fixture().await;
        assert!(
            export_for_mirror(state.clone(), "s-empty", ExportMode::Snapshot)
                .await
                .unwrap()
                .is_none()
        );
        let journal = state.chat.journal_dir().join("s-empty.jsonl");
        let original = std::fs::read(&journal).unwrap();
        let mut corrupt = original.clone();
        corrupt.extend_from_slice(b"not json\n");
        std::fs::write(&journal, corrupt).unwrap();
        assert!(
            export_for_mirror(state.clone(), "s-empty", ExportMode::Snapshot)
                .await
                .is_err(),
            "display replay tolerance must not hide corruption"
        );
        std::fs::write(journal, original).unwrap();
        assert!(
            export(state.clone(), "s-empty", ExportMode::Snapshot)
                .await
                .is_err(),
            "explicit export is strict"
        );
        let mut codex = crate::ledger::snapshot(&state)
            .0
            .into_iter()
            .find(|s| s.id == "s-empty")
            .unwrap();
        codex.id = "s-completed".into();
        codex.suspended = true;
        let a = codex.agent.as_mut().unwrap();
        a.kind = AgentKind::Codex;
        a.resume = Some("22222222-2222-4222-8222-222222222222".into());
        let native = super::super::native_destination(&state, &codex)
            .unwrap()
            .unwrap();
        let bytes = format!(
            "{}\n{}\n",
            json!({"type":"session_meta","payload":{"id":codex.agent.as_ref().unwrap().resume,"cwd":codex.cwd}}),
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"completed fixture work"}]}})
        );
        super::super::private_create(&native)
            .unwrap()
            .write_all(bytes.as_bytes())
            .unwrap();
        crate::ledger::defer(&state, codex).unwrap();
        let archive = export_for_mirror(state.clone(), "s-completed", ExportMode::Snapshot)
            .await
            .unwrap()
            .unwrap();
        let mut opened = super::super::open_archive(&archive).unwrap();
        let mut preserved = String::new();
        opened
            .zip
            .by_name("native.jsonl")
            .unwrap()
            .read_to_string(&mut preserved)
            .unwrap();
        assert_eq!(preserved, bytes);
        drop(opened);
        std::fs::remove_file(archive).unwrap();
        std::fs::remove_file(native).unwrap();
        assert!(
            export_for_mirror(state.clone(), "s-completed", ExportMode::Snapshot)
                .await
                .is_err(),
            "completed missing history cannot be skipped"
        );
        let pause = state.chat.pause_commands("s-empty").await.unwrap();
        drop(pause);
        state
            .chat
            .command(
                "s-empty",
                serde_json::from_value::<AgentCommand>(
                    json!({"type":"send","text":"not echoed yet","blocks":[]}),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        assert!(state.chat.has_submitted_input("s-empty"));
        assert!(
            export_for_mirror(state.clone(), "s-empty", ExportMode::Snapshot)
                .await
                .is_err(),
            "accepted input cannot be skipped before its echo"
        );
        finish(root, state).await;
    }
}
