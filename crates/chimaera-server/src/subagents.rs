//! One subagent's own conversation, read-only (bearer-authed):
//!
//! `GET /sessions/{id}/subagents/{agent_id}/transcript?live=&epoch=&after=&stamp=`
//!
//! The chat shows a subagent as one row; this is what it did, as the same
//! normalized events a chat of its own would have produced, for the
//! subagent view. Nothing here is journaled or cached: claude's comes from
//! the transcript file it keeps beside the parent's, codex's from the live
//! driver (the child thread is readable only on its app-server connection).
//!
//! A view of a working subagent re-reads every few seconds, so the answer
//! is incremental. Events are numbered from 0 within an `epoch`; a reader
//! holding the first `after` of that epoch gets only the rest (`from` says
//! where they start). A different `epoch` means the window moved: the
//! answer is the whole window (`from: 0`) and the reader starts over.
//! `stamp` (claude: the file's size) lets an unchanged file answer without
//! being read at all.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chimaera_agent::subagent::{valid_agent_id, SubagentTranscript};
use serde::Deserialize;
use serde_json::json;

use crate::agents::AgentKind;
use crate::AppState;

/// Project folders examined when the transcript is not where the session's
/// folder says (the session moved since it started). A bound, not a tuning
/// knob: one `stat` each.
const PROJECT_SCAN_MAX: usize = 4096;

#[derive(Deserialize)]
pub(crate) struct TranscriptQuery {
    /// The subagent is still working: leave its last turn open.
    #[serde(default)]
    live: bool,
    /// The window the reader already holds events of.
    #[serde(default)]
    epoch: Option<String>,
    /// How many of that window's events the reader holds.
    #[serde(default)]
    after: Option<usize>,
    /// The `stamp` of the reader's last answer.
    #[serde(default)]
    stamp: Option<String>,
}

fn refuse(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({"error": msg.into()}))).into_response()
}

/// Where claude keeps a session's subagent transcripts: a `subagents`
/// folder named after the session's own transcript.
fn beside_transcript(transcript: &Path, agent_id: &str) -> PathBuf {
    transcript
        .with_extension("")
        .join("subagents")
        .join(format!("agent-{agent_id}.jsonl"))
}

/// The subagent's transcript file, wherever this session's transcripts are.
/// Tries the path a hook reported, then the folder claude derives from the
/// session's cwd, then every project folder (claude files a session under
/// the cwd it STARTED in). BLOCKING.
fn find_claude_transcript(
    projects: &Path,
    reported: Option<&Path>,
    cwd: Option<&Path>,
    native: Option<&str>,
    agent_id: &str,
) -> Option<PathBuf> {
    if let Some(path) = reported.map(|t| beside_transcript(t, agent_id)) {
        if path.is_file() {
            return Some(path);
        }
    }
    let native = native?;
    let in_project = |project: &Path| beside_transcript(&project.join(native), agent_id);
    if let Some(cwd) = cwd {
        let path = in_project(&projects.join(crate::launcher::encode_cwd(cwd)));
        if path.is_file() {
            return Some(path);
        }
    }
    std::fs::read_dir(projects)
        .ok()?
        .flatten()
        .take(PROJECT_SCAN_MAX)
        .map(|entry| in_project(&entry.path()))
        .find(|path| path.is_file())
}

pub(crate) async fn subagent_transcript(
    State(state): State<Arc<AppState>>,
    AxPath((sid, agent_id)): AxPath<(String, String)>,
    Query(query): Query<TranscriptQuery>,
) -> Response {
    // The id names a file (claude) and a thread (codex): only what the
    // agents themselves mint.
    if !valid_agent_id(&agent_id) {
        return refuse(StatusCode::BAD_REQUEST, "invalid subagent id");
    }
    let record = crate::lock(&state.agents)
        .get(&sid)
        .map(|r| (r.kind, r.transcript_path.clone()));
    let chat = state.chat.get(&sid);
    let kind = match (&record, &chat) {
        (Some((kind, _)), _) => kind.as_str().to_string(),
        (None, Some(info)) => info.agent.clone(),
        (None, None) => return refuse(StatusCode::NOT_FOUND, format!("unknown session {sid}")),
    };

    let held = query.after.filter(|_| query.epoch.is_some()).unwrap_or(0);
    let read: Result<Option<(SubagentTranscript, Option<String>)>, String> =
        if kind == AgentKind::Claude.as_str() {
            let projects = state.claude_projects_dir.clone();
            let reported = record.and_then(|(_, path)| path);
            let cwd = chat.as_ref().map(|info| info.cwd.clone());
            let native = chat.and_then(|info| info.native_session_id);
            let id = agent_id.clone();
            let (live, epoch, stamp) = (query.live, query.epoch.clone(), query.stamp.clone());
            tokio::task::spawn_blocking(move || {
                let path = find_claude_transcript(
                    &projects,
                    reported.as_deref(),
                    cwd.as_deref(),
                    native.as_deref(),
                    &id,
                )
                .ok_or("this subagent has no transcript yet")?;
                let size = std::fs::metadata(&path)
                    .map(|m| m.len().to_string())
                    .map_err(|_| "this subagent has no transcript yet")?;
                // Same size, still working: nothing was appended, so the
                // reader's events stand. (A finished read always runs — it
                // adds the closing events without the file changing.)
                if live && epoch.is_some() && stamp.as_deref() == Some(size.as_str()) {
                    return Ok(None);
                }
                chimaera_agent::transcript::import_subagent_transcript(&path, live)
                    .map(|read| Some((read, Some(size))))
                    .ok_or_else(|| "this subagent's transcript could not be read".to_string())
            })
            .await
            .unwrap_or_else(|err| Err(format!("transcript read failed: {err}")))
        } else {
            state
                .chat
                .subagent_transcript(&sid, &agent_id, query.live)
                .await
                .map(|read| Some((read, None)))
                .map_err(|err| err.to_string())
        };

    match read {
        Ok(None) => Json(json!({
            "agent": kind,
            "epoch": query.epoch,
            "from": held,
            "events": [],
            "stamp": query.stamp,
        }))
        .into_response(),
        Ok(Some((read, stamp))) => {
            // Within one epoch a later read extends an earlier one, so the
            // reader's first `held` events are these first `held`.
            let from = if query.epoch.as_deref() == Some(read.epoch.as_str()) {
                held.min(read.events.len())
            } else {
                0
            };
            let mut body = json!({
                "agent": kind,
                "epoch": read.epoch,
                "from": from,
                "events": &read.events[from..],
                "model": read.model,
                "stamp": stamp,
            });
            // When the source dates its records: epoch ms per event, 0 = unknown.
            if read.timestamps.len() == read.events.len() {
                body["ts"] = json!(&read.timestamps[from..]);
            }
            Json(body).into_response()
        }
        Err(reason) => refuse(StatusCode::NOT_FOUND, reason),
    }
}
