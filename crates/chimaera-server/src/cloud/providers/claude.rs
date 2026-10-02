//! The official CLI owns OAuth, PKCE, token exchange and credential storage.
//! Only its verified browser URL and a one-time stdin reply cross this adapter.
use super::{home, process, Action, Attempt, Phase};
use crate::AppState;
use std::{path::Path, process::Stdio, sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(in crate::cloud::providers) fn authorization_url(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    for (start, _) in text.match_indices("https://") {
        let candidate = text[start..]
            .split(|c: char| c.is_whitespace() || c.is_control())
            .next()?;
        if candidate.len() > 4096 || candidate.contains('#') {
            continue;
        }
        let Ok(uri) = candidate.parse::<axum::http::Uri>() else {
            continue;
        };
        let Some(authority) = uri.authority() else {
            continue;
        };
        let origin = format!(
            "https://{}",
            authority
                .as_str()
                .strip_suffix(":443")
                .unwrap_or(authority.as_str())
        );
        if chimaera_core::cloud_providers::provider_auth_origins("claude").contains(&origin)
            && uri.path() == "/cai/oauth/authorize"
        {
            return Some(candidate.to_owned());
        }
    }
    None
}

pub(super) async fn login(
    state: &Arc<AppState>,
    bin: &Path,
    attempt: &Attempt,
) -> Result<(), &'static str> {
    let mut command = process::command(bin, &["auth", "login", "--claudeai"], &home(state));
    // The user opens the URL on their viewing device. Never launch a browser on
    // the worker, and never turn a CLI sign-in into a registered user workspace.
    command
        .env("BROWSER", "true")
        .stdin(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = process::Child::spawn(&mut command)?;
    *crate::lock(&attempt.process) = child.child.id();
    let mut input = child.child.stdin.take().ok_or("start_failed")?;
    let mut output = child.child.stdout.take().ok_or("start_failed")?;
    let mut captured = Vec::new();
    let url = tokio::time::timeout(Duration::from_secs(12), async {
        let mut buffer = [0; 2048];
        loop {
            let n = output
                .read(&mut buffer)
                .await
                .map_err(|_| "connection_closed")?;
            if n == 0 {
                return Err("browser_login_unavailable");
            }
            if captured.len() + n > process::LIMIT {
                return Err("output_limit");
            }
            captured.extend_from_slice(&buffer[..n]);
            // The prompt marks a complete CLI frame; do not publish a URL that
            // ended at an arbitrary OS pipe read boundary.
            if captured
                .windows(b"Paste code here".len())
                .any(|b| b == b"Paste code here")
            {
                return authorization_url(&captured).ok_or("browser_login_unavailable");
            }
        }
    })
    .await
    .map_err(|_| "browser_login_unavailable")??;
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    *crate::lock(&attempt.input) = Some(sender);
    attempt.update(
        Phase::Waiting,
        Some(Action::Browser {
            url,
            input: "authorization_code",
        }),
        None,
    );
    let remaining = (process::LIMIT - captured.len()) as u64;
    drop(captured);
    // Keep draining bounded output while waiting: an unexpected CLI protocol
    // cannot fill a pipe indefinitely or smuggle its diagnostics into status.
    let drain = async {
        let mut bytes = Vec::new();
        output
            .take(remaining + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "connection_closed")?;
        if bytes.len() as u64 > remaining {
            return Err("output_limit");
        }
        Ok::<(), &'static str>(())
    };
    let flow = async {
        tokio::select! {
            status = child.wait() => {
                return if status.map_err(|_| "sign_in_failed")?.success() { Ok(()) } else { Err("sign_in_failed") };
            }
            code = receiver.recv() => {
                let mut code: String = code.ok_or("connection_closed")?;
                code.push('\n');
                tokio::time::timeout(Duration::from_secs(3), async {
                    input.write_all(code.as_bytes()).await?;
                    input.flush().await
                }).await.map_err(|_| "sign_in_failed")?.map_err(|_| "sign_in_failed")?;
            }
        }
        crate::lock(&attempt.input).take();
        let status = tokio::time::timeout(Duration::from_secs(30), child.wait())
            .await
            .map_err(|_| "sign_in_not_confirmed")?
            .map_err(|_| "sign_in_failed")?;
        if status.success() {
            Ok(())
        } else {
            Err("sign_in_failed")
        }
    };
    tokio::try_join!(drain, flow)?;
    Ok(())
}

/// A pasted Claude authorization is `code#state`, both halves present.
pub(in crate::cloud::providers) fn complete_code(code: &str) -> bool {
    code.split_once('#')
        .is_some_and(|(code, state)| !code.is_empty() && !state.is_empty() && !state.contains('#'))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_a_whole_code_and_state_pair_is_submitted() {
        assert!(complete_code("abc123#state456"));
        for partial in ["abc123", "abc123#", "#state456", "a#b#c", ""] {
            assert!(!complete_code(partial), "{partial}");
        }
    }
    #[test]
    fn only_complete_known_authorization_urls_are_published() {
        let url = "https://claude.com/cai/oauth/authorize?state=fixture&code_challenge=fixture";
        assert_eq!(
            authorization_url(
                format!("\x1b]8;;{url}\x07Sign in\x1b]8;;\x07\nPaste code here").as_bytes()
            )
            .as_deref(),
            Some(url)
        );
        for url in [
            "https://claude.com.evil.test/cai/oauth/authorize",
            "https://user@claude.com/cai/oauth/authorize",
            "https://claude.com:444/cai/oauth/authorize",
            "https://claude.com/cai/oauth/authorize#fragment",
            "https://claude.com/unrelated",
            "http://claude.com/cai/oauth/authorize",
        ] {
            assert!(authorization_url(url.as_bytes()).is_none());
        }
    }
}
