//! The plugin host's runtime: WASM components (`chimaera:plugin`, the WIT in
//! `crates/chimaera-plugin-api/wit`) under wasmtime, one sandboxed instance
//! per (plugin, workspace). Design: docs/plugin-system-plan.md ("The host").
//!
//! - **One engine per process**, built on first use (Cranelift, epoch
//!   interruption, a 64 MiB virtual reservation per memory instead of 4 GiB:
//!   login nodes run under `ulimit -v`). Each component compiles once per
//!   cache lifetime, off the reactor (two builds per id, 32 ids).
//! - **One ticker thread** bumps the engine's epoch every 100 ms; a store's
//!   deadline callback yields to tokio on each tick and interrupts the guest
//!   once the call's wall-clock budget is spent. The deadline starts at 0, so
//!   it is armed before every call (an unarmed store traps at once).
//! - **Instances** are created lazily, one call at a time each (an async
//!   mutex), at most `MAX_INSTANCES` daemon-wide (least recently used idle slot goes),
//!   and dropped after `IDLE` (swept on access — no polling task).
//! - **A build the host can't run** (it doesn't compile, or its imports
//!   don't link) is remembered per daemon so it isn't recompiled per call,
//!   reported as the plugin's fault, and forgotten when the user switches
//!   the plugin off and on or its `current` moves — the user's retry.
//! - **A trap costs the instance, never the daemon**: the store is dropped
//!   and re-created on the next call (~20 µs). `FAULT_LIMIT` traps within
//!   `FAULT_WINDOW` mark the plugin faulted in that workspace until the user
//!   switches it off and on.
//! - **Traps ride Unix signal handlers on macOS too**
//!   (`macos_use_mach_ports(false)`): wasmtime's default Mach-port thread
//!   aborts the whole daemon when a caught signal — SIGCHLD from an ending
//!   PTY shell — interrupts its `mach_msg`.
//! - Linear memory is capped per instance (`MEMORY_CAP`); WASI grants
//!   nothing (no files, env, args, network); the guest's stderr is kept
//!   (last `STDERR_CAP` bytes) only to log why a call trapped.
//! - **A build is its SHA-256**: compiled components, offers and live
//!   instances are keyed by the component's digest, so a plugin whose
//!   `current` moved (an update, a rollback) never runs the old code — even
//!   a call that raced the change re-instantiates. `forget_plugin` drops
//!   the rest at once.
//! - **Events reach only the plugins that declared them**
//!   (`provides.events`): a hook never instantiates a plugin that has no
//!   use for it.
//!
//! Every host function the guest may call is in `hostfns.rs`, bounded there.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, ResourceLimiter, Store, UpdateDeadline};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use super::Manifest;
use crate::AppState;

/// The 0.2 world (`wit/`): what the host implements and speaks inside.
pub(crate) mod v2 {
    wasmtime::component::bindgen!({
        world: "chimaera-plugin",
        path: "../chimaera-plugin-api/wit",
        imports: { default: async },
        exports: { default: async },
    });
}

/// The 0.1 world (`wit-0.1/`), served unchanged beside 0.2: a component
/// built against it instantiates through these bindings, and the host
/// translates its few types at the edge (`v1_*` below).
pub(crate) mod v1 {
    wasmtime::component::bindgen!({
        world: "chimaera-plugin",
        path: "../chimaera-plugin-api/wit-0.1",
        imports: { default: async },
        exports: { default: async },
    });
}

pub(crate) use v2::chimaera::plugin::types as wit;

/// Wall-clock budget for `tools`, `instructions`, `call-tool`, `query` and
/// `on-event`; `knowledge` (a whole reader pass) gets `KNOWLEDGE_BUDGET`.
const CALL_BUDGET: Duration = Duration::from_secs(5);
const KNOWLEDGE_BUDGET: Duration = Duration::from_secs(30);
/// Past the budget, a call still waiting (a host function stuck on a slow
/// filesystem, where no guest code runs to be interrupted) is abandoned.
const HOST_GRACE: Duration = Duration::from_secs(2);
/// The epoch tick: the resolution of every deadline.
const TICK: Duration = Duration::from_millis(100);
/// Linear memory per instance.
const MEMORY_CAP: usize = 64 << 20;
const TABLE_ELEMENTS_CAP: usize = 65_536;
const MAX_INSTANCES: usize = 64;
const IDLE: Duration = Duration::from_secs(10 * 60);
const FAULT_LIMIT: usize = 5;
/// Compiled builds kept per plugin id: the running one and the one before
/// (a rollback runs it again without a compile). Older builds are dropped.
const COMPILED_PER_ID: usize = 2;
const COMPILED_IDS: usize = 32;
const FAULT_WINDOW: Duration = Duration::from_secs(60);
/// The guest's stderr kept per instance (its tail; a panic message).
const STDERR_CAP: usize = 4 * 1024;
/// What a plugin may hand back, whatever it returns: a tool result's text,
/// the instruction paragraph, a hook line.
const RESULT_MAX: usize = 256 * 1024;
const INSTRUCTIONS_MAX: usize = 8 * 1024;
/// A tool's description and input schema, as offered to agents: they ride
/// every session's `tools/list`, beside the core tools.
const TOOL_DESCRIPTION_MAX: usize = 2 * 1024;
const TOOL_SCHEMA_MAX: usize = 16 * 1024;
const HOOK_LINE_MAX: usize = 1024;
/// A Knowledge snapshot's JSON (the view's whole data, cached per
/// workspace) and its stamp: a real `.living/` of ~300 findings, ~160
/// decisions and ~300 learnings is well under a MiB.
pub(crate) const SNAPSHOT_MAX: usize = 4 << 20;
const STAMP_MAX: usize = 1 << 20;
/// Why a provider couldn't read, in its own words: shown on the Knowledge
/// view and logged, so bounded like a hook line.
const KNOWLEDGE_ERROR_MAX: usize = 1024;
/// `emit` frames kept for the `/ws/events` clients, and one frame's size.
const EVENTS_KEPT: usize = 256;
pub(crate) const EVENT_MAX: usize = 16 * 1024;

/// A compiled build, pre-linked for the world its manifest's `api` names.
#[derive(Clone)]
enum Pre {
    V1(v1::ChimaeraPluginPre<HostState>),
    V2(v2::ChimaeraPluginPre<HostState>),
}

/// A live instance's bindings, of the world it was built against.
enum Bindings {
    V1(v1::ChimaeraPlugin),
    V2(v2::ChimaeraPlugin),
}

/// One plugin's compiled builds by SHA-256, most recently used first. Only
/// builds that compiled: why one can't is the daemon's (`refused`).
type Builds = VecDeque<(Arc<str>, Pre)>;

/// The process-wide half: engine, linker, compiled components. A test
/// process builds many `AppState`s; they share this, as one daemon would.
struct Shared {
    engine: Engine,
    linker: Linker<HostState>,
    /// plugin id → its pre-instantiated builds by SHA-256, most recently
    /// used first, at most `COMPILED_PER_ID`.
    compiled: tokio::sync::Mutex<VecDeque<(String, Builds)>>,
}

static SHARED: OnceLock<Result<Shared, String>> = OnceLock::new();

fn shared() -> Result<&'static Shared, String> {
    SHARED
        .get_or_init(|| build_shared().map_err(|e| format!("the plugin engine failed: {e:#}")))
        .as_ref()
        .map_err(Clone::clone)
}

fn build_shared() -> wasmtime::Result<Shared> {
    let mut config = Config::new();
    // `Config::async_support` is a deprecated no-op in wasmtime 49: the
    // `_async` calls and async host functions imply it.
    config.epoch_interruption(true);
    config.memory_reservation(MEMORY_CAP as u64);
    config.memory_guard_size(64 << 10);
    config.memory_reservation_for_growth(0);
    // Unix signal handlers, not wasmtime's Mach-port thread: that thread
    // aborts the whole process when a caught signal (a child's SIGCHLD,
    // which tokio handles) interrupts its mach_msg. No-op off macOS.
    config.macos_use_mach_ports(false);
    let engine = Engine::new(&config)?;
    let mut linker: Linker<HostState> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    // Both worlds in one linker: their imports are versioned
    // (`chimaera:plugin/host@0.1.0`, `…@0.2.0`), so they never collide.
    v1::ChimaeraPlugin::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    v2::ChimaeraPlugin::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    // The daemon's one ticker: it only bumps an atomic, 10 times a second,
    // and only once a plugin has been used.
    let ticking = engine.clone();
    std::thread::Builder::new()
        .name("plugin-epoch".into())
        .spawn(move || loop {
            std::thread::sleep(TICK);
            ticking.increment_epoch();
        })?;
    Ok(Shared {
        engine,
        linker,
        compiled: tokio::sync::Mutex::new(VecDeque::new()),
    })
}

/// The component for `m`'s build, compiled on first use (Cranelift: tens
/// of ms of CPU, so on the blocking pool) and kept while it is one of the
/// plugin's last `COMPILED_PER_ID` builds. A failure isn't kept here: the
/// caller remembers it (`PluginRuntime::refused`), where a retry clears it.
async fn component(m: &Manifest) -> Result<Pre, String> {
    let shared = shared()?;
    let mut compiled = shared.compiled.lock().await;
    let mut builds = compiled
        .iter()
        .position(|(id, _)| id == &m.id)
        .and_then(|at| compiled.remove(at))
        .map(|(_, builds)| builds)
        .unwrap_or_default();
    if let Some(at) = builds.iter().position(|(sha, _)| *sha == m.wasm.sha256) {
        let build = builds.remove(at).expect("found above");
        let done = build.1.clone();
        builds.push_front(build);
        compiled.push_front((m.id.clone(), builds));
        return Ok(done);
    }
    let bytes = m.wasm.bytes.clone();
    let engine = shared.engine.clone();
    let linker = shared.linker.clone();
    let name = m.name.clone();
    let api = m.api.clone();
    let started = Instant::now();
    let result = tokio::task::spawn_blocking(move || {
        let component = Component::new(&engine, &**bytes)?;
        let pre = linker.instantiate_pre(&component)?;
        // The gate already refused an `api` this host doesn't serve.
        Ok::<_, wasmtime::Error>(if api == "0.1" {
            Pre::V1(v1::ChimaeraPluginPre::new(pre)?)
        } else {
            Pre::V2(v2::ChimaeraPluginPre::new(pre)?)
        })
    })
    .await
    .map_err(|e| format!("{name}: compile task failed: {e}"))
    .and_then(|r| r.map_err(|e| format!("{name} is not a component this host can run: {e:#}")));
    match &result {
        Ok(_) => tracing::info!(
            plugin = %m.id,
            version = %m.version,
            ms = started.elapsed().as_millis() as u64,
            "plugin compiled"
        ),
        Err(err) => tracing::error!(plugin = %m.id, %err, "plugin refused"),
    }
    if let Ok(pre) = &result {
        builds.push_front((m.wasm.sha256.clone(), pre.clone()));
        builds.truncate(COMPILED_PER_ID);
    }
    if !builds.is_empty() {
        compiled.push_front((m.id.clone(), builds));
        compiled.truncate(COMPILED_IDS);
    }
    result
}

/// Compile, in the background, the builds of plugins switched on in some
/// workspace (or just `only`), so a window's first render or an agent's
/// first tool call never waits on Cranelift: seconds for a large component
/// on a slow login node. One at a time, after a short delay at boot; a build
/// already compiled is a cache hit. Faulted and held plugins are skipped
/// (they never run here).
pub(crate) fn warm(state: &Arc<AppState>, only: Option<String>, delay: Duration) {
    let state = state.clone();
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        let on: std::collections::HashSet<String> = crate::lock(&state.workspaces)
            .list()
            .into_iter()
            .flat_map(|w| w.plugins_on)
            .collect();
        for m in super::catalog(&state).iter() {
            if only.as_ref().is_some_and(|id| id != &m.id)
                || !on.contains(&m.id)
                || m.origin.fault.is_some()
                || super::trust::hold(&state, m).is_some()
            {
                continue;
            }
            let _ = component(m).await;
        }
    });
}

/// A component can contain several core memories and tables. Account for
/// their combined allocations; wasmtime's StoreLimits caps each separately.
#[derive(Default)]
struct AllocationBudget {
    used: usize,
    pending: usize,
}

impl AllocationBudget {
    fn grow(&mut self, current: usize, desired: usize, maximum: Option<usize>, cap: usize) -> bool {
        self.pending = 0;
        let delta = desired.saturating_sub(current);
        if maximum.is_some_and(|max| desired > max) || delta > cap.saturating_sub(self.used) {
            return false;
        }
        self.used += delta;
        self.pending = delta;
        true
    }

    fn failed(&mut self) {
        self.used -= std::mem::take(&mut self.pending);
    }
}

#[derive(Default)]
struct InstanceLimits {
    memory: AllocationBudget,
    table: AllocationBudget,
}

impl ResourceLimiter for InstanceLimits {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(self.memory.grow(current, desired, maximum, MEMORY_CAP))
    }

    fn memory_grow_failed(&mut self, _error: wasmtime::Error) -> wasmtime::Result<()> {
        self.memory.failed();
        Ok(())
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(self
            .table
            .grow(current, desired, maximum, TABLE_ELEMENTS_CAP))
    }

    fn table_grow_failed(&mut self, _error: wasmtime::Error) -> wasmtime::Result<()> {
        self.table.failed();
        Ok(())
    }

    fn instances(&self) -> usize {
        64
    }
    fn memories(&self) -> usize {
        16
    }
    fn tables(&self) -> usize {
        16
    }
}

/// A store's data: WASI (nothing granted), the limits, and — for the length
/// of one call only — the daemon state the host functions serve from.
pub(crate) struct HostState {
    wasi: WasiCtx,
    table: ResourceTable,
    limits: InstanceLimits,
    stderr: StderrTail,
    deadline: Instant,
    pub(super) plugin: String,
    pub(super) workspace: String,
    /// What this build's manifest lets it read through the host
    /// (`[access]`), enforced by every host function.
    pub(super) access: super::capabilities::Access,
    /// This build's manifest (its declared views and settings), for the
    /// platform imports; None only in unit tests.
    pub(super) manifest: Option<Arc<Manifest>>,
    /// Set by `begin`, cleared by `end`: an idle instance never holds the
    /// daemon's state (the store lives in that state — a cycle otherwise).
    pub(super) call: Option<CallScope>,
}

/// What the host knows about the call in flight. The host serves from this,
/// never from the `cx` the guest passes back (a guest could forge it).
pub(super) struct CallScope {
    pub(super) app: Arc<AppState>,
    pub(super) cx: wit::Context,
    /// `log` lines this call wrote (capped per call).
    pub(super) logs: usize,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl HostState {
    fn new(plugin: &str, workspace: &str, access: super::capabilities::Access) -> Self {
        let stderr = StderrTail::default();
        let mut wasi = WasiCtxBuilder::new();
        // Nothing granted: no preopens, env or args (the builder's
        // defaults), stdin closed, stdout discarded, no sockets at all.
        wasi.stderr(stderr.clone())
            .allow_tcp(false)
            .allow_udp(false)
            .allow_ip_name_lookup(false);
        HostState {
            wasi: wasi.build(),
            table: ResourceTable::new(),
            limits: InstanceLimits::default(),
            stderr,
            deadline: Instant::now(),
            plugin: plugin.to_string(),
            workspace: workspace.to_string(),
            access,
            manifest: None,
            call: None,
        }
    }

    fn begin(&mut self, app: &Arc<AppState>, cx: &wit::Context, budget: Duration) {
        self.deadline = Instant::now() + budget;
        self.stderr.clear();
        self.call = Some(CallScope {
            app: app.clone(),
            cx: cx.clone(),
            logs: 0,
        });
    }

    fn end(&mut self) {
        if let Some(scope) = self.call.take() {
            if scope.logs > super::hostfns::LOGS_PER_CALL {
                tracing::warn!(
                    plugin = %self.plugin,
                    dropped = scope.logs - super::hostfns::LOGS_PER_CALL,
                    "plugin log lines over the per-call cap were dropped"
                );
            }
        }
    }
}

/// A guest's stderr, its last `STDERR_CAP` bytes. Writes never fail: a
/// plugin printing a lot must not trap for it.
#[derive(Clone, Default)]
struct StderrTail(Arc<Mutex<VecDeque<u8>>>);

impl StderrTail {
    fn push(&self, bytes: &[u8]) {
        let mut tail = crate::lock(&self.0);
        let keep = bytes.len().min(STDERR_CAP);
        tail.extend(&bytes[bytes.len() - keep..]);
        let over = tail.len().saturating_sub(STDERR_CAP);
        tail.drain(..over);
    }

    fn clear(&self) {
        crate::lock(&self.0).clear();
    }

    /// The last non-empty line (a Rust panic's message line comes last but
    /// one; its `note:` line is skipped).
    fn last_line(&self) -> Option<String> {
        self.contents()
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with("note:"))
            .map(|l| crate::timeline::cap(l, 300))
    }

    fn contents(&self) -> String {
        let bytes: Vec<u8> = crate::lock(&self.0).iter().copied().collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// `text` cut to at most `max` bytes on a char boundary, marked when cut —
/// never trimmed: what a plugin says reaches the agent byte for byte.
pub(crate) fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max.saturating_sub('…'.len_utf8());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

impl wasmtime_wasi::cli::IsTerminal for StderrTail {
    fn is_terminal(&self) -> bool {
        false
    }
}

impl wasmtime_wasi::cli::StdoutStream for StderrTail {
    fn p2_stream(&self) -> Box<dyn wasmtime_wasi::p2::OutputStream> {
        Box::new(self.clone())
    }

    fn async_stream(&self) -> Box<dyn tokio::io::AsyncWrite + Send + Sync> {
        Box::new(self.clone())
    }
}

impl wasmtime_wasi::p2::OutputStream for StderrTail {
    fn write(&mut self, bytes: bytes::Bytes) -> wasmtime_wasi::p2::StreamResult<()> {
        self.push(&bytes);
        Ok(())
    }

    fn flush(&mut self) -> wasmtime_wasi::p2::StreamResult<()> {
        Ok(())
    }

    fn check_write(&mut self) -> wasmtime_wasi::p2::StreamResult<usize> {
        Ok(STDERR_CAP)
    }
}

// `Pollable` is an `async_trait` trait; this is its expansion (the crate
// isn't a dependency). Always ready: writes never block.
impl wasmtime_wasi::p2::Pollable for StderrTail {
    fn ready<'a, 'b>(&'a mut self) -> Pin<Box<dyn Future<Output = ()> + Send + 'b>>
    where
        'a: 'b,
        Self: 'b,
    {
        Box::pin(async {})
    }
}

impl tokio::io::AsyncWrite for StderrTail {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.push(buf);
        std::task::Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

/// A live instance: its store, the bindings into it, and which build it
/// runs.
struct Live {
    store: Store<HostState>,
    plugin: Bindings,
    sha256: Arc<str>,
    // A reset can detach a running slot. Its allocation still counts until
    // that call finishes and drops the store.
    _permit: tokio::sync::OwnedSemaphorePermit,
}

struct InstanceBudget(Arc<tokio::sync::Semaphore>);

impl Default for InstanceBudget {
    fn default() -> Self {
        Self(Arc::new(tokio::sync::Semaphore::new(MAX_INSTANCES)))
    }
}

/// One (plugin, workspace)'s instance slot. `live` is None until first use
/// and after a trap.
struct Slot {
    live: tokio::sync::Mutex<Option<Live>>,
    last_used: Mutex<Instant>,
}

impl Slot {
    fn idle_for(&self) -> Duration {
        crate::lock(&self.last_used).elapsed()
    }

    fn touch(&self) {
        *crate::lock(&self.last_used) = Instant::now();
    }
}

#[derive(Default)]
struct Faults {
    traps: VecDeque<Instant>,
    faulted: Option<String>,
}

/// What a plugin offers agents, asked once per plugin (the exports take no
/// context) and checked against its manifest.
#[derive(Clone)]
pub(crate) struct Offer {
    pub(crate) tools: Vec<Value>,
    pub(crate) instructions: Option<String>,
}

/// `emit` frames, a small boot-scoped ring (the notices idiom): each
/// `/ws/events` client starts at the head and sends what is newer.
#[derive(Default)]
struct Events {
    next: u64,
    /// (id, the workspace it belongs to, the frame).
    ring: VecDeque<(u64, Arc<str>, Arc<str>)>,
}

type Key = (String, String);
/// (plugin id, the build's SHA-256).
type BuildKey = (String, Arc<str>);

/// The per-daemon half of the runtime (on `AppState`).
#[derive(Default)]
pub(crate) struct PluginRuntime {
    instances: InstanceBudget,
    slots: Mutex<HashMap<Key, Arc<Slot>>>,
    faults: Mutex<HashMap<Key, Faults>>,
    /// (plugin id, build) → its offer, or why it is refused everywhere (a
    /// component whose tools don't match its manifest).
    offers: Mutex<HashMap<BuildKey, Result<Offer, String>>>,
    /// (plugin id, build) → why the host can't run it (it didn't compile or
    /// link). Not asked again until a reset or `forget_plugin`.
    refused: Mutex<HashMap<BuildKey, String>>,
    events: Mutex<Events>,
    /// Tests only: a shorter call budget, in ms (0 = the real ones).
    budget_override_ms: AtomicU64,
}

enum Call {
    Tools,
    Instructions,
    Tool(String, String),
    Knowledge(Option<String>),
    Query(String, String),
    OnEvent(wit::Event),
    Render(String, String),
    Action(String, String, String),
    ToolResume(String, String),
}

enum Reply {
    Tools(Vec<wit::ToolDef>),
    Instructions(Option<String>),
    ToolResult(wit::ToolResult),
    Knowledge(Result<Option<wit::Snapshot>, String>),
    Json(Result<String, String>),
    Event(Option<String>),
    /// A 0.2 export asked of a 0.1 build (the routes check `api` first).
    Unsupported,
}

async fn invoke(live: &mut Live, cx: &wit::Context, call: Call) -> wasmtime::Result<Reply> {
    let store = &mut live.store;
    let plugin = match &live.plugin {
        Bindings::V2(plugin) => plugin,
        Bindings::V1(old) => return invoke_v1(old, store, cx, call).await,
    };
    let exports = plugin.chimaera_plugin_plugin();
    let screens = plugin.chimaera_plugin_screens();
    Ok(match call {
        Call::Tools => Reply::Tools(exports.call_tools(store).await?),
        Call::Instructions => Reply::Instructions(exports.call_instructions(store).await?),
        Call::Tool(name, args) => {
            Reply::ToolResult(exports.call_call_tool(store, cx, &name, &args).await?)
        }
        Call::Knowledge(known) => {
            Reply::Knowledge(exports.call_knowledge(store, cx, known.as_ref()).await?)
        }
        Call::Query(name, args) => Reply::Json(exports.call_query(store, cx, &name, &args).await?),
        Call::OnEvent(event) => Reply::Event(exports.call_on_event(store, cx, &event).await?),
        Call::Render(view, args) => {
            Reply::Json(screens.call_render(store, cx, &view, &args).await?)
        }
        Call::Action(view, action, payload) => Reply::Json(
            screens
                .call_on_action(store, cx, &view, &action, &payload)
                .await?,
        ),
        Call::ToolResume(name, job) => {
            Reply::ToolResult(screens.call_tool_resume(store, cx, &name, &job).await?)
        }
    })
}

/// A call into a 0.1 build: the same exports, its own types.
async fn invoke_v1(
    plugin: &v1::ChimaeraPlugin,
    store: &mut Store<HostState>,
    cx: &wit::Context,
    call: Call,
) -> wasmtime::Result<Reply> {
    use v1::chimaera::plugin::types as old;
    let exports = plugin.chimaera_plugin_plugin();
    let cx = old::Context {
        workspace: cx.workspace.clone(),
        session: cx.session.clone(),
        mastermind: cx.mastermind,
    };
    Ok(match call {
        Call::Tools => Reply::Tools(
            exports
                .call_tools(store)
                .await?
                .into_iter()
                .map(|d| wit::ToolDef {
                    name: d.name,
                    description: d.description,
                    input_schema: d.input_schema,
                })
                .collect(),
        ),
        Call::Instructions => Reply::Instructions(exports.call_instructions(store).await?),
        Call::Tool(name, args) => {
            let r = exports.call_call_tool(store, &cx, &name, &args).await?;
            Reply::ToolResult(wit::ToolResult {
                text: r.text,
                is_error: r.is_error,
                wait: None,
            })
        }
        Call::Knowledge(known) => Reply::Knowledge(
            exports
                .call_knowledge(store, &cx, known.as_ref())
                .await?
                .map(|s| {
                    s.map(|s| wit::Snapshot {
                        stamp: s.stamp,
                        data: s.data,
                    })
                }),
        ),
        Call::Query(name, args) => Reply::Json(exports.call_query(store, &cx, &name, &args).await?),
        Call::OnEvent(event) => {
            // 0.1 has four events; the rest never reach a 0.1 build (its
            // manifest can't declare them).
            let event = match event {
                wit::Event::Hook(h) => old::Event::Hook(old::Hook {
                    session: h.session,
                    name: h.name,
                }),
                wit::Event::SessionEnded(s) => old::Event::SessionEnded(s),
                wit::Event::SwitchedOn => old::Event::SwitchedOn,
                wit::Event::SwitchedOff => old::Event::SwitchedOff,
                _ => return Ok(Reply::Event(None)),
            };
            Reply::Event(exports.call_on_event(store, &cx, &event).await?)
        }
        Call::Render(..) | Call::Action(..) | Call::ToolResume(..) => Reply::Unsupported,
    })
}

/// Why a call failed, in words for the log and the caller.
fn describe(
    m: &Manifest,
    err: &wasmtime::Error,
    budget: Duration,
    stderr: Option<String>,
) -> String {
    let what = match err.downcast_ref::<wasmtime::Trap>() {
        Some(wasmtime::Trap::Interrupt) => {
            format!(
                "ran past its {} s budget and was stopped",
                budget.as_secs_f32()
            )
        }
        Some(trap) => format!("stopped: {trap}"),
        None => format!("failed: {}", crate::timeline::cap(&format!("{err:#}"), 300)),
    };
    match stderr {
        Some(line) => format!("{} {what} ({line})", m.name),
        None => format!("{} {what}", m.name),
    }
}

impl PluginRuntime {
    fn budget(&self, call: &Call) -> Duration {
        match self.budget_override_ms.load(Ordering::Relaxed) {
            // A reader pass, and digesting a finished job's outputs.
            0 if matches!(
                call,
                Call::Knowledge(_) | Call::OnEvent(wit::Event::JobFinished(_))
            ) =>
            {
                KNOWLEDGE_BUDGET
            }
            0 => CALL_BUDGET,
            ms => Duration::from_millis(ms),
        }
    }

    /// Tests only: every call's budget becomes `budget`.
    #[cfg(test)]
    pub(crate) fn set_budget_for_tests(&self, budget: Duration) {
        self.budget_override_ms
            .store(budget.as_millis() as u64, Ordering::Relaxed);
    }

    /// Why `m` isn't answering in `ws`, if it isn't (for the card).
    pub(crate) fn fault(&self, m: &Manifest, ws: &str) -> Option<String> {
        if let Some(refused) = crate::lock(&self.refused).get(&offer_key(m)) {
            return Some(refused.clone());
        }
        if let Some(Err(refused)) = crate::lock(&self.offers).get(&offer_key(m)) {
            return Some(refused.clone());
        }
        crate::lock(&self.faults)
            .get(&(m.id.clone(), ws.to_string()))
            .and_then(|f| f.faulted.clone())
    }

    /// Start `plugin` over in `ws`: drop its instance and clear its fault
    /// (the plugin's switch flipped) — and a build the host couldn't run is
    /// compiled again on next use, since the switch is the user's retry.
    pub(crate) fn reset(&self, plugin: &str, ws: &str) {
        let key = (plugin.to_string(), ws.to_string());
        crate::lock(&self.slots).remove(&key);
        crate::lock(&self.faults).remove(&key);
        crate::lock(&self.refused).retain(|(p, _), _| p != plugin);
    }

    /// `plugin`'s `current` moved (installed, updated, rolled back,
    /// removed): every instance of it goes, everywhere, with its faults and
    /// offers — the next call instantiates the build that is current now.
    /// A call in flight finishes on the old instance, which is then dropped.
    pub(crate) fn forget_plugin(&self, plugin: &str) {
        crate::lock(&self.slots).retain(|(p, _), _| p != plugin);
        crate::lock(&self.faults).retain(|(p, _), _| p != plugin);
        crate::lock(&self.offers).retain(|(p, _), _| p != plugin);
        crate::lock(&self.refused).retain(|(p, _), _| p != plugin);
    }

    /// Tests only: how many live instances `plugin` has.
    #[cfg(test)]
    pub(crate) fn live_instances(&self, plugin: &str) -> usize {
        crate::lock(&self.slots)
            .iter()
            .filter(|((p, _), slot)| {
                p == plugin && slot.live.try_lock().map_or(true, |live| live.is_some())
            })
            .count()
    }

    /// A deleted workspace: every plugin's instance and fault there.
    pub(crate) fn forget_workspace(&self, ws: &str) {
        crate::lock(&self.slots).retain(|(_, w), _| w != ws);
        crate::lock(&self.faults).retain(|(_, w), _| w != ws);
    }

    /// The instance slot for `key`, making room: idle slots go (the sweep
    /// runs on every access, so no timer is needed), then — at the cap —
    /// the least recently used unused one. In-flight and queued calls keep
    /// their slot, so they always serialize against the same instance.
    fn slot(&self, key: &Key) -> Result<Arc<Slot>, String> {
        let mut slots = crate::lock(&self.slots);
        slots.retain(|_, s| Arc::strong_count(s) > 1 || s.idle_for() < IDLE);
        if let Some(slot) = slots.get(key) {
            slot.touch();
            return Ok(slot.clone());
        }
        if slots.len() >= MAX_INSTANCES {
            let lru = slots
                .iter()
                .filter(|(_, s)| Arc::strong_count(s) == 1)
                .max_by_key(|(_, s)| s.idle_for())
                .map(|(k, _)| k.clone());
            if let Some(lru) = lru {
                slots.remove(&lru);
            } else {
                return Err("all plugin instances are busy — retry shortly".into());
            }
        }
        let slot = Arc::new(Slot {
            live: tokio::sync::Mutex::new(None),
            last_used: Mutex::new(Instant::now()),
        });
        slots.insert(key.clone(), slot.clone());
        Ok(slot)
    }

    fn faulted(&self, key: &Key) -> Option<String> {
        crate::lock(&self.faults)
            .get(key)
            .and_then(|f| f.faulted.clone())
    }

    /// Count a failure; the `FAULT_LIMIT`th within `FAULT_WINDOW` faults the
    /// plugin in this workspace.
    fn record_fault(&self, m: &Manifest, key: &Key, why: &str) {
        let mut faults = crate::lock(&self.faults);
        let entry = faults.entry(key.clone()).or_default();
        let now = Instant::now();
        entry.traps.push_back(now);
        while entry
            .traps
            .front()
            .is_some_and(|t| now.duration_since(*t) > FAULT_WINDOW)
        {
            entry.traps.pop_front();
        }
        if entry.traps.len() >= FAULT_LIMIT && entry.faulted.is_none() {
            entry.faulted = Some(format!(
                "{FAULT_LIMIT} failures within a minute; the last: {why}"
            ));
            tracing::warn!(plugin = %m.id, workspace = %key.1, "plugin faulted in this workspace");
        }
    }

    /// One call into `m`'s instance for `ws`, as `session` (if any): the
    /// fault gate, lazy instantiation, the armed deadline, trap handling.
    async fn run(
        &self,
        state: &Arc<AppState>,
        m: &Manifest,
        ws: &str,
        session: Option<&str>,
        call: Call,
    ) -> Result<Reply, String> {
        let key: Key = (m.id.clone(), ws.to_string());
        if let Some(fault) = self.faulted(&key) {
            return Err(format!(
                "{} is faulted in this workspace ({fault}) — the user can switch it off and \
                 on again to retry",
                m.name
            ));
        }
        if let Some(refused) = crate::lock(&self.refused).get(&offer_key(m)) {
            return Err(refused.clone());
        }
        let pre = match component(m).await {
            Ok(pre) => pre,
            Err(refused) => {
                crate::lock(&self.refused).insert(offer_key(m), refused.clone());
                return Err(refused);
            }
        };
        let budget = self.budget(&call);
        let cx = wit::Context {
            workspace: ws.to_string(),
            session: session.map(str::to_string),
            mastermind: session.is_some_and(|s| crate::mcp::mastermind_of(state, s)),
        };
        let slot = self.slot(&key)?;
        let mut guard = slot.live.lock().await;
        // An instance of another build (the plugin's `current` moved while
        // this slot lived) is never called again.
        if guard
            .as_ref()
            .is_some_and(|live| live.sha256 != m.wasm.sha256)
        {
            *guard = None;
        }
        if guard.is_none() {
            let permit = self
                .instances
                .0
                .clone()
                .try_acquire_owned()
                .map_err(|_| "all plugin instances are busy — retry shortly".to_string())?;
            let access = super::capabilities::Access::of(m);
            let mut host = HostState::new(&m.id, ws, access);
            host.manifest = Some(Arc::new(m.clone()));
            let mut store = Store::new(&shared()?.engine, host);
            store.limiter(|s| &mut s.limits);
            // Yield to tokio on every tick; stop the guest past its budget.
            store.epoch_deadline_callback(|ctx| {
                Ok(if Instant::now() < ctx.data().deadline {
                    UpdateDeadline::Yield(1)
                } else {
                    UpdateDeadline::Interrupt
                })
            });
            store.data_mut().begin(state, &cx, budget);
            store.set_epoch_deadline(1);
            let made = tokio::time::timeout(budget + HOST_GRACE, async {
                match &pre {
                    Pre::V1(pre) => pre.instantiate_async(&mut store).await.map(Bindings::V1),
                    Pre::V2(pre) => pre.instantiate_async(&mut store).await.map(Bindings::V2),
                }
            })
            .await;
            store.data_mut().end();
            match made {
                Ok(Ok(plugin)) => {
                    *guard = Some(Live {
                        store,
                        plugin,
                        sha256: m.wasm.sha256.clone(),
                        _permit: permit,
                    })
                }
                Ok(Err(err)) => {
                    let why = describe(m, &err, budget, store.data().stderr.last_line());
                    tracing::warn!(plugin = %m.id, workspace = ws, %why, "plugin instantiate failed");
                    self.record_fault(m, &key, &why);
                    return Err(why);
                }
                Err(_) => {
                    let why = format!("{} did not start within its budget", m.name);
                    self.record_fault(m, &key, &why);
                    return Err(why);
                }
            }
        }
        // Out of the slot for the call, back only when it finished: a
        // caller dropped mid-call (a hook claude stopped waiting for, a
        // closed window's request) drops the instance with it, instead of
        // leaving one wasmtime can't enter again to trap the next call.
        let mut live = guard.take().expect("instantiated above");
        live.store.data_mut().begin(state, &cx, budget);
        live.store.set_epoch_deadline(1);
        let outcome = tokio::time::timeout(budget + HOST_GRACE, invoke(&mut live, &cx, call)).await;
        live.store.data_mut().end();
        slot.touch();
        let why = match outcome {
            Ok(Ok(reply)) => {
                *guard = Some(live);
                return Ok(reply);
            }
            Ok(Err(err)) => {
                let stderr = live.store.data().stderr.clone();
                let why = describe(m, &err, budget, stderr.last_line());
                tracing::warn!(
                    plugin = %m.id,
                    workspace = ws,
                    %why,
                    stderr = %stderr.contents(),
                    "plugin call trapped; its instance is dropped"
                );
                why
            }
            Err(_) => {
                let why = format!(
                    "{} did not answer within {} s and was abandoned",
                    m.name,
                    (budget + HOST_GRACE).as_secs()
                );
                tracing::warn!(plugin = %m.id, workspace = ws, "{why}");
                why
            }
        };
        // A trapped (or abandoned) instance is dead (`live` drops here): the
        // next call makes a fresh one.
        drop(live);
        self.record_fault(m, &key, &why);
        Err(why)
    }

    /// What `m` offers agents (its tool definitions as MCP JSON and its
    /// instruction paragraph), asked once per plugin. A component whose tool
    /// names differ from its manifest's `provides.mcp_tools` is refused
    /// everywhere: the card's Adds line and the call gate come from the
    /// manifest, so the two must agree.
    pub(crate) async fn offer(
        &self,
        state: &Arc<AppState>,
        m: &Manifest,
        ws: &str,
    ) -> Result<Offer, String> {
        if let Some(known) = crate::lock(&self.offers).get(&offer_key(m)) {
            return known.clone();
        }
        let Reply::Tools(defs) = self.run(state, m, ws, None, Call::Tools).await? else {
            unreachable!("tools answers Tools")
        };
        let Reply::Instructions(instructions) =
            self.run(state, m, ws, None, Call::Instructions).await?
        else {
            unreachable!("instructions answers Instructions")
        };
        let offer = check_offer(m, defs, instructions);
        if let Err(refused) = &offer {
            tracing::error!(plugin = %m.id, %refused, "plugin refused");
        }
        crate::lock(&self.offers).insert(offer_key(m), offer.clone());
        offer
    }

    /// A tool call, answered in the MCP result shape. The caller already
    /// checked the plugin is active in `ws`.
    pub(crate) async fn call_tool(
        &self,
        state: &Arc<AppState>,
        m: &Manifest,
        ws: &str,
        session: &str,
        name: &str,
        args: &Value,
    ) -> Value {
        if let Err(refused) = self.offer(state, m, ws).await {
            return tool_error(refused);
        }
        let call = Call::Tool(name.to_string(), args.to_string());
        let mut answer = self.run(state, m, ws, Some(session), call).await;
        // A long tool: the call waits (the instance doesn't) for its job,
        // then the plugin gives the final answer; past the hold, its own
        // "still running" text stands.
        if let Ok(Reply::ToolResult(result)) = &answer {
            if let Some(job) = result.wait.clone() {
                if super::jobs::tool_wait(state, &m.id, &job).await {
                    let resume = Call::ToolResume(name.to_string(), job);
                    answer = self.run(state, m, ws, Some(session), resume).await;
                }
            }
        }
        match answer {
            Ok(Reply::Unsupported) => tool_error(format!("{} can't resume a tool", m.name)),
            Ok(Reply::ToolResult(result)) => {
                let text = clip(&result.text, RESULT_MAX);
                if result.is_error {
                    tool_error(text)
                } else {
                    json!({ "content": [{ "type": "text", "text": text }] })
                }
            }
            Ok(_) => unreachable!("call-tool answers ToolResult"),
            Err(err) => tool_error(err),
        }
    }

    /// The Knowledge snapshot from a provider plugin: None when `known` is
    /// still current, else its stamp and its data — or why the data was
    /// refused (over `SNAPSHOT_MAX`, not JSON). A refused snapshot keeps its
    /// stamp, so the caller can hand it back and an unchanged tree is
    /// refused again without being read again.
    pub(crate) async fn knowledge(
        &self,
        state: &Arc<AppState>,
        m: &Manifest,
        ws: &str,
        known: Option<&Value>,
    ) -> Result<Option<(Value, Result<Value, String>)>, String> {
        let call = Call::Knowledge(known.map(Value::to_string));
        let Reply::Knowledge(answer) = self.run(state, m, ws, None, call).await? else {
            unreachable!("knowledge answers Knowledge")
        };
        let Some(snapshot) = answer.map_err(|e| clip(&e, KNOWLEDGE_ERROR_MAX))? else {
            return Ok(None);
        };
        let over = |what: &str, len: usize, max: usize| {
            (len > max).then(|| {
                format!(
                    "{}'s {what} is {:.1} MiB, over the {} MiB it may be",
                    m.name,
                    len as f64 / f64::from(1 << 20),
                    max >> 20
                )
            })
        };
        if let Some(why) = over("stamp", snapshot.stamp.len(), STAMP_MAX) {
            return Err(why);
        }
        let too_big = over("snapshot", snapshot.data.len(), SNAPSHOT_MAX);
        // Megabytes of JSON: parsed on the blocking pool.
        let name = m.name.clone();
        tokio::task::spawn_blocking(move || {
            let parse = |what: &str, text: &str| {
                serde_json::from_str::<Value>(text)
                    .map_err(|e| format!("{name}: the snapshot's {what} is not JSON ({e})"))
            };
            let stamp = parse("stamp", &snapshot.stamp)?;
            let data = match too_big {
                Some(why) => Err(why),
                None => parse("data", &snapshot.data),
            };
            Ok(Some((stamp, data)))
        })
        .await
        .map_err(|e| format!("{}: reading its snapshot failed ({e})", m.name))?
    }

    /// Tell `m` something happened; a hook event may get one line back for
    /// the agent. Failures are logged and counted, never surfaced.
    pub(crate) async fn on_event(
        &self,
        state: &Arc<AppState>,
        m: &Manifest,
        ws: &str,
        session: Option<&str>,
        event: wit::Event,
    ) -> Option<String> {
        match self.run(state, m, ws, session, Call::OnEvent(event)).await {
            Ok(Reply::Event(line)) => line
                .map(|l| clip(&l, HOOK_LINE_MAX))
                .filter(|l| !l.is_empty()),
            Ok(_) => unreachable!("on-event answers Event"),
            Err(err) => {
                tracing::debug!(plugin = %m.id, %err, "plugin event not handled");
                None
            }
        }
    }

    /// A 0.2 export that answers JSON text (`render`, `on-action`, `query`).
    async fn json_call(
        &self,
        state: &Arc<AppState>,
        m: &Manifest,
        ws: &str,
        call: Call,
    ) -> Result<String, String> {
        match self.run(state, m, ws, None, call).await? {
            Reply::Json(answer) => answer.map_err(|e| clip(&e, KNOWLEDGE_ERROR_MAX)),
            Reply::Unsupported => Err(format!(
                "{} is built for plugin API {}, which has no screens",
                m.name, m.api
            )),
            _ => unreachable!("render, on-action and query answer Json"),
        }
    }

    /// A view's tree (JSON text), unchecked (`screens::check_tree`).
    pub(crate) async fn render(
        &self,
        state: &Arc<AppState>,
        m: &Manifest,
        ws: &str,
        view: &str,
        args: &str,
    ) -> Result<String, String> {
        let call = Call::Render(view.to_string(), args.to_string());
        self.json_call(state, m, ws, call).await
    }

    /// A node's action: the view's new tree, or `null`.
    pub(crate) async fn on_action(
        &self,
        state: &Arc<AppState>,
        m: &Manifest,
        ws: &str,
        view: &str,
        action: &str,
        payload: &str,
    ) -> Result<String, String> {
        let call = Call::Action(view.to_string(), action.to_string(), payload.to_string());
        self.json_call(state, m, ws, call).await
    }

    /// A read the UI makes (`query`, 0.1 and 0.2).
    pub(crate) async fn query(
        &self,
        state: &Arc<AppState>,
        m: &Manifest,
        ws: &str,
        name: &str,
        args: &str,
    ) -> Result<String, String> {
        let call = Call::Query(name.to_string(), args.to_string());
        self.json_call(state, m, ws, call).await
    }

    /// Record a frame for the `/ws/events` clients showing `workspace`
    /// (`emit`, and the platform's `surface` and `view` frames).
    pub(crate) fn push_event(&self, workspace: &str, frame: String) {
        let mut events = crate::lock(&self.events);
        events.next += 1;
        let id = events.next;
        events
            .ring
            .push_back((id, Arc::from(workspace), Arc::from(frame)));
        while events.ring.len() > EVENTS_KEPT {
            events.ring.pop_front();
        }
    }

    /// The newest `emit` frame id: a (re)connecting client starts here.
    pub(crate) fn events_head(&self) -> u64 {
        crate::lock(&self.events).next
    }

    /// Frames newer than `last` for one client showing `workspace`,
    /// advancing its mark: a plugin's frames reach only the windows on its
    /// workspace (none, for a window on no workspace).
    pub(crate) fn events_since(&self, last: &mut u64, workspace: Option<&str>) -> Vec<Arc<str>> {
        let events = crate::lock(&self.events);
        let frames: Vec<Arc<str>> = events
            .ring
            .iter()
            .filter(|(id, ws, _)| *id > *last && Some(&**ws) == workspace)
            .map(|(_, _, f)| f.clone())
            .collect();
        *last = events.next;
        frames
    }
}

fn tool_error(text: String) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": true })
}

fn offer_key(m: &Manifest) -> BuildKey {
    (m.id.clone(), m.wasm.sha256.clone())
}

fn check_offer(
    m: &Manifest,
    defs: Vec<wit::ToolDef>,
    instructions: Option<String>,
) -> Result<Offer, String> {
    let mut offered: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    let mut declared: Vec<&str> = m.provides.mcp_tools.iter().map(String::as_str).collect();
    offered.sort_unstable();
    declared.sort_unstable();
    if offered != declared {
        return Err(format!(
            "{} is refused: its component offers the tools [{}] but its manifest names [{}]",
            m.name,
            offered.join(", "),
            declared.join(", ")
        ));
    }
    let mut tools = Vec::with_capacity(defs.len());
    for def in defs {
        if def.input_schema.len() > TOOL_SCHEMA_MAX {
            return Err(format!(
                "{} is refused: the input schema of {} is over {} KiB",
                m.name,
                def.name,
                TOOL_SCHEMA_MAX / 1024
            ));
        }
        let schema: Value = serde_json::from_str(&def.input_schema).map_err(|e| {
            format!(
                "{} is refused: the input schema of {} is not JSON ({e})",
                m.name, def.name
            )
        })?;
        // MCP clients validate the whole `tools/list`: one schema that isn't
        // an object schema loses the agent every chimaera tool, not just
        // this plugin's.
        if schema.get("type").and_then(Value::as_str) != Some("object") {
            return Err(format!(
                "{} is refused: the input schema of {} is not a JSON object schema \
                 (`\"type\": \"object\"`)",
                m.name, def.name
            ));
        }
        tools.push(json!({
            "name": def.name,
            "description": clip(&def.description, TOOL_DESCRIPTION_MAX),
            "inputSchema": schema,
        }));
    }
    Ok(Offer {
        tools,
        instructions: instructions.map(|i| clip(&i, INSTRUCTIONS_MAX)),
    })
}

/// A hook the agent fired (`SessionStart`, `UserPromptSubmit`): each active
/// plugin that declared `hook` may add one line to the hook's context.
pub(crate) async fn hook(state: &Arc<AppState>, session: &str, event: &str) -> Vec<String> {
    let Some(ws) = super::workspace_of_session(state, session) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for m in super::active(state, &ws).await {
        if !m.provides.hears(super::EventKind::Hook) {
            continue;
        }
        let ev = wit::Event::Hook(wit::Hook {
            session: session.to_string(),
            name: event.to_string(),
        });
        if let Some(line) = state
            .plugin_runtime
            .on_event(state, &m, &ws, Some(session), ev)
            .await
        {
            lines.push(line);
        }
    }
    lines
}

/// A session ended for good: the active plugins that declared
/// `session-ended` and keep state in its workspace hear it (so they can
/// drop what they kept for it).
/// Resolved now — the caller is about to drop the session's workspace
/// mapping — and delivered off the caller's path.
pub(crate) fn session_ended(state: &Arc<AppState>, session: &str) {
    let Some(ws) = super::workspace_of_session(state, session) else {
        return;
    };
    let state = state.clone();
    let session = session.to_string();
    tokio::spawn(async move {
        for m in super::active(&state, &ws).await {
            // A plugin that keeps nothing here has nothing to forget; don't
            // instantiate it just to say so.
            if !m.provides.hears(super::EventKind::SessionEnded)
                || !crate::lock(&state.plugin_state).holds(&m.id, &ws)
            {
                continue;
            }
            let ev = wit::Event::SessionEnded(session.clone());
            state
                .plugin_runtime
                .on_event(&state, &m, &ws, None, ev)
                .await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_busy_slots_keep_the_same_instance_for_queued_calls() {
        let runtime = PluginRuntime::default();
        let key = ("test".into(), "0".into());
        let first = runtime.slot(&key).unwrap();
        let held: Vec<_> = (1..MAX_INSTANCES)
            .map(|i| runtime.slot(&("test".into(), i.to_string())).unwrap())
            .collect();
        assert!(runtime.slot(&("test".into(), "overflow".into())).is_err());
        let again = runtime.slot(&key).unwrap();
        assert!(
            Arc::ptr_eq(&first, &again),
            "an in-flight slot must not be evicted"
        );
        drop(held);
        assert!(runtime.slot(&("test".into(), "overflow".into())).is_ok());
    }

    #[test]
    fn review_memory_budget_is_shared_by_all_memories_in_a_store() {
        let mut store = Store::new(
            &shared().unwrap().engine,
            HostState::new("test", "ws", crate::plugins::capabilities::Access::NONE),
        );
        store.limiter(|s| &mut s.limits);
        let forty_mb = wasmtime::MemoryType::new(640, None);
        assert!(wasmtime::Memory::new(&mut store, forty_mb.clone()).is_ok());
        assert!(
            wasmtime::Memory::new(&mut store, forty_mb).is_err(),
            "two memories must not bypass the 64 MiB budget"
        );
    }

    #[test]
    fn review_table_budget_is_shared_by_all_tables_in_a_store() {
        let mut store = Store::new(
            &shared().unwrap().engine,
            HostState::new("test", "ws", crate::plugins::capabilities::Access::NONE),
        );
        store.limiter(|s| &mut s.limits);
        let table = wasmtime::TableType::new(wasmtime::RefType::FUNCREF, 40_000, None);
        assert!(wasmtime::Table::new(&mut store, table.clone(), wasmtime::Ref::Func(None)).is_ok());
        assert!(wasmtime::Table::new(&mut store, table, wasmtime::Ref::Func(None)).is_err());
    }

    /// MCP clients validate the whole `tools/list`: a plugin whose schema
    /// isn't an object schema is refused, so it can't cost agents every
    /// chimaera tool; long descriptions are clipped.
    #[test]
    fn a_tool_offer_needs_object_schemas() {
        let m = crate::plugins::test_catalog::fixture();
        let defs = |schema: &str, description: &str| -> Vec<wit::ToolDef> {
            m.provides
                .mcp_tools
                .iter()
                .map(|name| wit::ToolDef {
                    name: name.clone(),
                    description: description.to_string(),
                    input_schema: schema.to_string(),
                })
                .collect()
        };
        let offer = check_offer(&m, defs(r#"{"type":"object"}"#, &"d".repeat(5000)), None).unwrap();
        assert_eq!(offer.tools.len(), m.provides.mcp_tools.len());
        assert!(offer.tools[0]["description"].as_str().unwrap().len() <= TOOL_DESCRIPTION_MAX);
        for bad in [
            r#""string""#,
            r#"[]"#,
            r#"{"properties":{}}"#,
            r#"{"type":"array"}"#,
        ] {
            let err = check_offer(&m, defs(bad, "x"), None).err().unwrap();
            assert!(err.contains("object schema"), "{bad}: {err}");
        }
        let huge = format!(
            r#"{{"type":"object","description":"{}"}}"#,
            "x".repeat(TOOL_SCHEMA_MAX)
        );
        assert!(check_offer(&m, defs(&huge, "x"), None).is_err());
    }

    #[test]
    fn review_failed_allocations_refund_only_their_own_budget() {
        let mut budget = AllocationBudget::default();
        assert!(budget.grow(0, 10, None, 64));
        assert!(budget.grow(0, 20, None, 64));
        budget.failed();
        assert_eq!(budget.used, 10);
        assert!(!budget.grow(0, 20, Some(15), 64));
        budget.failed();
        assert_eq!(budget.used, 10);
        assert!(budget.grow(10, 64, None, 64));
        assert!(!budget.grow(0, 1, None, 64));
    }
}
