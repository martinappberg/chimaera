//! Opt-in actual pinned CLI + production frontend; synthetic Unix authority only.
//! The reviewed wrapper must establish local-only network/PID/mount containment
//! first. Ordinary tests never acquire a package, execute this CLI or use network.
#![cfg(target_os = "linux")]
use super::super::{
    provider_client::tests::request,
    provider_ready::{self, tests::Fixture},
};
use super::*;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    sync::oneshot,
};

// Mirror the fixed wrapper phases, never forward arbitrary stderr or CLI text.
fn refusal_phase(stderr: &[u8]) -> Option<&str> {
    const PHASES: &[&str] = &[
        "actual-tui-count-tokens",
        "artifact-hash",
        "artifact-selector",
        "artifact-type",
        "census-size",
        "child-deadline",
        "child-exit-deadline",
        "child-output",
        "child-spawn",
        "descendant-external-canary",
        "descendant-ipv6-canary",
        "drop-privileges",
        "external-canary",
        "group-cleanup",
        "inherited-fd-count",
        "inherited-network-fd",
        "interrupted",
        "ipv6-external-canary",
        "lock-type",
        "loopback-setup",
        "namespace-identity",
        "namespace-links",
        "namespace-routes",
        "namespace-test-receipt",
        "namespace-test-result",
        "namespace-uid",
        "official-cli-version",
        "official-header-receipt",
        "official-tests",
        "outer-address",
        "outer-canary-reached",
        "outer-platform",
        "role",
        "root-identity",
        "run-caps",
        "run-namespace",
        "run-no-new-privs",
        "run-status",
        "system-symlink",
        "tui-admission",
        "tui-auth",
        "tui-body-bound",
        "tui-cli-early-exit",
        "tui-complete-body",
        "tui-header-bound",
        "tui-length",
        "tui-messages-receipt",
        "tui-no-other-auth",
        "tui-no-count-tokens",
        "tui-no-messages",
        "tui-no-rendered-response",
        "tui-output-bound",
        "tui-recorder-cleanup",
        "tui-reply-bound",
        "tui-request-count",
        "tui-route",
        "tui-target",
        "unexpected",
    ];
    std::str::from_utf8(stderr).ok()?.lines().find_map(|line| {
        let phase = line.strip_prefix("contained Claude probe refused: ")?;
        PHASES.contains(&phase).then_some(phase)
    })
}

#[test]
fn diagnostic_forwarding_accepts_only_fixed_refusal_phases() {
    for phase in [
        "tui-cli-early-exit",
        "tui-no-messages",
        "tui-no-rendered-response",
        "tui-no-count-tokens",
    ] {
        let stderr = format!("contained Claude probe refused: {phase}\n");
        assert_eq!(refusal_phase(stderr.as_bytes()), Some(phase));
    }
    assert_eq!(
        refusal_phase(b"contained Claude probe refused: run-caps\n"),
        Some("run-caps")
    );
    assert_eq!(
        refusal_phase(
            b"raw CLI synthetic credential\ncontained Claude probe refused: secret-value\n"
        ),
        None
    );
    assert_eq!(
        refusal_phase(b"contained Claude probe refused: run-caps extra\n"),
        None
    );
    assert_eq!(
        refusal_phase(b"contained Claude probe refused: \xff\n"),
        None
    );
}

fn contained() {
    use std::io::Read;
    assert_eq!(
        std::env::var("CHIMAERA_TEST_CLAUDE_21287_CONTAINED").as_deref(),
        Ok("1"),
        "official CLI fixture requires the reviewed containment wrapper"
    );
    let read = |path: &str| {
        let mut value = String::with_capacity(65537);
        std::fs::File::open(path)
            .unwrap()
            .take(65537)
            .read_to_string(&mut value)
            .unwrap();
        assert!(value.len() <= 65536, "containment census exceeded bound");
        value
    };
    let status = read("/proc/self/status");
    for name in ["CapEff", "CapPrm", "CapInh", "CapAmb", "CapBnd"] {
        let value = status
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name}:")))
            .unwrap();
        assert_eq!(
            u64::from_str_radix(value.trim(), 16).unwrap(),
            0,
            "official fixture must have no namespace/network privileges"
        );
    }
    assert!(status
        .lines()
        .any(|line| line.starts_with("NoNewPrivs:") && line.ends_with('1')));
    let devices = read("/proc/net/dev");
    let devices: Vec<_> = devices
        .lines()
        .skip(2)
        .map(|line| line.split_once(':').unwrap().0.trim())
        .collect();
    assert_eq!(devices, ["lo"], "official fixture requires loopback only");
    for path in ["/proc/net/route", "/proc/net/ipv6_route"] {
        let routes = read(path);
        for line in routes.lines() {
            if line.starts_with("Iface") {
                continue;
            }
            let columns: Vec<_> = line.split_whitespace().collect();
            assert!(
                columns.first() == Some(&"lo") || columns.last() == Some(&"lo"),
                "official fixture has a non-loopback route"
            );
        }
    }
}

async fn frame(socket: &mut UnixStream, kind: u8, body: &[u8]) {
    let mut header = [kind, 0, 0, 0, 0];
    header[1..].copy_from_slice(&(body.len() as u32).to_be_bytes());
    socket.write_all(&header).await.unwrap();
    socket.write_all(body).await.unwrap();
}

async fn answer(socket: &mut UnixStream, req: &wire::Request) -> wire::ClaudeRoute {
    let wire::Command::ClaudeStreamPinned {
        route,
        content_length,
        headers,
    } = &req.command
    else {
        panic!("unexpected official CLI fixture command")
    };
    let (route, content_length) = (*route, *content_length);
    assert!(headers.validate(route).is_ok());
    assert!(content_length <= 1024 * 1024);
    let mut uploaded = Zeroizing::new(Vec::with_capacity(1024 * 1024));
    loop {
        let mut header = [0; 5];
        socket.read_exact(&mut header).await.unwrap();
        let header = wire::FrameHeader::decode(&header).unwrap();
        let mut bytes = Zeroizing::new(vec![0; header.length]);
        socket.read_exact(&mut bytes).await.unwrap();
        match header.kind {
            wire::FrameKind::RequestData => {
                assert!(uploaded.len() + bytes.len() <= content_length as usize);
                uploaded.extend_from_slice(&bytes);
            }
            wire::FrameKind::RequestEnd => {
                let end: wire::StreamEnd = serde_json::from_slice(&bytes).unwrap();
                end.validate(req, uploaded.len() as u64).unwrap();
                assert_eq!(uploaded.len() as u64, content_length);
                break;
            }
            _ => panic!("unexpected official CLI upload frame"),
        }
    }
    let body: Value = serde_json::from_slice(&uploaded).unwrap();
    let streaming = route == wire::ClaudeRoute::Messages
        && body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("claude-sonnet-4-6");
    let message = json!({"id":"msg_synthetic","type":"message","role":"assistant",
        "model":model,"content":[{"type":"text","text":"SYNTHETIC_OK"}],
        "stop_reason":"end_turn","stop_sequence":null,
        "usage":{"input_tokens":1,"output_tokens":1}});
    let body = if route == wire::ClaudeRoute::CountTokens {
        serde_json::to_vec(&json!({"input_tokens":1})).unwrap()
    } else if !streaming {
        serde_json::to_vec(&message).unwrap()
    } else {
        let mut start = message.clone();
        start["content"] = json!([]);
        start["stop_reason"] = Value::Null;
        start["usage"]["output_tokens"] = json!(0);
        let events = [
            (
                "message_start",
                json!({"type":"message_start","message":start}),
            ),
            (
                "content_block_start",
                json!({"type":"content_block_start","index":0,
                "content_block":{"type":"text","text":""}}),
            ),
            (
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,
                "delta":{"type":"text_delta","text":"SYNTHETIC_OK"}}),
            ),
            (
                "content_block_stop",
                json!({"type":"content_block_stop","index":0}),
            ),
            (
                "message_delta",
                json!({"type":"message_delta",
                "delta":{"stop_reason":"end_turn","stop_sequence":null},
                "usage":{"output_tokens":1}}),
            ),
            ("message_stop", json!({"type":"message_stop"})),
        ];
        events
            .into_iter()
            .map(|(name, data)| format!("event: {name}\ndata: {data}\n\n"))
            .collect::<String>()
            .into_bytes()
    };
    let response = wire::Response {
        version: 1,
        binding: req.binding.clone(),
        request_id: req.request_id.clone(),
        result: wire::Reply::ClaudeHead {
            head: wire::ClaudeHead {
                status: 200,
                headers: wire::Headers {
                    content_type: if streaming {
                        wire::ContentType::EventStream
                    } else {
                        wire::ContentType::Json
                    },
                    retry_after_seconds: None,
                },
            },
        },
    };
    frame(socket, 3, &wire::encode_control(&response).unwrap()).await;
    frame(socket, 4, &body).await;
    let end = wire::StreamEnd {
        version: 1,
        binding: req.binding.clone(),
        request_id: req.request_id.clone(),
        bytes: body.len() as u64,
    };
    frame(socket, 5, &wire::encode_control(&end).unwrap()).await;
    socket.shutdown().await.unwrap();
    route
}

async fn peer(fixture: &Fixture, mut stop: oneshot::Receiver<()>) -> Vec<wire::ClaudeRoute> {
    let mut routes = Vec::new();
    loop {
        tokio::select! {
            biased;
            _ = &mut stop => break,
            next = async {
                let (mut socket, req) = request(&fixture.listener).await;
                tokio::time::timeout(Duration::from_secs(5), answer(&mut socket, &req))
                    .await.expect("official CLI peer deadline")
            } => {
                assert!(routes.len() < 16, "official CLI effect count exceeded");
                routes.push(next);
            }
        }
    }
    routes
}

fn home(fixture: &Fixture) -> PathBuf {
    let home = fixture.root.join("official-home");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(
        home.join(".claude/.claude.json"),
        br#"{"hasCompletedOnboarding":true,"bypassPermissionsModeAccepted":true}"#,
    )
    .unwrap();
    home
}

#[tokio::test]
#[ignore = "exact pinned official CLI; requires reviewed local-only wrapper, never vendor inference"]
async fn official_print_uses_production_frontend_and_child_owner() {
    contained();
    let fixture = Fixture::new(Duration::from_secs(1));
    fixture.verified().await;
    let home = home(&fixture);
    let (sent, received) = oneshot::channel();
    let child = async {
        let result = exercise_at(
            &fixture.state,
            Zeroizing::new(b"Respond with SYNTHETIC_OK".to_vec()),
            PathBuf::from(CLI),
            home,
            Duration::from_secs(30),
        )
        .await;
        let _ = sent.send(());
        result
    };
    let (result, routes) = tokio::join!(child, peer(&fixture, received));
    assert_eq!(provider_ready::active(&fixture.state), 0);
    fixture.finish().await;
    let result = result.expect("actual pinned print did not complete");
    assert!(result.windows(12).any(|v| v == b"SYNTHETIC_OK"));
    assert!(routes.contains(&wire::ClaudeRoute::Messages));
}

#[tokio::test]
#[ignore = "exact pinned TUI/CountTokens/settings/config-FD; local-only wrapper required"]
async fn official_tui_counts_tokens_with_captured_config_and_ignored_hostile_settings() {
    contained();
    let fixture = Fixture::new(Duration::from_secs(1));
    fixture.verified().await;
    let home = home(&fixture);
    let config = Config::capture(&home).unwrap();
    let original = home.join("original-config");
    std::fs::rename(home.join(".claude"), &original).unwrap();
    std::fs::create_dir(home.join(".claude")).unwrap();
    let sentinel = fixture.root.join("settings-effect");
    let hostile = json!({"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:1",
        "ANTHROPIC_API_KEY":"synthetic-hostile-key", "CLAUDE_CODE_USE_BEDROCK":"1",
        "HTTPS_PROXY":"http://127.0.0.1:1"},
        "apiKeyHelper":format!("/usr/bin/touch {}", sentinel.display()),
        "hooks":{"SessionStart":[{"hooks":[{"type":"command",
            "command":format!("/usr/bin/touch {}",sentinel.display())}]}]}});
    std::fs::create_dir_all(fixture.root.join("project/.claude")).unwrap();
    for file in [
        original.join("settings.json"),
        home.join(".claude/settings.json"),
        fixture.root.join("project/.claude/settings.json"),
        fixture.root.join("project/.claude/settings.local.json"),
    ] {
        std::fs::write(file, serde_json::to_vec(&hostile).unwrap()).unwrap();
    }
    let child =
        ChildLifetime::new(&fixture.state, Instant::now() + Duration::from_secs(30)).unwrap();
    let observer = child.observer();
    let frontend = Frontend::start(child.clone()).await.unwrap();
    let (sent, received) = oneshot::channel();
    let run = async {
        let mut command = tokio::process::Command::new("/usr/bin/python3");
        command
            .args(["-I", "/probe/wrapper.py", "--tui-child"])
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .env("CLAUDE_CONFIG_DIR", config.path())
            .env("ANTHROPIC_BASE_URL", frontend.url())
            .env("CLAUDE_CODE_OAUTH_TOKEN", frontend.token())
            .env("CHIMAERA_TEST_CLAUDE_21287_CONTAINED", "1")
            .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
            .env("DISABLE_TELEMETRY", "1")
            .env("DISABLE_ERROR_REPORTING", "1")
            .current_dir(child.project_root().unwrap())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        config.inherit(&mut command);
        child.current().unwrap();
        child.process_pending(true);
        let result = match Child::spawn(&mut command) {
            Ok(mut process) => {
                let original = process.child.id();
                let stdout = process.child.stdout.take().unwrap();
                let stderr = process.child.stderr.take().unwrap();
                let result = child
                    .wait(async {
                        tokio::try_join!(bounded(stdout), bounded(stderr), async {
                            process.wait().await.map_err(|_| wire::Error::Unavailable)
                        })
                    })
                    .await;
                while process.terminate(original).await.is_err() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                result
            }
            Err(_) => Err(wire::Error::Unavailable),
        };
        drop(command);
        child.process_pending(false);
        let _ = sent.send(());
        result
    };
    let (result, routes) = tokio::join!(run, peer(&fixture, received));
    frontend.stop().await;
    drop(config);
    drop(observer);
    drop(child);
    assert_eq!(provider_ready::active(&fixture.state), 0);
    assert!(!sentinel.exists(), "disabled settings performed an effect");
    // No file may be written through the replacement configuration directory.
    assert_eq!(std::fs::read_dir(home.join(".claude")).unwrap().count(), 1);
    fixture.finish().await;
    let (output, errors, status) = result.unwrap().unwrap();
    if !status.success() {
        if let Some(phase) = refusal_phase(&errors) {
            eprintln!("contained Claude probe refused: {phase}");
        }
    }
    assert!(status.success(), "actual pinned TUI probe refused");
    let report: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(report["auth_frontend_only"], true);
    println!("\nCLAUDE_PROBE_HEADERS={}", report["headers"]);
    assert!(routes.contains(&wire::ClaudeRoute::Messages));
    assert!(
        routes.contains(&wire::ClaudeRoute::CountTokens),
        "actual CLI CountTokens not exercised"
    );
}
