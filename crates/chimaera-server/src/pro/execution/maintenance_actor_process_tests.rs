//! Real Linux actor/socket/child/parking path with synthetic account authority
//! and pinned-agent protocol. It does not establish a full namespace census or
//! the real provider CLI's resumability.
use super::{maintenance::tests::Fixture, maintenance_actor, maintenance_channel::Channel};
use crate::{lock, AppState};
use chimaera_agent::{claude::ClaudeAdapter, driver::SpawnSpec, model::AgentEvent};
use chimaera_core::project_secret_idle::*;
use std::{os::unix::fs::PermissionsExt, sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
const NATIVE: &str = "11111111-2222-4333-8444-555555555555";
const ID: &str = "s-actor-process";
const WAIT: Duration = Duration::from_secs(10);

struct Child(Arc<chimaera_agent::ChatManager>);
impl Drop for Child {
    fn drop(&mut self) {
        self.0.fence(ID);
    }
}
async fn wait(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(WAIT, async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
async fn spawn(fixture: &mut Fixture) -> Child {
    let root = lock(&fixture.state.pro.authority)
        .acknowledgment()
        .unwrap()
        .workspace_root;
    let state = Arc::get_mut(&mut fixture.state).unwrap();
    state.claude_projects_dir = state.pro.root.join("fixture-claude-projects");
    state.managed_root = state.pro.root.join("fixture-managed-runtime");
    state.legacy_managed_root = None;
    let bin = root.join("synthetic-claude");
    // Fixed local protocol only, no external executable/vendor/authentication.
    std::fs::write(&bin, format!(
        "#!/bin/sh\n\
         printf '%s\\n' '{{\"type\":\"control_response\",\"response\":{{\"subtype\":\"success\",\"request_id\":\"init\",\"response\":{{\"commands\":[]}}}}}}'\n\
         printf '%s\\n' '{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"{NATIVE}\",\"model\":\"synthetic\",\"permissionMode\":\"default\",\"slash_commands\":[]}}'\n\
         printf '%s\\n' '{{\"type\":\"assistant\",\"message\":{{\"id\":\"completed-message\",\"content\":[{{\"type\":\"text\",\"text\":\"synthetic completed turn\"}}]}}}}'\n\
         printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"{NATIVE}\",\"result\":\"done\",\"num_turns\":1}}'\n\
         cat >/dev/null\n")).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let transcript = state
        .claude_projects_dir
        .join(crate::launcher::encode_cwd(&root))
        .join(format!("{NATIVE}.jsonl"));
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(transcript, format!("{{\"type\":\"summary\",\"sessionId\":\"{NATIVE}\",\"summary\":\"retained native bytes\"}}\n")).unwrap();
    lock(&state.session_workspaces).insert(ID.into(), "w-a".into());
    let mut spec = SpawnSpec::new(ID, vec![bin.to_string_lossy().into_owned()], root);
    spec.managed_execution = true;
    spec.agent_version = Some(chimaera_agent::claude::TESTED_CLAUDE_VERSION.into());
    state.chat.spawn(&ClaudeAdapter, spec).unwrap();
    let child = Child(state.chat.clone());
    let attached = state.chat.attach(ID, 0).unwrap();
    let mut events = attached.live;
    if !attached
        .replay
        .iter()
        .any(|event| matches!(event.ev, AgentEvent::TurnCompleted { .. }))
    {
        tokio::time::timeout(WAIT, async {
            loop {
                if matches!(
                    events.recv().await.unwrap().ev,
                    AgentEvent::TurnCompleted { .. }
                ) {
                    break;
                }
            }
        })
        .await
        .unwrap();
    }
    assert!(state.chat.resumed_native_ready(ID, NATIVE).await.unwrap());
    assert!(crate::ledger::snapshot(state)
        .0
        .iter()
        .any(|entry| entry.id == ID
            && entry.agent.as_ref().unwrap().resume.as_deref() == Some(NATIVE)));
    child
}
async fn socket(fixture: &Fixture) -> (tokio::task::JoinHandle<()>, tokio::net::UnixStream) {
    fixture
        .state
        .stopping
        .store(false, std::sync::atomic::Ordering::Release);
    let (left, right) = std::os::unix::net::UnixStream::pair().unwrap();
    right.set_nonblocking(true).unwrap();
    let channel = Channel::from_inherited(
        &fixture.state,
        left.into(),
        fixture.binding.clone(),
        "A".repeat(43),
    )
    .unwrap();
    (
        tokio::spawn(maintenance_actor::run(fixture.state.clone(), channel)),
        tokio::net::UnixStream::from_std(right).unwrap(),
    )
}
async fn send(peer: &mut tokio::net::UnixStream, request: &Request) {
    let bytes = request.encode().unwrap();
    peer.write_all(&(bytes.len() as u32).to_be_bytes())
        .await
        .unwrap();
    peer.write_all(&bytes).await.unwrap();
}
async fn reply(peer: &mut tokio::net::UnixStream) -> Reply {
    tokio::time::timeout(WAIT, async {
        let size = peer.read_u32().await.unwrap() as usize;
        assert!(size <= REPLY_MAX);
        let mut bytes = vec![0; size];
        peer.read_exact(&mut bytes).await.unwrap();
        Reply::decode(&bytes).unwrap()
    })
    .await
    .unwrap()
}
fn stopped(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .unwrap()
        .lines()
        .any(|line| line.starts_with("State:") && line.contains('T'))
}
async fn cleanup(state: &AppState) {
    state.chat.fence(ID);
    wait(|| state.chat.process_group(ID).is_none() && !state.chat.get(ID).unwrap().alive).await;
    assert!(!state.chat.get(ID).unwrap().alive);
}
#[tokio::test]
async fn inherited_nonempty_actor_parks_durably_aborts_exact_leader_and_owns_channel_loss() {
    let mut fixture = Fixture::new().await;
    let _child = spawn(&mut fixture).await;
    let pid = fixture.state.chat.process_group(ID).unwrap();
    let head = fixture.state.chat.attach(ID, 0).unwrap().head_seq;
    let (task, mut peer) = socket(&fixture).await;
    assert!(matches!(reply(&mut peer).await, Reply::Ready(_)));
    let prepare = fixture.prepare();
    send(&mut peer, &Request::Prepare(prepare.clone())).await;
    let Reply::Prepared(prepared) = reply(&mut peer).await else {
        panic!("nonempty actor did not positively park")
    };
    assert_eq!(prepared.leaders.len(), 1);
    assert_eq!(prepared.leaders[0].session_id, ID);
    assert_eq!(prepared.leaders[0].namespace_pid, pid);
    assert!(prepared.leaders[0].start_ticks > 0 && stopped(pid));
    let record = super::maintenance_store::read(&fixture.state)
        .unwrap()
        .unwrap();
    assert_eq!(
        record.sessions().unwrap()[0]
            .agent
            .as_ref()
            .unwrap()
            .resume
            .as_deref(),
        Some(NATIVE)
    );
    assert!(lock(&fixture.state.ledger)
        .load_boot()
        .sessions
        .iter()
        .any(|entry| entry.id == ID
            && entry.manual_resume_reason.as_deref() == Some("project_secrets_idle")));
    assert!(!crate::ws::session_writable(&fixture.state, ID));
    assert!(fixture
        .state
        .chat
        .command(ID, chimaera_agent::model::AgentCommand::Interrupt)
        .await
        .is_err());
    let abort = Abort {
        version: 1,
        request_id: 2,
        binding: prepare.binding.clone(),
        attempt_id: prepare.attempt_id.clone(),
        operation_id: prepare.operation_id.clone(),
        pending_id: prepare.pending_id.clone(),
        expected_applied_revision: prepare.expected_applied_revision,
        fence_id: prepared.fence_id,
    };
    send(&mut peer, &Request::Abort(abort)).await;
    assert!(matches!(reply(&mut peer).await, Reply::Aborted(_)));
    assert_eq!(fixture.state.chat.process_group(ID), Some(pid));
    assert!(!stopped(pid));
    assert!(crate::ws::session_writable(&fixture.state, ID));
    assert_eq!(fixture.state.chat.attach(ID, 0).unwrap().head_seq, head);
    assert!(super::maintenance_store::read(&fixture.state)
        .unwrap()
        .is_none());
    assert!(lock(&fixture.state.ledger)
        .load_boot()
        .sessions
        .iter()
        .any(|entry| entry.id == ID && entry.manual_resume_reason.is_none()));
    let mut next = prepare;
    next.request_id = 3;
    next.attempt_id = "44444444-4444-4444-8444-444444444444".into();
    send(&mut peer, &Request::Prepare(next)).await;
    assert!(matches!(reply(&mut peer).await, Reply::Prepared(_)));
    drop(peer);
    tokio::time::timeout(WAIT, task).await.unwrap().unwrap();
    assert_eq!(fixture.state.chat.process_group(ID), Some(pid));
    assert!(!stopped(pid));
    assert!(super::maintenance_store::read(&fixture.state)
        .unwrap()
        .is_none());
    cleanup(&fixture.state).await;
}
#[tokio::test]
async fn inherited_nonempty_actor_authority_loss_never_thaws_and_keeps_durable_recovery() {
    let mut fixture = Fixture::new().await;
    let _child = spawn(&mut fixture).await;
    let pid = fixture.state.chat.process_group(ID).unwrap();
    let (task, mut peer) = socket(&fixture).await;
    assert!(matches!(reply(&mut peer).await, Reply::Ready(_)));
    send(&mut peer, &Request::Prepare(fixture.prepare())).await;
    assert!(matches!(reply(&mut peer).await, Reply::Prepared(_)));
    fixture
        .state
        .pro
        .generation
        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    drop(peer);
    tokio::time::timeout(WAIT, task).await.unwrap().unwrap();
    assert!(stopped(pid));
    assert!(!crate::pro::may_execute(&fixture.state, "w-a"));
    assert!(!crate::ws::session_writable(&fixture.state, ID));
    assert!(super::maintenance_store::read(&fixture.state)
        .unwrap()
        .is_some());
    assert!(lock(&fixture.state.deferred_sessions)
        .get(ID)
        .unwrap()
        .manual_resume_reason
        .is_some());
    cleanup(&fixture.state).await;
}
