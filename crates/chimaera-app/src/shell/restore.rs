//! Opening UI windows, the live-tunnel health monitor, and reopening the
//! persisted window set at launch. One central monitor owns liveness
//! transitions; successful connect flights publish endpoint identity too.
//! Windows and the home screen only listen, so there is no per-window probe
//! stampede.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use super::connect::{do_connect, HostStatus};
use super::{authorize_daemon_origin, daemon_navigation_allowed, lock, Shell, WindowScope};
use crate::windows::WindowRecord;

static WINDOW_SEQ: AtomicU64 = AtomicU64::new(0);
/// One timeout is weak evidence on an SSH/ProxyJump/HPC path. Three
/// consecutive authenticated misses still detect a dead forward promptly,
/// while a scheduler or network hiccup does not tear down a usable tunnel.
const HEALTH_FAILURES_BEFORE_DOWN: u8 = 3;

#[derive(Default)]
struct HealthConfidence {
    consecutive_failures: u8,
    down: bool,
}

impl HealthConfidence {
    /// `Some(true)` = recovered, `Some(false)` = confirmed down, `None` = no
    /// externally visible transition. A newly installed tunnel starts from a
    /// proven-good baseline: `open_proven_tunnel` authenticated it before the
    /// shell inserted it.
    fn sample(&mut self, up: bool) -> Option<bool> {
        if up {
            self.consecutive_failures = 0;
            return std::mem::take(&mut self.down).then_some(true);
        }
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if !self.down && self.consecutive_failures >= HEALTH_FAILURES_BEFORE_DOWN {
            self.down = true;
            Some(false)
        } else {
            None
        }
    }

    /// A connect flight performed its own authenticated proof and published
    /// the recovery edge, so monitoring starts a fresh confidence window
    /// without emitting a duplicate `connected`.
    fn reset_after_external_proof(&mut self) {
        *self = Self::default();
    }
}

struct TunnelEndpoint {
    key: String,
    port: u16,
    token: String,
    is_compute: bool,
}

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
    let home_hub = record.alias.is_none() && record.ws.is_none() && record.compute.is_none();
    let appearance = lock(&app.state::<Shell>().appearance).get(record.alias.as_deref());
    let url = daemon_window_url(
        port,
        token,
        &record.id,
        record.ws.as_deref(),
        record.alias.as_deref(),
        home_hub,
        appearance.as_ref(),
    )?;
    let title = match &record.alias {
        Some(alias) => format!("{alias} — chimaera"),
        None => "chimaera".to_string(),
    };
    let scope = WindowScope::new(record.alias.clone(), record.ws.clone(), record.id.clone());
    open_shell_window(app, url.as_str(), &title, record, scope)
}

/// One daemon-served window URL. The unused Home launcher carries `hub=1`
/// across local↔remote navigation so each origin clears stale route state.
/// Entering a workspace then promotes it into an ordinary workbench window.
pub(super) fn daemon_window_url(
    port: u16,
    token: &str,
    stable_id: &str,
    ws: Option<&str>,
    alias: Option<&str>,
    home_hub: bool,
    appearance: Option<&crate::appearance::AppearanceBootstrap>,
) -> tauri::Result<tauri::Url> {
    let mut hash = format!("token={}", urlencoding::encode(token));
    hash.push_str(&format!("&win={}", urlencoding::encode(stable_id)));
    if let Some(ws) = ws {
        hash.push_str(&format!("&ws={}", urlencoding::encode(ws)));
    }
    if let Some(alias) = alias {
        hash.push_str(&format!("&host={}", urlencoding::encode(alias)));
    }
    if home_hub {
        hash.push_str("&hub=1");
    }
    if let Some(appearance) = appearance {
        hash.push_str("&appearance=");
        hash.push_str(&urlencoding::encode(&appearance.json()));
    }
    format!("http://127.0.0.1:{port}/#{hash}")
        .parse()
        .map_err(tauri::Error::InvalidUrl)
}

/// Open a window on a compute-node daemon (Mode 2). Same shell wiring as
/// `open_ui_window`, but the URL is the ComputeTunnel's own (token + host +
/// job + node already ride its hash; only `win=` is added here) and the
/// tracked scope alias is the composite `"{alias}#job{id}"` so focus-existing
/// never confuses a job window with the login host's. Its askpass identity is
/// recorded separately as the login alias; it is never inferred by parsing
/// that composite key.
pub(super) fn open_compute_window(
    app: &AppHandle,
    url: &str,
    title: &str,
    record: &WindowRecord,
    scope_alias: &str,
) -> tauri::Result<()> {
    let mut url = format!("{url}&win={}", urlencoding::encode(&record.id));
    let appearance = lock(&app.state::<Shell>().appearance).get(record.alias.as_deref());
    if let Some(appearance) = appearance {
        url.push_str("&appearance=");
        url.push_str(&urlencoding::encode(&appearance.json()));
    }
    let login_alias = record
        .alias
        .clone()
        .expect("compute windows always carry their login alias");
    let scope = WindowScope::new_compute(
        scope_alias.to_string(),
        login_alias,
        record.ws.clone(),
        record.id.clone(),
    );
    open_shell_window(app, &url, title, record, scope)
}

fn open_shell_window(
    app: &AppHandle,
    url: &str,
    title: &str,
    record: &WindowRecord,
    window_scope: WindowScope,
) -> tauri::Result<()> {
    let url: tauri::Url = url.parse().expect("daemon url is always valid");
    let port = url
        .port()
        .expect("daemon URLs always carry an explicit loopback port");
    let label = format!("win-{}", WINDOW_SEQ.fetch_add(1, Ordering::Relaxed));
    authorize_daemon_origin(app, &label, port)?;
    let navigation_app = app.clone();
    let navigation_label = label.clone();
    let page_load_app = app.clone();
    let page_load_label = label.clone();
    let mut builder = WebviewWindowBuilder::new(app, label.clone(), WebviewUrl::External(url))
        .on_navigation(move |url| {
            daemon_navigation_allowed(&navigation_app, &navigation_label, url)
        })
        .on_page_load(move |_window, payload| {
            if !matches!(payload.event(), tauri::webview::PageLoadEvent::Started)
                || !daemon_navigation_allowed(&page_load_app, &page_load_label, payload.url())
            {
                return;
            }
            let Some(shell) = page_load_app.try_state::<Shell>() else {
                return;
            };
            let mut windows = lock(&shell.windows);
            if let Some(scope) = windows.get_mut(&page_load_label) {
                scope.complete_home_navigation();
            }
        })
        .title(title)
        // Tauri's own drag-drop handler intercepts OS file drops and suppresses
        // the webview's DOM drop events. The workbench handles drops itself in
        // the web UI (upload to the session's owning host, then reference the
        // path) — one code path for browser and native shell, correct for local
        // AND tunneled-remote windows by construction (a bare local path means
        // nothing on a remote host). Disabling it hands HTML5 dnd back to the UI.
        .disable_drag_drop_handler()
        .inner_size(1280.0, 840.0)
        .min_inner_size(680.0, 440.0);
    // The workbench already carries its workspace identity in the rail and
    // pane tabs, so a separate title row only repeats context and steals
    // vertical room. On macOS keep the native traffic lights, but let them
    // overlay the webview and keep the dynamic title as OS metadata only.
    // App.svelte provides the custom drag region Overlay requires.
    #[cfg(target_os = "macos")]
    {
        builder = builder
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true);
    }
    if let (Some(w), Some(h)) = (record.width, record.height) {
        builder = builder.inner_size(w, h);
    }
    if let (Some(x), Some(y)) = (record.x, record.y) {
        builder = builder.position(x, y);
    }
    // Register the immutable host scope before the webview can execute a
    // command. `build()` starts navigation, so inserting afterward leaves a
    // short startup race where `list_askpass` rejects a legitimate window and
    // the only visible authentication prompt is missed.
    if let Some(shell) = app.try_state::<Shell>() {
        // The scope already carries both the window identity and its separate
        // immutable askpass authorization. Register it before navigation can
        // execute a native command.
        lock(&shell.windows).insert(label.clone(), window_scope);
    }
    if let Err(error) = builder.build() {
        if let Some(shell) = app.try_state::<Shell>() {
            lock(&shell.allowed_daemon_ports).remove(&label);
            lock(&shell.windows).remove(&label);
        }
        return Err(error);
    }
    // Persist the new window so the next launch reopens it. Startup manages
    // Shell before opening any window, so every daemon window registered
    // above has an authoritative scope before its first native command.
    if let Some(shell) = app.try_state::<Shell>() {
        lock(&shell.registry).upsert(record.clone());
    }
    // A new window changes the tray's open-windows list.
    crate::tray::rebuild(app);
    Ok(())
}

/// Watch live tunnels and broadcast confirmed `host-status` up↔down
/// transitions. The probes are concurrent across hosts (one slow cluster
/// cannot delay every other host) and hysteretic (one timeout is suspect, not
/// down). A probe is an authenticated end-to-end HTTP health check on the
/// loopback port, with no extra ssh child: a bare TCP connect keeps reporting
/// "up" after laptop sleep when ssh's local listener survives its dead
/// connection.
pub(super) fn spawn_health_monitor(handle: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tracing::debug!("ssh health monitor started");
        let mut confidence: HashMap<String, HealthConfidence> = HashMap::new();
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            // No managed Shell means the app is tearing down — stop the loop.
            let Some(shell) = handle.try_state::<Shell>() else {
                break;
            };
            // Snapshot under the locks, then drop them before probing
            // sockets. Compute tunnels ride the same loop under their
            // composite key ("alias#job{id}") — a job window listens on
            // that key, NEVER on the login alias (listening on the alias
            // made every login-tunnel blip re-home job windows onto the
            // login daemon — found live). Their `token` field stays None on
            // the wire. Every probe is authed with its tunnel's own token:
            // identity, not just liveness, because a stale relay or unrelated
            // loopback server can answer on a recycled port.
            let mut snap: Vec<TunnelEndpoint> = {
                let tunnels = shell.tunnels.lock().await;
                tunnels
                    .iter()
                    .map(|(key, tunnel)| TunnelEndpoint {
                        key: key.clone(),
                        port: tunnel.local_port,
                        token: tunnel.manifest.token.clone(),
                        is_compute: false,
                    })
                    .collect()
            };
            {
                let compute = shell.compute_tunnels.lock().await;
                snap.extend(compute.iter().map(|(key, tunnel)| TunnelEndpoint {
                    key: key.clone(),
                    port: tunnel.local_port,
                    token: tunnel.token.clone(),
                    is_compute: true,
                }));
            }
            tracing::trace!(tunnels = snap.len(), "ssh health monitor snapshot");
            // Keys gone from the maps were disconnected by the user; forget
            // them without emitting a spurious `down`.
            let current_keys: HashSet<String> =
                snap.iter().map(|endpoint| endpoint.key.clone()).collect();
            confidence.retain(|key, _| current_keys.contains(key));
            let unhealthy = {
                let mut unhealthy = lock(&shell.unhealthy_tunnels);
                unhealthy.retain(|key| current_keys.contains(key));
                unhealthy.clone()
            };
            // A successful connect flight clears `unhealthy_tunnels` after
            // its own authenticated proof. Reset this monitor's old down
            // edge too, including when the healed tunnel reused the same
            // port/token; otherwise a second outage could never emit down.
            for (key, health) in &mut confidence {
                if health.down && !unhealthy.contains(key) {
                    health.reset_after_external_proof();
                }
            }

            // Every host gets the same observation time regardless of how
            // many other hosts are saved or how slowly one of them answers.
            let mut probes = tokio::task::JoinSet::new();
            for endpoint in snap {
                probes.spawn(async move {
                    let up =
                        chimaera_remote::http_alive_authed(endpoint.port, &endpoint.token).await;
                    (endpoint, up)
                });
            }
            while let Some(result) = probes.join_next().await {
                let Ok((endpoint, up)) = result else {
                    continue;
                };
                // A connect flight may have replaced this endpoint while its
                // probe was in flight. Never let an old port/token mark the
                // new tunnel down (or recovered).
                if !endpoint_is_current(&shell, &endpoint).await {
                    confidence.remove(&endpoint.key);
                    continue;
                }
                let transition = confidence
                    .entry(endpoint.key.clone())
                    .or_default()
                    .sample(up);
                tracing::trace!(
                    alias = %endpoint.key,
                    port = endpoint.port,
                    up,
                    ?transition,
                    "ssh health sample"
                );
                let Some(recovered) = transition else {
                    continue;
                };
                if recovered {
                    lock(&shell.unhealthy_tunnels).remove(&endpoint.key);
                } else {
                    lock(&shell.unhealthy_tunnels).insert(endpoint.key.clone());
                }
                let _ = handle.emit(
                    "host-status",
                    HostStatus {
                        alias: endpoint.key,
                        status: if recovered { "connected" } else { "down" },
                        local_port: Some(endpoint.port),
                        token: (recovered && !endpoint.is_compute).then_some(endpoint.token),
                        error: None,
                        reason: (!recovered).then(|| {
                            "Several authenticated health checks failed. Already-loaded views \
                             stay visible while remote actions reconnect."
                                .to_string()
                        }),
                        build: None,
                    },
                );
            }
        }
    });
}

async fn endpoint_is_current(shell: &Shell, endpoint: &TunnelEndpoint) -> bool {
    if endpoint.is_compute {
        let tunnels = shell.compute_tunnels.lock().await;
        tunnels.get(&endpoint.key).is_some_and(|tunnel| {
            tunnel.local_port == endpoint.port && tunnel.token == endpoint.token
        })
    } else {
        let tunnels = shell.tunnels.lock().await;
        tunnels.get(&endpoint.key).is_some_and(|tunnel| {
            tunnel.local_port == endpoint.port && tunnel.manifest.token == endpoint.token
        })
    }
}

/// Reopen the persisted window set: local-daemon windows immediately;
/// remote windows as their host tunnels come up (one connect per alias, in
/// the background — an unreachable host must not hold up launch). A local
/// home window is registered before those connects start whenever no restored
/// home can receive their startup askpass prompts.
pub(super) fn restore_windows(handle: &AppHandle, port: u16, token: &str) -> tauri::Result<()> {
    let records = {
        let shell = handle.state::<Shell>();
        let records = lock(&shell.registry).list();
        records
    };
    let (records, duplicate_home_ids) = dedupe_local_homes(records);
    if !duplicate_home_ids.is_empty() {
        let shell = handle.state::<Shell>();
        let mut registry = lock(&shell.registry);
        for id in duplicate_home_ids {
            registry.remove(&id);
        }
    }
    let mut opened = false;
    let mut home_opened = false;
    let mut remote_aliases: Vec<String> = Vec::new();
    for record in &records {
        // A compute window was a view onto a walltime-bounded job tunnel that
        // did not survive the restart — walltime death is honest, and the
        // home-screen card is the reconnect path. Purge the record so
        // windows.json doesn't accumulate dead jobs across quits.
        if record.compute.is_some() {
            let shell = handle.state::<Shell>();
            lock(&shell.registry).remove(&record.id);
            continue;
        }
        match &record.alias {
            None => match open_ui_window(handle, port, token, record) {
                Ok(()) => {
                    opened = true;
                    home_opened |= record.ws.is_none();
                }
                Err(e) => tracing::warn!("could not reopen window {}: {e}", record.id),
            },
            Some(alias) => {
                if !remote_aliases.contains(alias) {
                    remote_aliases.push(alias.clone());
                }
            }
        }
    }

    // Register the safe cross-host askpass surface before spawning any ssh.
    // A local workspace is already visible, but deliberately cannot observe
    // another host's prompt; treating it as the startup fallback strands a
    // password/2FA connect until the 180-second askpass timeout.
    if needs_startup_home(opened, home_opened, !remote_aliases.is_empty()) {
        open_ui_window(handle, port, token, &WindowRecord::new(None, None))?;
        tracing::info!("startup home window open on 127.0.0.1:{port}");
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
    Ok(())
}

/// Older builds allowed File → New Window to persist several local Home
/// records. Keep the oldest stable identity and retire the rest before
/// restoring, while leaving remote host pages and workspace windows alone.
fn dedupe_local_homes(records: Vec<WindowRecord>) -> (Vec<WindowRecord>, Vec<String>) {
    let mut saw_home = false;
    let mut kept = Vec::with_capacity(records.len());
    let mut duplicates = Vec::new();
    for record in records {
        let local_home = record.compute.is_none() && record.alias.is_none() && record.ws.is_none();
        if local_home && std::mem::replace(&mut saw_home, true) {
            duplicates.push(record.id);
        } else {
            kept.push(record);
        }
    }
    (kept, duplicates)
}

fn needs_startup_home(opened: bool, home_opened: bool, has_remote: bool) -> bool {
    !opened || (has_remote && !home_opened)
}

#[cfg(test)]
mod tests {
    use super::{
        dedupe_local_homes, needs_startup_home, HealthConfidence, HEALTH_FAILURES_BEFORE_DOWN,
    };
    use crate::windows::WindowRecord;

    #[test]
    fn startup_home_precedes_remote_auth_when_no_home_was_restored() {
        assert!(needs_startup_home(false, false, false));
        assert!(!needs_startup_home(true, false, false));
        assert!(needs_startup_home(true, false, true));
        assert!(!needs_startup_home(true, true, true));
    }

    #[test]
    fn one_health_miss_never_drops_a_tunnel() {
        let mut health = HealthConfidence::default();
        for _ in 0..HEALTH_FAILURES_BEFORE_DOWN - 1 {
            assert_eq!(health.sample(false), None);
        }
        assert_eq!(health.sample(true), None, "a success clears suspicion");
        assert_eq!(health.sample(false), None, "the failure count must reset");
    }

    #[test]
    fn health_requires_consecutive_failures_and_emits_each_edge_once() {
        let mut health = HealthConfidence::default();
        for _ in 1..HEALTH_FAILURES_BEFORE_DOWN {
            assert_eq!(health.sample(false), None);
        }
        assert_eq!(health.sample(false), Some(false));
        assert_eq!(health.sample(false), None, "down is one transition");
        assert_eq!(health.sample(true), Some(true));
        assert_eq!(health.sample(true), None, "recovery is one transition");
    }

    #[test]
    fn proven_reconnect_arms_a_second_down_edge() {
        let mut health = HealthConfidence::default();
        for expected in [None, None, Some(false)] {
            assert_eq!(health.sample(false), expected);
        }
        health.reset_after_external_proof();
        for expected in [None, None, Some(false)] {
            assert_eq!(health.sample(false), expected);
        }
    }

    #[test]
    fn restore_keeps_one_local_home_without_collapsing_host_pages() {
        let first = WindowRecord::new(None, None);
        let duplicate = WindowRecord::new(None, None);
        let remote = WindowRecord::new(Some("cluster".into()), None);
        let workspace = WindowRecord::new(None, Some("ws-1".into()));
        let first_id = first.id.clone();
        let duplicate_id = duplicate.id.clone();
        let remote_id = remote.id.clone();

        let (kept, duplicates) = dedupe_local_homes(vec![first, remote, duplicate, workspace]);

        assert_eq!(duplicates, vec![duplicate_id]);
        assert!(kept.iter().any(|record| record.id == first_id));
        assert!(kept.iter().any(|record| record.id == remote_id));
        assert_eq!(kept.len(), 3);
    }
}
