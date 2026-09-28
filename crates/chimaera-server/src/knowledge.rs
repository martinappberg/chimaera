//! Knowledge: a read-only view over what agents RECORD — through a provider
//! plugin (mycelium's `.living/` today, when that plugin is active) — plus
//! the guidance and memory files agents already keep. Chimaera never writes
//! knowledge and never curates it (plan decision 3): agents record as they
//! work; the user reads, corrects in the files, and asks an agent to record.
//!
//! The provider is the active plugin whose manifest names
//! `provides.knowledge`; its `knowledge` export answers a snapshot in the
//! fixed shape the Knowledge view reads (the daemon↔UI wire) plus a stamp
//! naming the files it came from, or "unchanged" for the stamp held here.
//! The snapshot's own tools (`knowledge_search` / `knowledge_get`) are the
//! plugin's; this module never parses the provider's files.
//!
//! Two consumers: `GET /workspaces/{id}/knowledge` (the Knowledge view and
//! "Where things stand"), and the Timeline — at each episode end the
//! provider is re-checked (a few stats; a re-parse only when its files
//! changed) and what appeared is attributed to that turn ONLY when
//! unambiguous: the entry's file changed after the turn started AND no other
//! agent in the workspace was running AND the turn's own session hadn't
//! started another. Anything else is recorded unattributed — never guessed.
//! A finding's confidence move is its own Timeline entry; moves to or from
//! `unknown` (a torn read) never are. The Timeline's checks run on
//! `episodes`' per-workspace queue, never on the task that saw the turn end.
//!
//! A provider that fails keeps the last snapshot it answered: the route
//! serves it with an `error`, so a slow or faulted plugin reads as "couldn't
//! refresh", not as "no provider". The cache holds one snapshot per
//! workspace, of the build active there — dropped when the provider is
//! switched off, updated, removed or changes build.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::plugins::Manifest;
use crate::timeline::{self, Entry, Kind, KnowledgeChange, Recorded};
use crate::AppState;

/// Grace for mtime vs turn start (clock granularity, a write racing the
/// first event).
const ATTRIBUTION_SLACK_MS: u64 = 2_000;
/// Unattributed "new finding" entries per check (a bulk import is one line
/// of news, not fifty).
const NEW_FINDINGS_MAX: usize = 5;
/// Memory notes counted.
const MEMORY_NOTES_MAX: usize = 200;

#[derive(Default)]
pub(crate) struct KnowledgeState {
    cache: HashMap<String, Cached>,
    /// A snapshot the provider gave but that was refused (too big, not a
    /// JSON object), per workspace: its stamp is what the next ask hands
    /// back, so an unchanged tree is refused again without a re-read.
    refused: HashMap<String, Refused>,
    /// What the Timeline last saw, per workspace (the diff baseline).
    baseline: HashMap<String, Baseline>,
}

struct Refused {
    build: (String, Arc<str>),
    stamp: Value,
    error: String,
}

impl KnowledgeState {
    pub(crate) fn forget_workspace(&mut self, ws: &str) {
        self.cache.remove(ws);
        self.refused.remove(ws);
        self.baseline.remove(ws);
    }

    /// Tests: whether a snapshot is cached for `ws`.
    #[cfg(test)]
    pub(crate) fn holds(&self, ws: &str) -> bool {
        self.cache.contains_key(ws)
    }

    /// Drop what plugin `id` answered — everywhere, or in `ws` only (its
    /// switch flipped there). An install, update, rollback or remove
    /// changes the build; switched off, it isn't the provider any more.
    pub(crate) fn forget_provider(&mut self, id: &str, ws: Option<&str>) {
        let gone =
            |w: &String, build: &(String, Arc<str>)| build.0 == id && ws.is_none_or(|ws| ws == w);
        self.cache.retain(|w, c| !gone(w, &c.build));
        self.refused.retain(|w, r| !gone(w, &r.build));
        self.baseline.retain(|w, b| !gone(w, &b.build));
    }
}

struct Cached {
    build: (String, Arc<str>),
    /// The provider's stamp, handed back so it can answer "unchanged".
    stamp: Value,
    files: Arc<Stamp>,
    knowledge: Arc<Value>,
}

/// The files a snapshot was read from, as its stamp names them: `(path,
/// mtime_ms, len)`, workspace-relative. A stamp in another shape names no
/// files, and then nothing is attributed to a turn.
#[derive(Deserialize, Default, Debug)]
struct Stamp {
    #[serde(default)]
    files: Vec<(String, u64, u64)>,
}

impl Stamp {
    /// mtime (ms) of one stamped file — the Timeline attributes a new entry
    /// to a turn only when its file changed after that turn started.
    fn mtime_of(&self, rel: &str) -> Option<u64> {
        self.files
            .iter()
            .find(|(path, _, _)| path == rel)
            .map(|(_, mtime, _)| *mtime)
    }
}

/// The provider's current snapshot.
struct Current {
    build: (String, Arc<str>),
    /// The provider's name for the route (`provides.knowledge`).
    provider: String,
    knowledge: Arc<Value>,
    files: Arc<Stamp>,
}

impl Current {
    fn of(cached: &Cached, provider: &str) -> Self {
        Current {
            build: cached.build.clone(),
            provider: provider.to_string(),
            knowledge: cached.knowledge.clone(),
            files: cached.files.clone(),
        }
    }
}

/// What asking a workspace's provider came to.
enum Answer {
    /// No provider is active there.
    NoProvider,
    Fresh(Current),
    /// It failed (faulted, trapped, too slow, a snapshot that isn't a JSON
    /// object): why, and the last snapshot this build answered here.
    Failed {
        provider: String,
        last: Option<Current>,
        error: String,
    },
}

#[derive(Default)]
struct Baseline {
    build: (String, Arc<str>),
    ids: HashSet<String>,
    statuses: HashMap<String, String>,
}

/// The active Knowledge provider plugin in `ws`, if any.
async fn provider(state: &AppState, ws: &str) -> Option<Arc<Manifest>> {
    crate::plugins::active(state, ws)
        .await
        .into_iter()
        .find(|m| m.provides.knowledge.is_some())
}

/// Ask the provider, handing it the stamp held here (it re-stats — cheap)
/// so it re-reads only when its files moved. With no provider the
/// workspace's snapshot and baseline go; another build's go before asking.
async fn ask(state: &Arc<AppState>, ws: &str) -> Answer {
    let Some(m) = provider(state, ws).await else {
        crate::lock(&state.knowledge).forget_workspace(ws);
        return Answer::NoProvider;
    };
    let Some(name) = m.provides.knowledge.clone() else {
        return Answer::NoProvider;
    };
    // Stamps belong to one provider build; another build may interpret the
    // same files differently, even when none of their mtimes changed.
    let build = (m.id.clone(), m.wasm.sha256.clone());
    {
        let mut st = crate::lock(&state.knowledge);
        if st.cache.get(ws).is_some_and(|c| c.build != build) {
            st.cache.remove(ws);
        }
        if st.refused.get(ws).is_some_and(|r| r.build != build) {
            st.refused.remove(ws);
        }
        if st.baseline.get(ws).is_some_and(|b| b.build != build) {
            st.baseline.remove(ws);
        }
    }
    let failed = |error: String| {
        tracing::warn!(plugin = %m.id, workspace = ws, %error, "no fresh knowledge snapshot");
        let last = crate::lock(&state.knowledge)
            .cache
            .get(ws)
            .filter(|c| c.build == build)
            .map(|c| Current::of(c, &name));
        Answer::Failed {
            provider: name.clone(),
            last,
            error,
        }
    };
    let refuse = |stamp: Value, error: String| {
        let refused = Refused {
            build: build.clone(),
            stamp,
            error: error.clone(),
        };
        crate::lock(&state.knowledge)
            .refused
            .insert(ws.to_string(), refused);
        failed(error)
    };
    // Twice at most: "unchanged" for a stamp whose entry was replaced or
    // dropped meanwhile (another ask, the workspace forgotten) is asked
    // again with what is held now.
    for _ in 0..2 {
        // What to hand back: a refused snapshot's stamp (its tree unchanged,
        // it's refused again unread), else the cached one's — this build's.
        let (held, refused) = {
            let st = crate::lock(&state.knowledge);
            match st.refused.get(ws).filter(|r| r.build == build) {
                Some(r) => (Some(r.stamp.clone()), Some(r.error.clone())),
                None => (
                    st.cache
                        .get(ws)
                        .filter(|c| c.build == build)
                        .map(|c| c.stamp.clone()),
                    None,
                ),
            }
        };
        let answer = state
            .plugin_runtime
            .knowledge(state, &m, ws, held.as_ref())
            .await;
        match answer {
            Ok(None) => {
                let st = crate::lock(&state.knowledge);
                if let Some(error) = refused {
                    let still = st
                        .refused
                        .get(ws)
                        .is_some_and(|r| r.build == build && Some(&r.stamp) == held.as_ref());
                    drop(st);
                    if still {
                        return failed(error);
                    }
                } else if let Some(c) = st
                    .cache
                    .get(ws)
                    .filter(|c| c.build == build && Some(&c.stamp) == held.as_ref())
                {
                    return Answer::Fresh(Current::of(c, &name));
                }
            }
            Ok(Some((stamp, Err(error)))) => return refuse(stamp, error),
            Ok(Some((stamp, Ok(data)))) => {
                if !data.is_object() {
                    return refuse(stamp, format!("{}'s snapshot is not a JSON object", m.name));
                }
                let files = Arc::new(Stamp::deserialize(&stamp).unwrap_or_default());
                let cached = Cached {
                    build: build.clone(),
                    stamp,
                    files,
                    knowledge: Arc::new(data),
                };
                let now = Current::of(&cached, &name);
                let mut st = crate::lock(&state.knowledge);
                st.refused.remove(ws);
                st.cache.insert(ws.to_string(), cached);
                return Answer::Fresh(now);
            }
            Err(err) => return failed(err),
        }
    }
    // Asks racing this one kept replacing what it held: the snapshot they
    // left is this build's current answer.
    let settled = {
        let st = crate::lock(&state.knowledge);
        match st.refused.get(ws).filter(|r| r.build == build) {
            Some(r) => Err(r.error.clone()),
            None => Ok(st
                .cache
                .get(ws)
                .filter(|c| c.build == build)
                .map(|c| Current::of(c, &name))),
        }
    };
    match settled {
        Ok(Some(now)) => Answer::Fresh(now),
        Err(error) => failed(error),
        Ok(None) => failed(format!(
            "{} answered \"unchanged\" for a snapshot it never gave",
            m.name
        )),
    }
}

/// (entry id, kind, the workspace-relative file it lives in).
type EntryIds = Vec<(String, &'static str, String)>;

/// The items of the array `key` in a snapshot object (none when absent).
fn items<'a>(v: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    v.get(key).and_then(Value::as_array).into_iter().flatten()
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Every entry id in a knowledge snapshot, with the file it lives in, and
/// the findings' statuses. The snapshot is the Knowledge view's shape:
/// `topics[].{path, findings[].{id, status}}`, `decisions[].fp`,
/// `learnings[].fp`. Ids repeat in real repositories (`### F-027 addendum:`
/// under F-027, a number reused): the FIRST entry with an id is the one
/// meant — its status is the finding's (an addendum's "unrated" must not
/// read as a move), and a repeat is neither news nor a second credit.
fn ids_of(k: &Value) -> (EntryIds, HashMap<String, String>) {
    let mut ids = Vec::new();
    let mut seen = HashSet::new();
    let mut statuses = HashMap::new();
    for topic in items(k, "topics") {
        for f in items(topic, "findings") {
            let id = text(f, "id");
            if id.is_empty() || !seen.insert(id.to_string()) {
                continue;
            }
            ids.push((id.to_string(), "finding", text(topic, "path").to_string()));
            statuses.insert(id.to_string(), text(f, "status").to_string());
        }
    }
    for (list, kind, file) in [
        ("decisions", "decision", ".living/decisions.md"),
        ("learnings", "learning", ".living/learnings.md"),
    ] {
        for e in items(k, list) {
            let fp = text(e, "fp");
            if !fp.is_empty() && seen.insert(fp.to_string()) {
                ids.push((fp.to_string(), kind, file.to_string()));
            }
        }
    }
    (ids, statuses)
}

fn claim_of(k: &Value, id: &str) -> String {
    items(k, "topics")
        .flat_map(|t| items(t, "findings"))
        .find(|f| text(f, "id") == id)
        .map(|f| timeline::cap(text(f, "claim"), 200))
        .unwrap_or_default()
}

/// Whether another agent in the workspace may have written during this turn
/// (`start_ts`..now) — then crediting this one would be a guess. It may if it
/// is mid-turn now (running, awaiting permission, rate-limited) or untracked
/// (a hook-less TUI — codex/gemini terminals — whose turns we never see), or
/// if it finished a turn since `start_ts` (its episode is on the Timeline).
/// `Unknown` on a chat session or a claude TUI only means "no turn yet". A
/// human editing the files by hand stays invisible; the mtime gate is all
/// that bounds that.
async fn another_agent_active_since(
    state: &AppState,
    ws: &str,
    sid: &str,
    start_ts: Option<u64>,
) -> bool {
    use crate::agent_state::AgentState;
    let in_ws: Vec<String> = crate::lock(&state.session_workspaces)
        .iter()
        .filter(|(s, w)| w.as_str() == ws && s.as_str() != sid)
        .map(|(s, _)| s.clone())
        .collect();
    let chats: HashSet<&String> = in_ws
        .iter()
        .filter(|s| state.chat.get(s).is_some())
        .collect();
    let unsettled = {
        let agents = crate::lock(&state.agents);
        in_ws.iter().any(|s| {
            agents.get(s).is_some_and(|r| match r.state {
                AgentState::Running | AgentState::NeedsPermission | AgentState::RateLimited => true,
                AgentState::Unknown => {
                    !chats.contains(s) && r.kind != crate::agents::AgentKind::Claude
                }
                AgentState::Finished | AgentState::IdlePrompt | AgentState::Errored => false,
            })
        })
    };
    if unsettled {
        return true;
    }
    let Some(start) = start_ts else {
        return true;
    };
    state
        .timeline
        .latest(ws, RECENT_EPISODES)
        .await
        .iter()
        .any(|e| {
            e.kind == timeline::Kind::Episode
                && e.ts >= start
                && e.sid.as_deref().is_some_and(|s| s != sid)
        })
}

/// How far back the "did anyone else finish a turn" check looks — a turn's
/// window rarely holds more than a handful of other entries.
const RECENT_EPISODES: usize = 100;

/// Entry id (F-003 or a fingerprint) → who recorded it (sid, name), read
/// back from the episodes' own evidence on the Timeline — durable, so the
/// credits survive a restart — over the in-memory ring (`RING_CAP` newest
/// entries; the newest credit wins). Never the file: the Knowledge view
/// refetches on every Timeline change.
async fn recorded_by(state: &AppState, ws: &str) -> HashMap<String, (String, String)> {
    let mut by = HashMap::new();
    for e in state.timeline.in_memory(ws).await {
        let (Some(sid), Some(rec)) = (
            e.sid.as_ref(),
            e.evidence.as_ref().and_then(|ev| ev.recorded.as_ref()),
        ) else {
            continue;
        };
        let name = e.name.clone().unwrap_or_else(|| sid.clone());
        for fp in &rec.fps {
            by.entry(fp.clone())
                .or_insert_with(|| (sid.clone(), name.clone()));
        }
    }
    by
}

/// Take the diff baseline if none exists yet — when a turn STARTS (from
/// `episodes`' queue) and when the Knowledge view loads — so the first turn
/// after a daemon start can still be credited with what it records. A few
/// stats when the provider is active; nothing otherwise.
pub(crate) async fn prime_workspace(state: &Arc<AppState>, ws: &str) {
    if crate::lock(&state.knowledge).baseline.contains_key(ws) {
        return;
    }
    if let Answer::Fresh(now) = ask(state, ws).await {
        take_baseline(state, ws, &now);
    }
}

/// `now` as the baseline, unless one was taken meanwhile. The ids are
/// collected outside the lock (hundreds of entries).
fn take_baseline(state: &AppState, ws: &str, now: &Current) {
    if has_baseline(state, ws) {
        return;
    }
    let (ids, statuses) = ids_of(&now.knowledge);
    crate::lock(&state.knowledge)
        .baseline
        .entry(ws.to_string())
        .or_insert(Baseline {
            build: now.build.clone(),
            ids: ids.into_iter().map(|(id, _, _)| id).collect(),
            statuses,
        });
}

pub(crate) fn has_baseline(state: &AppState, ws: &str) -> bool {
    crate::lock(&state.knowledge).baseline.contains_key(ws)
}

/// Forget the diff baseline: the next check only takes one again, so what
/// was recorded meanwhile is history, never credited to a later turn (the
/// episode queue fell behind and skipped checks).
pub(crate) fn forget_baseline(state: &AppState, ws: &str) {
    crate::lock(&state.knowledge).baseline.remove(ws);
}

/// Called at an episode end in `sid` (`name` on the Timeline; turn started
/// at `start_ts`): diff the provider against the last check, attribute what
/// is unambiguous to this turn (the returned `Recorded`), and put status
/// moves (and unattributed new findings) on the Timeline. `may_credit` is
/// false when the session has moved on to another turn since this one
/// ended — what the files hold now may be that turn's. The first check in a
/// workspace only sets the baseline — history is not news. A provider that
/// fails leaves the baseline as it was, for the next check to diff against.
pub(crate) async fn recorded_since_last_check(
    state: &Arc<AppState>,
    ws: &str,
    sid: &str,
    name: &str,
    start_ts: Option<u64>,
    may_credit: bool,
) -> Option<Recorded> {
    let Answer::Fresh(Current {
        build,
        knowledge: k,
        files: stamp,
        ..
    }) = ask(state, ws).await
    else {
        return None;
    };
    let (ids, statuses) = ids_of(&k);
    let previous = {
        let mut st = crate::lock(&state.knowledge);
        st.baseline.insert(
            ws.to_string(),
            Baseline {
                build: build.clone(),
                ids: ids.iter().map(|(id, _, _)| id.clone()).collect(),
                statuses: statuses.clone(),
            },
        )
    };
    let previous = previous.filter(|p| p.build == build)?;
    let sole = may_credit && !another_agent_active_since(state, ws, sid, start_ts).await;
    let fresh = |file: &str| {
        start_ts.is_some_and(|start| {
            stamp
                .mtime_of(file)
                .is_some_and(|m| m + ATTRIBUTION_SLACK_MS >= start)
        })
    };

    let mut recorded = Recorded::default();
    let mut unattributed_findings = Vec::new();
    for (id, kind, file) in &ids {
        if previous.ids.contains(id) {
            continue;
        }
        if sole && fresh(file) {
            match *kind {
                "finding" => recorded.findings.push(id.clone()),
                "decision" => recorded.decisions += 1,
                _ => recorded.learnings += 1,
            }
            recorded.fps.push(id.clone());
        } else if *kind == "finding" {
            unattributed_findings.push(id.clone());
        }
    }
    // Confidence moves: their own Timeline entries (never via `unknown`), in
    // id order so one check's moves always land in the same sequence.
    let mut moves: Vec<(&String, &String)> = statuses.iter().collect();
    moves.sort();
    for (id, to) in moves {
        let Some(from) = previous.statuses.get(id) else {
            continue;
        };
        if from == to || from == "unknown" || to == "unknown" {
            continue;
        }
        let mut entry = Entry::new(Kind::Knowledge);
        if sole {
            entry.sid = Some(sid.to_string());
            entry.name = Some(name.to_string());
        }
        entry.knowledge = Some(KnowledgeChange {
            change: "status".into(),
            id: id.clone(),
            from: Some(from.clone()),
            to: to.clone(),
            claim: claim_of(&k, id),
        });
        state.timeline.append(ws, entry).await;
    }
    for id in unattributed_findings.into_iter().take(NEW_FINDINGS_MAX) {
        let mut entry = Entry::new(Kind::Knowledge);
        entry.knowledge = Some(KnowledgeChange {
            change: "new".into(),
            id: id.clone(),
            from: None,
            to: statuses.get(&id).cloned().unwrap_or_default(),
            claim: claim_of(&k, &id),
        });
        state.timeline.append(ws, entry).await;
    }
    (!recorded.fps.is_empty()).then_some(recorded)
}

/// The guidance agents are already handed + claude's per-project memory.
fn guidance(root: &Path) -> Vec<Value> {
    let mut out = Vec::new();
    for (file, label, fallback) in [
        (
            "MYCELIUM.md",
            "MYCELIUM.md",
            "How agents record knowledge here",
        ),
        (
            "AGENTS.md",
            "AGENTS.md",
            "What codex (and other agents) are told",
        ),
        ("CLAUDE.md", "CLAUDE.md", "What claude is told"),
    ] {
        let path = root.join(file);
        // Follow a symlink only when it stays inside the project (the common
        // CLAUDE.md → AGENTS.md); one pointing elsewhere is not listed.
        let inside = std::fs::canonicalize(&path)
            .ok()
            .zip(std::fs::canonicalize(root).ok())
            .is_some_and(|(target, root)| target.starts_with(root));
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !inside || !meta.is_file() || meta.len() > 1024 * 1024 {
            continue;
        }
        // A thin adapter (mycelium's, a one-line @-include, or a symlink)
        // says where it points rather than pretending to be the guidance.
        let link = std::fs::read_link(&path)
            .ok()
            .and_then(|t| t.file_name().map(|n| n.to_string_lossy().into_owned()));
        let head = std::fs::read_to_string(&path)
            .map(|t| t.chars().take(4096).collect::<String>())
            .unwrap_or_default();
        let description = if let Some(target) = link.filter(|t| t != file) {
            format!("{fallback} → {target}")
        } else if head.contains("MYCELIUM:BEGIN") && file != "MYCELIUM.md" {
            format!("{fallback} → MYCELIUM.md")
        } else if head.trim().lines().any(|l| l.trim() == "@AGENTS.md") {
            format!("{fallback} → AGENTS.md")
        } else {
            fallback.to_string()
        };
        out.push(json!({"path": file, "label": label, "description": description}));
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let dir = home
            .join(".claude/projects")
            .join(crate::launcher::encode_cwd(root))
            .join("memory");
        if let Ok(entries) = std::fs::read_dir(&dir) {
            let notes = entries
                .flatten()
                .take(MEMORY_NOTES_MAX)
                .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
                .filter(|e| e.file_name() != "MEMORY.md")
                .count();
            if notes > 0 {
                let index = dir.join("MEMORY.md");
                let path = if index.is_file() { index } else { dir };
                out.push(json!({
                    "path": path,
                    "label": "claude memory",
                    "description": format!(
                        "{notes} note{} claude keeps for this project on this host",
                        if notes == 1 { "" } else { "s" }
                    ),
                }));
            }
        }
    }
    out
}

/// The body with no snapshot: guidance only, every list empty.
fn empty_body(provider: Value, guidance: Vec<Value>) -> Value {
    json!({
        "schema": 1,
        "provider": provider,
        "left_off": Value::Null,
        "topics": [],
        "decisions": [],
        "learnings": [],
        "todos": [],
        "questions": [],
        "counts": {"findings": 0, "decisions": 0, "learnings": 0, "open": 0},
        "guidance": guidance,
        "warnings": [],
    })
}

/// GET /workspaces/{id}/knowledge — the Knowledge view's data. With no
/// structured provider active: `provider: null`, guidance only, empty lists.
/// A provider that failed to answer adds `error` (additive, only then) to
/// the last snapshot it gave here — or to empty lists, still naming itself:
/// it is the provider, it just couldn't read.
pub(crate) async fn get_knowledge(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let Some(root) = crate::lock(&state.workspaces).get(&id).map(|w| w.root) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "unknown workspace"})),
        )
            .into_response();
    };
    // A few stats on an NFS home, overlapping the provider's answer.
    let guidance = async {
        let root = root.clone();
        tokio::task::spawn_blocking(move || guidance(&root))
            .await
            .unwrap_or_default()
    };
    let (guidance, answer) = tokio::join!(guidance, ask(&state, &id));
    let (now, error) = match answer {
        Answer::NoProvider => return Json(empty_body(Value::Null, guidance)).into_response(),
        // The one ask also primes the Timeline's baseline.
        Answer::Fresh(now) => {
            take_baseline(&state, &id, &now);
            (now, None)
        }
        Answer::Failed {
            last: Some(last),
            error,
            ..
        } => (last, Some(error)),
        Answer::Failed {
            provider,
            last: None,
            error,
        } => {
            let mut body = empty_body(json!(provider), guidance);
            body["error"] = json!(error);
            return Json(body).into_response();
        }
    };
    // Who recorded what, where the Timeline knows it.
    let by = recorded_by(&state, &id).await;
    // A snapshot is up to `runtime::SNAPSHOT_MAX` of JSON: copied,
    // annotated and serialized on the blocking pool.
    let body = tokio::task::spawn_blocking(move || {
        // The provider's snapshot as it serialized it: the wire the view reads.
        let mut body = (*now.knowledge).clone();
        let annotate = |v: &mut Value, key: &str| {
            if let Some(id) = v.get(key).and_then(Value::as_str) {
                if let Some((sid, name)) = by.get(id) {
                    v["recorded_by"] = json!({"sid": sid, "name": name});
                }
            }
        };
        if let Some(topics) = body.get_mut("topics").and_then(Value::as_array_mut) {
            for t in topics {
                if let Some(fs) = t.get_mut("findings").and_then(Value::as_array_mut) {
                    fs.iter_mut().for_each(|f| annotate(f, "id"));
                }
            }
        }
        for list in ["decisions", "learnings"] {
            if let Some(items) = body.get_mut(list).and_then(Value::as_array_mut) {
                items.iter_mut().for_each(|e| annotate(e, "fp"));
            }
        }
        body["schema"] = json!(1);
        body["provider"] = json!(now.provider);
        body["guidance"] = json!(guidance);
        // `error` is the daemon's word that the provider couldn't answer,
        // never a field of the provider's own.
        if let Some(fields) = body.as_object_mut() {
            fields.remove("error");
        }
        if body.get("left_off").is_none() {
            body["left_off"] = Value::Null;
        }
        if let Some(error) = error {
            body["error"] = json!(error);
        }
        serde_json::to_vec(&body)
    })
    .await;
    match body {
        Ok(Ok(bytes)) => (
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            bytes,
        )
            .into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "could not serialize the knowledge snapshot"})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `.living/` files addenda under their finding's id: the first
    /// entry is the finding (its status counts), repeats are counted once.
    #[test]
    fn a_repeated_id_is_its_first_entry() {
        let k = json!({
            "topics": [
                {"path": ".living/findings/a.md", "findings": [
                    {"id": "F-027", "status": "supported", "claim": "the finding"},
                    {"id": "F-027", "status": "unknown", "claim": "addendum: more"},
                ]},
                {"path": ".living/findings/b.md", "findings": [
                    {"id": "F-027", "status": "contradicted"},
                    {"id": "F-028", "status": "robust"},
                ]},
            ],
            "decisions": [{"fp": "d1"}, {"fp": "d1"}],
            "learnings": [{"fp": "l1"}],
        });
        let (ids, statuses) = ids_of(&k);
        let names: Vec<&str> = ids.iter().map(|(id, _, _)| id.as_str()).collect();
        assert_eq!(names, ["F-027", "F-028", "d1", "l1"]);
        assert_eq!(ids[0].2, ".living/findings/a.md");
        assert_eq!(statuses["F-027"], "supported");
        assert_eq!(claim_of(&k, "F-027"), "the finding");
    }
}
