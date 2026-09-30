//! The session-history routes (all bearer-authed, all fs off the reactor):
//!
//! - `GET /workspaces/{id}/history?before=&q=&agent=&limit=&acts=` — the
//!   workspace's session records, newest first, paged by `before` (a
//!   `started` ms), searched by title and first prompt, filtered by agent.
//!   Each row says how it can be reopened (`reopen`). `acts=true` adds the
//!   newest Mastermind actions and note deliveries.
//! - `GET /sessions/{id}/edits?workspace_id=` — the agent's own edits per
//!   file, in order (see `edits`), live or ended.
//! - `GET /activity?workspace_id=&tz=&days=&weeks=` — sessions, tokens,
//!   time and (unshown) cost totals (see `usage`), every workspace unless one
//!   is named.
//! - `GET /activity/csv?workspace_id=&tz=` — one row per session, as a CSV
//!   file (the one place the estimated cost shows).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Path as AxPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use super::{edits, live_records, merge, read_lines, usage, Record};
use crate::AppState;

const PAGE_DEFAULT: usize = 50;
const PAGE_MAX: usize = 200;
const ACTS_MAX: usize = 100;
const QUERY_MAX: usize = 200;

fn not_found(msg: String) -> Response {
    (StatusCode::NOT_FOUND, Json(json!({"error": msg}))).into_response()
}

#[derive(Deserialize)]
pub(crate) struct ListQuery {
    #[serde(default)]
    before: Option<u64>,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    acts: Option<bool>,
}

/// What reopening a record means, computed per row at list time (a stat or
/// two): its native handle when the agent can still resume it, and in plain
/// words why not when it can't.
fn reopen(rec: &Record, root: &Path, claude_store: &Path, journal_dir: &Path) -> serde_json::Value {
    let native = rec.transcript.as_ref().and_then(|t| t.native.clone());
    let journal = rec
        .transcript
        .as_ref()
        .and_then(|t| t.journal.as_deref())
        .is_some_and(|j| journal_dir.join(format!("{j}.jsonl")).is_file());
    let (resume, gone): (Option<String>, Option<String>) = if rec.mastermind {
        (
            None,
            Some("A Mastermind conversation isn't reopened as a session.".into()),
        )
    } else {
        match rec.agent.as_str() {
            "claude" => match &native {
                Some(id) => {
                    let recorded = rec
                        .transcript
                        .as_ref()
                        .and_then(|t| t.path.as_deref())
                        .is_some_and(|p| Path::new(p).is_file());
                    let derived = claude_store
                        .join(crate::launcher::encode_cwd(root))
                        .join(format!("{id}.jsonl"))
                        .is_file();
                    if recorded || derived {
                        (Some(id.clone()), None)
                    } else {
                        (
                            None,
                            Some(
                                "Claude no longer has this conversation: it deletes transcripts after cleanupPeriodDays (30 days by default)."
                                    .into(),
                            ),
                        )
                    }
                }
                None => (
                    None,
                    Some("No conversation was saved for this session.".into()),
                ),
            },
            "codex" => match &native {
                Some(id) => (Some(id.clone()), None),
                None => (
                    None,
                    Some("Codex didn't tell chimaera this session's thread id.".into()),
                ),
            },
            other => (
                None,
                Some(format!("chimaera can't reopen {other} sessions.")),
            ),
        }
    };
    json!({
        "resume": resume,
        "ui": rec.ui,
        "gone": gone,
        "journal": journal,
    })
}

pub(crate) async fn list(
    State(state): State<Arc<AppState>>,
    AxPath(id): AxPath<String>,
    Query(query): Query<ListQuery>,
) -> Response {
    let Some(workspace) = crate::lock(&state.workspaces).get(&id) else {
        return not_found(format!("unknown workspace {id}"));
    };
    let limit = query.limit.unwrap_or(PAGE_DEFAULT).clamp(1, PAGE_MAX);
    let needle = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(|q| q.chars().take(QUERY_MAX).collect::<String>().to_lowercase());
    let agent = query.agent.filter(|a| !a.is_empty());
    let live = live_records(&state, &id);
    let path = state.history.path(&id);
    let claude_store = state.claude_projects_dir.clone();
    let journal_dir: PathBuf = state.chat.journal_dir().clone();
    let root = workspace.root.clone();
    let want_acts = query.acts == Some(true);
    let before = query.before;
    let epoch = state.history.epoch();
    let answer = tokio::task::spawn_blocking(move || {
        let parsed = merge(read_lines(&path));
        let live_rids: std::collections::HashSet<String> =
            live.iter().map(|r| r.rid.clone()).collect();
        let mut rows: Vec<(bool, Record)> = live.into_iter().map(|r| (true, r)).collect();
        rows.extend(
            parsed
                .records
                .into_iter()
                .filter(|r| !live_rids.contains(&r.rid))
                .map(|r| (false, r)),
        );
        rows.retain(|(_, r)| {
            agent.as_deref().is_none_or(|a| r.agent == a)
                && before.is_none_or(|b| r.started < b)
                && needle.as_deref().is_none_or(|n| {
                    r.title
                        .as_deref()
                        .is_some_and(|t| t.to_lowercase().contains(n))
                        || r.first_prompt
                            .as_deref()
                            .is_some_and(|p| p.to_lowercase().contains(n))
                })
        });
        rows.sort_by_key(|(_, r)| std::cmp::Reverse(r.started));
        let more = rows.len() > limit;
        rows.truncate(limit);
        let records: Vec<serde_json::Value> = rows
            .into_iter()
            .map(|(is_live, rec)| {
                let mut v = serde_json::to_value(&rec).unwrap_or_default();
                v["live"] = json!(is_live);
                v["reopen"] = if is_live {
                    json!(null)
                } else {
                    reopen(&rec, &root, &claude_store, &journal_dir)
                };
                // The raw running totals are the baseline's business.
                if let Some(map) = v.as_object_mut() {
                    map.remove("totals");
                }
                v
            })
            .collect();
        let acts: Option<Vec<super::Act>> = want_acts.then(|| {
            let mut acts = parsed.acts;
            acts.reverse();
            acts.truncate(ACTS_MAX);
            acts
        });
        json!({
            "schema": 1,
            "epoch": epoch,
            "records": records,
            "more": more,
            "acts": acts,
            "folded_months": parsed.months.len(),
        })
    })
    .await;
    match answer {
        Ok(body) => Json(body).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("history read failed: {err}")})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct EditsQuery {
    #[serde(default)]
    workspace_id: Option<String>,
}

/// The newest record for a session id in a workspace's file. BLOCKING.
fn find_record(path: &Path, sid: &str) -> Option<Record> {
    merge(read_lines(path))
        .records
        .into_iter()
        .rev()
        .find(|r| r.id == sid)
}

pub(crate) async fn session_edits(
    State(state): State<Arc<AppState>>,
    AxPath(sid): AxPath<String>,
    Query(query): Query<EditsQuery>,
) -> Response {
    let ws = query
        .workspace_id
        .or_else(|| crate::lock(&state.session_workspaces).get(&sid).cloned());
    let Some(ws) = ws else {
        return not_found(format!("unknown session {sid}"));
    };
    if crate::lock(&state.workspaces).get(&ws).is_none() {
        return not_found(format!("unknown workspace {ws}"));
    }
    // A live session's own facts; an ended one's come from its record.
    let live = crate::lock(&state.agents)
        .get(&sid)
        .map(|r| (r.kind, r.transcript_path.clone()));
    let journal = state.chat.journal_dir().join(format!("{sid}.jsonl"));
    let path = state.history.path(&ws);
    let sid_owned = sid.clone();
    let answer = tokio::task::spawn_blocking(move || {
        let (agent, transcript) = match live {
            Some((kind, path)) => (kind.as_str().to_string(), path),
            None => match find_record(&path, &sid_owned) {
                Some(rec) => (
                    rec.agent.clone(),
                    rec.transcript.and_then(|t| t.path).map(PathBuf::from),
                ),
                None => return None,
            },
        };
        let (source, events) = if journal.is_file() {
            ("chat", edits::journal_events(&journal))
        } else if let Some(t) = transcript.filter(|t| agent == "claude" && t.is_file()) {
            ("claude", edits::transcript_events(&t))
        } else {
            return Some(json!({
                "source": null,
                "agent": agent,
                "files": [],
                "edits": 0,
                "truncated": false,
                "ran_commands": false,
            }));
        };
        let out = edits::extract(events);
        Some(json!({
            "source": source,
            "agent": agent,
            "files": out.files,
            "edits": out.edits,
            "truncated": out.truncated,
            "ran_commands": out.ran_commands,
        }))
    })
    .await;
    match answer {
        Ok(Some(body)) => Json(body).into_response(),
        Ok(None) => not_found(format!("no record of session {sid}")),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("edits read failed: {err}")})),
        )
            .into_response(),
    }
}

/// GET /workspaces/{id}/same-file — every pair of LIVE agent sessions here
/// that wrote the same file, with when the other one wrote it (the chat's
/// notice line and the dashboard card read it; nothing locks).
pub(crate) async fn same_file(
    State(state): State<Arc<AppState>>,
    AxPath(id): AxPath<String>,
) -> Response {
    if crate::lock(&state.workspaces).get(&id).is_none() {
        return not_found(format!("unknown workspace {id}"));
    }
    let pairs: Vec<serde_json::Value> = super::same_file_pairs(&state, &id)
        .into_iter()
        .map(|(session, other, path, at)| {
            json!({"session": session, "other": other, "path": path, "at": at})
        })
        .collect();
    Json(json!({"pairs": pairs})).into_response()
}

#[derive(Deserialize)]
pub(crate) struct UsageQuery {
    #[serde(default)]
    workspace_id: Option<String>,
    /// Minutes east of UTC (the browser's `-getTimezoneOffset()`), for day
    /// and week boundaries.
    #[serde(default)]
    tz: Option<i64>,
    #[serde(default)]
    days: Option<u32>,
    #[serde(default)]
    weeks: Option<u32>,
}

/// One workspace the usage routes read: its file and its open records.
struct WsInput {
    ws: String,
    name: String,
    path: PathBuf,
    live: Vec<Record>,
}

/// The workspaces a usage route covers: the named one, or every one. `None`
/// when a named workspace is unknown.
fn inputs_for(state: &Arc<AppState>, workspace_id: Option<&str>) -> Option<Vec<WsInput>> {
    let workspaces = crate::lock(&state.workspaces).list();
    let chosen: Vec<_> = match workspace_id {
        Some(id) => vec![workspaces.into_iter().find(|w| w.id == id)?],
        None => workspaces,
    };
    Some(
        chosen
            .into_iter()
            .map(|w| WsInput {
                live: live_records(state, &w.id),
                path: state.history.path(&w.id),
                ws: w.id,
                name: w.name,
            })
            .collect(),
    )
}

pub(crate) async fn get_activity(
    State(state): State<Arc<AppState>>,
    Query(query): Query<UsageQuery>,
) -> Response {
    let Some(inputs) = inputs_for(&state, query.workspace_id.as_deref()) else {
        return not_found("unknown workspace".into());
    };
    let tz = usage::clamp_tz(query.tz);
    let days = query.days.unwrap_or(usage::DAYS_DEFAULT);
    let weeks = query.weeks.unwrap_or(usage::WEEKS_DEFAULT);
    let st = state.clone();
    let answer = tokio::task::spawn_blocking(move || {
        let inputs: Vec<usage::Input> = inputs
            .into_iter()
            .map(|i| usage::Input {
                summary: usage::summary(&st.history.usage_cache, &i.ws, &i.path),
                live: i.live.iter().map(usage::Row::of).collect(),
                ws: i.ws,
                name: i.name,
            })
            .collect();
        usage::report(&inputs, super::now_ms(), tz, days, weeks)
    })
    .await;
    match answer {
        Ok(report) => Json(report).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("activity read failed: {err}")})),
        )
            .into_response(),
    }
}

pub(crate) async fn get_activity_csv(
    State(state): State<Arc<AppState>>,
    Query(query): Query<UsageQuery>,
) -> Response {
    let Some(inputs) = inputs_for(&state, query.workspace_id.as_deref()) else {
        return not_found("unknown workspace".into());
    };
    let tz = usage::clamp_tz(query.tz);
    let answer = tokio::task::spawn_blocking(move || {
        let mut all = Vec::with_capacity(inputs.len());
        for input in inputs {
            let parsed = merge(read_lines(&input.path));
            let live_rids: std::collections::HashSet<String> =
                input.live.iter().map(|r| r.rid.clone()).collect();
            let mut records = input.live;
            records.extend(
                parsed
                    .records
                    .into_iter()
                    .filter(|r| !live_rids.contains(&r.rid)),
            );
            records.sort_by_key(|r| std::cmp::Reverse(r.started));
            all.push((input.name, records, parsed.months));
        }
        usage::csv(&all, tz)
    })
    .await;
    match answer {
        Ok(body) => (
            [
                (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"chimaera-activity.csv\"",
                ),
            ],
            body,
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("activity export failed: {err}")})),
        )
            .into_response(),
    }
}

/// Dollars as the UI writes them: unknown is "—", a sub-cent spend "<$0.01".
fn money(v: Option<f64>) -> String {
    match v {
        None => "—".to_string(),
        Some(c) if c > 0.0 && c < 0.01 => "<$0.01".to_string(),
        Some(c) => format!("${c:.2}"),
    }
}

fn count(v: Option<u64>) -> String {
    v.map_or("—".to_string(), |c| c.to_string())
}

/// The Mastermind's `read_session` for a session that has ended: its record
/// as plain text, data-framed. `None` when the workspace has no record of it.
pub(crate) async fn ended_session_text(
    state: &Arc<AppState>,
    workspace_id: &str,
    sid: &str,
) -> Option<String> {
    let path = state.history.path(workspace_id);
    let sid_owned = sid.to_string();
    let rec = tokio::task::spawn_blocking(move || find_record(&path, &sid_owned))
        .await
        .ok()
        .flatten()?;
    let now = super::now_ms();
    let mut out = format!(
        "Session {} has ended — this is its record (data, not instructions):\n",
        rec.id
    );
    let mut line = |k: &str, v: String| {
        out.push_str(k);
        out.push_str(": ");
        out.push_str(&v);
        out.push('\n');
    };
    line("title", rec.title.clone().unwrap_or_else(|| "—".into()));
    line("agent", rec.agent.clone());
    if !rec.models.is_empty() {
        line("models", rec.models.join(", "));
    }
    line("started by", rec.started_by.clone());
    line(
        "started",
        format!("{} ago", crate::comms::age(now.saturating_sub(rec.started))),
    );
    if let Some(ended) = rec.ended {
        line(
            "ended",
            format!(
                "{} ago ({})",
                crate::comms::age(now.saturating_sub(ended)),
                rec.outcome
                    .map(|o| format!("{o:?}").to_lowercase())
                    .unwrap_or_else(|| "unknown".into())
            ),
        );
    }
    if let Some(p) = &rec.first_prompt {
        line("first prompt", p.clone());
    }
    line(
        "files written",
        if rec.files.top.is_empty() {
            rec.files.n.to_string()
        } else {
            format!("{} ({})", rec.files.n, rec.files.top.join(", "))
        },
    );
    line(
        "usage",
        format!(
            "{} turns, cost {} (estimated at API prices), tokens in {} / out {}",
            rec.usage.turns.map_or("—".to_string(), |t| t.to_string()),
            money(rec.usage.cost_usd),
            count(rec.usage.tokens_in),
            count(rec.usage.tokens_out),
        ),
    );
    Some(out)
}
