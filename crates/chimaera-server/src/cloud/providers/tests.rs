use super::*;
use std::{os::unix::fs::PermissionsExt, path::Path, process::Stdio};
use tokio::io::AsyncReadExt;

pub(super) fn fixture(label: &str) -> (PathBuf, Arc<AppState>) {
    let root = std::env::temp_dir().join(format!(
        "chimaera-provider-{label}-{}",
        chimaera_core::generate_token()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let mut state = AppState::new(
        "fixture".into(),
        "fixture".into(),
        4242,
        0,
        root.join("data"),
        root.join("config"),
    );
    state.claude_settings_path = root.join(".claude/settings.json");
    (root, Arc::new(state))
}
pub(super) fn executable(path: &Path, source: &str) {
    std::fs::write(path, source).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}
pub(super) fn preset(state: &AppState, kind: AgentKind, path: PathBuf) {
    crate::lock(&state.agent_bins).insert(
        kind,
        crate::launcher::AgentDetection {
            path: Ok(path),
            version: Some("fixture".into()),
            managed: false,
            explicit: true,
            mtime: None,
        },
    );
}
fn output(value: Value, code: i32) -> process::Output {
    process::Output {
        success: code == 0,
        code: Some(code),
        stdout: serde_json::to_vec(&value).unwrap(),
    }
}

#[test]
fn status_reads_only_explicit_allowlisted_auth_fields() {
    assert_eq!(
        auth_status(
            "claude",
            &output(
                json!({"loggedIn":true,"access_token":"must-never-leave-probe"}),
                0
            )
        ),
        Ok(true)
    );
    assert_eq!(
        auth_status("claude", &output(json!({"loggedIn":false}), 1)),
        Ok(false)
    );
    assert_eq!(
        auth_status("claude", &output(json!({"loggedIn":"true"}), 0)),
        Err("invalid_status")
    );
    assert_eq!(
        auth_status("claude", &output(json!({"loggedIn":true}), 1)),
        Err("invalid_status")
    );
    assert_eq!(
        auth_status("github", &output(json!({"hosts":{}}), 0)),
        Ok(false)
    );
    assert_eq!(
        auth_status(
            "github",
            &output(
                json!({"hosts":{"github.com":[{"active":false,"state":"success"}]}}),
                0
            )
        ),
        Err("invalid_status")
    );
    assert_eq!(
        auth_status(
            "github",
            &output(
                json!({"hosts":{"github.com":[{"active":true,"state":"success"}]}}),
                0
            )
        ),
        Ok(true)
    );
    assert_eq!(
        auth_status(
            "github",
            &output(
                json!({"hosts":{"github.com":[{"active":true,"state":"invalid-token","error":"private-value"}]}}),
                0
            )
        ),
        Err("invalid_status")
    );
}

#[tokio::test]
async fn fresh_probes_singleflight_and_cached_status_never_starts_login() {
    let (root, state) = fixture("singleflight");
    let cli = root.join("claude");
    executable(&cli, &format!("#!/bin/sh\n[ \"$1 $2\" = 'auth status' ] || exit 99\nprintf x >> '{}'\nsleep 0.15\nif [ -f '{}' ]; then printf '%s' '{{\"loggedIn\":true,\"access_token\":\"private-fixture\"}}'; else printf '%s' '{{\"loggedIn\":false}}'; exit 1; fi\n",root.join("probes").display(),root.join("signed-in").display()));
    preset(&state, AgentKind::Claude, cli);
    let ids = vec!["claude".into()];
    let (a, b) = tokio::join!(readiness(&state, &ids, true), readiness(&state, &ids, true));
    assert_eq!(a[0].state, ProviderState::NeedsSignIn);
    assert_eq!(b[0].state, ProviderState::NeedsSignIn);
    assert_eq!(std::fs::read(root.join("probes")).unwrap(), b"x");
    std::fs::write(root.join("signed-in"), b"").unwrap();
    assert_eq!(
        readiness(&state, &ids, false).await[0].state,
        ProviderState::NeedsSignIn
    );
    let status = readiness(&state, &ids, true).await;
    assert_eq!(status[0].state, ProviderState::SignedIn);
    assert!(!serde_json::to_string(&status)
        .unwrap()
        .contains("private-fixture"));
    assert_eq!(std::fs::read(root.join("probes")).unwrap(), b"xx");
    assert!(crate::lock(&state.cloud_providers.connections).is_empty());
    let unknown = readiness(&state, &["future-provider".into()], true).await;
    assert_eq!(unknown[0].reason.as_deref(), Some("unsupported_provider"));
    assert_eq!(crate::lock(&state.cloud_providers.cache).len(), 1);
    assert!(!readiness(&state, &vec!["claude".into(); 17], true)
        .await
        .is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn canceled_auth_child_stops_forked_writer_and_output_is_bounded() {
    let mut command = tokio::process::Command::new("/bin/sh");
    command
        .args(["-c", "(printf ready; sleep 0.2; printf escaped) & wait"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = process::Child::spawn(&mut command).unwrap();
    let mut output = child.child.stdout.take().unwrap();
    let mut ready = [0; 5];
    tokio::time::timeout(Duration::from_secs(2), output.read_exact(&mut ready))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&ready, b"ready");
    drop(child);
    let mut remaining = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), output.read_to_end(&mut remaining))
        .await
        .unwrap()
        .unwrap();
    assert!(remaining.is_empty());
    let mut large = tokio::process::Command::new("/bin/sh");
    large
        .args(["-c", "head -c 65537 /dev/zero"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    assert!(matches!(
        process::output(&mut large).await,
        Err("output_limit")
    ));
}

#[test]
fn every_auth_adapter_uses_the_shared_provider_catalog() {
    for adapter in PROVIDERS {
        let catalog = chimaera_core::cloud_providers::provider_definition(adapter.id)
            .expect("adapter catalog entry");
        let status = ProviderStatus::new(adapter.id);
        assert_eq!(status.label, catalog.label);
        assert_eq!(status.category, catalog.category);
        assert!(!status.methods.is_empty());
    }
}

#[test]
fn provider_commands_bind_auth_storage_to_the_explicit_worker_home() {
    let home = Path::new("/tmp/chimaera-synthetic-auth-home");
    let command = process::command(Path::new("/fixture/cli"), &["auth", "logout"], home);
    let env: std::collections::HashMap<_, _> = command
        .as_std()
        .get_envs()
        .filter_map(|(key, value)| {
            value.map(|value| {
                (
                    key.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
        })
        .collect();
    assert_eq!(env["HOME"], home.to_string_lossy());
    assert_eq!(
        env["CODEX_HOME"],
        "/tmp/chimaera-synthetic-auth-home/.codex"
    );
    assert_eq!(
        env["CLAUDE_CONFIG_DIR"],
        "/tmp/chimaera-synthetic-auth-home/.claude"
    );
    assert_eq!(
        env["GH_CONFIG_DIR"],
        "/tmp/chimaera-synthetic-auth-home/.config/gh"
    );
}

#[test]
fn new_catalog_entries_do_not_imply_a_disconnect_adapter() {
    for id in ["claude", "codex", "github"] {
        assert!(ProviderStatus::new(id).disconnect_supported);
    }
    assert!(!super::disconnect::supported("future-provider"));
    assert!(!ProviderStatus::new("future-provider").disconnect_supported);
}
