use super::super::tests::{executable, fixture, preset, preset_github};
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
            operation: Operation::Connect,
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
    let first = start(state.clone(), def).unwrap();
    assert_eq!(first.id, start(state.clone(), def).unwrap().id);
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
        start(state.clone(), def).unwrap().id,
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
    let c = start(state.clone(), super::super::definition("claude").unwrap()).unwrap();
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
    // Half a pasted code keeps the attempt waiting for the whole one.
    for partial in ["one-time-fixture", "one-time-fixture#", "#state"] {
        assert_eq!(
            attempt.submit(partial.into()),
            Err("authorization_code_incomplete")
        );
        assert_eq!(attempt.snapshot().phase, Phase::Waiting);
    }
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
    let c = start(state.clone(), super::super::definition("codex").unwrap()).unwrap();
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

async fn finished(state: &AppState, id: &str) -> Connection {
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let attempt = crate::lock(&state.cloud_providers.connections)
                .get(id)
                .unwrap()
                .clone();
            if attempt.finished() {
                break attempt.snapshot();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn disconnect_is_serialized_idempotent_and_invalidates_cached_readiness() {
    let (root, state) = fixture("disconnect");
    let cli = root.join("claude");
    executable(
        &cli,
        &format!(
            r##"#!/bin/sh
case "$1 $2" in
'auth status') if [ -f '{ready}' ]; then printf '%s' '{{"loggedIn":true}}'; else printf '%s' '{{"loggedIn":false}}'; exit 1; fi;;
'auth logout') printf x >> '{log}'; sleep 0.2; rm -f '{ready}'; printf '%s' 'private-fixture';;
*) exit 99;;
esac
"##,
            ready = root.join("ready").display(),
            log = root.join("logouts").display()
        ),
    );
    std::fs::write(root.join("ready"), b"synthetic-credential").unwrap();
    preset(&state, AgentKind::Claude, cli);
    let def = super::super::definition("claude").unwrap();
    assert_eq!(
        readiness(&state, &["claude".into()], false).await[0].state,
        ProviderState::SignedIn
    );
    let request = start_disconnect(state.clone(), def).unwrap();
    assert_eq!(start_disconnect(state.clone(), def).unwrap().id, request.id);
    assert_eq!(pending_disconnect(&state).unwrap().id, request.id);
    assert_eq!(start(state.clone(), def).unwrap_err(), "provider_busy");
    let attempt = crate::lock(&state.cloud_providers.connections)
        .get(&request.id)
        .unwrap()
        .clone();
    attempt.cancel();
    assert!(
        attempt.snapshot().phase.pending(),
        "disconnect cannot pretend cancellation undoes logout"
    );
    assert_eq!(
        readiness(&state, &["claude".into()], true).await[0].state,
        ProviderState::Unknown
    );
    let completed = finished(&state, &request.id).await;
    assert_eq!(completed.phase, Phase::Disconnected);
    assert_eq!(completed.operation, Operation::Disconnect);
    assert!(pending_disconnect(&state).is_none());
    assert!(!serde_json::to_string(&completed)
        .unwrap()
        .contains("private-fixture"));
    assert_eq!(
        readiness(&state, &["claude".into()], false).await[0].state,
        ProviderState::NeedsSignIn
    );
    let repeated = start_disconnect(state.clone(), def).unwrap();
    assert_eq!(
        finished(&state, &repeated.id).await.phase,
        Phase::Disconnected
    );
    assert_eq!(std::fs::read(root.join("logouts")).unwrap(), b"xx");
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn logout_success_without_authoritative_negative_is_not_disconnected() {
    for status in [r#"{"loggedIn":true}"#, r#"{"error":"private-fixture"}"#] {
        let (root, state) = fixture("disconnect-proof");
        let cli = root.join("claude");
        executable(&cli, &format!("#!/bin/sh\ncase \"$1 $2\" in\n'auth status') printf '%s' '{status}';;\n'auth logout') exit 0;;\n*) exit 99;;\nesac\n"));
        preset(&state, AgentKind::Claude, cli);
        let request =
            start_disconnect(state.clone(), super::super::definition("claude").unwrap()).unwrap();
        let completed = finished(&state, &request.id).await;
        assert_eq!(completed.phase, Phase::Failed);
        assert_eq!(
            completed.error_code.as_deref(),
            Some("disconnect_not_confirmed")
        );
        assert!(!serde_json::to_string(&completed)
            .unwrap()
            .contains("private-fixture"));
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn codex_disconnect_uses_official_logout_and_a_new_readiness_probe() {
    let (root, state) = fixture("codex-disconnect");
    let cli = root.join("codex");
    executable(
        &cli,
        &format!(
            r##"#!/bin/sh
while IFS= read -r line; do
case "$line" in
*'"method":"initialize"'*) printf '%s\n' '{{"id":1,"result":{{}}}}';;
*'"method":"account/read"'*)
if [ -f '{ready}' ]; then printf '%s\n' '{{"id":2,"result":{{"account":{{"type":"chatgpt"}},"requiresOpenaiAuth":true}}}}'; else printf '%s\n' '{{"id":2,"result":{{"account":null,"requiresOpenaiAuth":true}}}}'; fi;;
*'"method":"account/logout"'*) rm -f '{ready}'; printf x >> '{log}'; printf '%s\n' '{{"id":2,"result":{{}}}}';;
*'"method":"account/login/start"'*) exit 99;;
esac
done
"##,
            ready = root.join("ready").display(),
            log = root.join("logouts").display()
        ),
    );
    std::fs::write(root.join("ready"), b"synthetic-credential").unwrap();
    preset(&state, AgentKind::Codex, cli);
    let request =
        start_disconnect(state.clone(), super::super::definition("codex").unwrap()).unwrap();
    assert_eq!(
        finished(&state, &request.id).await.phase,
        Phase::Disconnected
    );
    assert_eq!(std::fs::read(root.join("logouts")).unwrap(), b"x");
    assert!(state.sessions.list().is_empty());
    assert!(crate::lock(&state.workspaces).list().is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn github_disconnect_removes_each_named_account_and_requires_an_empty_store() {
    let (root, state) = fixture("github-disconnect");
    let cli = root.join("gh-fixture");
    executable(
        &cli,
        r##"#!/bin/sh
case "$1 $2" in
'auth status')
  [ "$3 $4 $5 $6" = '--hostname github.com --json hosts' ] || exit 97
  if [ -f "$HOME/removed-b" ]; then
    printf '%s' '{"hosts":{}}'
  else
    printf '%s' '{"hosts":{"github.com":[{"login":"fixture-a","tokenSource":"keyring","state":"success"},{"login":"fixture-b","tokenSource":"keyring","state":"invalid-token"}]}}'
  fi;;
'auth logout')
  [ "$3 $4 $5" = '--hostname github.com --user' ] || exit 98
  case "$6" in
    fixture-a) touch "$HOME/removed-a";;
    fixture-b) [ -f "$HOME/removed-a" ] || exit 99; touch "$HOME/removed-b";;
    *) exit 96;;
  esac;;
*) exit 95;;
esac
"##,
    );
    let (cancel, _) = watch::channel(false);
    let attempt = Attempt {
        value: Mutex::new(Connection {
            id: "fixture".into(),
            provider_id: "github".into(),
            operation: Operation::Disconnect,
            phase: Phase::Preparing,
            expires_at: now() + 60,
            action: None,
            error_code: None,
        }),
        cancel,
        finished: AtomicBool::new(false),
        session: Mutex::new(None),
        process: Mutex::new(None),
        input: Mutex::new(None),
    };
    let def = super::super::definition("github").unwrap();
    assert!(
        super::super::disconnect::run_with_bin(&state, def, &attempt, &cli)
            .await
            .is_ok()
    );
    assert!(super::super::home(&state).join("removed-a").exists());
    assert!(super::super::home(&state).join("removed-b").exists());
    // An already empty store is a successful no-op, not an interactive logout.
    assert!(
        super::super::disconnect::run_with_bin(&state, def, &attempt, &cli)
            .await
            .is_ok()
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn codex_external_auth_after_logout_remains_unknown() {
    let (root, state) = fixture("codex-external-disconnect");
    let cli = root.join("codex");
    executable(
        &cli,
        r##"#!/bin/sh
while IFS= read -r line; do
case "$line" in
*'"method":"initialize"'*) printf '%s\n' '{"id":1,"result":{}}';;
*'"method":"account/logout"'*) printf '%s\n' '{"id":2,"result":{}}';;
*'"method":"account/read"'*) printf '%s\n' '{"id":2,"result":{"account":null,"requiresOpenaiAuth":false}}';;
esac
done
"##,
    );
    preset(&state, AgentKind::Codex, cli);
    let request =
        start_disconnect(state.clone(), super::super::definition("codex").unwrap()).unwrap();
    let completed = finished(&state, &request.id).await;
    assert_eq!(completed.phase, Phase::Failed);
    assert_eq!(
        completed.error_code.as_deref(),
        Some("disconnect_not_confirmed")
    );
    let status = super::super::readiness(&state, &["codex".into()], true).await;
    assert_eq!(status[0].state, super::super::ProviderState::Unknown);
    std::fs::remove_dir_all(root).unwrap();
}

/// An uncertain cleanup keeps the provider reserved only while the old
/// process group lives; once it is gone the provider is released without a
/// daemon restart.
#[cfg(unix)]
#[tokio::test]
async fn an_uncertain_cleanup_releases_the_provider_once_the_old_group_is_gone() {
    let (root, state) = fixture("cleanup-release");
    let mut command = tokio::process::Command::new("/bin/sleep");
    command.arg("30").process_group(0).kill_on_drop(true);
    let mut child = command.spawn().unwrap();
    let group = child.id().unwrap();
    let (cancel, _receiver) = watch::channel(false);
    let attempt = Arc::new(Attempt {
        value: Mutex::new(Connection {
            id: "id".into(),
            operation: Operation::Connect,
            provider_id: "github".into(),
            phase: Phase::Canceled,
            expires_at: now() + 900,
            action: None,
            error_code: None,
        }),
        cancel,
        finished: AtomicBool::new(false),
        session: Mutex::new(None),
        process: Mutex::new(Some(group)),
        input: Mutex::new(None),
    });
    let release = {
        let (state, attempt) = (state.clone(), attempt.clone());
        tokio::spawn(async move {
            release_when_gone(
                &state,
                &attempt,
                None,
                Some(group),
                Duration::from_millis(20),
            )
            .await
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        attempt.snapshot().error_code.as_deref(),
        Some("cleanup_failed")
    );
    assert!(
        !attempt.finished(),
        "the provider stays reserved while the group lives"
    );
    child.kill().await.unwrap();
    let _ = child.wait().await;
    tokio::time::timeout(Duration::from_secs(2), release)
        .await
        .unwrap()
        .unwrap();
    assert!(attempt.finished());
    std::fs::remove_dir_all(root).unwrap();
}

/// A fake GitHub CLI: signed out until its login writes `$HOME/signed-in`.
/// Every call is recorded in `$HOME/calls`; `help` is its `auth login --help`
/// and `login` the body run for the exact login command it expects.
fn fake_github(root: &std::path::Path, help: &str, login: &str, args: &str) -> std::path::PathBuf {
    let cli = root.join("gh-fixture");
    executable(
        &cli,
        &format!(
            r##"#!/bin/sh
printf '%s\n' "$*" >> "$HOME/calls"
case "$*" in
'auth status --active --hostname github.com --json hosts')
  if [ -f "$HOME/signed-in" ]; then printf '%s' '{{"hosts":{{"github.com":[{{"active":true,"state":"success","login":"private-fixture"}}]}}}}'
  else printf '%s' '{{"hosts":{{}}}}'; fi;;
'auth login --help') printf '%s\n' '{help}';;
'{args}')
  [ "$GH_PROMPT_DISABLED" = 1 ] || exit 94
{login};;
'auth setup-git --hostname github.com') printf x >> "$HOME/git-setup";;
*) exit 95;;
esac
"##
        ),
    );
    cli
}
const GITHUB_LOGIN: &str = "auth login --hostname github.com --git-protocol https --web";
const GITHUB_CODE: &str = r##"  printf '%s\n' 'profile noise'
  printf '! First copy your one-time code: ABCD-1234\n' >&2
  printf 'Open this URL to continue in your web browser: https://github.com/login/device\n' >&2"##;

fn attempt(state: &AppState, id: &str) -> Arc<Attempt> {
    crate::lock(&state.cloud_providers.connections)
        .get(id)
        .unwrap()
        .clone()
}

#[tokio::test]
async fn github_signs_in_with_a_one_time_code_and_sets_up_git_without_a_terminal() {
    let (root, state) = fixture("github-connect");
    let home = super::super::home(&state);
    let cli = fake_github(
        &root,
        "  --skip-ssh-key   Skip generate/upload SSH key prompt",
        &format!(
            "{GITHUB_CODE}\n  while [ ! -f \"$HOME/approved\" ]; do sleep 0.1; done\n  : > \"$HOME/signed-in\"\n  printf '%s\\n' 'Logged in as private-fixture' >&2"
        ),
        &format!("{GITHUB_LOGIN} --skip-ssh-key"),
    );
    preset_github(&state, cli);
    let def = super::super::definition("github").unwrap();
    let first = start(state.clone(), def).unwrap();
    let current = waiting(&state, &first.id).await;
    match current.action {
        Some(Action::DeviceCode {
            verification_url,
            user_code,
        }) => {
            assert_eq!(verification_url, "https://github.com/login/device");
            assert_eq!(user_code, "ABCD-1234");
        }
        other => panic!("expected a one-time code, got {other:?}"),
    }
    assert_eq!(start(state.clone(), def).unwrap().id, first.id);
    assert!(state.sessions.list().is_empty());
    assert!(crate::lock(&state.workspaces).list().is_empty());
    std::fs::write(home.join("approved"), b"").unwrap();
    let done = finished(&state, &first.id).await;
    assert_eq!(done.phase, Phase::Connected);
    assert!(done.action.is_none() && done.error_code.is_none());
    assert!(!serde_json::to_string(&done)
        .unwrap()
        .contains("private-fixture"));
    assert_eq!(std::fs::read(home.join("git-setup")).unwrap(), b"x");
    // Connecting an already signed-in CLI still makes it Git's helper.
    let again = start(state.clone(), def).unwrap();
    assert_ne!(again.id, first.id);
    assert_eq!(finished(&state, &again.id).await.phase, Phase::Connected);
    assert_eq!(std::fs::read(home.join("git-setup")).unwrap(), b"xx");
    assert!(state.sessions.list().is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_declined_or_expired_github_code_is_a_plain_failure() {
    let (root, state) = fixture("github-declined");
    let home = super::super::home(&state);
    let cli = fake_github(
        &root,
        "",
        &format!(
            "{GITHUB_CODE}\n  while [ ! -f \"$HOME/declined\" ]; do sleep 0.1; done\n  printf '%s\\n' 'SECRET raw failure' >&2\n  exit 1"
        ),
        GITHUB_LOGIN,
    );
    preset_github(&state, cli);
    let c = start(state.clone(), super::super::definition("github").unwrap()).unwrap();
    waiting(&state, &c.id).await;
    std::fs::write(home.join("declined"), b"").unwrap();
    let done = finished(&state, &c.id).await;
    assert_eq!(done.phase, Phase::Failed);
    assert_eq!(done.error_code.as_deref(), Some("sign_in_failed"));
    assert!(done.action.is_none());
    assert!(!serde_json::to_string(&done).unwrap().contains("SECRET"));
    assert!(!home.join("git-setup").exists());
    // Without the flag in its help, the CLI is not handed one.
    let calls = std::fs::read_to_string(home.join("calls")).unwrap();
    assert!(calls.lines().any(|line| line == GITHUB_LOGIN));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_github_cli_that_shows_no_code_fails_closed_without_a_terminal() {
    for (label, body, code) in [
        // Exits without a code (an environment token, an old CLI, no network).
        (
            "github-exits",
            "  printf '%s\\n' 'SECRET raw failure' >&2\n  exit 1",
            "sign_in_unavailable",
        ),
        // Never prints one.
        (
            "github-stalls",
            "  printf '%s\\n' 'Welcome ABCD-1234'\n  exec sleep 300",
            "sign_in_unavailable",
        ),
        // Reports success without signing in: the status check decides.
        ("github-code-exits", GITHUB_CODE, "sign_in_not_confirmed"),
    ] {
        let (root, state) = fixture(label);
        let cli = fake_github(&root, "", body, GITHUB_LOGIN);
        preset_github(&state, cli);
        let c = start(state.clone(), super::super::definition("github").unwrap()).unwrap();
        let done = finished(&state, &c.id).await;
        assert_eq!(done.phase, Phase::Failed, "{label}");
        assert_eq!(done.error_code.as_deref(), Some(code), "{label}");
        assert!(done.action.is_none());
        assert!(!serde_json::to_string(&done).unwrap().contains("SECRET"));
        let process = *crate::lock(&attempt(&state, &c.id).process);
        assert!(!process.is_some_and(process::group_alive), "{label}");
        assert!(state.sessions.list().is_empty());
        assert!(crate::lock(&state.workspaces).list().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn canceling_github_sign_in_stops_its_cli_and_keeps_the_writer_until_cleanup() {
    let (root, state) = fixture("github-cancel");
    let cli = fake_github(
        &root,
        "",
        &format!("{GITHUB_CODE}\n  exec sleep 300"),
        GITHUB_LOGIN,
    );
    preset_github(&state, cli);
    let def = super::super::definition("github").unwrap();
    let c = start(state.clone(), def).unwrap();
    waiting(&state, &c.id).await;
    let attempt = attempt(&state, &c.id);
    let process = *crate::lock(&attempt.process);
    assert!(process.is_some_and(process::group_alive));
    attempt.cancel();
    assert!(attempt.snapshot().action.is_none());
    assert_eq!(start(state.clone(), def).unwrap().id, c.id);
    attempt.wait_finished().await;
    assert!(attempt.finished());
    assert_eq!(attempt.snapshot().phase, Phase::Canceled);
    assert!(!process.is_some_and(process::group_alive));
    assert!(!super::super::home(&state).join("git-setup").exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_github_cli_waiting_for_enter_is_answered_and_its_page_is_githubs_own() {
    let (root, state) = fixture("github-enter");
    let home = super::super::home(&state);
    let cli = fake_github(
        &root,
        "",
        r##"  printf '! First copy your one-time code: WXYZ-9876\n' >&2
  printf 'Press Enter to open github.com in your browser... ' >&2
  IFS= read -r line
  : > "$HOME/entered"
  exec sleep 300"##,
        GITHUB_LOGIN,
    );
    preset_github(&state, cli);
    let c = start(state.clone(), super::super::definition("github").unwrap()).unwrap();
    let current = waiting(&state, &c.id).await;
    assert!(matches!(
        current.action,
        Some(Action::DeviceCode { ref verification_url, ref user_code })
            if verification_url == "https://github.com/login/device" && user_code == "WXYZ-9876"
    ));
    assert!(home.join("entered").exists());
    let attempt = attempt(&state, &c.id);
    attempt.cancel();
    attempt.wait_finished().await;
    assert_eq!(attempt.snapshot().phase, Phase::Canceled);
    std::fs::remove_dir_all(root).unwrap();
}
