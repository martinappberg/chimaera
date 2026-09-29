//! Voice dictation — the daemon half of the chat composer's `/voice`.
//!
//! The microphone is wherever the window is (the daemon may be an HPC login
//! node with no audio device), so the browser captures and the daemon relays:
//! `GET /ws/voice` takes 16 kHz mono PCM from the client and streams it to
//! Claude's speech-to-text service — the one Claude Code's own `/voice` uses —
//! with the claude.ai login that `claude` keeps on this host (`login.rs`).
//! Transcripts come back as they form. One socket is one recording.
//!
//! Wire (client → daemon): `{"type":"auth","token"}` first, then
//! `{"type":"start","language"?,"keyterms"?}`, then binary frames of
//! little-endian i16 samples, then `{"type":"finalize"}` (stop, wait for the
//! last words) or `{"type":"cancel"}`.
//! Wire (daemon → client): `{"type":"ready"}` once the service is connected
//! (audio sent earlier is buffered, not lost), `{"type":"interim","text"}` —
//! the utterance being heard, replacing the previous interim —
//! `{"type":"final","text"}` — an utterance settled, to append —
//! `{"type":"error","code","message"}` and last `{"type":"done","reason"}`.
//!
//! The service's own protocol (as claude 2.1.283 speaks it): query
//! `encoding=linear16&sample_rate=16000&channels=1&endpointing_ms=300&
//! utterance_end_ms=1000&language=..&use_conversation_engine=true`, binary
//! audio, `{"type":"KeepAlive"}` every 8 s, `{"type":"CloseStream"}` to
//! finish; it answers `TranscriptInterim`/`TranscriptText {data}` (the
//! utterance so far), `TranscriptEndpoint` (that utterance is done),
//! `TranscriptError {description|error_code}` and `error {message}`.

mod login;
mod upstream;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use axum::Json;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::{sleep_until, Instant};
use tokio_tungstenite::tungstenite::Message as UpMessage;

use crate::AppState;

const SERVICE_BASE: &str = "wss://api.anthropic.com";
const SERVICE_PATH: &str = "/api/ws/speech_to_text/voice_stream";
/// Replaces `SERVICE_BASE` (e.g. `ws://127.0.0.1:9123`) — a local stand-in
/// for tests and live verification, like claude's `VOICE_STREAM_BASE_URL`.
const SERVICE_BASE_ENV: &str = "CHIMAERA_VOICE_STREAM_URL";

/// The dictation languages the service takes (claude's list); anything else
/// dictates in English, as claude does.
pub(crate) const LANGUAGES: [&str; 20] = [
    "en", "es", "fr", "ja", "de", "pt", "it", "ko", "hi", "id", "ru", "pl", "tr", "nl", "uk", "el",
    "cs", "da", "sv", "no",
];

/// One audio frame from the client: ~100 ms is 3.2 KB; this is generous.
const MAX_CLIENT_FRAME: usize = 64 * 1024;
/// Audio held while the service connects (and across one reconnect): ~30 s.
const MAX_BUFFERED_AUDIO: usize = 1024 * 1024;
/// A recording that runs this long finishes on its own.
const MAX_RECORDING: Duration = Duration::from_secs(10 * 60);
/// Recordings in flight daemon-wide (one person, a few windows).
const MAX_SESSIONS: usize = 4;
const KEEPALIVE_EVERY: Duration = Duration::from_secs(8);
/// After `CloseStream`: the longest wait for the last transcript, and the
/// wait when nothing more arrives at all (claude's 5 s / 1.5 s).
const FINALIZE_SAFETY: Duration = Duration::from_secs(5);
const FINALIZE_NO_DATA: Duration = Duration::from_millis(1500);
/// Before any transcript, one failed connection is retried after this pause.
const RETRY_PAUSE: Duration = Duration::from_millis(250);
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
/// Keyterm biasing header budget, as claude caps it.
const MAX_KEYTERMS_LEN: usize = 1024;

static ACTIVE: AtomicUsize = AtomicUsize::new(0);

struct Slot;
impl Slot {
    fn take() -> Option<Slot> {
        ACTIVE
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_SESSIONS).then_some(n + 1)
            })
            .ok()
            .map(|_| Slot)
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::AcqRel);
    }
}

/// GET /api/v1/voice — can this host dictate? `/voice` asks before turning
/// on, so a missing login says so at once rather than on the first recording.
pub(crate) async fn availability() -> Json<Value> {
    if service_override().is_some() && chimaera_core::is_dev_build() {
        return Json(json!({ "available": true }));
    }
    match login::access_token().await {
        Ok(_) => Json(json!({ "available": true })),
        Err(e) => Json(json!({ "available": false, "code": e.code(), "reason": e.to_string() })),
    }
}

/// GET /ws/voice — one recording (module docs).
pub(crate) async fn voice_ws(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> Response {
    ws.max_message_size(MAX_CLIENT_FRAME)
        .max_frame_size(MAX_CLIENT_FRAME)
        .on_upgrade(move |socket| async move {
            let mut socket = socket;
            relay(&mut socket, &state).await;
            let _ = socket.close().await;
        })
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientFrame {
    Auth {
        token: String,
    },
    Start {
        #[serde(default)]
        language: Option<String>,
        #[serde(default)]
        keyterms: Vec<String>,
    },
    Finalize,
    Cancel,
}

fn parse_frame(text: &str) -> Option<ClientFrame> {
    serde_json::from_str(text).ok()
}

async fn send(socket: &mut WebSocket, value: Value) -> bool {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .is_ok()
}

async fn fail(socket: &mut WebSocket, code: &str, message: &str) {
    let _ = send(
        socket,
        json!({ "type": "error", "code": code, "message": message }),
    )
    .await;
    let _ = send(socket, json!({ "type": "done", "reason": "error" })).await;
}

async fn relay(socket: &mut WebSocket, state: &AppState) {
    match tokio::time::timeout(AUTH_TIMEOUT, socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) if matches!(parse_frame(&text), Some(ClientFrame::Auth { ref token }) if *token == state.token) =>
            {}
        _ => {
            let _ = send(
                socket,
                json!({ "type": "error", "code": "unauthorized", "message": "unauthorized" }),
            )
            .await;
            return;
        }
    }
    let (language, keyterms) = match tokio::time::timeout(AUTH_TIMEOUT, socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) => match parse_frame(&text) {
            Some(ClientFrame::Start { language, keyterms }) => (language, keyterms),
            _ => return fail(socket, "protocol", "expected a start frame").await,
        },
        _ => return,
    };
    let Some(_slot) = Slot::take() else {
        return fail(socket, "busy", "too many recordings at once on this host").await;
    };

    let base = service_override();
    let token = match login::access_token().await {
        Ok(token) => Some(token),
        // A local stand-in needs no login — dev builds only, so a release can
        // never be pointed at a service while skipping the login check.
        Err(_) if base.is_some() && chimaera_core::is_dev_build() => None,
        Err(e) => return fail(socket, e.code(), &e.to_string()).await,
    };
    let url = service_url(base.as_deref(), &dictation_language(language.as_deref()));
    let mut headers = vec![
        ("user-agent", format!("chimaera/{}", chimaera_core::VERSION)),
        // The service is Claude Code's and keys on its client id; this is
        // that client's login, used on the user's behalf.
        ("x-app", "cli".to_string()),
    ];
    if let Some(token) = token {
        headers.push(("authorization", format!("Bearer {token}")));
    }
    let terms = keyterms_header(&keyterms);
    if !terms.is_empty() {
        headers.push(("x-config-keyterms", terms));
    }

    Session::new(url, headers).run(socket).await;
}

/// Tests point the relay at their stand-in here (env vars are process-wide
/// and tests run in parallel).
#[cfg(test)]
pub(crate) static SERVICE_FOR_TESTS: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn service_override() -> Option<String> {
    #[cfg(test)]
    if let Some(base) = SERVICE_FOR_TESTS.lock().unwrap().clone() {
        return Some(base);
    }
    std::env::var(SERVICE_BASE_ENV)
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
}

fn service_url(base: Option<&str>, language: &str) -> String {
    format!(
        "{}{SERVICE_PATH}?encoding=linear16&sample_rate=16000&channels=1&endpointing_ms=300&utterance_end_ms=1000&language={language}&use_conversation_engine=true",
        base.unwrap_or(SERVICE_BASE)
    )
}

/// The service's code for `language` (`"sv"`, `"sv-SE"`), else English.
pub(crate) fn dictation_language(language: Option<&str>) -> String {
    let wanted = language.unwrap_or("").trim().to_ascii_lowercase();
    let base = wanted.split(['-', '_']).next().unwrap_or("");
    if LANGUAGES.contains(&base) {
        base.to_string()
    } else {
        "en".to_string()
    }
}

/// Words the recognizer should favor (project and agent names), as claude
/// sends them: printable ASCII, commas out, de-duplicated, ≤ 1 KiB joined.
fn keyterms_header(terms: &[String]) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut out = String::new();
    for term in terms.iter().take(256) {
        let cleaned: String = term
            .chars()
            .map(|c| if c == ',' { ' ' } else { c })
            .filter(|c| (' '..='~').contains(c))
            .collect();
        let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
        if cleaned.is_empty() || !seen.insert(cleaned.clone()) {
            continue;
        }
        let extra = cleaned.len() + usize::from(!out.is_empty());
        if out.len() + extra > MAX_KEYTERMS_LEN {
            break;
        }
        if !out.is_empty() {
            out.push(',');
        }
        out.push_str(&cleaned);
    }
    out
}

/// One service message, as the relay reads it.
#[derive(Debug, PartialEq)]
enum Heard {
    /// The utterance so far.
    Interim(String),
    /// The current utterance ended.
    Endpoint,
    /// The service gave up on this stream.
    Failed(String),
    Other,
}

fn heard(text: &str) -> Heard {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Heard::Other;
    };
    let field = |k: &str| value.get(k).and_then(Value::as_str).map(str::to_string);
    match value.get("type").and_then(Value::as_str) {
        Some("TranscriptInterim" | "TranscriptText") => {
            Heard::Interim(field("data").unwrap_or_default())
        }
        Some("TranscriptEndpoint") => Heard::Endpoint,
        Some("TranscriptError") => Heard::Failed(
            field("description")
                .or_else(|| field("error_code"))
                .unwrap_or_else(|| "unknown transcription error".to_string()),
        ),
        Some("error") => {
            Heard::Failed(field("message").unwrap_or_else(|| "speech service error".to_string()))
        }
        _ => Heard::Other,
    }
}

/// Why the relay stopped, reported as `done.reason`.
enum End {
    Finished,
    Cancelled,
    Failed,
    ClientGone,
}

struct Session {
    url: String,
    headers: Vec<(&'static str, String)>,
    /// Audio not yet on the service's socket.
    buffered: std::collections::VecDeque<Bytes>,
    buffered_bytes: usize,
    /// The utterance heard so far, not yet settled by an endpoint.
    pending: String,
    heard_any: bool,
    retried: bool,
    /// Set once the client asked to finish; `CloseStream` goes out as soon as
    /// the service is connected and the buffer is flushed.
    finalizing: bool,
    close_sent: bool,
    safety_deadline: Option<Instant>,
    no_data_deadline: Option<Instant>,
}

impl Session {
    fn new(url: String, headers: Vec<(&'static str, String)>) -> Self {
        Session {
            url,
            headers,
            buffered: Default::default(),
            buffered_bytes: 0,
            pending: String::new(),
            heard_any: false,
            retried: false,
            finalizing: false,
            close_sent: false,
            safety_deadline: None,
            no_data_deadline: None,
        }
    }

    fn connect(
        &self,
    ) -> futures::future::BoxFuture<'static, Result<upstream::Stream, upstream::ConnectError>> {
        let url = self.url.clone();
        let headers = self.headers.clone();
        Box::pin(async move { upstream::connect(&url, &headers).await })
    }

    fn buffer(&mut self, audio: Bytes) {
        if self.buffered_bytes + audio.len() > MAX_BUFFERED_AUDIO {
            // Keep the start of what was said; a service that takes this long
            // to answer is about to fail the recording anyway.
            return;
        }
        self.buffered_bytes += audio.len();
        self.buffered.push_back(audio);
    }

    /// Settle the utterance in flight, as claude does when a stream ends
    /// without an endpoint for it.
    async fn promote(&mut self, socket: &mut WebSocket) {
        if !self.pending.is_empty() {
            let text = std::mem::take(&mut self.pending);
            send(socket, json!({ "type": "final", "text": text })).await;
        }
    }

    async fn run(mut self, socket: &mut WebSocket) {
        let end = self.drive(socket).await;
        self.promote(socket).await;
        let reason = match end {
            End::Finished => "finished",
            End::Cancelled => "cancelled",
            End::Failed => "error",
            End::ClientGone => return,
        };
        send(socket, json!({ "type": "done", "reason": reason })).await;
    }

    async fn drive(&mut self, socket: &mut WebSocket) -> End {
        let started = Instant::now();
        let mut connecting = Some(self.connect());
        let mut retry_at: Option<Instant> = None;
        let mut up: Option<upstream::Stream> = None;
        let mut keepalive = tokio::time::interval_at(started + KEEPALIVE_EVERY, KEEPALIVE_EVERY);

        loop {
            let deadline = [self.safety_deadline, self.no_data_deadline]
                .into_iter()
                .flatten()
                .min();
            tokio::select! {
                result = async { connecting.as_mut().unwrap().await }, if connecting.is_some() => {
                    connecting = None;
                    match result {
                        Ok(mut stream) => {
                            // claude opens with a KeepAlive before any audio.
                            if stream.send(UpMessage::text(r#"{"type":"KeepAlive"}"#)).await.is_err() {
                                return self.lost(socket, "the speech service closed the connection").await;
                            }
                            send(socket, json!({ "type": "ready" })).await;
                            while let Some(audio) = self.buffered.pop_front() {
                                if stream.send(UpMessage::Binary(audio)).await.is_err() {
                                    return self.lost(socket, "the speech service closed the connection").await;
                                }
                            }
                            self.buffered_bytes = 0;
                            up = Some(stream);
                            if self.finalizing && !self.close(&mut up).await {
                                return self.lost(socket, "the speech service closed the connection").await;
                            }
                        }
                        Err(upstream::ConnectError::Rejected(status)) if status == 401 || status == 403 => {
                            let message = "Claude's speech service refused this login — sign in again with /login in a Claude chat";
                            send(socket, json!({ "type": "error", "code": "auth", "message": message })).await;
                            return End::Failed;
                        }
                        Err(e) if !self.retried && !self.heard_any => {
                            tracing::info!("voice: connect failed ({e}); retrying once");
                            self.retried = true;
                            retry_at = Some(Instant::now() + RETRY_PAUSE);
                        }
                        Err(e) => {
                            send(socket, json!({ "type": "error", "code": "network", "message": e.to_string() })).await;
                            return End::Failed;
                        }
                    }
                }
                _ = async { sleep_until(retry_at.unwrap()).await }, if retry_at.is_some() => {
                    retry_at = None;
                    connecting = Some(self.connect());
                }
                frame = socket.recv() => match frame {
                    Some(Ok(Message::Binary(audio))) => {
                        if self.close_sent {
                            continue;
                        }
                        match up.as_mut() {
                            Some(stream) => {
                                if stream.send(UpMessage::Binary(audio)).await.is_err() {
                                    return self.lost(socket, "the speech service closed the connection").await;
                                }
                            }
                            None => self.buffer(audio),
                        }
                    }
                    Some(Ok(Message::Text(text))) => match parse_frame(&text) {
                        Some(ClientFrame::Finalize) if !self.finalizing => {
                            self.finalizing = true;
                            if up.is_some() && !self.close(&mut up).await {
                                return self.lost(socket, "the speech service closed the connection").await;
                            }
                        }
                        Some(ClientFrame::Cancel) => {
                            self.pending.clear();
                            if let Some(mut stream) = up.take() {
                                let _ = stream.close(None).await;
                            }
                            return End::Cancelled;
                        }
                        _ => {}
                    },
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    // The window went away mid-recording: nobody to hand words to.
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                        if let Some(mut stream) = up.take() {
                            let _ = stream.close(None).await;
                        }
                        return End::ClientGone;
                    }
                },
                message = async { up.as_mut().unwrap().next().await }, if up.is_some() => match message {
                    Some(Ok(UpMessage::Text(text))) => match heard(&text) {
                        Heard::Interim(text) => {
                            self.heard_any = true;
                            self.no_data_deadline = None;
                            if !text.is_empty() {
                                self.pending = text.clone();
                                send(socket, json!({ "type": "interim", "text": text })).await;
                            }
                        }
                        Heard::Endpoint => {
                            self.heard_any = true;
                            self.promote(socket).await;
                            if self.close_sent {
                                return End::Finished;
                            }
                        }
                        Heard::Failed(why) => {
                            self.promote(socket).await;
                            if self.close_sent {
                                return End::Finished;
                            }
                            send(socket, json!({ "type": "error", "code": "transcription", "message": why })).await;
                            return End::Failed;
                        }
                        Heard::Other => {}
                    },
                    Some(Ok(UpMessage::Close(frame))) => {
                        if self.close_sent {
                            return End::Finished;
                        }
                        let code = frame.as_ref().map(|f| u16::from(f.code)).unwrap_or(1005);
                        if code == 1000 || code == 1005 {
                            return End::Finished;
                        }
                        let reason = frame.map(|f| f.reason.to_string()).unwrap_or_default();
                        let message = if reason.is_empty() {
                            format!("the speech service closed the connection (code {code})")
                        } else {
                            format!("the speech service closed the connection (code {code} — {reason})")
                        };
                        send(socket, json!({ "type": "error", "code": "upstream", "message": message })).await;
                        return End::Failed;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        if self.close_sent {
                            return End::Finished;
                        }
                        return self.lost(socket, &format!("speech service connection error: {e}")).await;
                    }
                    None => {
                        if self.close_sent {
                            return End::Finished;
                        }
                        return self.lost(socket, "the speech service closed the connection").await;
                    }
                },
                _ = keepalive.tick(), if up.is_some() && !self.close_sent => {
                    if let Some(stream) = up.as_mut() {
                        if stream.send(UpMessage::text(r#"{"type":"KeepAlive"}"#)).await.is_err() {
                            return self.lost(socket, "the speech service closed the connection").await;
                        }
                    }
                }
                _ = async { sleep_until(deadline.unwrap()).await }, if deadline.is_some() => {
                    // The last words never got their endpoint: what was heard stands.
                    if let Some(mut stream) = up.take() {
                        let _ = stream.close(None).await;
                    }
                    return End::Finished;
                }
                _ = sleep_until(started + MAX_RECORDING), if !self.finalizing => {
                    self.finalizing = true;
                    if up.is_some() && !self.close(&mut up).await {
                        return self.lost(socket, "the speech service closed the connection").await;
                    }
                }
            }
        }
    }

    /// Send `CloseStream` and arm the finalize deadlines. False when the
    /// service socket is already gone.
    async fn close(&mut self, up: &mut Option<upstream::Stream>) -> bool {
        let Some(stream) = up.as_mut() else {
            return false;
        };
        self.close_sent = true;
        let now = Instant::now();
        self.safety_deadline = Some(now + FINALIZE_SAFETY);
        self.no_data_deadline = Some(now + FINALIZE_NO_DATA);
        stream
            .send(UpMessage::text(r#"{"type":"CloseStream"}"#))
            .await
            .is_ok()
    }

    async fn lost(&mut self, socket: &mut WebSocket, message: &str) -> End {
        send(
            socket,
            json!({ "type": "error", "code": "upstream", "message": message }),
        )
        .await;
        End::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages() {
        assert_eq!(dictation_language(None), "en");
        assert_eq!(dictation_language(Some("sv")), "sv");
        assert_eq!(dictation_language(Some("sv-SE")), "sv");
        assert_eq!(dictation_language(Some("PT_br")), "pt");
        assert_eq!(dictation_language(Some("zh-CN")), "en");
        assert_eq!(dictation_language(Some("")), "en");
    }

    #[test]
    fn keyterms() {
        let terms =
            |t: &[&str]| keyterms_header(&t.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(
            terms(&["chimaera", "Claude, Codex", "chimaera", "  "]),
            "chimaera,Claude Codex"
        );
        assert_eq!(terms(&["café", "naïve"]), "caf,nave");
        // The budget stops the list at the first term that doesn't fit, as
        // claude's does — later short terms are not squeezed in.
        let long = "x".repeat(600);
        assert_eq!(terms(&[&long, &long.replace('x', "y"), "z"]), long);
    }

    #[test]
    fn service_messages() {
        assert_eq!(
            heard(r#"{"type":"TranscriptText","data":"hello there"}"#),
            Heard::Interim("hello there".into())
        );
        assert_eq!(
            heard(r#"{"type":"TranscriptInterim","data":"hel"}"#),
            Heard::Interim("hel".into())
        );
        assert_eq!(heard(r#"{"type":"TranscriptEndpoint"}"#), Heard::Endpoint);
        assert_eq!(
            heard(r#"{"type":"TranscriptError","error_code":"bad_audio"}"#),
            Heard::Failed("bad_audio".into())
        );
        assert_eq!(
            heard(r#"{"type":"error","message":"nope"}"#),
            Heard::Failed("nope".into())
        );
        assert_eq!(heard(r#"{"type":"Metadata"}"#), Heard::Other);
        assert_eq!(heard("not json"), Heard::Other);
    }

    #[test]
    fn url() {
        assert_eq!(
            service_url(None, "sv"),
            "wss://api.anthropic.com/api/ws/speech_to_text/voice_stream?encoding=linear16&sample_rate=16000&channels=1&endpointing_ms=300&utterance_end_ms=1000&language=sv&use_conversation_engine=true"
        );
        assert!(service_url(Some("ws://127.0.0.1:9"), "en").starts_with("ws://127.0.0.1:9/api/ws/"));
    }
}
