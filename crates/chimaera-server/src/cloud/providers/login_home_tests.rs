use super::*;
use crate::cloud::providers::authority::ControlBinding;
use serde_json::json;
use std::time::Duration;
use std::{
    io::Write,
    os::unix::fs::{symlink, OpenOptionsExt, PermissionsExt},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "chimaera-provider-login-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn command(provider: &str) -> ControlCommand {
    const CAP: &str = "synthetic-private-control-capability-not-runtime-00000";
    let binding = ControlBinding::parse(&serde_json::to_vec(&json!({"version":1,"account_id":"a-one","holder_id":"worker-one","process_boot":"01234567-89ab-4def-8012-3456789abcde","registration_generation":1,"worker_credential_digest":"0000000000000000000000000000000000000000000000000000000000000000","capability":CAP})).unwrap()).unwrap();
    binding.consume(CAP,&binding.acknowledgment(),"device-one",serde_json::to_vec(&json!({"version":1,"operation_id":"01234567-89ab-4def-8012-3456789abcd0","provider":provider,"expected_connection_generation":0,"command":{"type":"connect"}})).unwrap()).unwrap()
}
fn write(path: &Path, bytes: &[u8]) {
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}
fn seed(home: &LoginHome) {
    std::fs::create_dir(home.path().join(".claude")).unwrap();
    write(&home.path().join(".claude/.credentials.json"), &serde_json::to_vec(&json!({"claudeAiOauth":{"accessToken":"synthetic-access","refreshToken":"synthetic-refresh","expiresAt":2000000000000_i64,"refreshTokenExpiresAt":2100000000000_i64,"scopes":["user:inference","user:profile"],"subscriptionType":"max","rateLimitTier":"default_claude_max_20x","clientId":"9d1c250a-e61b-44d9-88ed-5944d1962f5e"},"unrelatedKey":"must-not-import"})).unwrap());
    write(&home.path().join(".claude.json"), &serde_json::to_vec(&json!({"oauthAccount":{"accountUuid":"user-one","organizationUuid":"org-one","emailAddress":"not-returned@example.invalid"},"projects":{"/unrelated/history":"must-not-import"}})).unwrap());
}

#[test]
fn fixed_login_extracts_only_selected_identity_and_subscription_metadata() {
    let root = Root::new();
    let home = LoginHome::prepare_at(&root.0, &command("claude")).unwrap();
    seed(&home);
    let credential = home.claude_leaf().unwrap();
    assert_eq!(credential.identity.user, "user-one");
    assert_eq!(credential.identity.workspace.as_deref(), Some("org-one"));
    assert_eq!(credential.expires_at, Some(2000000000));
    assert_eq!(
        credential
            .claude
            .as_ref()
            .unwrap()
            .rate_limit_tier
            .as_deref(),
        Some("default_claude_max_20x")
    );
    let bytes = Zeroizing::new(serde_json::to_vec(&credential).unwrap());
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(value.get("projects").is_none());
    assert!(value.get("emailAddress").is_none());
    assert!(value.get("unrelatedKey").is_none());
    assert!(LoginHome::prepare_at(&root.0, &command("claude")).is_err());
}

#[test]
fn fixed_login_refuses_link_hardlink_fifo_and_oversized_credential_leaves() {
    for kind in ["symlink", "hardlink", "fifo", "oversize"] {
        let root = Root::new();
        let home = LoginHome::prepare_at(&root.0, &command("claude")).unwrap();
        seed(&home);
        let path = home.path().join(".claude/.credentials.json");
        let original = home.path().join("original.json");
        std::fs::rename(&path, &original).unwrap();
        match kind {
            "symlink" => symlink(&original, &path).unwrap(),
            "hardlink" => std::fs::hard_link(&original, &path).unwrap(),
            "fifo" => {
                use std::ffi::CString;
                use std::os::unix::ffi::OsStrExt;
                let path = CString::new(path.as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { nix::libc::mkfifo(path.as_ptr(), 0o600) }, 0);
            }
            "oversize" => write(&path, &vec![b' '; MAX_LEAF as usize + 1]),
            _ => unreachable!(),
        }
        let before = std::time::Instant::now();
        assert!(home.claude_leaf().is_err());
        assert!(before.elapsed() < Duration::from_secs(1));
    }
}

#[test]
fn replaced_root_or_ancestor_never_recaptures_a_login_home() {
    let root = Root::new();
    let home = LoginHome::prepare_at(&root.0, &command("claude")).unwrap();
    seed(&home);
    let aside = home.path().with_extension("old");
    std::fs::rename(home.path(), &aside).unwrap();
    std::fs::create_dir(home.path()).unwrap();
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(home.check(), Err(Error::Changed)));
    assert!(home.claude_leaf().is_err());
    std::fs::remove_dir(home.path()).unwrap();
    symlink(&aside, home.path()).unwrap();
    assert!(home.check().is_err());
}

#[test]
fn missing_org_or_another_provider_mode_refuses_import() {
    let root = Root::new();
    let home = LoginHome::prepare_at(&root.0, &command("claude")).unwrap();
    seed(&home);
    std::fs::remove_file(home.path().join(".claude.json")).unwrap();
    write(
        &home.path().join(".claude.json"),
        br#"{"oauthAccount":{"accountUuid":"user-one"}}"#,
    );
    assert!(home.claude_leaf().is_err());
    let other = LoginHome::prepare_at(&root.0, &command("github")).unwrap();
    seed(&other);
    assert!(matches!(other.claude_leaf(), Err(Error::InvalidCommand)));
}
fn codex_token(user: &str, account: &str) -> String {
    let payload=base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"exp":2000000000,"https://api.openai.com/auth":{"user_id":user,"chatgpt_account_id":account,"chatgpt_plan_type":"pro"}})).unwrap());
    format!("e30.{payload}.synthetic")
}
fn codex(home: &LoginHome, account: &str, mode: serde_json::Value) {
    std::fs::create_dir(home.path().join(".codex")).unwrap();
    write(&home.path().join(".codex/auth.json"),&serde_json::to_vec(&json!({"auth_mode":mode,"OPENAI_API_KEY":null,"tokens":{"access_token":codex_token("user-one","account-one"),"refresh_token":"synthetic-refresh","id_token":codex_token("user-one","account-one"),"account_id":account},"last_refresh":"2026-10-02T00:00:00Z"})).unwrap());
}
#[test]
fn codex_import_retains_exact_subscription_identity_and_legacy_user_alias() {
    let root = Root::new();
    let home = LoginHome::prepare_at(&root.0, &command("codex")).unwrap();
    codex(&home, "account-one", json!("chatgpt"));
    let credential = home.codex_leaf().unwrap();
    assert_eq!(credential.identity.user, "user-one");
    assert_eq!(
        credential.identity.workspace.as_deref(),
        Some("account-one")
    );
    assert_eq!(credential.identity.plan.as_deref(), Some("pro"));
    assert_eq!(credential.expires_at, Some(2000000000));
    assert!(credential.claude.is_none());
    let mut oversized = credential;
    oversized.id_token = Some(Zeroizing::new("synthetic".repeat(MAX_SECRET / 9 + 2)));
    assert!(oversized.validate().is_err());
}
#[test]
fn codex_import_refuses_billing_mode_substitution_or_selected_account_drift() {
    for (account, mode) in [
        ("account-two", "chatgpt"),
        ("account-one", "apikey"),
        ("account-one", "agent_identity"),
    ] {
        let root = Root::new();
        let home = LoginHome::prepare_at(&root.0, &command("codex")).unwrap();
        codex(&home, account, json!(mode));
        assert!(home.codex_leaf().is_err());
    }
}

#[test]
fn official_login_commands_have_no_shell_prelude_or_ambient_provider_environment() {
    for provider in ["claude", "codex", "github"] {
        let root = Root::new();
        let home = LoginHome::prepare_at(&root.0, &command(provider)).unwrap();
        let command = home.official_login_command().unwrap();
        let command = command.as_std();
        assert_eq!(
            command.get_program(),
            match provider {
                "claude" => "/usr/local/bin/claude",
                "codex" => "/usr/local/bin/codex",
                "github" => "/usr/bin/gh",
                _ => unreachable!(),
            }
        );
        assert_eq!(command.get_current_dir(), Some(home.path()));
        let env: std::collections::HashMap<_, _> = command
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().to_string(),
                    v.map(|v| v.to_string_lossy().to_string()),
                )
            })
            .collect();
        assert_eq!(
            env.get("HOME"),
            Some(&Some(home.path().to_string_lossy().to_string()))
        );
        for name in [
            "CLAUDE_CONFIG_DIR",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "OPENAI_API_KEY",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "SSL_CERT_FILE",
            "CHIMAERA_HOME",
        ] {
            assert!(!env.contains_key(name));
        }
        assert!(!command.get_args().any(|arg| arg == "-lc"));
    }
}

#[tokio::test]
async fn fixed_login_overlay_removes_synthetic_token_and_proxy_overrides_in_a_real_child() {
    let root = Root::new();
    let home = LoginHome::prepare_at(&root.0, &command("claude")).unwrap();
    let mut child = tokio::process::Command::new("/bin/sh");
    child.args(["-c", "printf '%s\\n' \"$HOME\" \"${GH_TOKEN-unset}\" \"${HTTPS_PROXY-unset}\" \"${ANTHROPIC_API_KEY-unset}\"; touch private-created"])
        .env("HOME","/synthetic-wrong-home").env("GH_TOKEN","synthetic-untrusted-token")
        .env("HTTPS_PROXY","http://synthetic.invalid").env("ANTHROPIC_API_KEY","synthetic-untrusted-key");
    home.configure(&mut child);
    let (success, bytes) = crate::cloud::providers::bounded_process_output(&mut child)
        .await
        .unwrap();
    assert!(success);
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        format!("{}\nunset\nunset\nunset\n", home.path().display())
    );
    assert_eq!(
        std::fs::metadata(home.path().join("private-created"))
            .unwrap()
            .mode()
            & 0o777,
        0o600
    );
}
