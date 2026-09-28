use super::super::tests::{executable, fixture, preset};
use super::*;
use crate::agents::AgentKind;

#[test]
fn actions_reject_untrusted_origins_and_cancellation_cannot_be_resurrected() {
    let mut response = json!({"type":"chatgptDeviceCode","loginId":"abc-123","verificationUrl":"https://auth.openai.com/codex/device","userCode":"ABCD-1234"});
    assert!(device_action(&response).is_ok());
    for url in [
        "http://auth.openai.com/device",
        "https://auth.openai.com.evil.test/device",
        "https://user:secret@auth.openai.com/device",
        "https://auth.openai.com:444/device",
        "https://auth.openai.com/device#secret",
    ] {
        response["verificationUrl"] = json!(url);
        assert!(device_action(&response).is_err());
    }
    let (cancel, _receiver) = watch::channel(false);
    let attempt = Attempt {
        value: Mutex::new(Connection {
            id: "id".into(),
            provider_id: "codex".into(),
            phase: Phase::Waiting,
            expires_at: now() + 900,
            action: Some(Action::DeviceCode {
                verification_url: "https://auth.openai.com/device".into(),
                user_code: "ABCD".into(),
            }),
            error_code: None,
        }),
        cancel,
        finished: AtomicBool::new(false),
        session: Mutex::new(None),
        process: Mutex::new(None),
        input: Mutex::new(None),
    };
    attempt.cancel();
    attempt.update(Phase::Connected, None, None);
    assert_eq!(attempt.snapshot().phase, Phase::Canceled);
    assert!(attempt.snapshot().action.is_none());
}
async fn waiting(state: &AppState, id: &str) -> Connection {
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let c = crate::lock(&state.cloud_providers.connections)
                .get(id)
                .unwrap()
                .snapshot();
            if c.phase == Phase::Waiting {
                return c;
            }
            assert!(c.phase.pending(), "connection unexpectedly ended: {c:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn browser_connection_deduplicates_and_cancel_stops_owned_process_without_workspace() {
    let (root, state) = fixture("browser-connect");
    let cli = root.join("claude");
    executable(&cli, "#!/bin/sh\nif [ \"$1 $2\" = 'auth status' ]; then printf '%s' '{\"loggedIn\":false}'; exit 1; fi\n[ \"$1 $2 $3\" = 'auth login --claudeai' ] || exit 99\nprintf 'https://claude.com/cai/oauth/authorize?state=fixture\\nPaste code here if prompted > '\nexec sleep 300\n");
    preset(&state, AgentKind::Claude, cli);
    let def = super::super::definition("claude").unwrap();
    let first = start(state.clone(), def);
    assert_eq!(first.id, start(state.clone(), def).id);
    let c = waiting(&state, &first.id).await;
    assert!(matches!(
        c.action,
        Some(Action::Browser {
            input: "authorization_code",
            ..
        })
    ));
    assert!(state.sessions.list().is_empty());
    assert!(crate::lock(&state.workspaces).list().is_empty());
    let attempt = crate::lock(&state.cloud_providers.connections)
        .get(&first.id)
        .unwrap()
        .clone();
    let process = *crate::lock(&attempt.process);
    assert!(process.is_some_and(process::group_alive));
    attempt.cancel();
    assert_eq!(
        start(state.clone(), def).id,
        first.id,
        "cancel retains the writer until cleanup"
    );
    assert!(attempt.submit("fixture-code".into()).is_err());
    attempt.wait_finished().await;
    assert!(attempt.finished());
    assert!(!process.is_some_and(process::group_alive));
    assert_eq!(attempt.snapshot().phase, Phase::Canceled);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn browser_code_is_single_use_and_cli_confirmation_is_authoritative() {
    let (root, state) = fixture("browser-code");
    let cli = root.join("claude");
    let ready = root.join("ready");
    executable(
        &cli,
        &format!(
            r##"#!/bin/sh
if [ "$1 $2" = 'auth status' ]; then
    if [ -f '{ready}' ]; then printf '%s' '{{"loggedIn":true}}'; exit 0; fi
    printf '%s' '{{"loggedIn":false}}'; exit 1
fi
[ "$1 $2 $3" = 'auth login --claudeai' ] || exit 99
printf '\033]8;;https://claude.com/cai/oauth/authorize?state=fixture\007Sign in\033]8;;\007\nPaste code here if prompted > '
IFS= read -r code
[ "$code" = 'one-time-fixture#state' ] || exit 1
: > '{ready}'
"##,
            ready = ready.display()
        ),
    );
    preset(&state, AgentKind::Claude, cli);
    let c = start(state.clone(), super::super::definition("claude").unwrap());
    waiting(&state, &c.id).await;
    let attempt = crate::lock(&state.cloud_providers.connections)
        .get(&c.id)
        .unwrap()
        .clone();
    for invalid in ["", "line\nother", "space code", "zero\0", "carriage\r"] {
        assert!(attempt.submit(invalid.into()).is_err());
        assert_eq!(attempt.snapshot().phase, Phase::Waiting);
    }
    assert!(attempt.submit("a".repeat(4097)).is_err());
    attempt.submit("one-time-fixture#state".into()).unwrap();
    assert_eq!(attempt.snapshot().phase, Phase::Verifying);
    assert!(attempt.submit("one-time-fixture#state".into()).is_err());
    attempt.wait_finished().await;
    assert!(attempt.finished());
    assert_eq!(attempt.snapshot().phase, Phase::Connected);
    assert!(!serde_json::to_string(&attempt.snapshot())
        .unwrap()
        .contains("one-time-fixture"));
    assert!(state.sessions.list().is_empty());
    assert!(crate::lock(&state.workspaces).list().is_empty());
    assert!(crate::lock(&attempt.input).is_none());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn device_completion_requires_fresh_auth_confirmation() {
    let (root, state) = fixture("device-connect");
    let cli = root.join("codex");
    executable(
        &cli,
        &format!(
            r##"#!/bin/sh
while IFS= read -r line; do
case "$line" in
*'"method":"initialize"'*) printf '%s\n' '{{"id":1,"result":{{}}}}';;
*'"method":"account/read"'*)
if [ -f '{ready}' ]; then printf '%s\n' '{{"id":2,"result":{{"account":{{"type":"chatgpt","email":"private-fixture"}},"requiresOpenaiAuth":true}}}}'; else printf '%s\n' '{{"id":2,"result":{{"account":null,"requiresOpenaiAuth":true}}}}'; fi;;
*'"method":"account/login/start"'*)
printf '%s\n' '{{"id":2,"result":{{"type":"chatgptDeviceCode","loginId":"fixture-login","verificationUrl":"https://auth.openai.com/codex/device","userCode":"ABCD-1234"}}}}'
while [ ! -f '{ready}' ]; do sleep 0.1; done
printf '%s\n' '{{"method":"account/login/completed","params":{{"loginId":"fixture-login","success":true}}}}';;
esac
done
"##,
            ready = root.join("ready").display()
        ),
    );
    preset(&state, AgentKind::Codex, cli);
    let c = start(state.clone(), super::super::definition("codex").unwrap());
    let current = waiting(&state, &c.id).await;
    assert!(matches!(current.action, Some(Action::DeviceCode { .. })));
    std::fs::write(root.join("ready"), b"").unwrap();
    let final_state = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let c = crate::lock(&state.cloud_providers.connections)
                .get(&c.id)
                .unwrap()
                .snapshot();
            if !c.phase.pending() {
                break c;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(final_state.phase, Phase::Connected);
    assert!(final_state.action.is_none());
    assert!(!serde_json::to_string(&final_state)
        .unwrap()
        .contains("private-fixture"));
    std::fs::remove_dir_all(root).unwrap();
}
