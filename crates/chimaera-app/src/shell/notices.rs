//! Notices → OS notifications for every daemon this app has open (the local
//! one, each live tunnel, each compute-job tunnel). The shell — not the
//! windows — consumes the daemons' notice feeds (`GET /api/v1/notices`, a
//! long-poll per daemon), so a notification is posted exactly once however
//! many windows show that daemon — and covers every workspace on it, not
//! only the ones with a window open.
//!
//! Decisions made here, with the window facts only the shell has:
//! - **Is the user already looking?** A notice about a session visible in
//!   the focused window is dropped (windows report what they show via
//!   `report_window_view`). Everything else is posted — while Chimaera is
//!   frontmost too (the `whileFocused` setting can mute that), because a
//!   finish in another tab or window is exactly what a banner is for.
//! - **One alert per session.** A newer state supersedes a session's older
//!   alert in Notification Center (agent-sent messages are kept), a resolved
//!   blocker takes its alert back, and viewing a session clears its alerts.
//! - **Where a click goes.** The window already showing the session, else a
//!   window on its workspace, else a new window for that workspace — then
//!   the page focuses the session's tab (`focus-session` event).
//! - **The Dock badge** counts sessions waiting on the user across every
//!   daemon; a blocking notice bounces the Dock once while Chimaera is in the
//!   background.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager};

use super::{lock, Shell};
use crate::notify::{self, Route, Toast};

/// A daemon's identity, as window scopes name it: `None` = the local daemon,
/// `Some(alias)` = a tunnel (or a compute job's composite alias).
type Key = Option<String>;

/// How long a long-poll may park inside the daemon.
const POLL_WAIT_SECS: u64 = 25;
/// The request's own ceiling: the daemon's hold plus slack for a slow tunnel.
const POLL_TIMEOUT: Duration = Duration::from_secs(POLL_WAIT_SECS + 15);
/// How often the supervisor looks for daemons that need a watcher.
const SUPERVISE_EVERY: Duration = Duration::from_secs(2);
/// A notice older than this (a laptop waking after a long sleep) is state
/// the in-app marks and the badge already carry — not news.
const MAX_NOTICE_AGE_MS: u64 = 5 * 60 * 1000;
/// Alerts kept per session (older ones are taken back).
const PER_SESSION_ALERTS: usize = 4;
/// A click's workspace window has this long to report in before the pending
/// focus is forgotten.
const PENDING_FOCUS_TTL: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(crate) struct NoticeHub {
    inner: Mutex<Hub>,
}

#[derive(Default)]
struct Hub {
    /// Daemons with a live watcher task.
    watching: HashSet<Key>,
    /// Each watched daemon's latest attention set + presentation prefs.
    daemons: HashMap<Key, DaemonView>,
    /// Alerts currently in Notification Center, per (daemon, session).
    delivered: HashMap<(Key, String), Vec<Delivered>>,
    /// Clicks waiting for their newly opened window to report its scope.
    pending_focus: Vec<PendingFocus>,
    /// A reported window's owed focus, until its page takes it (the emit at
    /// scope-report time can beat the page's listener — see
    /// [`take_pending_focus`]).
    owed_focus: HashMap<String, String>,
}

struct DaemonView {
    attention: Vec<AttnRow>,
    prefs: Prefs,
}

struct Delivered {
    id: String,
    blocking: bool,
    agent: bool,
}

struct PendingFocus {
    alias: Key,
    ws: String,
    session: String,
    at: Instant,
}

#[derive(Deserialize, Clone)]
struct AttnRow {
    id: String,
    #[serde(default)]
    workspace_id: Option<String>,
}

#[derive(Deserialize, Clone, Copy)]
struct Prefs {
    #[serde(default = "yes")]
    sound: bool,
    #[serde(default = "yes")]
    while_focused: bool,
    #[serde(default = "yes")]
    dock_badge: bool,
}

fn yes() -> bool {
    true
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            sound: true,
            while_focused: true,
            dock_badge: true,
        }
    }
}

/// `GET /api/v1/notices` response (see `chimaera-server/src/notices.rs`).
#[derive(Deserialize)]
struct Poll {
    boot: String,
    head: u64,
    #[serde(default)]
    notices: Vec<WireNotice>,
    attention: Attention,
    #[serde(default)]
    prefs: Prefs,
}

#[derive(Deserialize)]
struct Attention {
    hash: u64,
    #[serde(default)]
    sessions: Vec<AttnRow>,
}

#[derive(Deserialize)]
struct WireNotice {
    id: u64,
    kind: String,
    #[serde(default)]
    blocking: bool,
    session_id: String,
    #[serde(default)]
    workspace_id: Option<String>,
    title: String,
    #[serde(default)]
    subtitle: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    age_ms: u64,
}

fn hub(app: &AppHandle) -> Option<tauri::State<'_, NoticeHub>> {
    app.try_state::<NoticeHub>()
}

/// Start the platform notifier and the watcher supervisor. Called once, when
/// startup first manages `Shell`.
pub(crate) fn start(app: &AppHandle) {
    app.manage(NoticeHub::default());
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            for key in live_daemons(&app).await {
                let Some(hub) = hub(&app) else {
                    return;
                };
                if lock(&hub.inner).watching.insert(key.clone()) {
                    tauri::async_runtime::spawn(watch(app.clone(), key));
                }
            }
            tokio::time::sleep(SUPERVISE_EVERY).await;
        }
    });
}

/// Every daemon the app currently has a route to.
async fn live_daemons(app: &AppHandle) -> Vec<Key> {
    let Some(shell) = app.try_state::<Shell>() else {
        return Vec::new();
    };
    let mut keys = vec![None];
    keys.extend(shell.tunnels.lock().await.keys().cloned().map(Some));
    keys.extend(shell.compute_tunnels.lock().await.keys().cloned().map(Some));
    keys
}

/// The daemon's current loopback port + token (both move on reconnect, so
/// they are re-read before every poll), or `None` once it is gone.
async fn endpoint(app: &AppHandle, key: &Key) -> Option<(u16, String)> {
    let shell = app.try_state::<Shell>()?;
    match key {
        None => {
            let local = lock(&shell.local);
            Some((local.port, local.token.clone()))
        }
        Some(alias) => {
            if let Some(t) = shell.tunnels.lock().await.get(alias) {
                return Some((t.local_port, t.manifest.token.clone()));
            }
            shell
                .compute_tunnels
                .lock()
                .await
                .get(alias)
                .map(|t| (t.local_port, t.token.clone()))
        }
    }
}

/// One daemon's watcher: long-poll until the daemon goes away. The cursor
/// (`boot` + `after`) makes a restarted daemon start fresh instead of
/// replaying, and survives the tunnel reconnects that move its port.
async fn watch(app: AppHandle, key: Key) {
    let mut boot = String::new();
    let mut after = 0u64;
    let mut attn: Option<u64> = None;
    let mut backoff = Duration::from_secs(1);
    while let Some((port, token)) = endpoint(&app, &key).await {
        let mut url = format!(
            "http://127.0.0.1:{port}/api/v1/notices?after={after}&boot={}&wait={POLL_WAIT_SECS}",
            urlencoding::encode(&boot)
        );
        if let Some(hash) = attn {
            url.push_str(&format!("&attn={hash}"));
        }
        let got = tauri::async_runtime::spawn_blocking(move || -> Result<Poll, String> {
            crate::http::agent()
                .get(&url)
                .header("Authorization", &format!("Bearer {token}"))
                .config()
                .timeout_global(Some(POLL_TIMEOUT))
                .build()
                .call()
                .map_err(|e| e.to_string())?
                .body_mut()
                .read_json::<Poll>()
                .map_err(|e| e.to_string())
        })
        .await;
        match got {
            Ok(Ok(poll)) => {
                backoff = Duration::from_secs(1);
                // A new boot's notices are all news to us, but the daemon
                // only ever starts a fresh cursor at its head.
                boot.clone_from(&poll.boot);
                after = poll.head;
                attn = Some(poll.attention.hash);
                apply(&app, &key, poll);
            }
            // An old daemon without the route answers 404: nothing to watch,
            // but it may be updated in place later — poll rarely.
            Ok(Err(e)) if e.contains("404") => {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
            Ok(Err(_)) | Err(_) => {
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
    // The daemon is gone (disconnected or its tunnel dropped): forget its
    // attention so the badge stops counting it, and let a reconnect start a
    // fresh watcher.
    if let Some(hub) = hub(&app) {
        let mut h = lock(&hub.inner);
        h.watching.remove(&key);
        h.daemons.remove(&key);
    }
    refresh_attention(&app);
}

/// Fold one poll: attention + prefs, take back resolved blockers, post new
/// alerts, bounce, badge.
fn apply(app: &AppHandle, key: &Key, poll: Poll) {
    let Some(hub) = hub(app) else {
        return;
    };
    let focused = focused_view(app);
    let app_active = focused.is_some();
    let prefs = poll.prefs;
    let mut toasts = Vec::new();
    let mut removals = Vec::new();
    let mut bounce = false;
    let attention_changed;
    {
        let mut h = lock(&hub.inner);
        let waiting: HashSet<&str> = poll
            .attention
            .sessions
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        // A blocker that no longer blocks (answered in the app, on another
        // device, or it timed out) takes its alert back.
        for ((k, session), list) in h.delivered.iter_mut() {
            if k == key && !waiting.contains(session.as_str()) {
                list.retain(|d| {
                    if d.blocking {
                        removals.push(d.id.clone());
                    }
                    !d.blocking
                });
            }
        }
        for n in poll.notices {
            if n.age_ms > MAX_NOTICE_AGE_MS {
                continue;
            }
            let looking = focused
                .as_ref()
                .is_some_and(|(alias, visible)| alias == key && visible.contains(&n.session_id));
            if looking || (app_active && !prefs.while_focused) {
                tracing::debug!(daemon = ?key, kind = %n.kind, looking, "notice not posted");
                continue;
            }
            tracing::info!(daemon = ?key, kind = %n.kind, session = %n.session_id, "posting notification");
            let route = Route {
                alias: key.clone(),
                ws: n.workspace_id.clone(),
                session: n.session_id.clone(),
            };
            let id = notify::toast_id(&route, &poll.boot, n.id);
            let agent = n.kind == "agent";
            let alerts = h
                .delivered
                .entry((key.clone(), n.session_id.clone()))
                .or_default();
            // The newest state is what matters: a session's older status
            // alerts give way (an agent's own messages are kept).
            if !agent {
                alerts.retain(|d| {
                    if !d.agent {
                        removals.push(d.id.clone());
                    }
                    d.agent
                });
            }
            alerts.push(Delivered {
                id: id.clone(),
                blocking: n.blocking,
                agent,
            });
            while alerts.len() > PER_SESSION_ALERTS {
                removals.push(alerts.remove(0).id);
            }
            let subtitle = match key {
                Some(alias) => {
                    let host = alias.split('#').next().unwrap_or(alias);
                    if n.subtitle.is_empty() {
                        host.to_string()
                    } else {
                        format!("{} · {host}", n.subtitle)
                    }
                }
                None => n.subtitle,
            };
            toasts.push(Toast {
                id,
                thread: format!(
                    "{}/{}",
                    key.as_deref().unwrap_or("local"),
                    n.workspace_id.as_deref().unwrap_or("")
                ),
                title: n.title,
                subtitle,
                body: n.body,
                sound: prefs.sound,
            });
            bounce |= n.blocking && prefs.dock_badge && !app_active;
        }
        h.delivered.retain(|_, list| !list.is_empty());
        let ids = |rows: &[AttnRow]| rows.iter().map(|r| r.id.clone()).collect::<Vec<_>>();
        attention_changed = h.daemons.get(key).is_none_or(|d| {
            ids(&d.attention) != ids(&poll.attention.sessions)
                || d.prefs.dock_badge != prefs.dock_badge
        });
        h.daemons.insert(
            key.clone(),
            DaemonView {
                attention: poll.attention.sessions,
                prefs,
            },
        );
    }
    notify::remove(removals);
    for toast in toasts {
        notify::post(toast);
    }
    if bounce {
        notify::bounce(app);
    }
    // A timed-out poll (nothing new) leaves the badge and tray as they are.
    if attention_changed {
        refresh_attention(app);
    }
}

/// The focused window's (daemon, visible sessions), or `None` while no
/// Chimaera window has focus (the app is in the background).
fn focused_view(app: &AppHandle) -> Option<(Key, Vec<String>)> {
    let shell = app.try_state::<Shell>()?;
    let focused = app
        .webview_windows()
        .into_values()
        .find(|w| w.is_focused().unwrap_or(false))?;
    let windows = lock(&shell.windows);
    let scope = windows.get(focused.label())?;
    Some((scope.alias.clone(), scope.visible.clone()))
}

/// Re-derive the Dock badge and the tray's per-window counts.
fn refresh_attention(app: &AppHandle) {
    notify::set_badge(app, attention_total(app));
    crate::tray::rebuild(app);
}

/// Sessions waiting on the user across every daemon whose badge is on.
pub(crate) fn attention_total(app: &AppHandle) -> usize {
    let Some(hub) = hub(app) else {
        return 0;
    };
    let h = lock(&hub.inner);
    h.daemons
        .values()
        .filter(|d| d.prefs.dock_badge)
        .map(|d| d.attention.len())
        .sum()
}

/// Sessions waiting on the user in one window's workspace (the tray's
/// per-window count). Detached and Home windows count none — the workspace
/// window carries its workspace's number.
pub(crate) fn window_attention(app: &AppHandle, label: &str) -> usize {
    let (Some(shell), Some(hub)) = (app.try_state::<Shell>(), hub(app)) else {
        return 0;
    };
    let Some(scope) = lock(&shell.windows).get(label).cloned() else {
        return 0;
    };
    if scope.detached || scope.ws.is_none() {
        return 0;
    }
    let h = lock(&hub.inner);
    h.daemons.get(&scope.alias).map_or(0, |d| {
        d.attention
            .iter()
            .filter(|r| r.workspace_id == scope.ws)
            .count()
    })
}

/// The user is looking at these sessions (a focused window shows them):
/// their alerts have done their job.
pub(crate) fn mark_seen(app: &AppHandle, key: &Key, sessions: &[String]) {
    let Some(hub) = hub(app) else {
        return;
    };
    let mut removals = Vec::new();
    {
        let mut h = lock(&hub.inner);
        for session in sessions {
            if let Some(list) = h.delivered.remove(&(key.clone(), session.clone())) {
                removals.extend(list.into_iter().map(|d| d.id));
            }
        }
    }
    notify::remove(removals);
}

/// A window gained focus: whatever it shows is now seen.
pub(crate) fn window_focused(app: &AppHandle, label: &str) {
    let Some(shell) = app.try_state::<Shell>() else {
        return;
    };
    let Some((alias, visible)) = lock(&shell.windows)
        .get(label)
        .map(|s| (s.alias.clone(), s.visible.clone()))
    else {
        return;
    };
    mark_seen(app, &alias, &visible);
}

/// A window just reported its scope: if a notification click opened it for
/// a session, focus that session now that the page is listening.
pub(crate) fn window_scoped(app: &AppHandle, label: &str, alias: &Key, ws: &Option<String>) {
    let (Some(hub), Some(ws)) = (hub(app), ws.as_ref()) else {
        return;
    };
    let session = {
        let mut h = lock(&hub.inner);
        h.pending_focus
            .retain(|p| p.at.elapsed() < PENDING_FOCUS_TTL);
        let found = h
            .pending_focus
            .iter()
            .position(|p| &p.alias == alias && &p.ws == ws);
        found.map(|i| h.pending_focus.remove(i).session)
    };
    if let Some(session) = session {
        lock(&hub.inner)
            .owed_focus
            .insert(label.to_string(), session.clone());
        let _ = app.emit_to(label, "focus-session", session);
    }
}

/// The session a notification click opened this window for, if the page has
/// not focused it yet. The page asks once its `focus-session` listener is
/// live, so the owed focus lands whichever of the two arrives first.
pub(crate) fn take_pending_focus(app: &AppHandle, label: &str) -> Option<String> {
    lock(&hub(app)?.inner).owed_focus.remove(label)
}

/// A window closed: nothing is owed to it any more.
pub(crate) fn window_gone(app: &AppHandle, label: &str) {
    if let Some(hub) = hub(app) {
        lock(&hub.inner).owed_focus.remove(label);
    }
}

/// A notification was clicked: bring the right window forward and focus the
/// session in it.
pub(crate) fn route_click(app: &AppHandle, route: Route) {
    tracing::info!(daemon = ?route.alias, session = %route.session, "notification clicked");
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(shell) = app.try_state::<Shell>() else {
            return;
        };
        let label = {
            // One lock at a time: every other site takes focus_order alone.
            let order = lock(&shell.focus_order).clone();
            let windows = lock(&shell.windows);
            let recency = |label: &str| order.iter().position(|l| l == label).unwrap_or(usize::MAX);
            let mut candidates: Vec<(u8, usize, String)> = windows
                .iter()
                .filter(|(_, scope)| scope.alias == route.alias)
                .filter_map(|(label, scope)| {
                    let tier = if scope.visible.contains(&route.session) {
                        0
                    } else if !scope.detached && route.ws.is_some() && scope.ws == route.ws {
                        1
                    } else {
                        return None;
                    };
                    Some((tier, recency(label), label.clone()))
                })
                .collect();
            candidates.sort();
            candidates.into_iter().next().map(|(_, _, label)| label)
        };
        if let Some(label) = label {
            if let Some(window) = app.get_webview_window(&label) {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
                let _ = app.emit_to(label.as_str(), "focus-session", route.session);
                return;
            }
        }
        // No window shows that workspace: open one, and hand it the session
        // once it reports in. A compute job's window can only be reopened
        // through its job flow, and a click without a workspace has nowhere
        // specific to go — both just bring the app forward.
        let is_tunnel = match &route.alias {
            None => true,
            Some(alias) => shell.tunnels.lock().await.contains_key(alias),
        };
        let (Some(ws), true) = (route.ws.clone(), is_tunnel) else {
            super::activate_app(&app, false);
            return;
        };
        let Some((port, token)) = endpoint(&app, &route.alias).await else {
            super::activate_app(&app, false);
            return;
        };
        if let Some(hub) = hub(&app) {
            lock(&hub.inner).pending_focus.push(PendingFocus {
                alias: route.alias.clone(),
                ws: ws.clone(),
                session: route.session,
                at: Instant::now(),
            });
        }
        let record = crate::windows::WindowRecord::new(route.alias, Some(ws));
        if let Err(e) = super::open_ui_window(&app, port, &token, &record) {
            tracing::warn!("could not open a window for a notification: {e}");
        }
    });
}
