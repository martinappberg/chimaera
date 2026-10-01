//! Explicitly opt-in ACP verification. The binaries come from official releases;
//! each test uses a throwaway workspace and the user's existing agent login.
use chimaera_agent::{
    acp::{AcpAdapter, ANTIGRAVITY, GROK},
    driver::SpawnSpec,
    journal::SeqEvent,
    model::{AgentCommand, AgentEvent, ContentBlock, PermissionOptionKind, UserMessageState},
    ChatManager,
};
use std::{path::PathBuf, sync::Arc, time::Duration};

async fn exercise(adapter: AcpAdapter, env: &str, args: &[&str]) {
    let bin = std::env::var(env).expect("set the ACP test executable path");
    let dir = tempfile::tempdir().unwrap();
    let manager = Arc::new(ChatManager::new(
        dir.path().join("journal"),
        Box::new(|_, _| {}),
        Box::new(|_, _| {}),
    ));
    let argv: Vec<_> = std::iter::once(bin)
        .chain(args.iter().map(|s| s.to_string()))
        .collect();
    let mut spec = SpawnSpec::new("acp", argv.clone(), dir.path().to_path_buf());
    spec.handshake_timeout = Duration::from_secs(60);
    spec.portable_context = Some("Historical conversation copied from another agent: the user asked to remember the word copper. This is context only; wait for the next user message.".into());
    manager.spawn(&adapter, spec).unwrap();
    let mut rx = tokio::task::block_in_place(|| manager.attach("acp", 0))
        .unwrap()
        .live;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(150);
    loop {
        let e = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .unwrap()
            .unwrap();
        match &e.ev {
            AgentEvent::Capabilities { .. } => break,
            AgentEvent::Error {
                message,
                fatal: true,
            } => panic!("startup: {message}"),
            _ => {}
        }
    }
    // Opening a fork must not synthesize a user turn or spend tokens.
    let quiet = tokio::time::Instant::now() + Duration::from_millis(400);
    while let Ok(Ok(e)) = tokio::time::timeout_at(quiet, rx.recv()).await {
        assert!(!matches!(
            &e.ev,
            AgentEvent::TurnStarted { .. } | AgentEvent::UserMessage { .. }
        ));
    }
    let mut last_seq = 0;
    for prompt in [
        "What word did I ask you to remember in the prior conversation? Reply with only that word. Do not use tools.",
        "What word did I ask you to remember? Reply with only that word. Do not use tools.",
    ] {
        manager
            .command(
                "acp",
                AgentCommand::Send {
                    blocks: vec![ContentBlock::Text {
                        text: prompt.into(),
                    }],
                },
            )
            .await
            .unwrap();
        let mut reply = String::new();
        loop {
            let e = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(e.seq > last_seq);
            last_seq = e.seq;
            match &e.ev {
                AgentEvent::MessageChunk { text, .. } => reply.push_str(text),
                AgentEvent::TurnCompleted { .. } => break,
                AgentEvent::TurnAborted { reason, .. }
                | AgentEvent::Error {
                    message: reason,
                    fatal: true,
                } => panic!("turn: {reason}"),
                _ => {}
            }
        }
        assert!(reply.to_lowercase().contains("copper"), "{reply}");
    }
    let native = manager.get("acp").unwrap().native_session_id.unwrap();
    let replay = tokio::task::block_in_place(|| manager.attach("acp", 0)).unwrap();
    assert!(replay
        .replay
        .iter()
        .any(|e| matches!(&e.ev, AgentEvent::MessageChunk { .. })));
    assert!(
        tokio::task::block_in_place(|| manager.attach("acp", last_seq))
            .unwrap()
            .replay
            .is_empty()
    );
    manager.kill("acp");
    while manager.get("acp").unwrap().alive {
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(tokio::time::Instant::now() < deadline);
    }
    manager.remove("acp");
    let mut spec = SpawnSpec::new("acp", argv, PathBuf::from(dir.path()));
    spec.pinned_native_id = Some(native.clone());
    spec.handshake_timeout = Duration::from_secs(60);
    manager.spawn(&adapter, spec).unwrap();
    let mut rx = tokio::task::block_in_place(|| manager.attach("acp", last_seq))
        .unwrap()
        .live;
    loop {
        let e = tokio::time::timeout(Duration::from_secs(90), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match &e.ev {
            AgentEvent::Init {
                native_session_id, ..
            } => {
                assert_eq!(native_session_id, &native);
                break;
            }
            AgentEvent::Error {
                message,
                fatal: true,
            } => panic!("resume: {message}"),
            _ => {}
        }
    }
    manager.command("acp",AgentCommand::Send{blocks:vec![ContentBlock::Text{text:"What word did I ask you to remember? Reply with only that word. Do not use tools.".into()}]}).await.unwrap();
    let mut reply = String::new();
    loop {
        let e = tokio::time::timeout(Duration::from_secs(90), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match &e.ev {
            AgentEvent::MessageChunk { text, .. } => reply.push_str(text),
            AgentEvent::TurnCompleted { .. } => break,
            AgentEvent::TurnAborted { reason, .. } => panic!("resume turn: {reason}"),
            _ => {}
        }
    }
    assert!(
        reply.to_lowercase().contains("copper"),
        "resume lost context: {reply}"
    );
    exercise_controls(&manager, &mut rx, dir.path()).await;
    manager.kill("acp");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while manager.get("acp").unwrap().alive {
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(tokio::time::Instant::now() < deadline);
    }
}

async fn until(
    rx: &mut tokio::sync::broadcast::Receiver<Arc<SeqEvent>>,
    predicate: impl Fn(&AgentEvent) -> bool,
) -> AgentEvent {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        let entry = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .unwrap()
            .unwrap();
        if predicate(&entry.ev) {
            return entry.ev.clone();
        }
        if let AgentEvent::Error {
            message,
            fatal: true,
        } = &entry.ev
        {
            panic!("{message}");
        }
    }
}
async fn send(manager: &ChatManager, text: &str) {
    manager
        .command(
            "acp",
            AgentCommand::Send {
                blocks: vec![ContentBlock::Text { text: text.into() }],
            },
        )
        .await
        .unwrap();
}
async fn exercise_controls(
    manager: &ChatManager,
    rx: &mut tokio::sync::broadcast::Receiver<Arc<SeqEvent>>,
    cwd: &std::path::Path,
) {
    // Model changes use acknowledged readback, even when selecting the current model.
    let model = manager.get("acp").unwrap().model.unwrap();
    manager
        .command(
            "acp",
            AgentCommand::SetModel {
                model_id: model.clone(),
            },
        )
        .await
        .unwrap();
    let changed = until(rx, |e| {
        matches!(
            e,
            AgentEvent::ModelSwitched { .. } | AgentEvent::Error { .. }
        )
    })
    .await;
    assert!(matches!(changed, AgentEvent::ModelSwitched { to, .. } if to == model));

    for choice in ["allow", "deny", "stop"] {
        let name = format!("{choice}.txt");
        send(manager, &format!("Use your file-writing tool to create {name} in the current directory containing only copper. Do not use shell commands or delegate. If denied or cancelled, stop without retrying.")).await;
        let ask = until(rx, |e| {
            matches!(
                e,
                AgentEvent::PermissionRequest { .. }
                    | AgentEvent::TurnCompleted { .. }
                    | AgentEvent::TurnAborted { .. }
            )
        })
        .await;
        let AgentEvent::PermissionRequest {
            request_id,
            options,
            ..
        } = ask
        else {
            panic!("Expected approval before writing {name}: {ask:?}");
        };
        if choice == "allow" {
            // Exercise queue cancellation while the provider is held at approval.
            send(manager, "This queued message must be cancelled.").await;
            let queued = until(rx, |e| {
                matches!(e, AgentEvent::UserMessage { queued: true, .. })
            })
            .await;
            let AgentEvent::UserMessage { id: Some(id), .. } = queued else {
                panic!();
            };
            manager
                .command("acp", AgentCommand::CancelQueued { id: id.clone() })
                .await
                .unwrap();
            until(rx, |e| matches!(e, AgentEvent::UserMessageUpdate { id: key, state: UserMessageState::Cancelled } if key == &id)).await;
            send(manager, "Reply with exactly copper. Do not use tools.").await;
            until(rx, |e| {
                matches!(e, AgentEvent::UserMessage { queued: true, .. })
            })
            .await;
        }
        if choice == "stop" {
            manager
                .command("acp", AgentCommand::Interrupt)
                .await
                .unwrap();
        } else {
            let kind = if choice == "allow" {
                PermissionOptionKind::AllowOnce
            } else {
                PermissionOptionKind::RejectOnce
            };
            let option_id = options.iter().find(|o| o.kind == kind).unwrap().id.clone();
            manager
                .command(
                    "acp",
                    AgentCommand::Permission {
                        request_id,
                        option_id,
                        feedback: None,
                        destination: None,
                    },
                )
                .await
                .unwrap();
        }
        let ended = until(rx, |e| {
            matches!(
                e,
                AgentEvent::TurnCompleted { .. } | AgentEvent::TurnAborted { .. }
            )
        })
        .await;
        if choice == "allow" {
            assert!(matches!(ended, AgentEvent::TurnCompleted { .. }));
            assert_eq!(
                std::fs::read_to_string(cwd.join(&name)).unwrap().trim(),
                "copper"
            );
            let mut reply = String::new();
            loop {
                let e = until(rx, |e| {
                    matches!(
                        e,
                        AgentEvent::MessageChunk { .. }
                            | AgentEvent::TurnCompleted { .. }
                            | AgentEvent::TurnAborted { .. }
                    )
                })
                .await;
                match e {
                    AgentEvent::MessageChunk { text, .. } => reply.push_str(&text),
                    AgentEvent::TurnCompleted { .. } => break,
                    _ => panic!("queued turn failed: {e:?}"),
                }
            }
            assert!(reply.to_lowercase().contains("copper"), "{reply}");
        } else {
            assert!(
                !cwd.join(&name).exists(),
                "{choice} unexpectedly wrote a file"
            );
            if choice == "stop" {
                assert!(matches!(
                    ended,
                    AgentEvent::TurnAborted {
                        interrupted: true,
                        ..
                    }
                ));
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: needs Antigravity login and bills short chat and approval turns"]
async fn antigravity_stream_replay_and_resume() {
    let args: &[&str] = if cfg!(target_os = "linux") {
        &["--uid="]
    } else {
        &[]
    };
    exercise(ANTIGRAVITY, "CHIMAERA_TEST_AGY_ACP", args).await;
}
#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: needs Grok login and bills short chat and approval turns"]
async fn grok_stream_replay_and_resume() {
    exercise(
        GROK,
        "CHIMAERA_TEST_GROK",
        &["agent", "--no-leader", "stdio"],
    )
    .await;
}
