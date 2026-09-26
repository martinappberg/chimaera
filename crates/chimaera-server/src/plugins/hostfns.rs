//! The `host` interface of `chimaera:plugin`: everything a plugin may ask
//! the daemon, each call bounded HERE so no plugin can forget a limit.
//! Design: docs/plugin-system-plan.md ("The host", the limits table).
//!
//! Every function serves the workspace the instance belongs to and the
//! context the runtime made for the call in flight (`CallScope`). The `cx`
//! a guest passes back is ignored: a plugin can't speak for another
//! workspace or session by rewriting it.
//!
//! - Files: workspace-relative paths only (`..` and absolute paths
//!   refused), walked from the workspace root with `O_NOFOLLOW` on every
//!   component, so no symlink anywhere in the path is followed; reads stop
//!   at `cap` (≤ 8 MiB), listings at `cap` entries (≤ 4,096). Off the
//!   reactor, behind `fs::FILESYSTEM_WORK`.
//! - State: 64 KiB per (plugin, workspace), in memory (`PluginStates`).
//! - Timeline: `note` entries only, from a session's call, ≤ `TEXT_MAX`,
//!   addressed within the workspace, under the per-session posts-per-minute
//!   window `tell_mastermind` shares (`notes::take_post_slot`).
//! - `emit`: one JSON object ≤ 16 KiB per frame, a bounded ring.
//! - `log`: ≤ 64 lines per call, each ≤ 2 KiB.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};

use super::runtime::chimaera::plugin::host;
use super::runtime::{wit, CallScope, HostState, EVENT_MAX};
use crate::timeline;

/// A read's ceiling, whatever `cap` the plugin asks for.
const READ_CEILING: usize = 8 << 20;
/// A listing's ceiling.
const LIST_CEILING: usize = 4096;
/// State per (plugin, workspace): keys plus values, in bytes.
const STATE_CAP: usize = 64 << 10;
pub(super) const LOGS_PER_CALL: usize = 64;
const LOG_LINE_MAX: usize = 2 * 1024;
/// Sessions answered per call (a workspace has far fewer).
const SESSIONS_MAX: usize = 256;
/// Kinds one `timeline-recent` may name.
const KINDS_MAX: usize = 16;

/// Small per-(plugin, workspace) state plugins keep in the daemon (read
/// cursors, caches). In memory: a daemon restart starts it over.
#[derive(Default)]
pub(crate) struct PluginStates {
    by_key: HashMap<(String, String), BTreeMap<String, String>>,
}

impl PluginStates {
    /// Whether `plugin` keeps anything in `ws`.
    pub(crate) fn holds(&self, plugin: &str, ws: &str) -> bool {
        self.by_key
            .get(&(plugin.to_string(), ws.to_string()))
            .is_some_and(|m| !m.is_empty())
    }

    pub(crate) fn get(&self, plugin: &str, ws: &str, key: &str) -> Option<String> {
        self.by_key
            .get(&(plugin.to_string(), ws.to_string()))?
            .get(key)
            .cloned()
    }

    /// Store `value` (JSON text) under `key`; None removes it. Refused past
    /// `STATE_CAP` for the pair, leaving the old value in place.
    fn put(
        &mut self,
        plugin: &str,
        ws: &str,
        key: &str,
        value: Option<String>,
    ) -> Result<(), String> {
        let pair = (plugin.to_string(), ws.to_string());
        let map = self.by_key.entry(pair.clone()).or_default();
        let Some(value) = value else {
            map.remove(key);
            if map.is_empty() {
                self.by_key.remove(&pair);
            }
            return Ok(());
        };
        let others: usize = map
            .iter()
            .filter(|(k, _)| k.as_str() != key)
            .map(|(k, v)| k.len() + v.len())
            .sum();
        let total = others + key.len() + value.len();
        if total > STATE_CAP {
            return Err(format!(
                "state is capped at {} KiB per plugin per workspace ({total} bytes asked)",
                STATE_CAP >> 10
            ));
        }
        map.insert(key.to_string(), value);
        Ok(())
    }

    /// A deleted workspace: every plugin's state there.
    pub(crate) fn forget_workspace(&mut self, ws: &str) {
        self.by_key.retain(|(_, w), _| w != ws);
    }
}

impl HostState {
    fn scope(&self) -> Result<&CallScope, String> {
        self.call
            .as_ref()
            .ok_or_else(|| "no call is in flight".to_string())
    }

    /// The workspace root and a checked relative path under it.
    fn target(&self, path: &str) -> Result<(PathBuf, PathBuf), String> {
        let scope = self.scope()?;
        let root = crate::lock(&scope.app.workspaces)
            .get(&self.workspace)
            .map(|w| w.root)
            .ok_or_else(|| "this workspace is gone".to_string())?;
        Ok((root, relative(path)?))
    }
}

/// `path` as a relative path of plain components: `..` and absolute paths
/// are refused, `.` dropped.
fn relative(path: &str) -> Result<PathBuf, String> {
    let mut out = PathBuf::new();
    for part in Path::new(path).components() {
        match part {
            Component::Normal(name) => out.push(name),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!(
                    "{path}: `..` is refused — paths stay inside the workspace"
                ))
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "{path}: absolute paths are refused — paths are workspace-relative"
                ))
            }
        }
    }
    Ok(out)
}

fn shown(rel: &Path) -> String {
    if rel.as_os_str().is_empty() {
        ".".to_string()
    } else {
        rel.display().to_string()
    }
}

/// An open that failed: name the symlink when one is in the way (the
/// `O_NOFOLLOW` walk refuses it with a bare ELOOP/ENOTDIR).
fn refused(root: &Path, rel: &Path, err: std::io::Error) -> String {
    let mut at = root.to_path_buf();
    let mut walked = PathBuf::new();
    for part in rel.components() {
        at.push(part);
        walked.push(part);
        match std::fs::symlink_metadata(&at) {
            Ok(md) if md.is_symlink() => {
                return format!(
                    "{}: {} is a symlink — refused (no component of a path may be one)",
                    shown(rel),
                    walked.display()
                );
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    format!("{}: {err}", shown(rel))
}

/// Open `rel` beneath `root` with every component `O_NOFOLLOW`.
fn open_under(root: &Path, rel: &Path, dir: bool) -> Result<std::fs::File, String> {
    use rustix::fs::OFlags;
    let root_dir = std::fs::File::open(root).map_err(|e| format!("the workspace root: {e}"))?;
    let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    if dir {
        flags |= OFlags::DIRECTORY;
    }
    crate::download::open_beneath(&root_dir, rel, flags).map_err(|e| refused(root, rel, e))
}

fn read_capped(root: &Path, rel: &Path, cap: usize) -> Result<Vec<u8>, String> {
    let file = open_under(root, rel, false)?;
    let meta = file
        .metadata()
        .map_err(|e| format!("{}: {e}", shown(rel)))?;
    if !meta.is_file() {
        return Err(format!("{}: not a regular file", shown(rel)));
    }
    let mut bytes = Vec::with_capacity((meta.len() as usize).min(cap));
    file.take(cap as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{}: {e}", shown(rel)))?;
    Ok(bytes)
}

fn stat_under(root: &Path, rel: &Path) -> Result<wit::Stat, String> {
    let meta = open_under(root, rel, false)?
        .metadata()
        .map_err(|e| format!("{}: {e}", shown(rel)))?;
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Ok(wit::Stat {
        size: meta.len(),
        mtime_ms,
        is_dir: meta.is_dir(),
    })
}

fn list_under(root: &Path, rel: &Path, cap: usize) -> Result<Vec<wit::Entry>, String> {
    use rustix::fs::{AtFlags, FileType};
    let dir = open_under(root, rel, true)?;
    let entries = rustix::fs::Dir::read_from(&dir).map_err(|e| format!("{}: {e}", shown(rel)))?;
    let mut out = Vec::new();
    for entry in entries {
        if out.len() >= cap {
            break;
        }
        let entry = entry.map_err(|e| format!("{}: {e}", shown(rel)))?;
        let name = entry.file_name();
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        // Some filesystems (NFS, Lustre) don't fill in the type: ask,
        // without following a link.
        let kind = match entry.file_type() {
            FileType::Unknown => rustix::fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW)
                .map(|st| FileType::from_raw_mode(st.st_mode))
                .unwrap_or(FileType::Unknown),
            known => known,
        };
        out.push(wit::Entry {
            name: name.to_string_lossy().into_owned(),
            is_dir: kind == FileType::Directory,
            is_symlink: kind == FileType::Symlink,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Blocking filesystem work, off the reactor and behind the daemon's
/// filesystem limiter.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let permit = crate::fs::FILESYSTEM_WORK
        .acquire()
        .await
        .map_err(|_| "the filesystem limiter is closed".to_string())?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .map_err(|e| format!("filesystem work failed: {e}"))?
}

fn kind_name(kind: timeline::Kind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

impl wit::Host for HostState {}

impl host::Host for HostState {
    async fn read(&mut self, _cx: wit::Context, path: String, cap: u32) -> Result<Vec<u8>, String> {
        let (root, rel) = self.target(&path)?;
        let cap = (cap as usize).min(READ_CEILING);
        blocking(move || read_capped(&root, &rel, cap)).await
    }

    async fn stat(&mut self, _cx: wit::Context, path: String) -> Result<wit::Stat, String> {
        let (root, rel) = self.target(&path)?;
        blocking(move || stat_under(&root, &rel)).await
    }

    async fn list(
        &mut self,
        _cx: wit::Context,
        path: String,
        cap: u32,
    ) -> Result<Vec<wit::Entry>, String> {
        let (root, rel) = self.target(&path)?;
        let cap = (cap as usize).min(LIST_CEILING);
        blocking(move || list_under(&root, &rel, cap)).await
    }

    async fn state_get(&mut self, _cx: wit::Context, key: String) -> Option<String> {
        let scope = self.scope().ok()?;
        crate::lock(&scope.app.plugin_state).get(&self.plugin, &self.workspace, &key)
    }

    async fn state_put(
        &mut self,
        _cx: wit::Context,
        key: String,
        value: String,
    ) -> Result<(), String> {
        let scope = self.scope()?;
        let parsed: Value =
            serde_json::from_str(&value).map_err(|e| format!("state {key}: not JSON ({e})"))?;
        let value = (!parsed.is_null()).then_some(value);
        crate::lock(&scope.app.plugin_state).put(&self.plugin, &self.workspace, &key, value)
    }

    async fn sessions(&mut self, _cx: wit::Context) -> Vec<wit::Session> {
        let Ok(scope) = self.scope() else {
            return Vec::new();
        };
        let app = &scope.app;
        let mastermind = crate::lock(&app.workspaces)
            .get(&self.workspace)
            .and_then(|w| w.mastermind)
            .map(|m| m.session_id);
        let mut ids: Vec<String> = crate::lock(&app.session_workspaces)
            .iter()
            .filter(|(_, ws)| **ws == self.workspace)
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids.truncate(SESSIONS_MAX);
        ids.into_iter()
            .map(|id| {
                let kind = crate::lock(&app.agents)
                    .get(&id)
                    .map(|r| r.kind.as_str())
                    .unwrap_or("shell");
                let chat = app.chat.get(&id);
                let alive = chat.as_ref().is_some_and(|c| c.alive)
                    || app.sessions.get(&id).is_some_and(|s| s.alive);
                wit::Session {
                    name: crate::session_view::display_name_now(app, &id)
                        .unwrap_or_else(|| id.clone()),
                    kind: kind.to_string(),
                    chat: chat.is_some(),
                    alive,
                    mastermind: mastermind.as_deref() == Some(id.as_str()),
                    id,
                }
            })
            .collect()
    }

    async fn timeline_append(&mut self, _cx: wit::Context, entry: String) -> Result<u64, String> {
        let scope = self.scope()?;
        let app = scope.app.clone();
        let Some(sid) = scope.cx.session.clone() else {
            return Err("only a session's call may write the Timeline".into());
        };
        let entry: Value =
            serde_json::from_str(&entry).map_err(|e| format!("the entry is not JSON ({e})"))?;
        let Some(fields) = entry.as_object() else {
            return Err("an entry is a JSON object".into());
        };
        if fields.get("kind").and_then(Value::as_str) != Some("note") {
            return Err("a plugin may append `note` entries only".into());
        }
        if let Some(other) = fields
            .keys()
            .find(|k| !matches!(k.as_str(), "kind" | "to" | "text"))
        {
            return Err(format!("a note has kind, to and text — not {other}"));
        }
        let text = fields
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if text.is_empty() {
            return Err("missing required argument: text".into());
        }
        if text.len() > timeline::TEXT_MAX {
            return Err(format!(
                "a note is short — keep it under {} bytes",
                timeline::TEXT_MAX
            ));
        }
        let to = match fields.get("to") {
            None | Some(Value::Null) => None,
            Some(Value::String(to)) => match to.trim() {
                "" => None,
                "mastermind" => Some("mastermind".to_string()),
                target => {
                    // Notes never cross workspaces (the user's deliver
                    // click sends to `to`).
                    let here = crate::lock(&app.session_workspaces)
                        .get(target)
                        .is_some_and(|w| *w == self.workspace);
                    if !here {
                        return Err(format!(
                            "no session {target} in this workspace — use a session id from \
                             the workspace, \"mastermind\", or omit `to` for everyone"
                        ));
                    }
                    Some(target.to_string())
                }
            },
            Some(_) => return Err("`to` is a session id, \"mastermind\", or null".into()),
        };
        crate::notes::take_post_slot(&app, &sid)?;
        let posted = crate::notes::append_note(&app, &self.workspace, &sid, to, text, false).await;
        Ok(posted.seq)
    }

    async fn timeline_recent(
        &mut self,
        _cx: wit::Context,
        kinds: Vec<String>,
        limit: u32,
    ) -> Vec<String> {
        let Ok(scope) = self.scope() else {
            return Vec::new();
        };
        let app = scope.app.clone();
        let kinds: Vec<String> = kinds.into_iter().take(KINDS_MAX).collect();
        let limit = (limit as usize).min(timeline::PAGE_MAX);
        app.timeline
            .latest(&self.workspace, timeline::PAGE_MAX)
            .await
            .iter()
            .filter(|e| kinds.contains(&kind_name(e.kind)))
            .take(limit)
            .filter_map(|e| serde_json::to_string(&**e).ok())
            .collect()
    }

    async fn emit(&mut self, _cx: wit::Context, event: String) {
        let Ok(scope) = self.scope() else {
            return;
        };
        let app = scope.app.clone();
        let fields = match serde_json::from_str::<Value>(&event) {
            Ok(Value::Object(fields)) if event.len() <= EVENT_MAX => fields,
            _ => {
                tracing::warn!(
                    plugin = %self.plugin,
                    "plugin event refused: one JSON object of at most 16 KiB"
                );
                return;
            }
        };
        let mut frame = Value::Object(fields);
        // The host's keys last, so an event can't pose as another frame.
        frame["type"] = json!("plugin");
        frame["plugin"] = json!(self.plugin);
        frame["workspace"] = json!(self.workspace);
        app.plugin_runtime.push_event(frame.to_string());
        app.changes.notify_waiters();
    }

    async fn now_ms(&mut self) -> u64 {
        timeline::now_ms()
    }

    async fn log(&mut self, level: wit::Level, message: String) {
        let plugin = self.plugin.clone();
        let Some(scope) = self.call.as_mut() else {
            return;
        };
        scope.logs += 1;
        if scope.logs > LOGS_PER_CALL {
            return;
        }
        let message = timeline::cap(&message, LOG_LINE_MAX);
        match level {
            wit::Level::Debug => tracing::debug!(%plugin, "{message}"),
            wit::Level::Info => tracing::info!(%plugin, "{message}"),
            wit::Level::Warn => tracing::warn!(%plugin, "{message}"),
            wit::Level::Error => tracing::error!(%plugin, "{message}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_relative() {
        assert_eq!(relative("a/./b").unwrap(), PathBuf::from("a/b"));
        assert_eq!(relative("").unwrap(), PathBuf::new());
        assert!(relative("../x").unwrap_err().contains("`..` is refused"));
        assert!(relative("a/../../x").is_err());
        assert!(relative("/etc/passwd").unwrap_err().contains("absolute"));
    }

    #[test]
    fn state_is_capped_per_plugin_and_workspace() {
        let mut st = PluginStates::default();
        let half = "x".repeat(STATE_CAP / 2);
        st.put("p", "w", "a", Some(half.clone())).unwrap();
        assert!(st.put("p", "w", "b", Some(half.clone())).is_err());
        st.put("p", "other", "b", Some(half.clone())).unwrap();
        st.put("p", "w", "a", Some("1".into())).unwrap();
        st.put("p", "w", "b", Some(half)).unwrap();
        assert!(st.holds("p", "w"));
        st.put("p", "w", "a", None).unwrap();
        st.put("p", "w", "b", None).unwrap();
        assert!(!st.holds("p", "w"));
        st.forget_workspace("other");
        assert!(!st.holds("p", "other"));
    }
}
