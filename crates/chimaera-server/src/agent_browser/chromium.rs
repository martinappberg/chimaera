//! The Chromium engine: a headless Chrome or Chromium already on the
//! daemon's host, driven over `--remote-debugging-pipe` ([`super::cdp`]).
//!
//! It answers when no app window can (a cluster job while the laptop is
//! closed, a Linux host). Nothing is bundled or downloaded: the browser is
//! `browser.path`, else a system install on PATH, else a macOS app bundle.
//!
//! - **One browser per daemon**, launched on the first `Open`, in its own
//!   process group, with a throwaway profile under the node-local runtime
//!   dir (never NFS/Lustre/GPFS). It is closed when it has no pages left at
//!   a reap, and killed by group at daemon shutdown. Login-node daemons
//!   never launch it.
//! - **One target per page key**, at most [`MAX_PAGES`]; a page idle for
//!   [`IDLE`] is closed by [`Chromium::reap_idle`].
//! - **The allowlist holds for every main-frame document** the page loads,
//!   redirects and page-initiated navigations included: `Fetch` pauses each
//!   one and [`proxy::check_target`] decides, the same rule as pane views.
//!   Subframes and subresources pass.
//! - Console and network rings use the bridge's shapes (CONTRACTS §5):
//!   300 entries each, every entry ≤ 2 KB, ids monotonic from 1; a read
//!   returns the entries with `id >= since` and `*_next` = the last id + 1.
//!
//! Every CDP call has a timeout; a browser that dies fails its pending calls
//! at once ([`BrowserError::Gone`]). No std lock is held across an await.

use std::collections::{HashMap, VecDeque};
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, watch};

use super::cdp::{CdpError, Conn, Event};
use super::{
    Action, ActionOut, BrowserError, ConsoleEntry, HistoryOp, Modifier, NetworkEntry, Rect,
    RingsOut, TargetUrl, World,
};
use crate::state::lock;
use crate::{proxy, AppState};

/// Pages (targets) one browser holds at once: one per agent.
const MAX_PAGES: usize = 4;
/// A page untouched this long is closed at the next reap.
const IDLE: Duration = Duration::from_secs(600);
/// Entries kept per ring.
const RING: usize = 300;
/// Bytes of console text per entry (the entry stays under 2 KB).
const TEXT_MAX: usize = 1800;
/// Bytes of URL / error per network entry.
const URL_MAX: usize = 1500;
const ERROR_MAX: usize = 300;
/// Requests tracked for timing and the in-flight count; past this, new
/// ones go untracked (the count reads low rather than memory growing).
const MAX_REQUESTS: usize = 500;
/// The default CDP call timeout.
const CALL: Duration = Duration::from_secs(10);
/// From spawn to the first answer (a cold Chrome on a slow disk).
const LAUNCH: Duration = Duration::from_secs(20);
/// How long a navigation may take to settle before the result says it is
/// still loading.
const LOAD: Duration = Duration::from_secs(15);
/// After a click or key: how long to watch for a navigation it started,
/// and how long to wait for that navigation to settle.
const NAV_START_GRACE: Duration = Duration::from_millis(120);
const NAV_SETTLE: Duration = Duration::from_secs(5);
/// Bytes of browser stderr kept for startup diagnostics.
const STDERR_TAIL: usize = 16 * 1024;
/// The isolated world's name (the snapshot script runs there).
const WORLD: &str = "chimaera-agent";
/// A full-page or clipped screenshot's largest side, CSS px.
const SHOT_MAX_SIDE: f64 = 8000.0;
/// An eval result's JSON, in bytes.
const EVAL_RESULT_MAX: usize = 2 * 1024 * 1024;
/// Characters `Text { per_key: true }` will type one key at a time.
const PER_KEY_MAX: usize = 10_000;
/// How long a discovery answer is reused (PATH entries can sit on NFS).
const DISCOVERY_TTL: Duration = Duration::from_secs(30);
/// Executable names looked up on PATH, in order.
const PATH_NAMES: [&str; 6] = [
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
    "chrome",
    "chrome-headless-shell",
];
#[cfg(target_os = "macos")]
const MAC_BUNDLES: [&str; 2] = [
    "Google Chrome.app/Contents/MacOS/Google Chrome",
    "Chromium.app/Contents/MacOS/Chromium",
];

/// Daemon-wide; lives in the core's `Browsers` state. Cheap to construct
/// (launches nothing).
pub(crate) struct Chromium {
    browser: Mutex<Option<Arc<Browser>>>,
    /// Held across a launch and a page creation (and by the reaper while it
    /// closes things), so a page is never created on a browser being shut.
    launching: tokio::sync::Mutex<()>,
    discovery: Mutex<Option<Discovery>>,
}

struct Discovery {
    setting: Option<String>,
    at: Instant,
    result: Result<PathBuf, String>,
}

/// Why the engine can't run here, sorted into the error the core reports.
enum Unavailable {
    /// Policy: a login node, a profile on a network filesystem.
    Refused(String),
    /// Nothing to launch.
    NoEngine(String),
}

impl Unavailable {
    fn text(self) -> String {
        match self {
            Unavailable::Refused(s) | Unavailable::NoEngine(s) => s,
        }
    }
    fn error(self) -> BrowserError {
        match self {
            Unavailable::Refused(s) => BrowserError::Refused(s),
            Unavailable::NoEngine(s) => BrowserError::NoEngine(s),
        }
    }
}

/// A dropped engine (the daemon's state going away, a test ending) takes
/// its browser with it.
impl Drop for Chromium {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Default for Chromium {
    fn default() -> Self {
        Self::new()
    }
}

impl Chromium {
    pub(crate) fn new() -> Self {
        Chromium {
            browser: Mutex::new(None),
            launching: tokio::sync::Mutex::new(()),
            discovery: Mutex::new(None),
        }
    }

    /// The browser this daemon would launch, or why it can't (no browser
    /// found, login node, NFS profile dir…): model-facing text naming the
    /// fix.
    pub(crate) fn available(&self, state: &AppState) -> Result<PathBuf, String> {
        self.check(state).map_err(Unavailable::text)
    }

    fn check(&self, state: &AppState) -> Result<PathBuf, Unavailable> {
        if state.compute.is_login_node() {
            return Err(Unavailable::Refused(
                "This daemon runs on a cluster login node, where Chimaera never starts a browser. \
                 Open the workspace as a cluster job (or connect the Chimaera app) to use the \
                 browser tools."
                    .to_string(),
            ));
        }
        let setting = lock(&state.settings).browser_path();
        let exe = self.discover(setting)?;
        profile_fs_ok(&chimaera_core::runtime_dir()).map_err(Unavailable::Refused)?;
        Ok(exe)
    }

    fn discover(&self, setting: Option<String>) -> Result<PathBuf, Unavailable> {
        {
            let cached = lock(&self.discovery);
            if let Some(d) = cached.as_ref() {
                if d.setting == setting && d.at.elapsed() < DISCOVERY_TTL {
                    return d.result.clone().map_err(Unavailable::NoEngine);
                }
            }
        }
        let path_var = std::env::var_os("PATH").unwrap_or_default();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let result = find_browser(setting.as_deref(), &path_var, home.as_deref());
        *lock(&self.discovery) = Some(Discovery {
            setting,
            at: Instant::now(),
            result: result.clone(),
        });
        result.map_err(Unavailable::NoEngine)
    }

    /// Act on the page keyed `page`. `Action::Open` creates the target
    /// (launching the browser on first use); any other action on an unknown
    /// key is `Gone`. `Close` closes the target. Open/Navigate/History
    /// answer `Value({"url","title","status"})` once the load settles
    /// (≤ 15 s; `"loading": true` if it hadn't). Click/Key/Text answer
    /// `Done`, or `Value({"blocked": "<why>"})` when a main-frame navigation
    /// was refused since the last result.
    pub(crate) async fn act(
        &self,
        state: &Arc<AppState>,
        page: &str,
        action: Action,
    ) -> Result<ActionOut, BrowserError> {
        if let Action::Open { url, width, height } = action {
            return self.open(state, page, &url, width, height).await;
        }
        let (browser, pg) = self.find(page)?;
        let _op = pg.op.lock().await;
        pg.touch();
        let out = match pg.alive(&browser) {
            Ok(()) => self.run(&browser, &pg, page, action).await,
            Err(e) => Err(e),
        };
        pg.touch();
        if matches!(out, Err(BrowserError::Gone(_))) {
            self.forget(&browser, page);
        }
        out
    }

    async fn run(
        &self,
        browser: &Arc<Browser>,
        pg: &Arc<Page>,
        key: &str,
        action: Action,
    ) -> Result<ActionOut, BrowserError> {
        match action {
            Action::Open { .. } => unreachable!("handled by open"),
            Action::Navigate { url } => pg.navigate(&browser.conn, &url.url()).await,
            Action::History { op } => pg.history(&browser.conn, op).await,
            Action::Eval {
                world,
                body,
                args,
                timeout_ms,
            } => pg
                .eval(&browser.conn, world, &body, &args, timeout_ms)
                .await
                .map(ActionOut::Value),
            Action::Click {
                x,
                y,
                clicks,
                modifiers,
            } => {
                let mark = pg.mark();
                pg.click(&browser.conn, x, y, clicks.max(1), &modifiers)
                    .await?;
                pg.after_input(mark).await
            }
            Action::Move { x, y } => {
                pg.mouse_move(&browser.conn, x, y, 0).await?;
                Ok(ActionOut::Done)
            }
            Action::Drag { from, to, steps } => {
                let mark = pg.mark();
                pg.drag(&browser.conn, from, to, steps).await?;
                pg.after_input(mark).await
            }
            Action::Key { key, modifiers } => {
                let mark = pg.mark();
                pg.key(&browser.conn, &key, &modifiers).await?;
                pg.after_input(mark).await
            }
            Action::Text { text, per_key } => {
                let mark = pg.mark();
                pg.text(&browser.conn, &text, per_key).await?;
                pg.after_input(mark).await
            }
            Action::Scroll { x, y, dx, dy } => {
                pg.scroll(&browser.conn, x, y, dx, dy).await?;
                Ok(ActionOut::Done)
            }
            Action::Screenshot {
                clip,
                full_page,
                max_width,
            } => {
                pg.screenshot(&browser.conn, clip, full_page, max_width)
                    .await
            }
            Action::Rings {
                console_since,
                network_since,
            } => Ok(ActionOut::Rings(pg.rings(console_since, network_since))),
            Action::Close => {
                browser.close_page(key, pg).await;
                Ok(ActionOut::Done)
            }
        }
    }

    /// The live browser and the page keyed `key`, or why there is none.
    fn find(&self, key: &str) -> Result<(Arc<Browser>, Arc<Page>), BrowserError> {
        let browser = lock(&self.browser).clone();
        let Some(browser) = browser.filter(|b| b.alive.load(Ordering::Acquire)) else {
            return Err(BrowserError::Gone(
                "No page is open in the headless browser (it may have exited or been closed \
                 when idle). Call browser_navigate to start a new page."
                    .to_string(),
            ));
        };
        let pg = lock(&browser.pages).get(key).cloned();
        match pg {
            Some(pg) => Ok((browser, pg)),
            None => Err(BrowserError::Gone(
                "No page is open (it was closed, or reaped after 10 idle minutes). Call \
                 browser_navigate to start a new page."
                    .to_string(),
            )),
        }
    }

    /// Drops a dead page (and the browser, if it died) from the maps.
    fn forget(&self, browser: &Arc<Browser>, key: &str) {
        browser.drop_page(key);
        if !browser.alive.load(Ordering::Acquire) {
            let mut slot = lock(&self.browser);
            if slot.as_ref().is_some_and(|b| Arc::ptr_eq(b, browser)) {
                *slot = None;
            }
        }
    }

    async fn open(
        &self,
        state: &Arc<AppState>,
        key: &str,
        url: &TargetUrl,
        width: u32,
        height: u32,
    ) -> Result<ActionOut, BrowserError> {
        let (width, height) = (width.clamp(200, 4096), height.clamp(200, 4096));
        let (browser, pg, existed) = {
            let _launch = self.launching.lock().await;
            let browser = self.live_or_launch(state).await?;
            let existing = lock(&browser.pages).get(key).cloned();
            match existing {
                Some(pg) => (browser, pg, true),
                None => {
                    if lock(&browser.pages).len() >= MAX_PAGES {
                        return Err(BrowserError::Refused(format!(
                            "The headless browser already has {MAX_PAGES} agent pages open. \
                             Close one with browser_close, or wait: pages idle for 10 minutes \
                             are closed automatically."
                        )));
                    }
                    let pg = browser.create_page(state, width, height).await?;
                    lock(&browser.pages).insert(key.to_string(), Arc::clone(&pg));
                    (browser, pg, false)
                }
            }
        };
        let _op = pg.op.lock().await;
        pg.touch();
        let out = async {
            pg.alive(&browser)?;
            if existed {
                pg.set_viewport(&browser.conn, width, height).await?;
            }
            pg.navigate(&browser.conn, &url.url()).await
        }
        .await;
        pg.touch();
        if matches!(out, Err(BrowserError::Gone(_))) {
            self.forget(&browser, key);
        }
        out
    }

    /// The running browser, or a fresh one. Caller holds `launching`.
    async fn live_or_launch(&self, state: &Arc<AppState>) -> Result<Arc<Browser>, BrowserError> {
        let current = lock(&self.browser).clone();
        if let Some(b) = current {
            if b.alive.load(Ordering::Acquire) {
                return Ok(b);
            }
            *lock(&self.browser) = None;
        }
        // Never launch on an unasked daemon that may be a login node.
        if state.compute.login_node_unknown() {
            let _ = state.compute.detection().await;
        }
        let exe = self.check(state).map_err(Unavailable::error)?;
        let browser = Browser::launch(&exe).await?;
        *lock(&self.browser) = Some(Arc::clone(&browser));
        Ok(browser)
    }

    /// Close targets idle ≥ 10 min, and the browser itself when it has no
    /// targets left (call every minute from the core's reaper).
    pub(crate) async fn reap_idle(&self) {
        let _launch = self.launching.lock().await;
        let Some(browser) = lock(&self.browser).clone() else {
            return;
        };
        if !browser.alive.load(Ordering::Acquire) {
            *lock(&self.browser) = None;
            return;
        }
        let idle: Vec<(String, Arc<Page>)> = lock(&browser.pages)
            .iter()
            .filter(|(_, p)| p.idle_for() >= IDLE)
            .map(|(k, p)| (k.clone(), Arc::clone(p)))
            .collect();
        for (key, pg) in idle {
            // An action in progress keeps its page.
            let Ok(_op) = pg.op.try_lock() else { continue };
            if pg.idle_for() < IDLE {
                continue;
            }
            tracing::info!(page = %key, "agent browser: closing an idle page");
            browser.close_page(&key, &pg).await;
        }
        if lock(&browser.pages).is_empty() {
            *lock(&self.browser) = None;
            tracing::info!("agent browser: no pages left, closing the headless browser");
            browser.close().await;
        }
    }

    /// Kill the browser's process group now (daemon shutdown). Sync, never
    /// blocks long.
    pub(crate) fn shutdown(&self) {
        let browser = lock(&self.browser).take();
        if let Some(b) = browser {
            b.kill();
            // The exit watcher removes the profile too, if the runtime lives
            // long enough; this covers a daemon that exits right away. A
            // dying renderer may still write for a moment: retry briefly
            // (≤ 100 ms in all).
            for _ in 0..5 {
                match std::fs::remove_dir_all(&b.profile) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    _ => break,
                }
            }
        }
    }
}

// --- discovery -------------------------------------------------------------------

/// The browser to launch: `setting` (`browser.path`), else PATH, else (macOS)
/// an app bundle. Errors are model-facing and name the fix.
fn find_browser(
    setting: Option<&str>,
    path_var: &std::ffi::OsStr,
    home: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(raw) = setting {
        let expanded = match (raw.strip_prefix("~/"), home) {
            (Some(rest), Some(home)) => home.join(rest),
            _ => PathBuf::from(raw),
        };
        let candidate = if !raw.contains('/') {
            std::env::split_paths(path_var)
                .map(|d| d.join(raw))
                .find(|p| is_executable(p))
        } else {
            resolve_app_bundle(&expanded).filter(|p| is_executable(p))
        };
        return candidate.ok_or_else(|| {
            format!(
                "The browser.path setting is `{raw}`, but no executable is there. Point it at a \
                 Chrome or Chromium executable on this host, or clear it to use a system install."
            )
        });
    }
    for dir in std::env::split_paths(path_var) {
        for name in PATH_NAMES {
            let p = dir.join(name);
            if is_executable(&p) {
                return Ok(p);
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        let mut roots = vec![PathBuf::from("/Applications")];
        if let Some(home) = home {
            roots.push(home.join("Applications"));
        }
        for root in roots {
            for bundle in MAC_BUNDLES {
                let p = root.join(bundle);
                if is_executable(&p) {
                    return Ok(p);
                }
            }
        }
    }
    let host = hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "this host".to_string());
    Err(format!(
        "No Chrome or Chromium on {host}: looked for {} on PATH{}. Install one there (a \
         user-space Chromium or chrome-headless-shell works) and set browser.path in Chimaera's \
         settings to its executable, or connect the Chimaera app.",
        PATH_NAMES.join(", "),
        if cfg!(target_os = "macos") {
            " and in /Applications"
        } else {
            ""
        }
    ))
}

/// A `.app` bundle path → its main executable; anything else unchanged.
fn resolve_app_bundle(p: &Path) -> Option<PathBuf> {
    if p.extension().is_some_and(|e| e == "app") && p.is_dir() {
        let macos = p.join("Contents/MacOS");
        let stem = p.file_stem()?.to_string_lossy().into_owned();
        let named = macos.join(&stem);
        if named.is_file() {
            return Some(named);
        }
        return std::fs::read_dir(&macos)
            .ok()?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| p.is_file());
    }
    Some(p.to_path_buf())
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Refuses a profile parent on a network filesystem: Chrome's profile is a
/// set of SQLite databases, and SQLite on NFS/Lustre/GPFS corrupts or hangs.
#[cfg(target_os = "linux")]
fn profile_fs_ok(dir: &Path) -> Result<(), String> {
    let Ok(st) = rustix::fs::statfs(dir) else {
        return Ok(());
    };
    let magic = (st.f_type as u64) & 0xffff_ffff;
    match network_fs_name(magic) {
        None => Ok(()),
        Some(name) => Err(format!(
            "The browser profile would live in {} on {name}, a network filesystem; its \
             databases are unsafe there. Point CHIMAERA_RUNTIME_DIR or XDG_RUNTIME_DIR at a \
             node-local directory (for example /tmp or the job's local scratch) and restart \
             the daemon.",
            dir.display()
        )),
    }
}

#[cfg(not(target_os = "linux"))]
fn profile_fs_ok(_dir: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn network_fs_name(magic: u64) -> Option<&'static str> {
    Some(match magic {
        0x6969 => "NFS",
        0x0BD0_0BD0 => "Lustre",
        0x4750_4653 => "GPFS",
        0xFF53_4D42 => "CIFS",
        0xFE53_4D42 => "SMB",
        0x1983_0326 => "BeeGFS",
        0x00C3_6400 => "CephFS",
        0xAAD7_AAEA => "PanFS",
        _ => return None,
    })
}

// --- the browser process -------------------------------------------------------

struct Browser {
    conn: Arc<Conn>,
    /// The browser's pid, which is also its process group.
    pgid: i32,
    profile: PathBuf,
    /// False once the process exited or the pipe closed.
    alive: Arc<AtomicBool>,
    exited: watch::Receiver<bool>,
    stderr: Arc<Mutex<VecDeque<u8>>>,
    /// By page key.
    pages: Mutex<HashMap<String, Arc<Page>>>,
    /// By CDP session id: where the reader routes a page's events.
    sessions: Arc<Mutex<HashMap<String, Arc<PageShared>>>>,
    /// The launch's own about:blank tab(s), closed once a page exists.
    initial: Mutex<Vec<String>>,
}

impl Browser {
    async fn launch(exe: &Path) -> Result<Arc<Browser>, BrowserError> {
        let profile = tokio::task::spawn_blocking(make_profile)
            .await
            .map_err(|e| BrowserError::Engine(format!("profile setup failed: {e}")))?
            .map_err(BrowserError::Engine)?;
        match Self::spawn(exe, &profile).await {
            Ok(b) => Ok(b),
            Err(e) => {
                let p = profile.clone();
                let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(p)).await;
                Err(e)
            }
        }
    }

    async fn spawn(exe: &Path, profile: &Path) -> Result<Arc<Browser>, BrowserError> {
        let io = |what: &str, e: std::io::Error| {
            BrowserError::Engine(format!(
                "Couldn't start the headless browser ({what}: {e})."
            ))
        };
        // fd 3 in the child reads what we write; fd 4 in the child writes
        // what we read.
        let (child_in, our_out) = cloexec_pipe().map_err(|e| io("pipe", e))?;
        let (our_in, child_out) = cloexec_pipe().map_err(|e| io("pipe", e))?;
        let mut cmd = tokio::process::Command::new(exe);
        cmd.args(launch_args(profile))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        let (r, w) = (child_in.as_raw_fd(), child_out.as_raw_fd());
        // SAFETY: only async-signal-safe libc calls between fork and exec.
        // Both fds are first copied above 10 so the dup2 onto 3 and 4 can't
        // clobber one another; dup2 clears close-on-exec on 3 and 4 only.
        unsafe {
            cmd.pre_exec(move || {
                use nix::libc;
                let a = libc::fcntl(r, libc::F_DUPFD_CLOEXEC, 10);
                let b = libc::fcntl(w, libc::F_DUPFD_CLOEXEC, 10);
                if a < 0 || b < 0 || libc::dup2(a, 3) < 0 || libc::dup2(b, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn().map_err(|e| {
            BrowserError::Engine(format!(
                "Couldn't start {} ({e}). Check that browser.path points at a working Chrome or \
                 Chromium executable.",
                exe.display()
            ))
        })?;
        drop(child_in);
        drop(child_out);
        let pgid = child.id().map(|p| p as i32).unwrap_or(0);

        let stderr = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL)));
        if let Some(mut err) = child.stderr.take() {
            let tail = Arc::clone(&stderr);
            tokio::spawn(async move {
                let mut chunk = [0u8; 4096];
                while let Ok(n) = err.read(&mut chunk).await {
                    if n == 0 {
                        break;
                    }
                    let mut t = lock(&tail);
                    t.extend(&chunk[..n]);
                    let over = t.len().saturating_sub(STDERR_TAIL);
                    t.drain(..over);
                }
            });
        }

        let alive = Arc::new(AtomicBool::new(true));
        let (exited_tx, exited) = watch::channel(false);
        {
            let alive = Arc::clone(&alive);
            let profile = profile.to_path_buf();
            tokio::spawn(async move {
                let status = child.wait().await;
                alive.store(false, Ordering::Release);
                // Renderers left behind by a crashed browser. The group id
                // can't be reused while any member lives, so this reaches
                // only leftovers (or nothing).
                if pgid > 0 {
                    let _ = nix::sys::signal::killpg(
                        nix::unistd::Pid::from_raw(pgid),
                        nix::sys::signal::Signal::SIGKILL,
                    );
                }
                let _ = exited_tx.send(true);
                let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(profile)).await;
                tracing::info!(?status, "agent browser: headless browser exited");
            });
        }

        let reader =
            tokio::net::unix::pipe::Receiver::from_owned_fd(our_in).map_err(|e| io("pipe", e))?;
        let writer =
            tokio::net::unix::pipe::Sender::from_owned_fd(our_out).map_err(|e| io("pipe", e))?;
        let sessions: Arc<Mutex<HashMap<String, Arc<PageShared>>>> = Arc::default();
        let conn = {
            let route = Arc::clone(&sessions);
            let on_close = Arc::clone(&sessions);
            let alive = Arc::clone(&alive);
            Conn::start(
                reader,
                writer,
                move |ev| route_event(&route, ev),
                move |_why| {
                    alive.store(false, Ordering::Release);
                    let all: Vec<Arc<PageShared>> = lock(&on_close).values().cloned().collect();
                    for shared in all {
                        shared.update(|st| st.detached = true);
                    }
                },
            )
        };
        let browser = Arc::new(Browser {
            conn,
            pgid,
            profile: profile.to_path_buf(),
            alive,
            exited,
            stderr,
            pages: Mutex::new(HashMap::new()),
            sessions,
            initial: Mutex::new(Vec::new()),
        });

        if let Err(e) = browser
            .conn
            .call("Browser.getVersion", json!({}), None, LAUNCH)
            .await
        {
            // Give a crashing browser a moment to finish its stderr.
            let mut exited = browser.exited.clone();
            let _ = tokio::time::timeout(Duration::from_secs(2), exited.wait_for(|e| *e)).await;
            browser.kill();
            let tail = browser.stderr_tail();
            tracing::warn!(error = %e, stderr = %tail, "agent browser: startup failed");
            return Err(BrowserError::Engine(explain_startup(exe, &tail, &e)));
        }
        let _ = browser
            .conn
            .call(
                "Browser.setDownloadBehavior",
                json!({"behavior": "deny"}),
                None,
                CALL,
            )
            .await;
        if let Ok(targets) = browser
            .conn
            .call("Target.getTargets", json!({}), None, CALL)
            .await
        {
            let ids = targets["targetInfos"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|t| t["type"] == "page")
                .filter_map(|t| t["targetId"].as_str().map(str::to_string))
                .collect();
            *lock(&browser.initial) = ids;
        }
        tracing::info!(exe = %exe.display(), pgid, "agent browser: headless browser started");
        Ok(browser)
    }

    fn stderr_tail(&self) -> String {
        let t = lock(&self.stderr);
        let (a, b) = t.as_slices();
        let mut bytes = Vec::with_capacity(a.len() + b.len());
        bytes.extend_from_slice(a);
        bytes.extend_from_slice(b);
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// SIGKILL the whole group, now.
    fn kill(&self) {
        if self.pgid > 0 && !*self.exited.borrow() {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(self.pgid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        self.alive.store(false, Ordering::Release);
    }

    /// Ask the browser to quit, then kill the group if it hasn't.
    async fn close(&self) {
        let _ = self
            .conn
            .call("Browser.close", json!({}), None, Duration::from_secs(3))
            .await;
        let mut exited = self.exited.clone();
        if tokio::time::timeout(Duration::from_secs(3), exited.wait_for(|e| *e))
            .await
            .is_err()
        {
            self.kill();
        }
    }

    async fn create_page(
        &self,
        state: &Arc<AppState>,
        width: u32,
        height: u32,
    ) -> Result<Arc<Page>, BrowserError> {
        let created = self
            .conn
            .call(
                "Target.createTarget",
                // The viewport is set by emulation: Chrome takes a size here
                // only for new windows.
                json!({"url": "about:blank"}),
                None,
                CALL,
            )
            .await
            .map_err(engine)?;
        let target_id = created["targetId"]
            .as_str()
            .ok_or_else(|| BrowserError::Engine("createTarget gave no targetId".into()))?
            .to_string();
        match self.attach(state, &target_id, width, height).await {
            Ok(pg) => {
                let initial = std::mem::take(&mut *lock(&self.initial));
                for id in initial {
                    self.conn
                        .fire("Target.closeTarget", json!({"targetId": id}), None);
                }
                Ok(pg)
            }
            Err(e) => {
                self.conn
                    .fire("Target.closeTarget", json!({"targetId": target_id}), None);
                Err(e)
            }
        }
    }

    async fn attach(
        &self,
        state: &Arc<AppState>,
        target_id: &str,
        width: u32,
        height: u32,
    ) -> Result<Arc<Page>, BrowserError> {
        let attached = self
            .conn
            .call(
                "Target.attachToTarget",
                json!({"targetId": target_id, "flatten": true}),
                None,
                CALL,
            )
            .await
            .map_err(engine)?;
        let session = attached["sessionId"]
            .as_str()
            .ok_or_else(|| BrowserError::Engine("attachToTarget gave no sessionId".into()))?
            .to_string();
        let (to_task, rx) = mpsc::unbounded_channel();
        let shared = Arc::new(PageShared::new(target_id.to_string(), to_task));
        lock(&self.sessions).insert(session.clone(), Arc::clone(&shared));
        let task = tokio::spawn(page_task(
            Arc::clone(&self.conn),
            session.clone(),
            Arc::clone(&shared),
            Arc::downgrade(state),
            rx,
        ));
        let pg = Arc::new(Page {
            target_id: target_id.to_string(),
            session: session.clone(),
            shared,
            op: tokio::sync::Mutex::new(()),
            last_used: Mutex::new(Instant::now()),
            mouse: Mutex::new((0.0, 0.0)),
            task,
        });
        let setup = [
            ("Page.enable", json!({})),
            // targetCrashed / detached.
            ("Inspector.enable", json!({})),
            ("Runtime.enable", json!({})),
            (
                "Network.enable",
                // We never read bodies: keep the renderer's buffers small.
                json!({"maxTotalBufferSize": 2_000_000, "maxResourceBufferSize": 1_000_000}),
            ),
            (
                "Fetch.enable",
                json!({"patterns": [{"urlPattern": "*", "resourceType": "Document", "requestStage": "Request"}]}),
            ),
        ];
        for (method, params) in setup {
            if let Err(e) = self.conn.call(method, params, Some(&session), CALL).await {
                lock(&self.sessions).remove(&session);
                return Err(engine(e));
            }
        }
        if let Err(e) = pg.set_viewport(&self.conn, width, height).await {
            lock(&self.sessions).remove(&session);
            return Err(e);
        }
        Ok(pg)
    }

    async fn close_page(&self, key: &str, pg: &Page) {
        let _ = self
            .conn
            .call(
                "Target.closeTarget",
                json!({"targetId": pg.target_id}),
                None,
                Duration::from_secs(5),
            )
            .await;
        self.drop_page(key);
    }

    fn drop_page(&self, key: &str) {
        let removed = lock(&self.pages).remove(key);
        if let Some(pg) = removed {
            lock(&self.sessions).remove(&pg.session);
        }
    }
}

fn launch_args(profile: &Path) -> Vec<String> {
    let mut args: Vec<String> = [
        "--headless=new",
        "--remote-debugging-pipe",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-background-networking",
        "--disable-sync",
        "--disable-component-update",
        "--disable-default-apps",
        "--disable-extensions",
        "--mute-audio",
        "--hide-scrollbars",
        // Never touch the user's keychain or keyring for the throwaway
        // profile (macOS would prompt).
        "--password-store=basic",
        "--use-mock-keychain",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.push(format!("--user-data-dir={}", profile.display()));
    args.push("about:blank".to_string());
    args
}

/// A fresh 0700 profile dir under the node-local runtime dir.
fn make_profile() -> Result<PathBuf, String> {
    use std::hash::{BuildHasher, Hasher};
    use std::os::unix::fs::DirBuilderExt;
    let base = chimaera_core::runtime_dir();
    profile_fs_ok(&base)?;
    for _ in 0..8 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        );
        h.write_u32(std::process::id());
        let dir = base.join(format!("browser-{:016x}", h.finish()));
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(format!(
                    "Couldn't create the browser profile under {}: {e}",
                    base.display()
                ))
            }
        }
    }
    Err("Couldn't pick a free browser profile name.".to_string())
}

fn cloexec_pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC).map_err(std::io::Error::from)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        use nix::fcntl::{fcntl, FcntlArg, FdFlag};
        let (r, w) = nix::unistd::pipe().map_err(std::io::Error::from)?;
        for fd in [&r, &w] {
            fcntl(fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).map_err(std::io::Error::from)?;
        }
        Ok((r, w))
    }
}

/// A startup failure, explained from the browser's stderr: the known
/// causes and their fixes, never a silent workaround (no `--no-sandbox`).
fn explain_startup(exe: &Path, stderr: &str, err: &CdpError) -> String {
    let lower = stderr.to_ascii_lowercase();
    let mut out = format!("The headless browser ({}) didn't start", exe.display());
    out.push_str(match err {
        CdpError::Timeout => " within 20 s.",
        _ => ": it exited.",
    });
    if lower.contains("no usable sandbox")
        || lower.contains("setuid sandbox")
        || lower.contains("user namespace")
        || lower.contains("clone(clone_newuser")
    {
        out.push_str(
            " Chromium couldn't set up its sandbox: this host allows neither unprivileged user \
             namespaces nor a setuid chrome-sandbox helper. Ask the administrators to enable \
             user namespaces (user.max_user_namespaces / kernel.unprivileged_userns_clone), or \
             install a Chrome build whose chrome-sandbox is setuid root. Chimaera does not turn \
             the sandbox off.",
        );
    } else if lower.contains("/dev/shm") || lower.contains("shared memory") {
        out.push_str(
            " Chromium couldn't use shared memory: /dev/shm is missing or too small on this \
             host (common in containers). Give the container a larger /dev/shm (for example \
             --shm-size=1g) or ask the administrators to mount one.",
        );
    } else if let Some(lib) = missing_library(stderr) {
        out.push_str(&format!(
            " A shared library it needs is missing: {lib}. Install it (or load the module that \
             provides it), or use chrome-headless-shell, which needs fewer system libraries, and \
             point browser.path at it."
        ));
    } else if lower.contains("cannot open display") || lower.contains("missing x server") {
        out.push_str(
            " It tried to open a display: this browser build may not support --headless=new. \
             Use a current Chrome, Chromium or chrome-headless-shell.",
        );
    }
    let tail = last_lines(stderr, 6, 1200);
    if !tail.is_empty() {
        out.push_str("\nIts last output:\n");
        out.push_str(&tail);
    }
    out
}

fn missing_library(stderr: &str) -> Option<String> {
    let at = stderr.find("error while loading shared libraries: ")?;
    let rest = &stderr[at + "error while loading shared libraries: ".len()..];
    let lib = rest.split([':', '\n']).next()?.trim();
    (!lib.is_empty()).then(|| lib.to_string())
}

fn last_lines(s: &str, n: usize, max: usize) -> String {
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(n);
    let joined = lines[start..].join("\n");
    let skip = joined.len().saturating_sub(max);
    let mut cut = skip;
    while !joined.is_char_boundary(cut) {
        cut += 1;
    }
    joined[cut..].to_string()
}

fn engine(e: CdpError) -> BrowserError {
    match e {
        CdpError::Gone(why) => BrowserError::Gone(format!(
            "The headless browser exited ({why}); this page is gone. Call browser_navigate to \
             start a new page."
        )),
        CdpError::Timeout => {
            BrowserError::Timeout("The headless browser didn't answer in time.".to_string())
        }
        CdpError::Protocol { message, .. } => {
            BrowserError::Engine(format!("The headless browser refused: {message}"))
        }
    }
}

// --- one page --------------------------------------------------------------------

struct Page {
    target_id: String,
    session: String,
    shared: Arc<PageShared>,
    /// One action at a time per page.
    op: tokio::sync::Mutex<()>,
    last_used: Mutex<Instant>,
    /// Where the mouse is (drags and moves start here).
    mouse: Mutex<(f64, f64)>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Page {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Counters before an input action, to tell what it started.
#[derive(Clone, Copy)]
struct Mark {
    nav: u64,
    load: u64,
    blocked: u64,
}

impl Page {
    fn touch(&self) {
        *lock(&self.last_used) = Instant::now();
    }

    fn idle_for(&self) -> Duration {
        lock(&self.last_used).elapsed()
    }

    fn alive(&self, browser: &Browser) -> Result<(), BrowserError> {
        if !browser.alive.load(Ordering::Acquire) {
            return Err(BrowserError::Gone(
                "The headless browser exited; this page is gone. Call browser_navigate to start \
                 a new page."
                    .to_string(),
            ));
        }
        let st = lock(&self.shared.st);
        if st.crashed {
            return Err(BrowserError::Gone(
                "The page crashed (its renderer died). Call browser_navigate to start a new page."
                    .to_string(),
            ));
        }
        if st.detached {
            return Err(BrowserError::Gone(
                "The page was closed. Call browser_navigate to start a new page.".to_string(),
            ));
        }
        Ok(())
    }

    async fn call(&self, conn: &Conn, method: &str, params: Value) -> Result<Value, BrowserError> {
        conn.call(method, params, Some(&self.session), CALL)
            .await
            .map_err(engine)
    }

    async fn set_viewport(&self, conn: &Conn, width: u32, height: u32) -> Result<(), BrowserError> {
        self.call(
            conn,
            "Emulation.setDeviceMetricsOverride",
            json!({"width": width, "height": height, "deviceScaleFactor": 1, "mobile": false}),
        )
        .await
        .map(|_| ())
    }

    fn mark(&self) -> Mark {
        let st = lock(&self.shared.st);
        Mark {
            nav: st.nav_seq,
            load: st.load_seq,
            blocked: st.blocked_seq,
        }
    }

    // -- navigation --

    async fn navigate(&self, conn: &Conn, url: &str) -> Result<ActionOut, BrowserError> {
        let mark = self.mark();
        let deadline = Instant::now() + LOAD;
        let answer = conn
            .call(
                "Page.navigate",
                json!({"url": url}),
                Some(&self.session),
                LOAD,
            )
            .await;
        let answer = match answer {
            Ok(a) => a,
            Err(CdpError::Timeout) => Value::Null,
            Err(e) => return Err(engine(e)),
        };
        if let Some(err) = answer["errorText"].as_str().filter(|s| !s.is_empty()) {
            if let Some(why) = self.blocked_since(mark) {
                return Err(BrowserError::Refused(why));
            }
            return Err(BrowserError::Page(nav_error(url, err)));
        }
        self.settle(conn, mark, deadline).await
    }

    async fn history(&self, conn: &Conn, op: HistoryOp) -> Result<ActionOut, BrowserError> {
        let mark = self.mark();
        let deadline = Instant::now() + LOAD;
        match op {
            HistoryOp::Reload => {
                self.call(conn, "Page.reload", json!({})).await?;
            }
            HistoryOp::Stop => {
                self.call(conn, "Page.stopLoading", json!({})).await?;
                return self.page_value(conn, false).await;
            }
            HistoryOp::Back | HistoryOp::Forward => {
                let h = self
                    .call(conn, "Page.getNavigationHistory", json!({}))
                    .await?;
                let current = h["currentIndex"].as_i64().unwrap_or(0);
                let want = if op == HistoryOp::Back {
                    current - 1
                } else {
                    current + 1
                };
                let entry = h["entries"]
                    .as_array()
                    .and_then(|e| usize::try_from(want).ok().and_then(|i| e.get(i)));
                let Some(id) = entry.and_then(|e| e["id"].as_i64()) else {
                    return Err(BrowserError::Page(format!(
                        "There is no page to go {} to.",
                        if op == HistoryOp::Back {
                            "back"
                        } else {
                            "forward"
                        }
                    )));
                };
                self.call(conn, "Page.navigateToHistoryEntry", json!({"entryId": id}))
                    .await?;
            }
        }
        self.settle(conn, mark, deadline).await
    }

    /// Waits until a navigation since `mark` committed and the main frame
    /// stopped loading (or it was blocked, or the page died), then answers
    /// the page's URL, title and status.
    async fn settle(
        &self,
        conn: &Conn,
        mark: Mark,
        deadline: Instant,
    ) -> Result<ActionOut, BrowserError> {
        let settled = self
            .shared
            .wait(deadline, |st| {
                st.crashed
                    || st.detached
                    || st.blocked_seq > mark.blocked
                    || (st.nav_seq > mark.nav && !st.loading)
            })
            .await;
        if let Some(why) = self.blocked_since(mark) {
            return Err(BrowserError::Refused(why));
        }
        {
            let st = lock(&self.shared.st);
            if st.crashed {
                return Err(BrowserError::Gone(
                    "The page crashed while loading. Call browser_navigate to start a new page."
                        .to_string(),
                ));
            }
        }
        self.page_value(conn, !settled).await
    }

    /// The block reported since `mark` (marking it reported).
    fn blocked_since(&self, mark: Mark) -> Option<String> {
        let mut st = lock(&self.shared.st);
        if st.blocked_seq > mark.blocked {
            st.blocked_reported = st.blocked_seq;
            return st.blocked.clone();
        }
        None
    }

    async fn page_value(&self, conn: &Conn, loading: bool) -> Result<ActionOut, BrowserError> {
        let (mut url, status) = {
            let st = lock(&self.shared.st);
            (st.url.clone(), st.doc_status)
        };
        let mut title = String::new();
        if let Ok(info) = conn
            .call(
                "Target.getTargetInfo",
                json!({"targetId": self.target_id}),
                None,
                CALL,
            )
            .await
        {
            title = info["targetInfo"]["title"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if url.is_empty() {
                url = info["targetInfo"]["url"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
            }
        }
        if title == url {
            title.clear();
        }
        let mut out = json!({"url": url, "title": title, "status": status});
        if loading {
            out["loading"] = Value::Bool(true);
        }
        Ok(ActionOut::Value(out))
    }

    /// After a click/key/text: if it started a navigation, wait for that to
    /// settle so the next read sees the new page; report a block.
    async fn after_input(&self, mark: Mark) -> Result<ActionOut, BrowserError> {
        let started = self
            .shared
            .wait(Instant::now() + NAV_START_GRACE, |st| {
                st.load_seq > mark.load || st.nav_seq > mark.nav || st.blocked_seq > mark.blocked
            })
            .await;
        if started {
            self.shared
                .wait(Instant::now() + NAV_SETTLE, |st| {
                    st.crashed
                        || st.detached
                        || st.blocked_seq > mark.blocked
                        || ((st.nav_seq > mark.nav || st.load_seq > mark.load) && !st.loading)
                })
                .await;
        }
        let mut st = lock(&self.shared.st);
        if st.blocked_seq > st.blocked_reported {
            st.blocked_reported = st.blocked_seq;
            if let Some(why) = st.blocked.clone() {
                return Ok(ActionOut::Value(json!({"blocked": why})));
            }
        }
        Ok(ActionOut::Done)
    }

    // -- eval --

    async fn eval(
        &self,
        conn: &Conn,
        world: World,
        body: &str,
        args: &Value,
        timeout_ms: u64,
    ) -> Result<Value, BrowserError> {
        let expression = eval_expression(body, args)?;
        let timeout = Duration::from_millis(timeout_ms.clamp(100, 120_000));
        for attempt in 0..2 {
            let mut params = json!({
                "expression": expression,
                "awaitPromise": true,
                "returnByValue": true,
                "userGesture": true,
            });
            if world == World::Isolated {
                params["contextId"] = json!(self.isolated_context(conn).await?);
            }
            let answer = conn
                .call(
                    "Runtime.evaluate",
                    params,
                    Some(&self.session),
                    timeout + Duration::from_secs(2),
                )
                .await;
            match answer {
                Ok(a) => return eval_result(&a),
                Err(CdpError::Timeout) => {
                    return Err(BrowserError::Timeout(format!(
                        "The script didn't finish within {} ms.",
                        timeout.as_millis()
                    )))
                }
                Err(CdpError::Protocol { message, .. })
                    if message.contains("context") || message.contains("Context") =>
                {
                    // The page navigated under us: the isolated world went
                    // with the old document. Make a new one, once.
                    lock(&self.shared.st).isolated_ctx = None;
                    if attempt == 0 && world == World::Isolated {
                        continue;
                    }
                    return Err(BrowserError::Page(
                        "The page navigated while the script ran; try again.".to_string(),
                    ));
                }
                Err(e) => return Err(engine(e)),
            }
        }
        unreachable!("the loop returns")
    }

    async fn isolated_context(&self, conn: &Conn) -> Result<i64, BrowserError> {
        if let Some(id) = lock(&self.shared.st).isolated_ctx {
            return Ok(id);
        }
        let made = self
            .call(
                conn,
                "Page.createIsolatedWorld",
                json!({"frameId": self.target_id, "worldName": WORLD, "grantUniveralAccess": false}),
            )
            .await?;
        let id = made["executionContextId"]
            .as_i64()
            .ok_or_else(|| BrowserError::Engine("createIsolatedWorld gave no context".into()))?;
        lock(&self.shared.st).isolated_ctx = Some(id);
        Ok(id)
    }

    // -- input --

    async fn mouse(
        &self,
        conn: &Conn,
        kind: &str,
        x: f64,
        y: f64,
        extra: Value,
    ) -> Result<(), BrowserError> {
        let mut params = json!({"type": kind, "x": x, "y": y});
        if let (Some(p), Some(e)) = (params.as_object_mut(), extra.as_object()) {
            p.extend(e.clone());
        }
        self.call(conn, "Input.dispatchMouseEvent", params).await?;
        *lock(&self.mouse) = (x, y);
        Ok(())
    }

    async fn mouse_move(
        &self,
        conn: &Conn,
        x: f64,
        y: f64,
        buttons: u8,
    ) -> Result<(), BrowserError> {
        let extra = if buttons > 0 {
            json!({"button": "left", "buttons": buttons})
        } else {
            json!({})
        };
        self.mouse(conn, "mouseMoved", x, y, extra).await
    }

    async fn click(
        &self,
        conn: &Conn,
        x: f64,
        y: f64,
        clicks: u8,
        modifiers: &[Modifier],
    ) -> Result<(), BrowserError> {
        let m = modifier_bits(modifiers);
        self.mouse(conn, "mouseMoved", x, y, json!({"modifiers": m}))
            .await?;
        for n in 1..=clicks {
            let extra = json!({"button": "left", "buttons": 1, "clickCount": n, "modifiers": m});
            self.mouse(conn, "mousePressed", x, y, extra).await?;
            let extra = json!({"button": "left", "buttons": 0, "clickCount": n, "modifiers": m});
            self.mouse(conn, "mouseReleased", x, y, extra).await?;
        }
        Ok(())
    }

    async fn drag(
        &self,
        conn: &Conn,
        from: (f64, f64),
        to: (f64, f64),
        steps: u16,
    ) -> Result<(), BrowserError> {
        let steps = steps.clamp(1, 200);
        self.mouse_move(conn, from.0, from.1, 0).await?;
        self.mouse(
            conn,
            "mousePressed",
            from.0,
            from.1,
            json!({"button": "left", "buttons": 1, "clickCount": 1}),
        )
        .await?;
        for i in 1..=steps {
            let t = f64::from(i) / f64::from(steps);
            let (x, y) = (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
            tokio::time::sleep(Duration::from_millis(16)).await;
            self.mouse_move(conn, x, y, 1).await?;
        }
        self.mouse(
            conn,
            "mouseReleased",
            to.0,
            to.1,
            json!({"button": "left", "buttons": 0, "clickCount": 1}),
        )
        .await
    }

    async fn scroll(
        &self,
        conn: &Conn,
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
    ) -> Result<(), BrowserError> {
        self.mouse(
            conn,
            "mouseWheel",
            x,
            y,
            json!({"deltaX": dx, "deltaY": dy}),
        )
        .await
    }

    async fn key(
        &self,
        conn: &Conn,
        key: &str,
        modifiers: &[Modifier],
    ) -> Result<(), BrowserError> {
        let def = key_def(key, modifiers.contains(&Modifier::Shift)).ok_or_else(|| {
            BrowserError::Page(format!(
                "Unknown key `{key}`: use one character or Enter, Tab, Escape, Backspace, \
                 Delete, ArrowUp, ArrowDown, ArrowLeft, ArrowRight, Home, End, PageUp, PageDown, \
                 Space, Insert or F1–F12."
            ))
        })?;
        for params in key_events(&def, modifiers) {
            self.call(conn, "Input.dispatchKeyEvent", params).await?;
        }
        Ok(())
    }

    async fn text(&self, conn: &Conn, text: &str, per_key: bool) -> Result<(), BrowserError> {
        if !per_key {
            self.call(conn, "Input.insertText", json!({"text": text}))
                .await?;
            return Ok(());
        }
        if text.chars().count() > PER_KEY_MAX {
            return Err(BrowserError::Refused(format!(
                "That's over {PER_KEY_MAX} characters to type key by key; send it without \
                 per_key (inserted at once)."
            )));
        }
        for ch in text.chars() {
            let named = match ch {
                '\n' | '\r' => Some("Enter"),
                '\t' => Some("Tab"),
                _ => None,
            };
            let def = match named {
                Some(n) => key_def(n, false),
                None if ch.is_ascii() && !ch.is_ascii_control() => key_def(&ch.to_string(), false),
                None => None,
            };
            match def {
                Some(def) => {
                    for params in key_events(&def, &[]) {
                        self.call(conn, "Input.dispatchKeyEvent", params).await?;
                    }
                }
                None => {
                    self.call(conn, "Input.insertText", json!({"text": ch.to_string()}))
                        .await?;
                }
            }
        }
        Ok(())
    }

    // -- screenshot --

    async fn screenshot(
        &self,
        conn: &Conn,
        clip: Option<Rect>,
        full_page: bool,
        max_width: u32,
    ) -> Result<ActionOut, BrowserError> {
        let m = self.call(conn, "Page.getLayoutMetrics", json!({})).await?;
        let vv = if m["cssVisualViewport"].is_object() {
            &m["cssVisualViewport"]
        } else {
            &m["visualViewport"]
        };
        let content = if m["cssContentSize"].is_object() {
            &m["cssContentSize"]
        } else {
            &m["contentSize"]
        };
        let num = |v: &Value| v.as_f64().unwrap_or(0.0);
        let (page_x, page_y) = (num(&vv["pageX"]), num(&vv["pageY"]));
        let (vw, vh) = (num(&vv["clientWidth"]), num(&vv["clientHeight"]));
        let (x, y, w, h, beyond) = if full_page {
            let w = num(&content["width"]).max(vw);
            let h = num(&content["height"]).max(vh);
            (0.0, 0.0, w, h, true)
        } else if let Some(r) = clip {
            let beyond = r.x < 0.0 || r.y < 0.0 || r.x + r.width > vw || r.y + r.height > vh;
            (
                (page_x + r.x).max(0.0),
                (page_y + r.y).max(0.0),
                r.width,
                r.height,
                beyond,
            )
        } else {
            (page_x, page_y, vw, vh, false)
        };
        let (w, h) = (w.clamp(1.0, SHOT_MAX_SIDE), h.clamp(1.0, SHOT_MAX_SIDE));
        let scale = if max_width > 0 && w > f64::from(max_width) {
            f64::from(max_width) / w
        } else {
            1.0
        };
        let shot = conn
            .call(
                "Page.captureScreenshot",
                json!({
                    "format": "png",
                    "clip": {"x": x, "y": y, "width": w, "height": h, "scale": scale},
                    "captureBeyondViewport": beyond,
                    "fromSurface": true,
                }),
                Some(&self.session),
                Duration::from_secs(30),
            )
            .await
            .map_err(engine)?;
        let b64 = shot["data"]
            .as_str()
            .ok_or_else(|| BrowserError::Engine("captureScreenshot gave no data".into()))?
            .to_string();
        let (width, height) =
            png_size(&b64).unwrap_or(((w * scale).round() as u32, (h * scale).round() as u32));
        Ok(ActionOut::Png { b64, width, height })
    }

    fn rings(&self, console_since: u64, network_since: u64) -> RingsOut {
        let st = lock(&self.shared.st);
        RingsOut {
            console: st.console.since(console_since),
            network: st.network.since(network_since),
            inflight: u32::try_from(st.requests.len()).unwrap_or(u32::MAX),
            console_next: st.console.next,
            network_next: st.network.next,
        }
    }
}

fn nav_error(url: &str, err: &str) -> String {
    let hint = if err.contains("ERR_CONNECTION_REFUSED") {
        " (nothing is listening there: is the server running, and on that port?)"
    } else if err.contains("ERR_NAME_NOT_RESOLVED") {
        " (the host name doesn't resolve from the daemon's host)"
    } else if err.contains("ERR_ABORTED") {
        " (the navigation was cancelled, for example by a download or another navigation)"
    } else {
        ""
    };
    format!("Couldn't load {url}: {err}{hint}.")
}

/// Wraps an async function body so CDP awaits it with `args` bound as
/// named parameters (as `callAsyncJavaScript` does on the app engine) and
/// returns its value as a JSON string.
fn eval_expression(body: &str, args: &Value) -> Result<String, BrowserError> {
    let empty = serde_json::Map::new();
    let map = match args {
        Value::Null => &empty,
        Value::Object(m) => m,
        _ => {
            return Err(BrowserError::Engine(
                "eval args must be a JSON object".to_string(),
            ))
        }
    };
    let mut names = Vec::with_capacity(map.len());
    let mut values = Vec::with_capacity(map.len());
    for (k, v) in map {
        if !is_js_identifier(k) {
            return Err(BrowserError::Engine(format!(
                "eval arg name `{k}` is not a JavaScript identifier"
            )));
        }
        names.push(k.as_str());
        values.push(serde_json::to_string(v).unwrap_or_else(|_| "null".into()));
    }
    Ok(format!(
        "(async function({}) {{\n{body}\n}}).call(globalThis{}{}).then((v) => {{ \
         const s = JSON.stringify(v === undefined ? null : v); return s === undefined ? 'null' : s; }})",
        names.join(", "),
        if values.is_empty() { "" } else { ", " },
        values.join(", "),
    ))
}

fn is_js_identifier(s: &str) -> bool {
    const RESERVED: [&str; 12] = [
        "this",
        "arguments",
        "await",
        "yield",
        "let",
        "var",
        "const",
        "function",
        "return",
        "class",
        "new",
        "delete",
    ];
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_' || first == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        && !RESERVED.contains(&s)
}

fn eval_result(answer: &Value) -> Result<Value, BrowserError> {
    if let Some(ex) = answer.get("exceptionDetails") {
        let text = ex["exception"]["description"]
            .as_str()
            .or_else(|| ex["exception"]["value"].as_str())
            .or_else(|| ex["text"].as_str())
            .unwrap_or("unknown error");
        return Err(BrowserError::Page(format!(
            "The script threw: {}",
            cap(text, 2048)
        )));
    }
    let s = answer["result"]["value"].as_str().unwrap_or("null");
    if s.len() > EVAL_RESULT_MAX {
        return Err(BrowserError::Page(format!(
            "The script's result is over {} MB of JSON; return less.",
            EVAL_RESULT_MAX / (1024 * 1024)
        )));
    }
    serde_json::from_str(s)
        .map_err(|e| BrowserError::Engine(format!("unreadable script result: {e}")))
}

/// CDP's modifier bitmask: Alt 1, Ctrl 2, Meta 4, Shift 8.
fn modifier_bits(mods: &[Modifier]) -> u8 {
    mods.iter().fold(0, |acc, m| {
        acc | match m {
            Modifier::Alt => 1,
            Modifier::Ctrl => 2,
            Modifier::Meta => 4,
            Modifier::Shift => 8,
        }
    })
}

#[derive(Debug, PartialEq)]
struct KeyDef {
    key: String,
    code: String,
    key_code: u32,
    text: Option<String>,
}

/// A named key or one character → its DOM key, code, Windows key code and
/// the text it types (US layout). `shift` upper-cases a letter.
fn key_def(key: &str, shift: bool) -> Option<KeyDef> {
    let named = |key: &str, code: &str, key_code: u32, text: Option<&str>| KeyDef {
        key: key.to_string(),
        code: code.to_string(),
        key_code,
        text: text.map(str::to_string),
    };
    let def = match key {
        "Enter" => named("Enter", "Enter", 13, Some("\r")),
        "Tab" => named("Tab", "Tab", 9, None),
        "Escape" => named("Escape", "Escape", 27, None),
        "Backspace" => named("Backspace", "Backspace", 8, None),
        "Delete" => named("Delete", "Delete", 46, None),
        "Insert" => named("Insert", "Insert", 45, None),
        "ArrowUp" => named("ArrowUp", "ArrowUp", 38, None),
        "ArrowDown" => named("ArrowDown", "ArrowDown", 40, None),
        "ArrowLeft" => named("ArrowLeft", "ArrowLeft", 37, None),
        "ArrowRight" => named("ArrowRight", "ArrowRight", 39, None),
        "Home" => named("Home", "Home", 36, None),
        "End" => named("End", "End", 35, None),
        "PageUp" => named("PageUp", "PageUp", 33, None),
        "PageDown" => named("PageDown", "PageDown", 34, None),
        "Space" | " " => named(" ", "Space", 32, Some(" ")),
        _ => {
            if let Some(n) = key
                .strip_prefix('F')
                .and_then(|n| n.parse::<u32>().ok())
                .filter(|n| (1..=12).contains(n))
            {
                return Some(named(key, key, 111 + n, None));
            }
            let mut chars = key.chars();
            let ch = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            let ch = if shift && ch.is_ascii_lowercase() {
                ch.to_ascii_uppercase()
            } else {
                ch
            };
            let (code, key_code) = char_code(ch);
            KeyDef {
                key: ch.to_string(),
                code,
                key_code,
                text: Some(ch.to_string()),
            }
        }
    };
    Some(def)
}

/// The physical key a character sits on (US layout); unknown → ("", 0),
/// which still types through the event's `text`.
fn char_code(ch: char) -> (String, u32) {
    if ch.is_ascii_alphabetic() {
        let up = ch.to_ascii_uppercase();
        return (format!("Key{up}"), up as u32);
    }
    if ch.is_ascii_digit() {
        return (format!("Digit{ch}"), ch as u32);
    }
    let (code, kc) = match ch {
        ')' => ("Digit0", 48),
        '!' => ("Digit1", 49),
        '@' => ("Digit2", 50),
        '#' => ("Digit3", 51),
        '$' => ("Digit4", 52),
        '%' => ("Digit5", 53),
        '^' => ("Digit6", 54),
        '&' => ("Digit7", 55),
        '*' => ("Digit8", 56),
        '(' => ("Digit9", 57),
        ';' | ':' => ("Semicolon", 186),
        '=' | '+' => ("Equal", 187),
        ',' | '<' => ("Comma", 188),
        '-' | '_' => ("Minus", 189),
        '.' | '>' => ("Period", 190),
        '/' | '?' => ("Slash", 191),
        '`' | '~' => ("Backquote", 192),
        '[' | '{' => ("BracketLeft", 219),
        '\\' | '|' => ("Backslash", 220),
        ']' | '}' => ("BracketRight", 221),
        '\'' | '"' => ("Quote", 222),
        _ => ("", 0),
    };
    (code.to_string(), kc)
}

/// keyDown (with text when it types and no command modifier is held) and
/// keyUp.
fn key_events(def: &KeyDef, modifiers: &[Modifier]) -> Vec<Value> {
    let bits = modifier_bits(modifiers);
    let command = modifiers
        .iter()
        .any(|m| matches!(m, Modifier::Ctrl | Modifier::Meta | Modifier::Alt));
    let text = def.text.as_deref().filter(|_| !command);
    let mut down = json!({
        "type": if text.is_some() { "keyDown" } else { "rawKeyDown" },
        "modifiers": bits,
        "key": def.key,
        "code": def.code,
        "windowsVirtualKeyCode": def.key_code,
    });
    if let Some(t) = text {
        down["text"] = json!(t);
        down["unmodifiedText"] = json!(t);
    }
    // Headless Chrome on macOS doesn't map ⌘-shortcuts to editing commands
    // by itself.
    if cfg!(target_os = "macos") && modifiers.contains(&Modifier::Meta) {
        let cmd = match def.key.to_ascii_lowercase().as_str() {
            "a" => Some("selectAll"),
            "c" => Some("copy"),
            "x" => Some("cut"),
            "v" => Some("paste"),
            "z" if modifiers.contains(&Modifier::Shift) => Some("redo"),
            "z" => Some("undo"),
            _ => None,
        };
        if let Some(c) = cmd {
            down["commands"] = json!([c]);
        }
    }
    let up = json!({
        "type": "keyUp",
        "modifiers": bits,
        "key": def.key,
        "code": def.code,
        "windowsVirtualKeyCode": def.key_code,
    });
    vec![down, up]
}

/// Width and height from a base64 PNG's IHDR, decoding only its header.
fn png_size(b64: &str) -> Option<(u32, u32)> {
    let head = base64::engine::general_purpose::STANDARD
        .decode(b64.get(..32)?)
        .ok()?;
    if head.len() < 24 || &head[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let w = u32::from_be_bytes(head[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(head[20..24].try_into().ok()?);
    Some((w, h))
}

// --- page state, fed by CDP events -------------------------------------------------

struct Ring<T> {
    items: VecDeque<T>,
    /// The id the next entry gets.
    next: u64,
}

impl<T> Default for Ring<T> {
    fn default() -> Self {
        Ring {
            items: VecDeque::with_capacity(16),
            next: 1,
        }
    }
}

trait HasId {
    fn id(&self) -> u64;
}
impl HasId for ConsoleEntry {
    fn id(&self) -> u64 {
        self.id
    }
}
impl HasId for NetworkEntry {
    fn id(&self) -> u64 {
        self.id
    }
}

impl<T: Clone + HasId> Ring<T> {
    fn push(&mut self, make: impl FnOnce(u64) -> T) {
        let id = self.next;
        self.next += 1;
        if self.items.len() >= RING {
            self.items.pop_front();
        }
        self.items.push_back(make(id));
    }

    fn since(&self, since: u64) -> Vec<T> {
        self.items
            .iter()
            .filter(|e| e.id() >= since)
            .cloned()
            .collect()
    }
}

struct Req {
    method: String,
    url: String,
    /// CDP's monotonic seconds.
    start: f64,
    /// Wall clock, ms since the epoch.
    at: f64,
    status: Option<u16>,
    document: bool,
    /// Already in the ring (failed status): don't add it again as slow.
    logged: bool,
}

#[derive(Default)]
struct PageState {
    console: Ring<ConsoleEntry>,
    network: Ring<NetworkEntry>,
    requests: HashMap<String, Req>,
    /// Main-frame commits, same-document ones included.
    nav_seq: u64,
    /// Main-frame loads started.
    load_seq: u64,
    loading: bool,
    url: String,
    /// The main document's HTTP status.
    doc_status: Option<u16>,
    isolated_ctx: Option<i64>,
    /// Main-frame documents refused by the allowlist, and the last reason.
    blocked_seq: u64,
    blocked_reported: u64,
    blocked: Option<String>,
    crashed: bool,
    detached: bool,
}

struct PageShared {
    target_id: String,
    st: Mutex<PageState>,
    /// Bumped on every change a waiter may care about.
    changed: watch::Sender<u64>,
    /// Events that need an async answer (Fetch pauses, dialogs).
    to_task: mpsc::UnboundedSender<Event>,
}

impl PageShared {
    fn new(target_id: String, to_task: mpsc::UnboundedSender<Event>) -> Self {
        PageShared {
            target_id,
            st: Mutex::new(PageState::default()),
            changed: watch::channel(0).0,
            to_task,
        }
    }

    fn update(&self, f: impl FnOnce(&mut PageState)) {
        f(&mut lock(&self.st));
        self.changed.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Waits until `pred` holds (true) or `deadline` passes (false).
    async fn wait(&self, deadline: Instant, pred: impl Fn(&PageState) -> bool) -> bool {
        let mut rx = self.changed.subscribe();
        loop {
            let ok = {
                let st = lock(&self.st);
                pred(&st)
            };
            if ok {
                return true;
            }
            let woke = tokio::time::timeout_at(deadline.into(), rx.changed()).await;
            if !matches!(woke, Ok(Ok(()))) {
                let st = lock(&self.st);
                return pred(&st);
            }
        }
    }

    fn is_main(&self, frame: &Value) -> bool {
        frame.as_str() == Some(self.target_id.as_str())
    }

    /// One event for this page. Runs on the CDP reader task: no waiting.
    fn on_event(&self, ev: Event) {
        let p = &ev.params;
        match ev.method.as_str() {
            "Fetch.requestPaused" | "Page.javascriptDialogOpening" => {
                let _ = self.to_task.send(ev);
            }
            "Runtime.consoleAPICalled" => {
                let Some(level) = console_level(p["type"].as_str().unwrap_or("log")) else {
                    return;
                };
                let text = p["args"]
                    .as_array()
                    .map(|a| a.iter().map(remote_text).collect::<Vec<_>>().join(" "))
                    .unwrap_or_default();
                let at = p["timestamp"].as_f64().unwrap_or_else(now_ms);
                self.update(|st| {
                    st.console.push(|id| ConsoleEntry {
                        id,
                        level: level.to_string(),
                        text: cap(&text, TEXT_MAX),
                        at,
                    })
                });
            }
            "Runtime.exceptionThrown" => {
                let d = &p["exceptionDetails"];
                let mut text = d["exception"]["description"]
                    .as_str()
                    .or_else(|| d["text"].as_str())
                    .unwrap_or("Uncaught error")
                    .to_string();
                if let Some(url) = d["url"].as_str().filter(|u| !u.is_empty()) {
                    if !text.contains(url) {
                        let line = d["lineNumber"].as_i64().unwrap_or(0) + 1;
                        text.push_str(&format!(" ({url}:{line})"));
                    }
                }
                let at = p["timestamp"].as_f64().unwrap_or_else(now_ms);
                self.update(|st| {
                    st.console.push(|id| ConsoleEntry {
                        id,
                        level: "exception".to_string(),
                        text: cap(&text, TEXT_MAX),
                        at,
                    })
                });
            }
            "Runtime.executionContextCreated" => {
                let c = &p["context"];
                let aux = &c["auxData"];
                if self.is_main(&aux["frameId"]) && aux["isDefault"] == false && c["name"] == WORLD
                {
                    let id = c["id"].as_i64();
                    self.update(|st| st.isolated_ctx = id);
                }
            }
            "Runtime.executionContextDestroyed" => {
                let gone = p["executionContextId"].as_i64();
                self.update(|st| {
                    if st.isolated_ctx == gone {
                        st.isolated_ctx = None;
                    }
                });
            }
            "Runtime.executionContextsCleared" => self.update(|st| st.isolated_ctx = None),
            "Page.frameNavigated" => {
                let f = &p["frame"];
                if f.get("parentId").is_none_or(Value::is_null) {
                    let url = format!(
                        "{}{}",
                        f["url"].as_str().unwrap_or_default(),
                        f["urlFragment"].as_str().unwrap_or_default()
                    );
                    self.update(|st| {
                        st.url = url;
                        st.nav_seq += 1;
                    });
                }
            }
            "Page.navigatedWithinDocument" if self.is_main(&p["frameId"]) => {
                let url = p["url"].as_str().unwrap_or_default().to_string();
                self.update(|st| {
                    st.url = url;
                    st.nav_seq += 1;
                });
            }
            "Page.frameStartedLoading" if self.is_main(&p["frameId"]) => self.update(|st| {
                st.loading = true;
                st.load_seq += 1;
            }),
            "Page.frameStoppedLoading" if self.is_main(&p["frameId"]) => {
                self.update(|st| st.loading = false)
            }
            "Inspector.targetCrashed" => self.update(|st| st.crashed = true),
            "Inspector.detached" => self.update(|st| st.detached = true),
            "Network.requestWillBeSent" => self.on_request(p),
            "Network.responseReceived" => {
                let id = p["requestId"].as_str().unwrap_or_default();
                let status = p["response"]["status"]
                    .as_u64()
                    .or_else(|| p["response"]["status"].as_f64().map(|f| f as u64))
                    .and_then(|s| u16::try_from(s).ok());
                let ts = p["timestamp"].as_f64().unwrap_or(0.0);
                let main_doc = p["type"] == "Document" && self.is_main(&p["frameId"]);
                self.update(|st| {
                    if main_doc {
                        st.doc_status = status;
                    }
                    let Some(req) = st.requests.get_mut(id) else {
                        return;
                    };
                    req.status = status;
                    if status.is_some_and(|s| s >= 400) && !req.logged {
                        req.logged = true;
                        let entry = net_entry(req, status, None, ts);
                        st.network.push(|id| NetworkEntry { id, ..entry });
                    }
                });
            }
            "Network.loadingFinished" => {
                let id = p["requestId"].as_str().unwrap_or_default();
                let ts = p["timestamp"].as_f64().unwrap_or(0.0);
                self.update(|st| {
                    let Some(req) = st.requests.remove(id) else {
                        return;
                    };
                    if !req.logged && (ts - req.start) * 1000.0 >= 1000.0 {
                        let entry = net_entry(&req, req.status, None, ts);
                        st.network.push(|id| NetworkEntry { id, ..entry });
                    }
                });
            }
            "Network.loadingFailed" => {
                let id = p["requestId"].as_str().unwrap_or_default();
                let ts = p["timestamp"].as_f64().unwrap_or(0.0);
                let error = p["errorText"].as_str().unwrap_or("failed").to_string();
                let canceled = p["canceled"].as_bool().unwrap_or(false);
                self.update(|st| {
                    let Some(req) = st.requests.remove(id) else {
                        return;
                    };
                    // A cancelled request is a navigation away, not a
                    // failure; a document we blocked is already logged.
                    if canceled || (req.document && error.contains("BLOCKED_BY_CLIENT")) {
                        return;
                    }
                    let entry = net_entry(&req, req.status, Some(error), ts);
                    st.network.push(|id| NetworkEntry { id, ..entry });
                });
            }
            _ => {}
        }
    }

    fn on_request(&self, p: &Value) {
        let id = p["requestId"].as_str().unwrap_or_default().to_string();
        let url = p["request"]["url"].as_str().unwrap_or_default();
        if url.starts_with("data:") || url.starts_with("blob:") {
            return;
        }
        let method = p["request"]["method"].as_str().unwrap_or("GET").to_string();
        let ts = p["timestamp"].as_f64().unwrap_or(0.0);
        let at = p["wallTime"]
            .as_f64()
            .map(|s| s * 1000.0)
            .unwrap_or_else(now_ms);
        let document = p["type"] == "Document";
        let main_doc = document && self.is_main(&p["frameId"]);
        let redirect = p["redirectResponse"]["status"]
            .as_u64()
            .and_then(|s| u16::try_from(s).ok());
        let url = url.to_string();
        self.update(|st| {
            if main_doc && redirect.is_none() {
                st.doc_status = None;
            }
            // A redirect reuses the request id: the hop ended with its
            // redirect status; keep timing the request from its start.
            if let Some(req) = st.requests.get_mut(&id) {
                req.url = url;
                req.status = redirect;
                return;
            }
            if st.requests.len() < MAX_REQUESTS {
                st.requests.insert(
                    id,
                    Req {
                        method,
                        url,
                        start: ts,
                        at,
                        status: None,
                        document,
                        logged: false,
                    },
                );
            }
        });
    }

    /// A main-frame document refused by the allowlist.
    fn note_blocked(&self, method: &str, url: &str, why: &str) {
        let text = format!("Blocked a navigation to {url}: {why}");
        let error = format!("blocked: {why}");
        self.update(|st| {
            st.blocked_seq += 1;
            st.blocked = Some(text);
            st.network.push(|id| NetworkEntry {
                id,
                method: method.to_string(),
                url: cap(url, URL_MAX),
                status: None,
                error: Some(cap(&error, ERROR_MAX)),
                ms: None,
                at: now_ms(),
            });
        });
    }
}

fn net_entry(req: &Req, status: Option<u16>, error: Option<String>, ts: f64) -> NetworkEntry {
    let ms = (ts > 0.0 && req.start > 0.0).then(|| ((ts - req.start) * 1000.0).max(0.0).round());
    NetworkEntry {
        id: 0,
        method: req.method.clone(),
        url: cap(&req.url, URL_MAX),
        status,
        error: error.map(|e| cap(&e, ERROR_MAX)),
        ms,
        at: req.at,
    }
}

fn console_level(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "log"
        | "dir"
        | "dirxml"
        | "table"
        | "trace"
        | "count"
        | "timeEnd"
        | "startGroup"
        | "startGroupCollapsed"
        | "profile"
        | "profileEnd" => "log",
        "info" => "info",
        "warning" => "warn",
        "error" | "assert" => "error",
        "debug" => "debug",
        // clear, endGroup: no message.
        _ => return None,
    })
}

/// A console argument as text: primitives as themselves, objects from
/// their preview when CDP sent one.
fn remote_text(v: &Value) -> String {
    match v["type"].as_str().unwrap_or_default() {
        "string" => v["value"].as_str().unwrap_or_default().to_string(),
        "undefined" => "undefined".to_string(),
        "number" | "boolean" | "bigint" => v
            .get("unserializableValue")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| v.get("value").map(Value::to_string))
            .or_else(|| v["description"].as_str().map(str::to_string))
            .unwrap_or_default(),
        "object" if v["subtype"] == "null" => "null".to_string(),
        _ => {
            if let Some(props) = v["preview"]["properties"].as_array() {
                let array = v["subtype"] == "array";
                let parts: Vec<String> = props
                    .iter()
                    .map(|p| {
                        let val = p["value"].as_str().unwrap_or_default();
                        let val = if p["type"] == "string" {
                            format!("{val:?}")
                        } else {
                            val.to_string()
                        };
                        if array {
                            val
                        } else {
                            format!("{}: {val}", p["name"].as_str().unwrap_or_default())
                        }
                    })
                    .collect();
                let more = if v["preview"]["overflow"] == true {
                    ", …"
                } else {
                    ""
                };
                if array {
                    format!("[{}{more}]", parts.join(", "))
                } else {
                    format!("{{{}{more}}}", parts.join(", "))
                }
            } else {
                v["description"]
                    .as_str()
                    .or_else(|| v["className"].as_str())
                    .unwrap_or("[object]")
                    .to_string()
            }
        }
    }
}

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

/// At most `max` bytes, cut at a char boundary, marked with an ellipsis.
fn cap(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max.saturating_sub(3);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

fn route_event(sessions: &Mutex<HashMap<String, Arc<PageShared>>>, ev: Event) {
    match ev.session.as_deref() {
        Some(s) => {
            let shared = lock(sessions).get(s).cloned();
            if let Some(shared) = shared {
                shared.on_event(ev);
            }
        }
        None if ev.method == "Target.detachedFromTarget" => {
            let shared = ev.params["sessionId"]
                .as_str()
                .and_then(|s| lock(sessions).get(s).cloned());
            if let Some(shared) = shared {
                shared.update(|st| st.detached = true);
            }
        }
        None => {}
    }
}

/// Answers a page's Fetch pauses (the allowlist) and dialogs, for the
/// page's life.
async fn page_task(
    conn: Arc<Conn>,
    session: String,
    shared: Arc<PageShared>,
    state: Weak<AppState>,
    mut rx: mpsc::UnboundedReceiver<Event>,
) {
    while let Some(ev) = rx.recv().await {
        let p = &ev.params;
        match ev.method.as_str() {
            "Fetch.requestPaused" => {
                let Some(request_id) = p["requestId"].as_str() else {
                    continue;
                };
                let url = p["request"]["url"].as_str().unwrap_or_default();
                let method = p["request"]["method"].as_str().unwrap_or("GET");
                let main = p["resourceType"] == "Document" && shared.is_main(&p["frameId"]);
                let verdict = if main {
                    allow_document(&state, url).await
                } else {
                    Ok(())
                };
                let (cmd, params) = match verdict {
                    Ok(()) => ("Fetch.continueRequest", json!({"requestId": request_id})),
                    Err(why) => {
                        tracing::info!(%url, %why, "agent browser: blocked a main-frame navigation");
                        shared.note_blocked(method, url, &why);
                        (
                            "Fetch.failRequest",
                            json!({"requestId": request_id, "errorReason": "BlockedByClient"}),
                        )
                    }
                };
                let _ = conn
                    .call(cmd, params, Some(&session), Duration::from_secs(5))
                    .await;
            }
            "Page.javascriptDialogOpening" => {
                let kind = p["type"].as_str().unwrap_or("alert").to_string();
                let message = p["message"].as_str().unwrap_or_default();
                // Nobody can answer a dialog in a headless page, and an open
                // one stalls every script: accept alerts and leave-page
                // prompts, dismiss confirm/prompt.
                let accept = matches!(kind.as_str(), "alert" | "beforeunload");
                let text = format!(
                    "[{kind} dialog, {}] {message}",
                    if accept { "accepted" } else { "dismissed" }
                );
                shared.update(|st| {
                    st.console.push(|id| ConsoleEntry {
                        id,
                        level: "warn".to_string(),
                        text: cap(&text, TEXT_MAX),
                        at: now_ms(),
                    })
                });
                let _ = conn
                    .call(
                        "Page.handleJavaScriptDialog",
                        json!({"accept": accept}),
                        Some(&session),
                        Duration::from_secs(5),
                    )
                    .await;
            }
            _ => {}
        }
    }
}

/// The allowlist for a main-frame document: `Err` is why it was refused.
async fn allow_document(state: &Weak<AppState>, url: &str) -> Result<(), String> {
    let Some((host, port)) = http_target(url) else {
        return Err("only http and https pages can be opened".to_string());
    };
    let Some(state) = state.upgrade() else {
        return Err("the daemon is shutting down".to_string());
    };
    let host = proxy::normalize_host(&host).to_ascii_lowercase();
    match tokio::time::timeout(
        Duration::from_secs(10),
        proxy::check_target(&state, &host, port),
    )
    .await
    {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(proxy::Refusal::ConfirmRequired)) => Err(format!(
            "{host}:{port} is outside the allowlist (loopback, this host, or a node one of your \
             running jobs is on). Ask the user to open it in a browser pane instead."
        )),
        Ok(Err(proxy::Refusal::Daemon)) => Err(format!(
            "{host}:{port} is the Chimaera daemon itself, which the agent browser never opens."
        )),
        Ok(Err(proxy::Refusal::Invalid)) => Err(format!("`{host}` is not a valid host.")),
        Err(_) => Err("couldn't check the target against the allowlist in time".to_string()),
    }
}

/// `http(s)://[user@]host[:port]/…` → (host, port).
fn http_target(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let default = match scheme.to_ascii_lowercase().as_str() {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let (h, after) = v6.split_once(']')?;
        let port = after.strip_prefix(':');
        (h.to_string(), port)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), Some(p)),
            None => (authority.to_string(), None),
        }
    };
    let port = match port {
        None | Some("") => default,
        Some(p) => p.parse().ok()?,
    };
    (!host.is_empty()).then_some((host, port))
}

#[cfg(test)]
mod live_tests;
#[cfg(test)]
mod tests;
