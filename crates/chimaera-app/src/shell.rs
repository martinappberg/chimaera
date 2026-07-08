//! Shell state and the IPC command surface the daemon-served UI calls
//! (`web-ui/src/lib/native.ts` is the other half of this contract — change
//! command and event names in lockstep).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use chimaera_remote::hosts::HostsStore;
use chimaera_remote::{ConnectOpts, Phase, Tunnel};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

use crate::daemon::LocalDaemon;
use crate::windows::{WindowRecord, WindowRegistry};

/// A connect attempt's shared outcome: `None` while in flight, then the
/// result. Joiners wait on this instead of racing their own ssh — see
/// `do_connect`.
type ConnectFlight = tokio::sync::watch::Receiver<Option<Result<(), String>>>;

/// App-global shell state.
pub struct Shell {
    /// The local daemon (mutable: the update affordance replaces it).
    pub local: Mutex<LocalDaemon>,
    /// Live tunnels by host alias.
    tunnels: tokio::sync::Mutex<HashMap<String, Tunnel>>,
    /// In-flight connect per alias. One flight owns the ssh; every other
    /// caller (other windows' reconnects, the home screen, startup restore)
    /// awaits its outcome — so a drop never fans out into an auth-prompt
    /// stampede, and a click during a stuck attempt joins it instead of
    /// bouncing with an error.
    connecting: Mutex<HashMap<String, ConnectFlight>>,
    /// What each open window currently shows, keyed by window label. Drives
    /// focus-existing (open the same workspace → raise its window instead of a
    /// duplicate). The SPA reports its own scope because it swaps `ws`
    /// client-side without a shell round-trip.
    windows: Mutex<HashMap<String, WindowScope>>,
    /// The persisted window set (windows.json) — what the next launch reopens.
    registry: Mutex<WindowRegistry>,
    /// Set on ExitRequested: window teardown during quit must NOT remove
    /// records from the registry, or quitting would forget every window.
    quitting: AtomicBool,
}

/// The (host, workspace) a window is showing. `alias` None = the local
/// daemon; `ws` None = the home screen. `stable_id` is the window's
/// registry/view-state identity — unlike the volatile Tauri label, it
/// survives app restarts.
#[derive(Clone, Default, PartialEq)]
pub struct WindowScope {
    pub alias: Option<String>,
    pub ws: Option<String>,
    pub stable_id: String,
}

/// Host list entry as the UI sees it (see HostState in native.ts).
#[derive(Clone, Serialize)]
pub struct HostState {
    alias: String,
    status: &'static str,
    local_port: Option<u16>,
    last_connected_at: Option<u64>,
    error: Option<String>,
    /// The connected daemon is an older build; live sessions kept connect
    /// from replacing it (the row offers the explicit update).
    outdated: bool,
    remote_build: Option<String>,
    live_sessions: Option<usize>,
}

#[derive(Clone, Serialize)]
struct ConnectProgress {
    alias: String,
    phase: &'static str,
}

/// Live tunnel liveness, pushed as hosts drop or reconnect (see the health
/// monitor and `host-status` in native.ts). `token` rides the `connected`
/// transition so a window whose remote daemon restarted (fresh token) can
/// re-home; it is omitted on `down`. `error` rides the `error` transition (a
/// connect flight failed) so surfaces that only observe events — a home
/// screen watching a startup-restore connect — don't stay "connecting"
/// forever on a failure they never hear about.
#[derive(Clone, Serialize)]
struct HostStatus {
    alias: String,
    status: &'static str,
    local_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// The local daemon's build parity, as the home screen sees it.
#[derive(Clone, Serialize)]
pub struct LocalState {
    outdated: bool,
    build: Option<String>,
    live_sessions: Option<usize>,
}

/// Payload of the `local-daemon-updated` broadcast: every window on the
/// local daemon re-homes itself to the new port + token.
#[derive(Clone, Serialize)]
struct LocalDaemonMoved {
    port: u16,
    token: String,
}

static WINDOW_SEQ: AtomicU64 = AtomicU64::new(0);

/// Open a UI window on a daemon: local (`record.alias` None) or a connected
/// remote's tunnel. `record.ws` scopes the window to a workspace; None lands
/// on the home screen. The record's stable id rides the hash as `win=` — the
/// SPA keys its daemon-side view state on it, so reopening this record IS
/// reopening this window — and its geometry (when present: a restore)
/// replaces the default placement.
pub fn open_ui_window(
    app: &AppHandle,
    port: u16,
    token: &str,
    record: &WindowRecord,
) -> tauri::Result<()> {
    let mut hash = format!("token={}", urlencoding::encode(token));
    hash.push_str(&format!("&win={}", urlencoding::encode(&record.id)));
    if let Some(ws) = &record.ws {
        hash.push_str(&format!("&ws={}", urlencoding::encode(ws)));
    }
    if let Some(alias) = &record.alias {
        hash.push_str(&format!("&host={}", urlencoding::encode(alias)));
    }
    let url = format!("http://127.0.0.1:{port}/#{hash}")
        .parse()
        .expect("daemon url is always valid");
    let label = format!("win-{}", WINDOW_SEQ.fetch_add(1, Ordering::Relaxed));
    let title = match &record.alias {
        Some(alias) => format!("{alias} — chimaera"),
        None => "chimaera".to_string(),
    };
    let mut builder = WebviewWindowBuilder::new(app, label.clone(), WebviewUrl::External(url))
        .title(title)
        .inner_size(1280.0, 840.0)
        .min_inner_size(680.0, 440.0);
    if let (Some(w), Some(h)) = (record.width, record.height) {
        builder = builder.inner_size(w, h);
    }
    if let (Some(x), Some(y)) = (record.x, record.y) {
        builder = builder.position(x, y);
    }
    builder.build()?;
    // Track the new window's scope so focus-existing can raise it, and
    // persist it so the next launch reopens it. Startup manages Shell before
    // opening any window, so every window registers.
    if let Some(shell) = app.try_state::<Shell>() {
        lock(&shell.windows).insert(
            label,
            WindowScope {
                alias: record.alias.clone(),
                ws: record.ws.clone(),
                stable_id: record.id.clone(),
            },
        );
        lock(&shell.registry).upsert(record.clone());
    }
    Ok(())
}

/// The label of an open window showing `(alias, ws)`, if any, excluding
/// `exclude` — used to raise an already-open workspace instead of duplicating.
fn find_by_scope(
    windows: &Mutex<HashMap<String, WindowScope>>,
    alias: &Option<String>,
    ws: &Option<String>,
    exclude: Option<&str>,
) -> Option<String> {
    lock(windows)
        .iter()
        .find(|(label, scope)| {
            exclude != Some(label.as_str()) && &scope.alias == alias && &scope.ws == ws
        })
        .map(|(label, _)| label.clone())
}

fn state_for(
    entry: &chimaera_remote::hosts::HostEntry,
    status: &'static str,
    tunnel: Option<&Tunnel>,
) -> HostState {
    HostState {
        alias: entry.alias.clone(),
        status,
        local_port: tunnel.map(|t| t.local_port),
        last_connected_at: entry.last_connected_at,
        error: None,
        outdated: tunnel.is_some_and(|t| t.outdated),
        remote_build: tunnel.and_then(|t| t.remote_build.clone()),
        live_sessions: tunnel.and_then(|t| t.live_sessions),
    }
}

#[tauri::command]
async fn list_hosts(state: State<'_, Shell>) -> Result<Vec<HostState>, String> {
    tracing::debug!("ipc: list_hosts");
    let store = HostsStore::load_default();
    let tunnels = state.tunnels.lock().await;
    let connecting: HashSet<String> = lock(&state.connecting).keys().cloned().collect();
    Ok(store
        .list()
        .iter()
        .map(|h| {
            if let Some(t) = tunnels.get(&h.alias) {
                state_for(h, "connected", Some(t))
            } else if connecting.contains(&h.alias) {
                state_for(h, "connecting", None)
            } else {
                state_for(h, "disconnected", None)
            }
        })
        .collect())
}

#[tauri::command]
async fn add_host(alias: String) -> Result<HostState, String> {
    let alias = alias.trim().to_string();
    if alias.is_empty() || alias.starts_with('-') {
        return Err("that does not look like an ssh alias".to_string());
    }
    let mut store = HostsStore::load_default();
    let entry = store.add(&alias, None).map_err(|e| format!("{e:#}"))?;
    Ok(state_for(&entry, "disconnected", None))
}

#[tauri::command]
async fn remove_host(state: State<'_, Shell>, alias: String) -> Result<(), String> {
    if let Some(tunnel) = state.tunnels.lock().await.remove(&alias) {
        tunnel.close().await;
    }
    HostsStore::load_default()
        .remove(&alias)
        .map_err(|e| format!("{e:#}"))?;
    Ok(())
}

/// One `chimaera_remote::connect` attempt, wiring each phase to a
/// `connect-progress` event. Factored out so a reconnect can retry on a fresh
/// port after a reused one clashes.
async fn run_connect(
    app: &AppHandle,
    alias: &str,
    entry: &chimaera_remote::hosts::HostEntry,
    local_port: Option<u16>,
    update_daemon: bool,
) -> anyhow::Result<Tunnel> {
    let opts = ConnectOpts {
        local_port,
        binary: entry.binary.clone(),
        update_daemon,
    };
    let progress_app = app.clone();
    let progress_alias = alias.to_string();
    chimaera_remote::connect(alias, opts, move |phase| {
        let phase = match phase {
            Phase::Probing => "probing",
            Phase::Updating => "updating",
            Phase::Downloading { .. } => "downloading",
            Phase::Installing { .. } => "installing",
            Phase::Starting => "starting",
            Phase::Tunneling { .. } => "tunneling",
        };
        let _ = progress_app.emit(
            "connect-progress",
            ConnectProgress {
                alias: progress_alias.clone(),
                phase,
            },
        );
    })
    .await
}

#[tauri::command]
async fn connect_host(
    app: AppHandle,
    alias: String,
    update_daemon: Option<bool>,
) -> Result<HostState, String> {
    do_connect(&app, alias, update_daemon.unwrap_or(false)).await
}

/// The connect flow behind the `connect_host` command, callable from
/// startup window restore too (no `State` extractor). Coalescing: only one
/// attempt per alias runs at a time; every concurrent caller awaits that
/// flight's outcome, so N windows reconnecting share ONE ssh auth flow (one
/// 2FA prompt) instead of stampeding or bouncing with errors.
async fn do_connect(
    app: &AppHandle,
    alias: String,
    update_daemon: bool,
) -> Result<HostState, String> {
    let state = app.state::<Shell>();
    tracing::info!("ipc: connect_host {alias} (update_daemon: {update_daemon})");
    loop {
        // Reuse a live tunnel. `is_up` is an end-to-end HTTP probe: after
        // laptop sleep ssh's local listener often survives its dead
        // connection, and a bare TCP check here would return "connected"
        // without healing anything. An update never reuses — the old tunnel
        // points at the daemon being replaced.
        if !update_daemon {
            let tunnels = state.tunnels.lock().await;
            if let Some(t) = tunnels.get(&alias) {
                if t.is_up().await {
                    let entry = host_entry(&alias);
                    return Ok(state_for(&entry, "connected", Some(t)));
                }
            }
        }
        let flight = {
            let mut connecting = lock(&state.connecting);
            match connecting.get(&alias) {
                Some(rx) => Err(rx.clone()),
                None => {
                    let (tx, rx) = tokio::sync::watch::channel(None);
                    connecting.insert(alias.clone(), rx);
                    Ok(tx)
                }
            }
        };
        let tx = match flight {
            // Someone else owns the attempt: await its outcome. The clone
            // frees the watch borrow before we touch `rx` again below.
            Err(mut rx) => {
                let outcome = rx.wait_for(|v| v.is_some()).await.map(|v| v.clone());
                match outcome {
                    Ok(outcome) => match outcome.expect("wait_for guarantees Some") {
                        Ok(()) if !update_daemon => {
                            let tunnels = state.tunnels.lock().await;
                            match tunnels.get(&alias) {
                                Some(t) => {
                                    let entry = host_entry(&alias);
                                    return Ok(state_for(&entry, "connected", Some(t)));
                                }
                                // Disconnected between the flight landing and
                                // us looking — treat like any failed connect.
                                None => {
                                    return Err(format!("{alias} disconnected while connecting"))
                                }
                            }
                        }
                        // An update must still run its own flight — loop and
                        // own the next one.
                        Ok(()) => continue,
                        Err(e) => return Err(e),
                    },
                    // The owner died without reporting (task dropped). Clear
                    // the stale flight so the next iteration can own a fresh
                    // one instead of spinning on a closed channel.
                    Err(_) => {
                        let mut connecting = lock(&state.connecting);
                        if connecting.get(&alias).is_some_and(|r| r.same_channel(&rx)) {
                            connecting.remove(&alias);
                        }
                        continue;
                    }
                }
            }
            Ok(tx) => tx,
        };

        // We own the flight: run it, then publish the outcome — every path
        // out of `run_flight` must land here or joiners would hang.
        let result = run_flight(app, &alias, update_daemon).await;
        lock(&state.connecting).remove(&alias);
        let _ = tx.send(Some(match &result {
            Ok(_) => Ok(()),
            Err(e) => Err(e.clone()),
        }));
        // Surfaces that only watch events (a home screen observing a
        // startup-restore connect) need the failure too, or their row sits
        // in "connecting" forever. Windows ignore this status: it carries no
        // port/token to re-home to, and only "down" arms their reconnect.
        if let Err(e) = &result {
            let _ = app.emit(
                "host-status",
                HostStatus {
                    alias: alias.clone(),
                    status: "error",
                    local_port: None,
                    token: None,
                    error: Some(e.clone()),
                },
            );
        }
        return result;
    }
}

/// One owned connect attempt: tear down the dead tunnel (keeping its
/// loopback port so open windows heal in place), run the connect, install
/// the new tunnel, and reopen this host's persisted windows.
async fn run_flight(
    app: &AppHandle,
    alias: &str,
    update_daemon: bool,
) -> Result<HostState, String> {
    let state = app.state::<Shell>();
    let reuse_port = {
        let mut tunnels = state.tunnels.lock().await;
        match tunnels.remove(alias) {
            Some(old) => {
                let port = old.local_port;
                old.close().await;
                (!update_daemon).then_some(port)
            }
            None => None,
        }
    };
    let entry = HostsStore::load_default()
        .add(alias, None)
        .map_err(|e| format!("{e:#}"))?;
    let result = run_connect(app, alias, &entry, reuse_port, update_daemon).await;
    // The reused port was free a moment ago (we just cancelled the forward),
    // but socket teardown can lag; fall back to an OS-assigned port so a
    // reconnect never fails on a transient bind clash. Only forward-phase
    // failures retry — re-running the whole connect after an auth failure
    // or cancel would raise a second 2FA prompt.
    let result = match result {
        Err(e)
            if reuse_port.is_some()
                && e.downcast_ref::<chimaera_remote::TunnelPhaseError>()
                    .is_some() =>
        {
            tracing::warn!("reconnect on port {reuse_port:?} failed ({e:#}); retrying fresh");
            run_connect(app, alias, &entry, None, update_daemon).await
        }
        other => other,
    };

    let tunnel = result.map_err(|e| format!("{e:#}"))?;
    if let Err(e) = HostsStore::load_default().record_connected(alias) {
        tracing::debug!("could not record host {alias}: {e}");
    }
    let entry = host_entry(alias);
    let host_state = state_for(&entry, "connected", Some(&tunnel));
    // Tell open windows on this host to re-home if the port or token moved
    // (daemon restart / update); a same-port+token reconnect is a no-op for
    // them — their WebSocket just reconnects.
    let _ = app.emit(
        "host-status",
        HostStatus {
            alias: alias.to_string(),
            status: "connected",
            local_port: Some(tunnel.local_port),
            token: Some(tunnel.manifest.token.clone()),
            error: None,
        },
    );
    let (port, token) = (tunnel.local_port, tunnel.manifest.token.clone());
    state.tunnels.lock().await.insert(alias.to_string(), tunnel);
    // Any persisted windows for this host that aren't open come back now.
    // The registry keeps records across a failed launch-time restore
    // precisely so the first connect that DOES land — a home-screen click,
    // a window's reconnect — restores them, not just the next app start.
    reopen_windows(app, alias, port, &token);
    Ok(host_state)
}

/// Open every persisted window record for `alias` without a live window
/// (matched on the record's stable id — an open window re-homes in place
/// and must not be duplicated).
fn reopen_windows(app: &AppHandle, alias: &str, port: u16, token: &str) {
    let Some(shell) = app.try_state::<Shell>() else {
        return;
    };
    let open: HashSet<String> = lock(&shell.windows)
        .values()
        .map(|s| s.stable_id.clone())
        .collect();
    let records: Vec<WindowRecord> = lock(&shell.registry)
        .list()
        .into_iter()
        .filter(|r| r.alias.as_deref() == Some(alias) && !open.contains(&r.id))
        .collect();
    for record in records {
        if let Err(e) = open_ui_window(app, port, token, &record) {
            tracing::warn!("could not reopen window {}: {e}", record.id);
        }
    }
}

#[tauri::command]
async fn disconnect_host(state: State<'_, Shell>, alias: String) -> Result<(), String> {
    if let Some(tunnel) = state.tunnels.lock().await.remove(&alias) {
        tunnel.close().await;
    }
    Ok(())
}

/// The local daemon's build parity (home screen: quiet update note).
#[tauri::command]
async fn local_state(state: State<'_, Shell>) -> Result<LocalState, String> {
    let d = lock(&state.local).clone();
    Ok(LocalState {
        outdated: d.outdated,
        build: d.build,
        live_sessions: d.live_sessions,
    })
}

/// Explicit local-daemon update: graceful stop, respawn our build, then
/// broadcast the new port + token so every window on the local daemon can
/// re-home itself (the old origin is gone).
#[tauri::command]
async fn update_local_daemon(app: AppHandle, state: State<'_, Shell>) -> Result<(), String> {
    tracing::info!("ipc: update_local_daemon");
    let fresh = crate::daemon::update_local_daemon()
        .await
        .map_err(|e| format!("{e:#}"))?;
    let moved = LocalDaemonMoved {
        port: fresh.port,
        token: fresh.token.clone(),
    };
    *lock(&state.local) = fresh;
    let _ = app.emit("local-daemon-updated", moved);
    Ok(())
}

/// The connected host's registered workspaces, proxied through the tunnel
/// (the home page's own origin cannot reach another daemon's port).
#[tauri::command]
async fn remote_workspaces(
    state: State<'_, Shell>,
    alias: String,
) -> Result<serde_json::Value, String> {
    let (port, token) = {
        let tunnels = state.tunnels.lock().await;
        let t = tunnels
            .get(&alias)
            .ok_or_else(|| format!("{alias} is not connected"))?;
        (t.local_port, t.manifest.token.clone())
    };
    tokio::task::spawn_blocking(move || {
        let body = ureq::get(&format!("http://127.0.0.1:{port}/api/v1/workspaces"))
            .set("Authorization", &format!("Bearer {token}"))
            .timeout(Duration::from_secs(10))
            .call()
            .map_err(|e| format!("could not list workspaces: {e}"))?
            .into_string()
            .map_err(|e| format!("could not read workspaces: {e}"))?;
        serde_json::from_str(&body).map_err(|e| format!("bad workspaces payload: {e}"))
    })
    .await
    .map_err(|e| format!("{e}"))?
}

/// Open a window on the local daemon (`alias` None) or a connected remote.
/// `ws_id` None lands on the home screen. Unless `new_window`, an existing
/// window already showing this `(alias, ws)` is raised instead of duplicated.
#[tauri::command]
async fn open_window(
    app: AppHandle,
    state: State<'_, Shell>,
    alias: Option<String>,
    ws_id: Option<String>,
    new_window: Option<bool>,
) -> Result<(), String> {
    let new_window = new_window.unwrap_or(false);
    tracing::info!("ipc: open_window alias={alias:?} ws={ws_id:?} new_window={new_window}");
    if !new_window {
        if let Some(label) = find_by_scope(&state.windows, &alias, &ws_id, None) {
            if let Some(win) = app.get_webview_window(&label) {
                return win
                    .set_focus()
                    .map_err(|e| format!("could not focus window: {e}"));
            }
        }
    }
    let (port, token, host) = match alias {
        None => {
            let local = lock(&state.local);
            (local.port, local.token.clone(), None)
        }
        Some(alias) => {
            let tunnels = state.tunnels.lock().await;
            let t = tunnels
                .get(&alias)
                .ok_or_else(|| format!("{alias} is not connected"))?;
            (t.local_port, t.manifest.token.clone(), Some(alias.clone()))
        }
    };
    open_ui_window(&app, port, &token, &WindowRecord::new(host, ws_id))
        .map_err(|e| format!("could not open window: {e}"))
}

/// Check GitHub releases for a newer signed app build. Returns the new
/// version string when one is available, `None` when up to date. All
/// updater work runs in Rust; the web UI can only ask, never drive the
/// download — and the download is verified against the embedded minisign
/// pubkey regardless, so only a validly-signed release can ever install.
#[tauri::command]
async fn check_app_update(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| e.to_string())?;
    match updater.check().await {
        Ok(Some(update)) => Ok(Some(update.version)),
        Ok(None) => Ok(None),
        // A missing/unreachable endpoint (no releases yet) is "no update",
        // not an error the user should see on every launch.
        Err(e) => {
            tracing::debug!("update check unavailable: {e}");
            Ok(None)
        }
    }
}

/// Download, verify, and install the pending app update, then relaunch into
/// it. Diverges on success (the process restarts); returns `Err` only when
/// the download or signature check fails.
#[tauri::command]
async fn install_app_update(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no update available".to_string())?;
    tracing::info!("installing app update {}", update.version);
    update
        .download_and_install(|_downloaded, _total| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    // Bundle swapped and verified; relaunch into the new build (diverges).
    app.restart();
}

/// Answer an in-flight SSH auth prompt (see `askpass`): `secret` None means
/// the user cancelled, which lets the waiting ssh fail cleanly. The done
/// broadcast dismisses the prompt in every other window showing it.
#[tauri::command]
async fn answer_askpass(
    app: AppHandle,
    askpass: State<'_, crate::askpass::Askpass>,
    id: u64,
    secret: Option<String>,
) -> Result<(), String> {
    if askpass.answer(id, secret) {
        let _ = app.emit("ssh-askpass-done", id);
    }
    Ok(())
}

/// SSH prompts still awaiting an answer. Each window fetches this on mount:
/// the `ssh-askpass` emit reaches only windows that already exist, and
/// startup window restore starts connecting before the first webview has
/// loaded — without this, that prompt is lost and the host sits in
/// "connecting" until ssh times out, with nothing for the user to answer.
#[tauri::command]
fn list_askpass(askpass: State<'_, crate::askpass::Askpass>) -> Vec<crate::askpass::PromptEvent> {
    askpass.pending()
}

/// The SPA reporting what this window now shows — it swaps `ws` client-side,
/// so the shell can't see it otherwise. Keyed by the calling window's label;
/// the persisted record follows so the next launch reopens the window on
/// what it was ACTUALLY showing, not what it was opened on.
#[tauri::command]
fn report_window_scope(
    webview: tauri::WebviewWindow,
    state: State<'_, Shell>,
    alias: Option<String>,
    ws: Option<String>,
) {
    let mut windows = lock(&state.windows);
    let stable_id = windows
        .get(webview.label())
        .map(|s| s.stable_id.clone())
        .unwrap_or_default();
    windows.insert(
        webview.label().to_string(),
        WindowScope {
            alias: alias.clone(),
            ws: ws.clone(),
            stable_id: stable_id.clone(),
        },
    );
    drop(windows);
    if !stable_id.is_empty() {
        lock(&state.registry).set_scope(&stable_id, alias, ws);
    }
}

/// This app binary's build id, for daemon-skew detection in the UI (the
/// daemon's own build rides GET /api/v1/health).
#[tauri::command]
fn shell_build() -> String {
    chimaera_core::BUILD_ID.to_string()
}

/// The one-click update chain, step one: record the user's consent (the
/// intent file), then download, verify, and install the app bundle and
/// relaunch into it. Step two — replacing the local daemon — happens in the
/// NEW process's startup, which consumes the intent; the daemon's restart
/// handoff + session ledger are what make that step keep every window,
/// tab, and session. Diverges on success; on failure the intent is cleared
/// so nothing acts on it later.
#[tauri::command]
async fn begin_update(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;
    tracing::info!("ipc: begin_update");
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no update available".to_string())?;
    crate::update::write_intent().map_err(|e| format!("{e:#}"))?;
    match update
        .download_and_install(|_downloaded, _total| {}, || {})
        .await
    {
        Ok(()) => {
            tracing::info!("app update {} installed; relaunching", update.version);
            app.restart();
        }
        Err(e) => {
            crate::update::clear_intent();
            Err(e.to_string())
        }
    }
}

/// Raise an existing window showing `(alias, ws)`, other than the caller.
/// Returns whether one was found — the UI activates in-place only when false.
#[tauri::command]
fn focus_window(
    app: AppHandle,
    webview: tauri::WebviewWindow,
    state: State<'_, Shell>,
    alias: Option<String>,
    ws: Option<String>,
) -> bool {
    match find_by_scope(&state.windows, &alias, &ws, Some(webview.label())) {
        Some(label) => app
            .get_webview_window(&label)
            .map(|w| w.set_focus().is_ok())
            .unwrap_or(false),
        None => false,
    }
}

fn host_entry(alias: &str) -> chimaera_remote::hosts::HostEntry {
    HostsStore::load_default()
        .get(alias)
        .unwrap_or(chimaera_remote::hosts::HostEntry {
            alias: alias.to_string(),
            binary: None,
            added_at: 0,
            last_connected_at: None,
        })
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Watch live tunnels and broadcast `host-status` on up↔down transitions.
/// Without this a dropped forward (remote daemon or ssh died) goes unnoticed
/// until the user clicks. A single task is the sole emitter — windows and the
/// home screen just listen, so there is no per-window reconnect stampede. The
/// probe is an end-to-end HTTP health check on the loopback port (no extra
/// ssh child): a bare TCP connect would keep reporting "up" after laptop
/// sleep, when ssh's local listener survives its dead connection — the state
/// that made reconnect a silent no-op.
fn spawn_health_monitor(handle: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut prev: HashMap<String, bool> = HashMap::new();
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            // No managed Shell means the app is tearing down — stop the loop.
            let Some(shell) = handle.try_state::<Shell>() else {
                break;
            };
            // Snapshot under the lock, then drop it before probing sockets.
            let snap: Vec<(String, u16, String)> = {
                let tunnels = shell.tunnels.lock().await;
                tunnels
                    .iter()
                    .map(|(a, t)| (a.clone(), t.local_port, t.manifest.token.clone()))
                    .collect()
            };
            // Aliases gone from the map were disconnected by the user; forget
            // them without emitting a spurious `down`.
            prev.retain(|a, _| snap.iter().any(|(s, ..)| s == a));
            for (alias, port, token) in &snap {
                let up = chimaera_remote::http_alive(*port).await;
                if prev.insert(alias.clone(), up) == Some(up) {
                    continue; // no transition
                }
                let _ = handle.emit(
                    "host-status",
                    HostStatus {
                        alias: alias.clone(),
                        status: if up { "connected" } else { "down" },
                        local_port: Some(*port),
                        token: up.then(|| token.clone()),
                        error: None,
                    },
                );
            }
        }
    });
}

/// Reopen the persisted window set: local-daemon windows immediately;
/// remote windows as their host tunnels come up (one connect per alias, in
/// the background — an unreachable host must not hold up launch). Returns
/// whether any window opens immediately, so startup can fall back to a home
/// window and never come up invisible.
fn restore_windows(handle: &AppHandle, port: u16, token: &str) -> bool {
    let records = {
        let shell = handle.state::<Shell>();
        let records = lock(&shell.registry).list();
        records
    };
    let mut opened = false;
    let mut remote_aliases: Vec<String> = Vec::new();
    for record in &records {
        match &record.alias {
            None => match open_ui_window(handle, port, token, record) {
                Ok(()) => opened = true,
                Err(e) => tracing::warn!("could not reopen window {}: {e}", record.id),
            },
            Some(alias) => {
                if !remote_aliases.contains(alias) {
                    remote_aliases.push(alias.clone());
                }
            }
        }
    }
    for alias in remote_aliases {
        let handle = handle.clone();
        tauri::async_runtime::spawn(async move {
            // Window reopening rides the connect itself (`reopen_windows` in
            // `run_flight`), and the records survive failure: the windows
            // come back with the first connect that lands — a home-screen
            // click, a window's reconnect, or the next launch.
            if let Err(e) = do_connect(&handle, alias.clone(), false).await {
                tracing::warn!("could not reconnect {alias} to restore windows: {e}");
            }
        });
    }
    opened
}

/// Build and run the Tauri app.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            list_hosts,
            add_host,
            remove_host,
            connect_host,
            disconnect_host,
            local_state,
            update_local_daemon,
            remote_workspaces,
            open_window,
            check_app_update,
            install_app_update,
            begin_update,
            shell_build,
            answer_askpass,
            list_askpass,
            report_window_scope,
            focus_window,
        ])
        .on_window_event(|window, event| {
            let Some(shell) = window.try_state::<Shell>() else {
                return;
            };
            match event {
                // Forget a window's scope once it's gone, so focus-existing
                // never raises a dead label. Destroyed (not CloseRequested,
                // which can be vetoed) fires after teardown completes. The
                // persisted record goes too — a deliberately closed window
                // stays closed (macOS convention) — EXCEPT during quit, when
                // teardown destroys every window and forgetting them would
                // defeat restore.
                tauri::WindowEvent::Destroyed => {
                    let scope = lock(&shell.windows).remove(window.label());
                    if !shell.quitting.load(Ordering::Relaxed) {
                        if let Some(scope) = scope {
                            lock(&shell.registry).remove(&scope.stable_id);
                        }
                    }
                }
                // Track geometry in memory on every move/resize; a slow tick
                // (and exit) persists — never a file write per drag event.
                tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_) => {
                    let Some(stable_id) = lock(&shell.windows)
                        .get(window.label())
                        .map(|s| s.stable_id.clone())
                    else {
                        return;
                    };
                    let scale = window.scale_factor().unwrap_or(1.0);
                    if let (Ok(pos), Ok(size)) = (window.outer_position(), window.inner_size()) {
                        let pos = pos.to_logical::<f64>(scale);
                        let size = size.to_logical::<f64>(scale);
                        lock(&shell.registry).set_geometry(
                            &stable_id,
                            pos.x,
                            pos.y,
                            size.width,
                            size.height,
                        );
                    }
                }
                _ => {}
            }
        })
        .setup(|app| {
            let handle = app.handle().clone();
            crate::menu::install(app)?;
            // Route ssh auth prompts (password / 2FA) to the UI. Managed
            // before the listener starts so an early prompt finds the state.
            app.manage(crate::askpass::Askpass::default());
            if let Err(e) = crate::askpass::install(&handle) {
                // Non-fatal: hosts with key/agent auth still connect; only
                // password/2FA hosts lose the in-app prompt.
                tracing::warn!("ssh askpass unavailable: {e:#}");
            }
            // The daemon must be up before the first window points at it;
            // block setup on it (fast when a daemon is already running).
            let mut local = tauri::async_runtime::block_on(crate::daemon::ensure_local_daemon())
                .map_err(|e| {
                    tracing::error!("{e:#}");
                    std::io::Error::other(format!("{e:#}"))
                })?;
            // A fresh update intent = the user clicked "update" in the OLD
            // process and the app half already swapped; finish the chain by
            // replacing the outdated daemon NOW, before any window loads its
            // (old) embedded UI. The daemon's handoff keeps port + token and
            // its ledger resurrects the sessions, so the windows restored
            // below land on the new daemon with everything where it was.
            if crate::update::consume_intent() && local.outdated {
                tracing::info!("update intent: replacing the outdated local daemon");
                match tauri::async_runtime::block_on(crate::daemon::update_local_daemon()) {
                    Ok(fresh) => local = fresh,
                    // The old daemon keeps serving; the update stays a
                    // visible affordance instead of a silent failure.
                    Err(e) => tracing::warn!("daemon update after app update failed: {e:#}"),
                }
            }
            let (port, token) = (local.port, local.token.clone());
            // Manage Shell BEFORE opening any window so each can register
            // its scope in the window map.
            app.manage(Shell {
                local: Mutex::new(local),
                tunnels: tokio::sync::Mutex::new(HashMap::new()),
                connecting: Mutex::new(HashMap::new()),
                windows: Mutex::new(HashMap::new()),
                registry: Mutex::new(WindowRegistry::load_default()),
                quitting: AtomicBool::new(false),
            });
            // Reopen last session's windows; first launch (or an all-remote
            // set that is still connecting) gets the home window so the app
            // never comes up invisible.
            if !restore_windows(&handle, port, &token) {
                open_ui_window(&handle, port, &token, &WindowRecord::new(None, None))?;
                tracing::info!("home window open on 127.0.0.1:{port}");
            }
            spawn_health_monitor(handle.clone());
            crate::update::spawn_update_watch(handle.clone());
            // Slow flush for the geometry dirty flag (window drags).
            let flush = handle.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    let Some(shell) = flush.try_state::<Shell>() else {
                        break;
                    };
                    lock(&shell.registry).save_if_dirty();
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building chimaera")
        .run(|app, event| match event {
            // Quit teardown destroys every window; flag it FIRST so those
            // Destroyed events keep the registry intact for the next launch.
            tauri::RunEvent::ExitRequested { .. } => {
                if let Some(state) = app.try_state::<Shell>() {
                    state.quitting.store(true, Ordering::Relaxed);
                    lock(&state.registry).save_if_dirty();
                }
            }
            tauri::RunEvent::Exit => {
                // Kill tunnel children so forwarded ports don't leak past
                // the app (the daemons keep running by design).
                if let Some(state) = app.try_state::<Shell>() {
                    lock(&state.registry).save_if_dirty();
                    tauri::async_runtime::block_on(async {
                        let mut tunnels = state.tunnels.lock().await;
                        for (_, tunnel) in tunnels.drain() {
                            tunnel.close().await;
                        }
                    });
                }
            }
            _ => {}
        });
}
