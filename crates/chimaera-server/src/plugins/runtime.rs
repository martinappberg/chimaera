//! The plugin host's runtime: WASM components (`chimaera:plugin`, the WIT in
//! `crates/chimaera-plugin-api/wit`) under wasmtime, one sandboxed instance
//! per (plugin, workspace). Design: docs/plugin-system-plan.md ("The host").
//!
//! - **One engine per process**, built on first use (Cranelift, epoch
//!   interruption, a 64 MiB virtual reservation per memory instead of 4 GiB:
//!   login nodes run under `ulimit -v`). Each component compiles once per
//!   daemon lifetime, off the reactor, and stays in memory.
//! - **One ticker thread** bumps the engine's epoch every 100 ms; a store's
//!   deadline callback yields to tokio on each tick and interrupts the guest
//!   once the call's wall-clock budget is spent. The deadline starts at 0, so
//!   it is armed before every call (an unarmed store traps at once).
//! - **Instances** are created lazily, one call at a time each (an async
//!   mutex), at most `MAX_INSTANCES` daemon-wide (least recently used goes),
//!   and dropped after `IDLE` (swept on access — no polling task).
//! - **A trap costs the instance, never the daemon**: the store is dropped
//!   and re-created on the next call (~20 µs). `FAULT_LIMIT` traps within
//!   `FAULT_WINDOW` mark the plugin faulted in that workspace until the user
//!   switches it off and on.
//! - Linear memory is capped per instance (`MEMORY_CAP`); WASI grants
//!   nothing (no files, env, args, network); the guest's stderr is kept
//!   (last `STDERR_CAP` bytes) only to log why a call trapped.
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
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder, UpdateDeadline};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use super::{Manifest, Source};
use crate::AppState;

wasmtime::component::bindgen!({
    world: "chimaera-plugin",
    path: "../chimaera-plugin-api/wit",
    imports: { default: async },
    exports: { default: async },
});

pub(crate) use chimaera::plugin::types as wit;

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
const MAX_INSTANCES: usize = 64;
const IDLE: Duration = Duration::from_secs(10 * 60);
const FAULT_LIMIT: usize = 5;
const FAULT_WINDOW: Duration = Duration::from_secs(60);
/// The guest's stderr kept per instance (its tail; a panic message).
const STDERR_CAP: usize = 4 * 1024;
/// What a plugin may hand back, whatever it returns: a tool result's text,
/// the instruction paragraph, a hook line.
const RESULT_MAX: usize = 256 * 1024;
const INSTRUCTIONS_MAX: usize = 8 * 1024;
const HOOK_LINE_MAX: usize = 1024;
/// `emit` frames kept for the `/ws/events` clients, and one frame's size.
const EVENTS_KEPT: usize = 64;
pub(crate) const EVENT_MAX: usize = 16 * 1024;

/// The process-wide half: engine, linker, compiled components. A test
/// process builds many `AppState`s; they share this, as one daemon would.
struct Shared {
    engine: Engine,
    linker: Linker<HostState>,
    /// plugin id → its pre-instantiated component (or why it can't be).
    compiled: tokio::sync::Mutex<HashMap<String, Result<ChimaeraPluginPre<HostState>, String>>>,
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
    let engine = Engine::new(&config)?;
    let mut linker: Linker<HostState> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    ChimaeraPlugin::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
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
        compiled: tokio::sync::Mutex::new(HashMap::new()),
    })
}

/// The component for `m`, compiled on first use (Cranelift: tens of ms of
/// CPU, so on the blocking pool) and kept for the daemon's lifetime.
async fn component(m: &'static Manifest) -> Result<ChimaeraPluginPre<HostState>, String> {
    let Source::Wasm(bytes) = &m.source else {
        return Err(format!("{} is not a WASM plugin", m.name));
    };
    let shared = shared()?;
    let mut compiled = shared.compiled.lock().await;
    if let Some(done) = compiled.get(&m.id) {
        return done.clone();
    }
    let engine = shared.engine.clone();
    let linker = shared.linker.clone();
    let name = m.name.clone();
    let started = Instant::now();
    let result = tokio::task::spawn_blocking(move || {
        let component = Component::new(&engine, bytes)?;
        ChimaeraPluginPre::new(linker.instantiate_pre(&component)?)
    })
    .await
    .map_err(|e| format!("{name}: compile task failed: {e}"))
    .and_then(|r| r.map_err(|e| format!("{name} is not a component this host can run: {e:#}")));
    match &result {
        Ok(_) => {
            tracing::info!(plugin = %m.id, ms = started.elapsed().as_millis() as u64, "plugin compiled")
        }
        Err(err) => tracing::error!(plugin = %m.id, %err, "plugin refused"),
    }
    compiled.insert(m.id.clone(), result.clone());
    result
}

/// A store's data: WASI (nothing granted), the limits, and — for the length
/// of one call only — the daemon state the host functions serve from.
pub(crate) struct HostState {
    wasi: WasiCtx,
    table: ResourceTable,
    limits: StoreLimits,
    stderr: StderrTail,
    deadline: Instant,
    pub(super) plugin: String,
    pub(super) workspace: String,
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
    fn new(plugin: &str, workspace: &str) -> Self {
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
            limits: StoreLimitsBuilder::new().memory_size(MEMORY_CAP).build(),
            stderr,
            deadline: Instant::now(),
            plugin: plugin.to_string(),
            workspace: workspace.to_string(),
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
fn clip(text: &str, max: usize) -> String {
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

/// A live instance: its store and the bindings into it.
struct Live {
    store: Store<HostState>,
    plugin: ChimaeraPlugin,
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
    ring: VecDeque<(u64, Arc<str>)>,
}

type Key = (String, String);

/// The per-daemon half of the runtime (on `AppState`).
#[derive(Default)]
pub(crate) struct PluginRuntime {
    slots: Mutex<HashMap<Key, Arc<Slot>>>,
    faults: Mutex<HashMap<Key, Faults>>,
    /// plugin id → its offer, or why it is refused everywhere (a component
    /// whose tools don't match its manifest).
    offers: Mutex<HashMap<String, Result<Offer, String>>>,
    events: Mutex<Events>,
    /// Tests only: a shorter call budget, in ms (0 = the real ones).
    budget_override_ms: AtomicU64,
}

enum Call {
    Tools,
    Instructions,
    Tool(String, String),
    Knowledge(Option<String>),
    OnEvent(wit::Event),
}

enum Reply {
    Tools(Vec<wit::ToolDef>),
    Instructions(Option<String>),
    ToolResult(wit::ToolResult),
    Knowledge(Result<Option<wit::Snapshot>, String>),
    Event(Option<String>),
}

async fn invoke(live: &mut Live, cx: &wit::Context, call: Call) -> wasmtime::Result<Reply> {
    let exports = live.plugin.chimaera_plugin_plugin();
    let store = &mut live.store;
    Ok(match call {
        Call::Tools => Reply::Tools(exports.call_tools(store).await?),
        Call::Instructions => Reply::Instructions(exports.call_instructions(store).await?),
        Call::Tool(name, args) => {
            Reply::ToolResult(exports.call_call_tool(store, cx, &name, &args).await?)
        }
        Call::Knowledge(known) => {
            Reply::Knowledge(exports.call_knowledge(store, cx, known.as_ref()).await?)
        }
        Call::OnEvent(event) => Reply::Event(exports.call_on_event(store, cx, &event).await?),
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
            0 if matches!(call, Call::Knowledge(_)) => KNOWLEDGE_BUDGET,
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
        if let Some(Err(refused)) = crate::lock(&self.offers).get(&m.id) {
            return Some(refused.clone());
        }
        crate::lock(&self.faults)
            .get(&(m.id.clone(), ws.to_string()))
            .and_then(|f| f.faulted.clone())
    }

    /// Start `plugin` over in `ws`: drop its instance and clear its fault
    /// (the plugin's switch flipped).
    pub(crate) fn reset(&self, plugin: &str, ws: &str) {
        let key = (plugin.to_string(), ws.to_string());
        crate::lock(&self.slots).remove(&key);
        crate::lock(&self.faults).remove(&key);
    }

    /// A deleted workspace: every plugin's instance and fault there.
    pub(crate) fn forget_workspace(&self, ws: &str) {
        crate::lock(&self.slots).retain(|(_, w), _| w != ws);
        crate::lock(&self.faults).retain(|(_, w), _| w != ws);
    }

    /// The instance slot for `key`, making room: idle slots go (the sweep
    /// runs on every access, so no timer is needed), then — at the cap —
    /// the least recently used one. A slot with a call in flight is held by
    /// that call too and finishes it before its store is dropped.
    fn slot(&self, key: &Key) -> Arc<Slot> {
        let mut slots = crate::lock(&self.slots);
        slots.retain(|_, s| s.idle_for() < IDLE);
        if let Some(slot) = slots.get(key) {
            slot.touch();
            return slot.clone();
        }
        if slots.len() >= MAX_INSTANCES {
            let lru = slots
                .iter()
                .max_by_key(|(_, s)| s.idle_for())
                .map(|(k, _)| k.clone());
            if let Some(lru) = lru {
                slots.remove(&lru);
            }
        }
        let slot = Arc::new(Slot {
            live: tokio::sync::Mutex::new(None),
            last_used: Mutex::new(Instant::now()),
        });
        slots.insert(key.clone(), slot.clone());
        slot
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
        m: &'static Manifest,
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
        let pre = component(m).await?;
        let budget = self.budget(&call);
        let cx = wit::Context {
            workspace: ws.to_string(),
            session: session.map(str::to_string),
            mastermind: session.is_some_and(|s| crate::mcp::mastermind_of(state, s)),
        };
        let slot = self.slot(&key);
        let mut guard = slot.live.lock().await;
        if guard.is_none() {
            let mut store = Store::new(&shared()?.engine, HostState::new(&m.id, ws));
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
            let made =
                tokio::time::timeout(budget + HOST_GRACE, pre.instantiate_async(&mut store)).await;
            store.data_mut().end();
            match made {
                Ok(Ok(plugin)) => *guard = Some(Live { store, plugin }),
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
        let live = guard.as_mut().expect("instantiated above");
        live.store.data_mut().begin(state, &cx, budget);
        live.store.set_epoch_deadline(1);
        let outcome = tokio::time::timeout(budget + HOST_GRACE, invoke(live, &cx, call)).await;
        live.store.data_mut().end();
        slot.touch();
        let why = match outcome {
            Ok(Ok(reply)) => return Ok(reply),
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
        // A trapped (or abandoned) instance is dead: the next call makes a
        // fresh one.
        *guard = None;
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
        m: &'static Manifest,
        ws: &str,
    ) -> Result<Offer, String> {
        if let Some(known) = crate::lock(&self.offers).get(&m.id) {
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
        crate::lock(&self.offers).insert(m.id.clone(), offer.clone());
        offer
    }

    /// A tool call, answered in the MCP result shape. The caller already
    /// checked the plugin is active in `ws`.
    pub(crate) async fn call_tool(
        &self,
        state: &Arc<AppState>,
        m: &'static Manifest,
        ws: &str,
        session: &str,
        name: &str,
        args: &Value,
    ) -> Value {
        if let Err(refused) = self.offer(state, m, ws).await {
            return tool_error(refused);
        }
        let call = Call::Tool(name.to_string(), args.to_string());
        match self.run(state, m, ws, Some(session), call).await {
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

    /// The Knowledge snapshot `(stamp, data)` from a provider plugin, or
    /// None when `known` is still current.
    #[allow(dead_code)] // P2: knowledge.rs asks the provider plugin through this.
    pub(crate) async fn knowledge(
        &self,
        state: &Arc<AppState>,
        m: &'static Manifest,
        ws: &str,
        known: Option<&Value>,
    ) -> Result<Option<(Value, Value)>, String> {
        let call = Call::Knowledge(known.map(Value::to_string));
        let Reply::Knowledge(answer) = self.run(state, m, ws, None, call).await? else {
            unreachable!("knowledge answers Knowledge")
        };
        let Some(snapshot) = answer? else {
            return Ok(None);
        };
        let parse = |what: &str, text: &str| {
            serde_json::from_str::<Value>(text)
                .map_err(|e| format!("{}: the snapshot's {what} is not JSON ({e})", m.name))
        };
        Ok(Some((
            parse("stamp", &snapshot.stamp)?,
            parse("data", &snapshot.data)?,
        )))
    }

    /// Tell `m` something happened; a hook event may get one line back for
    /// the agent. Failures are logged and counted, never surfaced.
    pub(crate) async fn on_event(
        &self,
        state: &Arc<AppState>,
        m: &'static Manifest,
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

    /// Record an `emit` frame for the `/ws/events` clients.
    pub(super) fn push_event(&self, frame: String) {
        let mut events = crate::lock(&self.events);
        events.next += 1;
        let id = events.next;
        events.ring.push_back((id, Arc::from(frame)));
        while events.ring.len() > EVENTS_KEPT {
            events.ring.pop_front();
        }
    }

    /// The newest `emit` frame id: a (re)connecting client starts here.
    pub(crate) fn events_head(&self) -> u64 {
        crate::lock(&self.events).next
    }

    /// Frames newer than `last` for one client, advancing its mark.
    pub(crate) fn events_since(&self, last: &mut u64) -> Vec<Arc<str>> {
        let events = crate::lock(&self.events);
        let frames: Vec<Arc<str>> = events
            .ring
            .iter()
            .filter(|(id, _)| *id > *last)
            .map(|(_, f)| f.clone())
            .collect();
        *last = events.next;
        frames
    }
}

fn tool_error(text: String) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": true })
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
        let schema: Value = serde_json::from_str(&def.input_schema).map_err(|e| {
            format!(
                "{} is refused: the input schema of {} is not JSON ({e})",
                m.name, def.name
            )
        })?;
        tools.push(json!({
            "name": def.name,
            "description": def.description,
            "inputSchema": schema,
        }));
    }
    Ok(Offer {
        tools,
        instructions: instructions.map(|i| clip(&i, INSTRUCTIONS_MAX)),
    })
}

/// The WASM plugins active in the session's workspace (none without one).
async fn active_wasm(state: &AppState, ws: &str) -> Vec<&'static Manifest> {
    super::active(state, ws)
        .await
        .into_iter()
        .filter(|m| m.is_wasm())
        .collect()
}

/// A hook the agent fired (`SessionStart`, `UserPromptSubmit`): each active
/// plugin may add one line to the hook's context.
pub(crate) async fn hook(state: &Arc<AppState>, session: &str, event: &str) -> Vec<String> {
    let Some(ws) = super::workspace_of_session(state, session) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for m in active_wasm(state, &ws).await {
        let ev = wit::Event::Hook(wit::Hook {
            session: session.to_string(),
            name: event.to_string(),
        });
        if let Some(line) = state
            .plugin_runtime
            .on_event(state, m, &ws, Some(session), ev)
            .await
        {
            lines.push(line);
        }
    }
    lines
}

/// A session ended for good: the active plugins that keep state in its
/// workspace hear `session-ended` (so they can drop what they kept for it).
/// Resolved now — the caller is about to drop the session's workspace
/// mapping — and delivered off the caller's path.
pub(crate) fn session_ended(state: &Arc<AppState>, session: &str) {
    let Some(ws) = super::workspace_of_session(state, session) else {
        return;
    };
    let state = state.clone();
    let session = session.to_string();
    tokio::spawn(async move {
        for m in active_wasm(&state, &ws).await {
            // A plugin that keeps nothing here has nothing to forget; don't
            // instantiate it just to say so.
            if !crate::lock(&state.plugin_state).holds(&m.id, &ws) {
                continue;
            }
            let ev = wit::Event::SessionEnded(session.clone());
            state
                .plugin_runtime
                .on_event(&state, m, &ws, None, ev)
                .await;
        }
    });
}
