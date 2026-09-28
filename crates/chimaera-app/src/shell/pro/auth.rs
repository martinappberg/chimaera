//! One in-memory browser sign-in per shell. The PKCE secret and callback
//! listener belong to the attempt; cancellation never persists either.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
};

/// A presentation hint only: both screens keep the same PKCE and MFA gates.
#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScreenHint {
    SignUp,
    #[default]
    SignIn,
}
impl ScreenHint {
    pub fn apply(self, url: &mut url::Url) {
        url.query_pairs_mut().append_pair(
            "screen_hint",
            match self {
                Self::SignUp => "sign-up",
                Self::SignIn => "sign-in",
            },
        );
    }
}

pub(super) const WINDOW: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    Waiting,
    Finishing,
}

#[derive(Clone, Serialize)]
pub(super) struct Status {
    pub phase: Phase,
    pub expires_at: u64,
}
struct Pending {
    id: u64,
    status: Status,
    cancel: watch::Sender<bool>,
}
#[derive(Default)]
struct State {
    next: u64,
    pending: Option<Pending>,
}
#[derive(Default)]
pub(super) struct SignIn {
    state: Mutex<State>,
}
pub(super) struct Attempt {
    pub id: u64,
    cancelled: watch::Receiver<bool>,
}
impl Attempt {
    pub async fn cancelled(&mut self) {
        let _ = self.cancelled.wait_for(|value| *value).await;
    }
}
impl SignIn {
    pub fn status(&self) -> Option<Status> {
        super::lock(&self.state)
            .pending
            .as_ref()
            .map(|pending| pending.status.clone())
    }
    pub fn begin(&self) -> Result<Attempt> {
        let mut state = super::lock(&self.state);
        if state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.status.phase == Phase::Finishing)
        {
            bail!("Sign-in is finishing. Please wait a moment.");
        }
        if let Some(old) = state.pending.take() {
            old.cancel.send_replace(true);
        }
        state.next = state.next.wrapping_add(1);
        let id = state.next;
        let (cancel, cancelled) = watch::channel(false);
        state.pending = Some(Pending {
            id,
            cancel,
            status: Status {
                phase: Phase::Waiting,
                expires_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    + WINDOW.as_secs(),
            },
        });
        Ok(Attempt { id, cancelled })
    }
    pub fn finishing(&self, id: u64) -> bool {
        let mut state = super::lock(&self.state);
        if let Some(pending) = state.pending.as_mut().filter(|pending| pending.id == id) {
            pending.status.phase = Phase::Finishing;
            true
        } else {
            false
        }
    }
    pub fn complete(&self, id: u64) -> bool {
        let mut state = super::lock(&self.state);
        if state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.id == id)
        {
            state.pending = None;
            true
        } else {
            false
        }
    }
    pub fn cancel_waiting(&self) -> Result<()> {
        let mut state = super::lock(&self.state);
        if state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.status.phase == Phase::Finishing)
        {
            bail!("Sign-in is finishing. You can sign out once it completes.");
        }
        if let Some(pending) = state.pending.take() {
            pending.cancel.send_replace(true);
        }
        Ok(())
    }
    /// Sign-out calls this while holding the operation lock. An activation
    /// already in progress therefore finishes before sign-out clears it.
    pub fn cancel(&self) {
        if let Some(pending) = super::lock(&self.state).pending.take() {
            pending.cancel.send_replace(true);
        }
    }
}

pub(super) struct Callback {
    pub code: String,
    socket: TcpStream,
}
impl Callback {
    pub async fn finish(self, success: bool) {
        reply(self.socket, success).await;
    }
}

pub(super) async fn callback(
    listener: TcpListener,
    redirect: &str,
    pkce: &chimaera_link::Pkce,
) -> Result<Callback> {
    let expected = url::Url::parse(redirect)?;
    let host = format!(
        "127.0.0.1:{}",
        expected.port().context("callback port missing")?
    );
    loop {
        let (mut socket, _) = listener.accept().await?;
        let request = tokio::time::timeout(Duration::from_secs(5), async {
            let mut request = Vec::new();
            let mut bytes = [0; 1024];
            while request.len() < 8192 {
                let n = socket.read(&mut bytes).await?;
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&bytes[..n]);
                if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    return Ok::<_, std::io::Error>(Some(request));
                }
            }
            Ok(None)
        })
        .await;
        let target = request
            .ok()
            .and_then(Result::ok)
            .flatten()
            .and_then(|bytes| {
                let text = std::str::from_utf8(&bytes).ok()?;
                let mut lines = text.split("\r\n");
                let line = lines.next()?;
                let target = line.strip_prefix("GET ")?.split_once(" HTTP/1.")?.0;
                let hosts: Vec<_> = lines
                    .filter_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("host").then_some(value.trim())
                    })
                    .collect();
                if hosts != [host.as_str()] || !target.starts_with("/callback?") {
                    return None;
                }
                expected.join(target).ok()
            });
        let outcome = target
            .as_ref()
            .filter(|url| {
                let states: Vec<_> = url
                    .query_pairs()
                    .filter(|(key, _)| key == "state")
                    .collect();
                states.len() == 1 && states[0].1 == pkce.state
            })
            .map(|url| pkce.callback_code(url));
        match outcome {
            Some(Ok(code)) => return Ok(Callback { code, socket }),
            Some(Err(error)) => {
                reply(socket, false).await;
                return Err(error);
            }
            None => reply(socket, false).await,
        }
    }
}

pub(super) async fn wait_callback(
    listener: TcpListener,
    redirect: &str,
    pkce: &chimaera_link::Pkce,
    attempt: &mut Attempt,
    deadline: tokio::time::Instant,
) -> Result<Callback> {
    tokio::select! {
        _ = attempt.cancelled() => bail!("Sign-in cancelled"),
        result = tokio::time::timeout_at(deadline, callback(listener, redirect, pkce)) => {
            result.context("Sign-in expired. Choose Try again to open a fresh browser sign-in.")?
        }
    }
}

async fn reply(mut socket: TcpStream, success: bool) {
    let (status, title, message) = if success {
        ("200 OK", "You're signed in", "Your account is connected. Chimaera is bringing you back to the app. You can close this tab.")
    } else {
        ("400 Bad Request", "Sign-in wasn't completed", "Return to Chimaera and choose Try again to start a fresh sign-in. You can close this tab.")
    };
    let body = include_str!("../../../assets/sign-in.html")
        .replace("{{title}}", title)
        .replace("{{message}}", message)
        .replace("{{footer}}", "Secure desktop sign-in");
    let response = format!("HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n{body}", body.len());
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        socket.write_all(response.as_bytes()).await?;
        socket.shutdown().await
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn screen_hint_is_closed_and_preserves_pkce_and_callback() {
        let pkce = chimaera_link::Pkce::new();
        let original = pkce
            .authorization_url(
                "https://account.example.invalid",
                "http://127.0.0.1:45678/callback",
            )
            .unwrap();
        for (value, expected) in [
            (None, "sign-in"),
            (Some("sign-in"), "sign-in"),
            (Some("sign-up"), "sign-up"),
        ] {
            let hint = value
                .map(|v| serde_json::from_value::<ScreenHint>(serde_json::json!(v)).unwrap())
                .unwrap_or_default();
            let mut url = original.clone();
            hint.apply(&mut url);
            let before: Vec<_> = original.query_pairs().collect();
            let after: Vec<_> = url
                .query_pairs()
                .filter(|(key, _)| key != "screen_hint")
                .collect();
            assert_eq!(before, after);
            assert_eq!(
                url.query_pairs()
                    .find(|(key, _)| key == "screen_hint")
                    .unwrap()
                    .1,
                expected
            );
        }
        for invalid in ["signup", "admin", "", "sign-up&prompt=none"] {
            assert!(serde_json::from_value::<ScreenHint>(serde_json::json!(invalid)).is_err());
        }
    }

    #[tokio::test]
    async fn retry_and_cancel_fence_old_attempts_but_do_not_interrupt_activation() {
        let state = SignIn::default();
        let mut old = state.begin().unwrap();
        let current = state.begin().unwrap();
        tokio::time::timeout(Duration::from_secs(1), old.cancelled())
            .await
            .unwrap();
        assert!(!state.finishing(old.id));
        assert!(!state.complete(old.id));
        assert!(state.finishing(current.id));
        assert!(state.cancel_waiting().is_err());
        assert!(state.begin().is_err());
        assert!(state.complete(current.id));
        let mut cancelled = state.begin().unwrap();
        state.cancel_waiting().unwrap();
        tokio::time::timeout(Duration::from_secs(1), cancelled.cancelled())
            .await
            .unwrap();
        assert!(!state.finishing(cancelled.id));
        assert!(state.status().is_none());
    }

    #[tokio::test]
    async fn callback_is_bound_and_reports_success_only_after_completion() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let redirect = format!("http://{address}/callback");
        let pkce = chimaera_link::Pkce::new();
        let state = pkce.state.clone();
        let waiter = tokio::spawn(async move { callback(listener, &redirect, &pkce).await });
        for (query, host) in [
            ("code=bad&state=wrong".into(), address.to_string()),
            (format!("code=bad&state={state}"), "foreign.invalid".into()),
            (
                format!("code=bad&state={state}&state={state}"),
                address.to_string(),
            ),
        ] {
            let mut socket = TcpStream::connect(address).await.unwrap();
            socket
                .write_all(
                    format!("GET /callback?{query} HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes(),
                )
                .await
                .unwrap();
            let mut response = String::new();
            socket.read_to_string(&mut response).await.unwrap();
            assert!(response.starts_with("HTTP/1.1 400"));
            let main = response
                .split_once("<main>")
                .unwrap()
                .1
                .split_once("</main>")
                .unwrap()
                .0;
            assert!(!main.contains(['{', '}']));
            assert!(main.contains("<p>Return to Chimaera and choose Try again to start a fresh sign-in. You can close this tab.</p>"));
            assert!(!response.contains(&state));
        }
        let mut socket = TcpStream::connect(address).await.unwrap();
        socket.write_all(format!("GET /callback?code=bound-code&state={state} HTTP/1.1\r\nHost: {address}\r\n\r\n").as_bytes()).await.unwrap();
        let accepted = waiter.await.unwrap().unwrap();
        assert_eq!(accepted.code, "bound-code");
        let mut byte = [0];
        assert!(
            tokio::time::timeout(Duration::from_millis(30), socket.read(&mut byte))
                .await
                .is_err()
        );
        let reply = tokio::spawn(accepted.finish(true));
        let mut response = String::new();
        socket.read_to_string(&mut response).await.unwrap();
        reply.await.unwrap();
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        assert!(headers.starts_with("HTTP/1.1 200"));
        assert!(headers.contains(&format!("Content-Length: {}", body.len())));
        assert!(headers.contains("Cache-Control: no-store"));
        assert!(body.contains("You're signed in"));
        assert!(body.contains("Secure desktop sign-in"));
        let main = body
            .split_once("<main>")
            .unwrap()
            .1
            .split_once("</main>")
            .unwrap()
            .0;
        assert!(!main.contains(['{', '}']));
        assert!(main.contains("<p>Your account is connected. Chimaera is bringing you back to the app. You can close this tab.</p>"));
        assert!(!body.contains("{{"));
        assert!(!body.contains("bound-code"));
        assert!(!body.contains(&state));
    }

    #[tokio::test]
    async fn cancelled_and_expired_waits_close_their_loopback_listener() {
        for cancel in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let redirect = format!("http://{address}/callback");
            let pkce = chimaera_link::Pkce::new();
            let state = SignIn::default();
            let mut attempt = state.begin().unwrap();
            if cancel {
                state.cancel_waiting().unwrap();
            }
            let result = wait_callback(
                listener,
                &redirect,
                &pkce,
                &mut attempt,
                tokio::time::Instant::now() + Duration::from_millis(20),
            )
            .await;
            let error = result.err().unwrap().to_string();
            assert!(error.contains(if cancel { "cancelled" } else { "expired" }));
            assert!(TcpStream::connect(address).await.is_err());
            assert_eq!(state.complete(attempt.id), !cancel);
        }
    }
}
