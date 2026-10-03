use super::*;
use serde_json::json;
use std::{os::unix::fs::PermissionsExt, path::PathBuf};
const CAP: &str = "synthetic-private-control-capability-not-runtime-00000";
const OP: &str = "01234567-89ab-4def-8012-3456789abcd0";
fn binding() -> ControlBinding {
    ControlBinding::parse(&serde_json::to_vec(&json!({"version":1,"account_id":"account-one","holder_id":"worker-one","process_boot":"01234567-89ab-4def-8012-3456789abcde","registration_generation":1,"worker_credential_digest":"0000000000000000000000000000000000000000000000000000000000000000","capability":CAP})).unwrap()).unwrap()
}
fn command(
    binding: &ControlBinding,
    device: &str,
    provider: &str,
    op: &str,
    action: serde_json::Value,
) -> ControlCommand {
    binding.consume(CAP, &binding.acknowledgment(), device, serde_json::to_vec(&json!({"version":1,"operation_id":op,"provider":provider,"expected_connection_generation":0,"command":action})).unwrap()).unwrap()
}
struct Root(PathBuf);
impl Root {
    fn new(script: &str) -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "chimaera-provider-runner-{}",
            crate::agents::fresh_session_id()
        ));
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(path.join("fixture-script"), script).unwrap();
        Self(path)
    }
    fn attempt(&self, pending: &PendingLogin) -> LoginAttempt {
        pending.start_with(Some(self.0.clone())).unwrap()
    }
    fn home(&self, provider: &str) -> PathBuf {
        self.0.join(provider).join(OP)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
async fn phase(attempt: &LoginAttempt, phase: LoginPhase) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while attempt.snapshot().phase != phase {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
const PROMPT: &str =
    "printf 'https://claude.com/cai/oauth/authorize?state=fixture\nPaste code here\n';";
const LEAF: &str = "mkdir -p .claude; printf '%s' '{\"claudeAiOauth\":{\"accessToken\":\"synthetic-access\",\"refreshToken\":\"synthetic-refresh\",\"expiresAt\":2000000000000,\"scopes\":[\"user:inference\"],\"subscriptionType\":\"max\",\"rateLimitTier\":\"default_claude_max_20x\"}}' > .claude/.credentials.json; printf '%s' '{\"oauthAccount\":{\"accountUuid\":\"user-one\",\"organizationUuid\":\"org-one\"}}' > .claude.json;";
#[tokio::test]
async fn official_flow_consumes_one_nonce_stops_tree_before_credential_and_never_serializes_secrets(
) {
    let _serial = TEST_SERIAL.lock().await;
    let b = binding();
    let connect = command(&b, "device-one", "claude", OP, json!({"type":"connect"}));
    let pending = b.pending_login(&connect).unwrap();
    let root = Root::new(&format!("{PROMPT} read code; {LEAF} exit 0"));
    let attempt = root.attempt(&pending);
    phase(&attempt, LoginPhase::Waiting).await;
    let submit = |value| {
        command(
            &b,
            "device-one",
            "claude",
            "01234567-89ab-4def-8012-3456789abcd1",
            json!({"type":"submit","attempt_id":OP,"submission_nonce":"01234567-89ab-4def-8012-3456789abcd2","code":value}),
        )
    };
    attempt
        .command(submit("synthetic-code#synthetic-state"))
        .unwrap();
    // Consumed is independent of secret-value equality and never resends.
    attempt
        .command(submit("different-code#different-state"))
        .unwrap();
    let completion = tokio::time::timeout(Duration::from_secs(7), attempt.finish())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !root.home("claude").exists(),
        "official refresh owner and its private tree removed first"
    );
    let bytes = Zeroizing::new(completion.credential().unwrap().to_vec());
    assert!(bytes
        .windows(b"synthetic-access".len())
        .any(|s| s == b"synthetic-access"));
    assert!(
        pending.start().is_err(),
        "same pending operation cannot start twice"
    );
    let other = command(
        &b,
        "device-one",
        "claude",
        "01234567-89ab-4def-8012-3456789abcd3",
        json!({"type":"connect"}),
    );
    assert!(
        b.pending_login(&other).unwrap().start().is_err(),
        "different attempt cannot replace awaiting-publication writer"
    );
    pending.revoke();
    assert!(completion.credential().is_err());
    drop(completion);
    assert!(!WRITERS[slot(Provider::Claude)].load(Ordering::Acquire));
}
#[tokio::test]
async fn observer_abort_does_not_release_writer_but_revocation_cleans_home_and_descendants() {
    let _serial = TEST_SERIAL.lock().await;
    let b = binding();
    let c = command(&b, "device-one", "claude", OP, json!({"type":"connect"}));
    let pending = b.pending_login(&c).unwrap();
    let root = Root::new(&format!("{PROMPT} read next_request"));
    let attempt = root.attempt(&pending);
    phase(&attempt, LoginPhase::Waiting).await;
    let snapshot = serde_json::to_string(&attempt.snapshot()).unwrap();
    assert!(!snapshot.contains("synthetic-refresh"));
    let observer = tokio::spawn(attempt.finish());
    observer.abort();
    let _ = observer.await;
    assert!(
        pending.start().is_err(),
        "aborted observer cannot replace credential writer"
    );
    pending.revoke();
    tokio::time::timeout(Duration::from_secs(7), async {
        while WRITERS[slot(Provider::Claude)].load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!root.home("claude").exists());
}
#[tokio::test]
async fn exact_authority_and_expiry_required_for_renewal_and_submission() {
    let _serial = TEST_SERIAL.lock().await;
    let b = binding();
    let c = command(&b, "device-one", "claude", OP, json!({"type":"connect"}));
    let pending = b.pending_login(&c).unwrap();
    let wrong = command(&b, "device-two", "claude", OP, json!({"type":"connect"}));
    assert!(pending.renew(&b, &wrong).is_err());
    pending.renew(&b, &c).unwrap();
    let root = Root::new(&format!("{PROMPT} read next_request"));
    let attempt = root.attempt(&pending);
    phase(&attempt, LoginPhase::Waiting).await;
    let bad = command(
        &b,
        "device-two",
        "claude",
        "01234567-89ab-4def-8012-3456789abcd1",
        json!({"type":"cancel","attempt_id":OP}),
    );
    assert!(attempt.command(bad).is_err());
    crate::lock(&pending.0.state).deadline = Instant::now() + Duration::from_millis(30);
    pending.0.changes.send_modify(|v| *v += 1);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(7), attempt.finish())
            .await
            .unwrap()
            .err(),
        Some("canceled")
    );
    assert!(pending.renew(&b, &c).is_err());
    assert!(!root.home("claude").exists());
}
#[tokio::test]
async fn noisy_or_failed_child_returns_fixed_error_and_cleans_the_owned_tree() {
    let _serial = TEST_SERIAL.lock().await;
    for script in [
        "printf 'sensitive-provider-error'; exit 7",
        "head -c 70000 /dev/zero; sleep 60",
    ] {
        let b = binding();
        let c = command(&b, "device-one", "claude", OP, json!({"type":"connect"}));
        let pending = b.pending_login(&c).unwrap();
        let root = Root::new(script);
        let attempt = root.attempt(&pending);
        let error = tokio::time::timeout(Duration::from_secs(7), attempt.finish())
            .await
            .unwrap()
            .err()
            .unwrap();
        assert!(!error.contains("sensitive"));
        assert!(!root.home("claude").exists());
        assert!(!WRITERS[slot(Provider::Claude)].load(Ordering::Acquire));
    }
}
#[tokio::test]
async fn codex_fixed_rpc_device_flow_keeps_secret_errors_out_and_reaps_after_completion() {
    let _serial = TEST_SERIAL.lock().await;
    let b = binding();
    let c = command(&b, "device-one", "codex", OP, json!({"type":"connect"}));
    let pending = b.pending_login(&c).unwrap();
    let root=Root::new("read init; printf '%s\n' '{\"id\":1,\"result\":{}}'; read initialized; read login; printf '%s\n' '{\"id\":2,\"result\":{\"type\":\"chatgptDeviceCode\",\"loginId\":\"fixture\",\"verificationUrl\":\"https://auth.openai.com/codex/device\",\"userCode\":\"ABCD-1234\"}}'; printf '%s\n' '{\"method\":\"account/login/completed\",\"params\":{\"loginId\":\"fixture\",\"success\":false,\"error\":\"sensitive-error\"}}'; read next_request");
    let attempt = root.attempt(&pending);
    let result = tokio::time::timeout(Duration::from_secs(7), attempt.finish())
        .await
        .unwrap();
    assert_eq!(result.err(), Some("sign_in_failed"));
    assert!(!root.home("codex").exists());
}
#[test]
fn github_leaf_is_credential_only_exact_account_and_never_refresh_or_global_git_config() {
    let credential = LoginHome::github_leaf(
        Zeroizing::new(b"synthetic-gh-access\n".to_vec()),
        Zeroizing::new(
            br#"{"id":42,"login":"fixture","email":"unrelated@example.invalid"}"#.to_vec(),
        ),
    )
    .unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&serde_json::to_vec(&credential).unwrap()).unwrap();
    assert_eq!(value["identity"]["user"], "42");
    assert!(value["refresh"].is_null());
    assert!(value["expires_at"].is_null());
    assert!(!value.to_string().contains("email"));
    assert!(LoginHome::github_leaf(
        Zeroizing::new(b"bad token".to_vec()),
        Zeroizing::new(br#"{"id":42,"login":"fixture"}"#.to_vec())
    )
    .is_err());
}
#[test]
fn cleanup_never_follows_cli_created_links_into_another_home() {
    let root = Root::new("");
    let b = binding();
    let c = command(&b, "device-one", "claude", OP, json!({"type":"connect"}));
    let home = LoginHome::prepare_at(&root.0, &c).unwrap();
    let outside = root.0.join("other-home");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("auth.json"), "keep").unwrap();
    std::os::unix::fs::symlink(&outside, home.path().join("foreign")).unwrap();
    home.cleanup().unwrap();
    assert_eq!(
        std::fs::read_to_string(outside.join("auth.json")).unwrap(),
        "keep"
    );
}
#[tokio::test]
async fn completed_credential_and_writer_expire_even_if_supervisor_observer_retains_completion() {
    let _serial = TEST_SERIAL.lock().await;
    let b = binding();
    let c = command(&b, "device-one", "claude", OP, json!({"type":"connect"}));
    let pending = b.pending_login(&c).unwrap();
    let root = Root::new(&format!("{PROMPT} read code; {LEAF} exit 0"));
    let attempt = root.attempt(&pending);
    phase(&attempt, LoginPhase::Waiting).await;
    attempt.command(command(&b,"device-one","claude","01234567-89ab-4def-8012-3456789abcd1",json!({"type":"submit","attempt_id":OP,"submission_nonce":"01234567-89ab-4def-8012-3456789abcd2","code":"synthetic-code#synthetic-state"}))).unwrap();
    let completion = attempt.finish().await.unwrap();
    assert!(completion.credential().is_ok());
    crate::lock(&pending.0.state).deadline = Instant::now() + Duration::from_millis(30);
    pending.0.changes.send_modify(|v| *v += 1);
    tokio::time::timeout(Duration::from_secs(2), async {
        while WRITERS[slot(Provider::Claude)].load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(completion.credential().is_err());
    assert!(crate::lock(&completion.completion.credential).is_none());
}
#[tokio::test]
async fn github_official_device_parser_and_fixed_probes_return_one_account_without_git_setup() {
    let _serial = TEST_SERIAL.lock().await;
    let b = binding();
    let c = command(&b, "device-one", "github", OP, json!({"type":"connect"}));
    let pending = b.pending_login(&c).unwrap();
    let root=Root::new("printf '%s\n' '! First copy your one-time code: ABCD-1234' 'Open this URL to continue in your web browser: https://github.com/login/device' >&2; sleep 0.1; exit 0");
    std::fs::write(root.0.join("fixture-token"), "synthetic-gh-token\n").unwrap();
    std::fs::write(
        root.0.join("fixture-user"),
        "{\"id\":42,\"login\":\"fixture\",\"email\":\"not-imported\"}",
    )
    .unwrap();
    let attempt = root.attempt(&pending);
    phase(&attempt, LoginPhase::Waiting).await;
    let view = serde_json::to_string(&attempt.snapshot()).unwrap();
    assert!(view.contains("ABCD-1234") && !view.contains("synthetic-gh-token"));
    let complete = attempt.finish().await.unwrap();
    let bytes = complete.credential().unwrap();
    let leaf: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(leaf["identity"]["user"], "42");
    assert!(!String::from_utf8_lossy(&bytes).contains("not-imported"));
    assert!(!root.home("github").exists());
}
#[tokio::test]
async fn codex_successful_device_rpc_returns_exact_account_after_killing_its_refresh_owner() {
    use base64::Engine;
    let _serial = TEST_SERIAL.lock().await;
    let b = binding();
    let c = command(&b, "device-one", "codex", OP, json!({"type":"connect"}));
    let pending = b.pending_login(&c).unwrap();
    let claims=base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"exp":2000000000,"https://api.openai.com/auth":{"chatgpt_user_id":"user-one","chatgpt_account_id":"account-one","chatgpt_plan_type":"plus"}}"#);
    let leaf=json!({"auth_mode":"chatgpt","tokens":{"access_token":format!("header.{claims}.signature"),"refresh_token":"synthetic-codex-refresh","account_id":"account-one"}}).to_string();
    let root=Root::new(&format!("read init; printf '%s\n' '{{\"id\":1,\"result\":{{}}}}'; read initialized; read login; printf '%s\n' '{{\"id\":2,\"result\":{{\"type\":\"chatgptDeviceCode\",\"loginId\":\"fixture\",\"verificationUrl\":\"https://auth.openai.com/codex/device\",\"userCode\":\"ABCD-1234\"}}}}'; mkdir .codex; printf '%s' '{leaf}' > .codex/auth.json; printf '%s\n' '{{\"method\":\"account/login/completed\",\"params\":{{\"loginId\":\"fixture\",\"success\":true}}}}'; read next_request"));
    let complete = tokio::time::timeout(Duration::from_secs(7), root.attempt(&pending).finish())
        .await
        .unwrap()
        .unwrap();
    let bytes = complete.credential().unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["identity"]["workspace"], "account-one");
    assert_eq!(value["identity"]["user"], "user-one");
    assert!(!root.home("codex").exists());
}
#[tokio::test]
async fn zz_detached_login_child_stops_or_retains_fence_without_credential_on_uncertain_receipt() {
    let _serial = TEST_SERIAL.lock().await;
    let b = binding();
    let c = command(&b, "device-one", "claude", OP, json!({"type":"connect"}));
    let pending = b.pending_login(&c).unwrap();
    let root = Root::new("");
    let heartbeat = root.0.join("heartbeat");
    // Actual detached child remains in the created process group. Some macOS
    // test environments retain its dead orphan as a zombie; kill(0) then cannot
    // prove disappearance. That path must stay fenced, never pretend clean.
    std::fs::write(
        root.0.join("fixture-script"),
        format!(
            "{PROMPT} (while :; do printf x >> '{}'; sleep 0.02; done) & read next_request",
            heartbeat.display()
        ),
    )
    .unwrap();
    let attempt = root.attempt(&pending);
    phase(&attempt, LoginPhase::Waiting).await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while std::fs::metadata(&heartbeat).map_or(true, |m| m.len() == 0) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    pending.revoke();
    let result = tokio::time::timeout(Duration::from_secs(7), attempt.finish())
        .await
        .unwrap();
    let error = result.err().unwrap();
    assert!(matches!(error, "canceled" | "cleanup_failed"));
    let stopped = std::fs::metadata(&heartbeat).unwrap().len();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(std::fs::metadata(&heartbeat).unwrap().len(), stopped);
    if error == "cleanup_failed" {
        assert!(WRITERS[slot(Provider::Claude)].load(Ordering::Acquire));
        assert!(root.home("claude").exists());
    } else {
        assert!(!WRITERS[slot(Provider::Claude)].load(Ordering::Acquire));
        assert!(!root.home("claude").exists());
    }
}
