//! The claude.ai login dictation rides on — the one `claude` keeps on this
//! host, found where claude 2.1.283 looks: `CLAUDE_CODE_OAUTH_TOKEN`, else its
//! credential store. On macOS that is the login keychain (service
//! `Claude Code-credentials`, plus `-<8 hex of sha256(config dir)>` when
//! `CLAUDE_CONFIG_DIR` is set; account `$USER`), read the way claude reads it
//! (`security find-generic-password`, so no keychain prompt), with
//! `.credentials.json` as its fallback; elsewhere only `.credentials.json` in
//! claude's config dir. The token lives at `claudeAiOauth.accessToken`.
//!
//! Read-only: chimaera never refreshes it (claude's refresh tokens rotate, so
//! a second refresher would sign claude out); an expired login says so and
//! claude renews it on its next request. The token is never logged.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

/// claude's own keychain read timeout.
#[cfg(target_os = "macos")]
const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(2);
/// `security` exit codes: no such item, and the keychain can't be used
/// (locked, no user interaction allowed).
#[cfg(target_os = "macos")]
const KEYCHAIN_NOT_FOUND: i32 = 44;
#[cfg(target_os = "macos")]
const KEYCHAIN_UNAVAILABLE: i32 = 36;
/// A credentials file is a few hundred bytes; anything this big is not one.
const MAX_STORE: u64 = 64 * 1024;
/// Treat a token this close to expiry as expired: a recording outlasting it
/// would be cut off mid-sentence.
const EXPIRY_MARGIN: Duration = Duration::from_secs(60);

/// Why there is no usable login.
#[derive(Debug)]
pub(crate) enum LoginError {
    /// No claude.ai login on this host (not signed in, or an API key).
    NoLogin,
    /// Signed in, but the access token has expired.
    Expired,
    /// The login exists but could not be read.
    Unreadable(String),
}

impl LoginError {
    /// The wire `code` for the UI.
    pub(crate) fn code(&self) -> &'static str {
        match self {
            LoginError::NoLogin => "no_login",
            LoginError::Expired => "expired",
            LoginError::Unreadable(_) => "unreadable",
        }
    }
}

impl std::fmt::Display for LoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoginError::NoLogin => write!(
                f,
                "Voice dictation uses Claude's speech service and needs a Claude.ai login on this host. Sign in with /login in a Claude chat."
            ),
            LoginError::Expired => write!(
                f,
                "Claude's login on this host has expired. Send a message in any Claude chat (Claude renews it), then try again."
            ),
            LoginError::Unreadable(why) => write!(f, "Could not read Claude's login on this host: {why}"),
        }
    }
}

#[derive(Deserialize)]
struct Store {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<Oauth>,
}

#[derive(Deserialize)]
struct Oauth {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    /// Milliseconds since the epoch.
    #[serde(rename = "expiresAt")]
    expires_at: Option<u64>,
}

/// The current claude.ai access token.
pub(crate) async fn access_token() -> Result<String, LoginError> {
    if let Some(token) = env_nonempty("CLAUDE_CODE_OAUTH_TOKEN") {
        return Ok(token);
    }
    let stored = read_store().await?.ok_or(LoginError::NoLogin)?;
    token_from(&stored, now_ms())
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The usable token in a credential store document.
fn token_from(document: &str, now_ms: u64) -> Result<String, LoginError> {
    let store: Store = serde_json::from_str(document).map_err(|_| {
        LoginError::Unreadable("the stored login isn't in the format claude writes".to_string())
    })?;
    let oauth = store.claude_ai_oauth.ok_or(LoginError::NoLogin)?;
    let token = oauth
        .access_token
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or(LoginError::NoLogin)?;
    if let Some(expires_at) = oauth.expires_at {
        if now_ms + EXPIRY_MARGIN.as_millis() as u64 >= expires_at {
            return Err(LoginError::Expired);
        }
    }
    Ok(token)
}

/// claude's config dir: `CLAUDE_CONFIG_DIR`, else `~/.claude`
/// (`CLAUDE_SECURESTORAGE_CONFIG_DIR` overrides it for the credential store).
fn config_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_SECURESTORAGE_CONFIG_DIR") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
        return dirs_home().map(|h| h.join(".claude"));
    }
    match std::env::var("CLAUDE_CONFIG_DIR") {
        Ok(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => dirs_home().map(|h| h.join(".claude")),
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// `.credentials.json` in claude's config dir; `None` when there is none.
async fn read_file_store() -> Result<Option<String>, LoginError> {
    let Some(path) = config_dir().map(|d| d.join(".credentials.json")) else {
        return Ok(None);
    };
    let read = tokio::task::spawn_blocking(move || -> std::io::Result<Option<String>> {
        use std::io::Read;
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut text = String::new();
        file.take(MAX_STORE).read_to_string(&mut text)?;
        Ok(Some(text))
    })
    .await
    .map_err(|e| LoginError::Unreadable(e.to_string()))?;
    read.map_err(|e| LoginError::Unreadable(format!("~/.claude/.credentials.json: {e}")))
}

#[cfg(not(target_os = "macos"))]
async fn read_store() -> Result<Option<String>, LoginError> {
    read_file_store().await
}

#[cfg(target_os = "macos")]
async fn read_store() -> Result<Option<String>, LoginError> {
    match read_keychain().await? {
        Some(document) => Ok(Some(document)),
        None => read_file_store().await,
    }
}

/// claude's keychain service name for the credential store: a hash suffix
/// only when the store isn't claude's default — an explicit
/// `CLAUDE_SECURESTORAGE_CONFIG_DIR`, or `CLAUDE_CONFIG_DIR` alone. (claude
/// NFC-normalizes the dir before hashing; an ASCII path is unchanged by it.)
#[cfg(any(target_os = "macos", test))]
fn keychain_service(securestorage_dir: Option<&str>, config_dir: Option<&str>) -> String {
    use sha2::Digest;
    let hashed_dir = match (securestorage_dir, config_dir) {
        (Some(dir), _) => Some(dir).filter(|d| !d.is_empty()),
        (None, dir) => dir.filter(|d| !d.is_empty()),
    };
    match hashed_dir {
        Some(dir) => {
            let digest = sha2::Sha256::digest(dir.as_bytes());
            let hex: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
            format!("Claude Code-credentials-{hex}")
        }
        None => "Claude Code-credentials".to_string(),
    }
}

#[cfg(target_os = "macos")]
async fn read_keychain() -> Result<Option<String>, LoginError> {
    let securestorage = std::env::var("CLAUDE_SECURESTORAGE_CONFIG_DIR").ok();
    let config = std::env::var("CLAUDE_CONFIG_DIR").ok();
    let service = keychain_service(securestorage.as_deref(), config.as_deref());
    let account = std::env::var("USER")
        .ok()
        .filter(|u| {
            !u.is_empty()
                && u.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        })
        .unwrap_or_else(|| "claude-code-user".to_string());
    let mut command = tokio::process::Command::new("/usr/bin/security");
    command
        .args([
            "find-generic-password",
            "-a",
            &account,
            "-w",
            "-s",
            &service,
        ])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = match tokio::time::timeout(KEYCHAIN_TIMEOUT, command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => {
            return Err(LoginError::Unreadable(format!(
                "couldn't run security: {e}"
            )))
        }
        Err(_) => {
            return Err(LoginError::Unreadable(
                "the keychain didn't answer".to_string(),
            ))
        }
    };
    match output.status.code() {
        Some(0) => {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Ok((!text.is_empty()).then_some(text))
        }
        Some(KEYCHAIN_NOT_FOUND) => Ok(None),
        Some(KEYCHAIN_UNAVAILABLE) => Err(LoginError::Unreadable(
            "the login keychain is locked — unlock it (log in to this Mac) and try again"
                .to_string(),
        )),
        other => Err(LoginError::Unreadable(format!(
            "the keychain read failed (security exited {})",
            other
                .map(|c| c.to_string())
                .unwrap_or_else(|| "on a signal".to_string())
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        let doc = |expires: &str| {
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"sk-ant-oat01-abc","refreshToken":"r","expiresAt":{expires},"scopes":["user:inference"]}}}}"#
            )
        };
        assert_eq!(
            token_from(&doc("2000000"), 1_000_000).unwrap(),
            "sk-ant-oat01-abc"
        );
        assert!(matches!(
            token_from(&doc("1000000"), 1_000_000),
            Err(LoginError::Expired)
        ));
        // Inside the minute's margin counts as expired.
        assert!(matches!(
            token_from(&doc("1030000"), 1_000_000),
            Err(LoginError::Expired)
        ));
        assert!(matches!(
            token_from(r#"{"claudeAiOauth":null}"#, 0),
            Err(LoginError::NoLogin)
        ));
        assert!(matches!(
            token_from(r#"{"other":1}"#, 0),
            Err(LoginError::NoLogin)
        ));
        assert!(matches!(
            token_from(r#"{"claudeAiOauth":{"accessToken":""}}"#, 0),
            Err(LoginError::NoLogin)
        ));
        assert_eq!(
            token_from(r#"{"claudeAiOauth":{"accessToken":"t"}}"#, u64::MAX / 2).unwrap(),
            "t"
        );
        assert!(matches!(
            token_from("not json", 0),
            Err(LoginError::Unreadable(_))
        ));
    }

    #[test]
    fn keychain_services() {
        assert_eq!(keychain_service(None, None), "Claude Code-credentials");
        assert_eq!(keychain_service(None, Some("")), "Claude Code-credentials");
        // sha256("/tmp/cfg") = 519e587f…, the dir string hashed as given.
        let hashed = keychain_service(None, Some("/tmp/cfg"));
        assert_eq!(hashed, "Claude Code-credentials-519e587f");
        assert_eq!(keychain_service(Some("/tmp/cfg"), None), hashed);
        // An explicitly empty securestorage dir means claude's default store.
        assert_eq!(
            keychain_service(Some(""), Some("/tmp/cfg")),
            "Claude Code-credentials"
        );
    }
}
