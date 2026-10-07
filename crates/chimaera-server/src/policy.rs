//! The one workspace-admission hook shared daemon code consults. A composed
//! extension installs its own [`WorkspacePolicy`]; without one the daemon
//! uses [`Inert`]: everything is admitted, nothing is recorded, no timer or
//! connection starts and no state file is written. The only thing an
//! extension-less daemon still honours is the durable fence an earlier
//! composed daemon may have left ([`fence`]), so a project that runs on
//! another machine is not run twice, while its files stay readable here.
//!
//! Shared code never names the extension: it asks this module.
use std::{any::Any, future::Future, pin::Pin, sync::Arc};

use crate::AppState;

pub(crate) mod fence;

pub(crate) type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a caller wants to do in a workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Need {
    /// Start or drive an agent.
    Execute,
    /// Start or type into a plain shell.
    Shell,
    /// Bring back a session a previous daemon life left.
    Restore,
}

/// The kind of process a launch starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LaunchKind {
    Agent,
    Shell,
}

/// The admission changed between capture and use. The one error type
/// callers match on (`err.is::<Changed>()`).
#[derive(Debug)]
pub(crate) struct Changed;
impl std::fmt::Display for Changed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("workspace execution authority changed")
    }
}
impl std::error::Error for Changed {}

/// A counted reservation held until a commit or child registration is
/// done; dropping it releases. Opaque to shared code.
pub(crate) struct Reservation {
    _held: Box<dyn Any + Send + Sync>,
}
impl Reservation {
    pub(crate) fn new(inner: impl Any + Send + Sync) -> Self {
        Self {
            _held: Box::new(inner),
        }
    }
}

/// A short synchronous hold (it may own a lock guard) kept until a child is
/// registered or a read finished; never held across an await.
pub(crate) struct Hold<'a> {
    _held: Option<Box<dyn Held + 'a>>,
}
pub(crate) trait Held {}
impl<T> Held for T {}
impl<'a> Hold<'a> {
    pub(crate) fn none() -> Self {
        Self { _held: None }
    }
    pub(crate) fn new(inner: impl Held + 'a) -> Self {
        Self {
            _held: Some(Box::new(inner)),
        }
    }
}

/// An admission captured before asynchronous work and re-checked at the
/// final commit. `None` inside is the inert admission: it only re-asks
/// [`Need::Execute`].
#[derive(Clone)]
pub(crate) struct Admission {
    workspace: String,
    token: Option<Arc<dyn AdmissionToken>>,
}
pub(crate) trait AdmissionToken: Send + Sync {
    fn check(&self, state: &AppState) -> anyhow::Result<()>;
    fn begin(&self, state: &AppState) -> anyhow::Result<Option<Reservation>>;
    /// The workspace's agents run under the policy's process ownership.
    fn managed(&self, state: &AppState) -> bool;
    fn installer<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        workspace: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<Installer>>;
}
impl Admission {
    pub(crate) fn inert(workspace: &str) -> Self {
        Self {
            workspace: workspace.to_owned(),
            token: None,
        }
    }
    pub(crate) fn with(workspace: &str, token: Arc<dyn AdmissionToken>) -> Self {
        Self {
            workspace: workspace.to_owned(),
            token: Some(token),
        }
    }
    pub(crate) fn check(&self, state: &AppState) -> anyhow::Result<()> {
        match &self.token {
            Some(token) => token.check(state),
            None if state.policy().allows(state, &self.workspace, Need::Execute) => Ok(()),
            None => Err(Changed.into()),
        }
    }
    pub(crate) fn managed(&self, state: &AppState) -> bool {
        self.token
            .as_ref()
            .is_some_and(|token| token.managed(state))
    }
    /// Check, then reserve the final dispatch.
    pub(crate) fn begin(&self, state: &AppState) -> anyhow::Result<Option<Reservation>> {
        match &self.token {
            Some(token) => token.begin(state),
            None => self.check(state).map(|()| None),
        }
    }
    /// Admit an installer child under this admission.
    pub(crate) async fn installer(
        &self,
        state: &Arc<AppState>,
        workspace: &str,
    ) -> anyhow::Result<Installer> {
        match &self.token {
            Some(token) => token.installer(state, workspace).await,
            None => {
                self.check(state)?;
                Ok(Installer {
                    admission: self.clone(),
                    state: state.clone(),
                    token: None,
                })
            }
        }
    }
}

/// An admitted installer process; its cleanup stays counted until finished.
pub(crate) struct Installer {
    admission: Admission,
    state: Arc<AppState>,
    token: Option<Box<dyn InstallerToken>>,
}
pub(crate) trait InstallerToken: Send + Sync {
    /// Attach the spawned process group synchronously after spawn.
    fn attach(&mut self, group: u32);
    fn finish(self: Box<Self>) -> BoxFuture<'static, anyhow::Result<()>>;
    /// The installer holds a setup reservation whose process group must be
    /// drained on success.
    fn guarded(&self) -> bool;
}
impl Installer {
    pub(crate) fn with(
        admission: Admission,
        state: Arc<AppState>,
        token: Box<dyn InstallerToken>,
    ) -> Self {
        Self {
            admission,
            state,
            token: Some(token),
        }
    }
    pub(crate) fn captured(&self) -> Admission {
        self.admission.clone()
    }
    pub(crate) fn check(&self) -> anyhow::Result<()> {
        self.admission.check(&self.state)
    }
    pub(crate) fn guarded(&self) -> bool {
        self.token.as_ref().is_some_and(|token| token.guarded())
    }
    pub(crate) fn attach(&mut self, group: u32) {
        if let Some(token) = &mut self.token {
            token.attach(group);
        }
    }
    pub(crate) async fn finish(mut self) -> anyhow::Result<()> {
        match self.token.take() {
            Some(token) => token.finish().await,
            None => Ok(()),
        }
    }
}

/// One admitted launch, held until its child is registered.
pub(crate) struct Launch {
    token: Option<Box<dyn LaunchToken>>,
}
pub(crate) trait LaunchToken: Send + Sync {
    /// The child runs under the policy's process ownership (a fenceable,
    /// counted process group).
    fn managed(&self) -> bool;
    fn check(&self) -> anyhow::Result<()>;
    fn registered(self: Box<Self>, id: String);
}
impl Launch {
    pub(crate) fn inert() -> Self {
        Self { token: None }
    }
    pub(crate) fn with(token: Box<dyn LaunchToken>) -> Self {
        Self { token: Some(token) }
    }
    pub(crate) fn managed(&self) -> bool {
        self.token.as_ref().is_some_and(|token| token.managed())
    }
    pub(crate) fn check(&self) -> anyhow::Result<()> {
        self.token.as_ref().map_or(Ok(()), |token| token.check())
    }
    pub(crate) fn registered(self, id: String) {
        if let Some(token) = self.token {
            token.registered(id);
        }
    }
}

/// What a launch adds to an agent's start, beyond its environment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LaunchContext {
    /// The agent continues work an interrupted earlier run left: its pick-up
    /// says to check the files first.
    pub(crate) recovery: bool,
    /// The project's agents get the daemon's tools even in a terminal agent
    /// that otherwise has none.
    pub(crate) tools: bool,
}

/// A frame a project that runs on another daemon sends the window that
/// watches it, in place of this daemon's own file, git and timeline
/// watching.
pub(crate) enum ProjectFrame {
    /// A path-only invalidation, already in this window's paths.
    Fs(serde_json::Value),
    /// The project's Git epoch where it runs.
    Git(u64),
    /// The project's Timeline epoch where it runs.
    Timeline(u64),
}

/// The live view of a routed project for one window (`routed`): ends when
/// the project stops being served elsewhere or its owner refuses it.
pub(crate) trait ProjectFeed: Send {
    fn workspace(&self) -> &str;
    /// The window's mounted previews and listed folders, in its own paths.
    fn watch(&self, files: Vec<String>, dirs: Vec<String>);
    /// `None` once the feed ended.
    fn next(&mut self) -> BoxFuture<'_, Option<ProjectFrame>>;
}

/// A note an agent is told as it starts (once per change of machine), and
/// what the policy remembers once it has been delivered.
pub(crate) struct StartNote {
    pub(crate) text: String,
    /// Identifies the note's substance within one agent process, so two
    /// carriers racing at one start deliver it once.
    pub(crate) digest: u64,
    /// The policy's own record of it, handed back in `note_told`.
    pub(crate) record: Box<dyn Any + Send + Sync>,
}

/// The workspace-admission hook. Implementations are trusted in-process.
/// Every method must be cheap and must not block: shared code calls them on
/// the reactor and on hot paths.
pub(crate) trait WorkspacePolicy: Send + Sync + 'static {
    /// Whether an extension is composed here at all: only then does the
    /// daemon serve extension routes or add extension fields and frames.
    fn composed(&self, state: &AppState) -> bool;
    /// Whether this composition is doing paid work right now; shared code
    /// skips bookkeeping only an active extension reads (input stamps).
    fn active(&self, state: &AppState) -> bool;

    // Admission.
    fn allows(&self, state: &AppState, workspace: &str, need: Need) -> bool;
    /// Reserve a launch or input window synchronously.
    fn reserve(
        &self,
        state: &AppState,
        workspace: &str,
        kind: LaunchKind,
    ) -> anyhow::Result<Option<Reservation>>;
    /// Admit a launch: the final checks, the reservation and, for a managed
    /// agent, its durable launch record.
    fn admit_launch<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        workspace: &'a str,
        kind: LaunchKind,
    ) -> BoxFuture<'a, anyhow::Result<(Launch, Option<Reservation>)>>;
    /// Last check before a resume reads or spawns a session that may come
    /// from an import; the hold lives until the child is registered.
    fn hold_session<'a>(
        &'a self,
        state: &'a AppState,
        workspace: &str,
        session: &str,
        native: Option<&str>,
        read: bool,
    ) -> anyhow::Result<Hold<'a>>;
    /// Refuse a session an unfinished import still names.
    fn check_import(
        &self,
        state: &AppState,
        session: &str,
        native: Option<&str>,
    ) -> anyhow::Result<()>;
    /// Capture the admission an asynchronous dispatch commits under.
    fn capture(&self, state: &AppState, workspace: &str) -> anyhow::Result<Admission>;

    // What a launch carries.
    fn launch_context(&self, state: &AppState, workspace: &str) -> LaunchContext;
    /// The child-process environment overlay, applied at the final spawn
    /// point under the launch's admission.
    fn launch_env<'a>(
        &'a self,
        state: &'a AppState,
        workspace: &'a str,
        env: &'a mut Vec<(String, String)>,
        remove: &'a mut Vec<String>,
    ) -> BoxFuture<'a, anyhow::Result<()>>;
    /// Agent CLIs here are updated with the machine's image, not by
    /// themselves or by the daemon.
    fn updates_managed(&self, state: &AppState) -> bool;
    /// Extra argv for a Codex TUI in `workspace` (the policy's own notify
    /// chain, so it hears the turn end); empty leaves the user's argv alone.
    fn codex_notify_args<'a>(
        &'a self,
        state: &'a AppState,
        workspace: &'a str,
        session: &'a str,
        key: &'a str,
    ) -> BoxFuture<'a, Vec<String>>;
    /// A session's record is gone for good: whatever the policy wrote for
    /// it may go too.
    fn session_retired(&self, state: &AppState, session: &str);

    // Presentation.
    /// Why session `id` has no process here (a frame for its socket and its
    /// row), when the policy knows.
    fn session_pause(
        &self,
        state: &AppState,
        id: &str,
        entry: Option<&crate::ledger::LedgerEntry>,
    ) -> Option<serde_json::Value>;
    /// Where a workspace's work runs, when the policy knows.
    fn owner(&self, state: &AppState, workspace: &str) -> Option<&'static str>;
    /// What a socket answers when input cannot be delivered here.
    fn refusal(&self, state: &AppState, id: &str, watching: bool) -> serde_json::Value;
    /// This machine's own user acted in a workspace.
    fn acted(&self, state: &AppState, workspace: &str);
    /// Additive fields and rows on the shared sessions list; returns rows
    /// another daemon serves, which replace local rows with the same id.
    fn decorate_sessions(
        &self,
        state: &AppState,
        rows: &mut Vec<(u64, serde_json::Value)>,
    ) -> Vec<serde_json::Value>;
    /// Additive fields on a listed workspace.
    fn decorate_workspace(&self, state: &AppState, workspace: &str, value: &mut serde_json::Value);
    /// Additive `/health` fields.
    fn health(&self, state: &AppState, body: &mut serde_json::Value);

    // What a project's agents are told and offered.
    /// Tools the policy offers `session`'s agent; each replaces a plugin
    /// tool of the same name.
    fn tools(&self, state: &AppState, session: &str) -> Vec<serde_json::Value>;
    /// Answer a call to one of [`Self::tools`]; `None` when `name` is not
    /// the policy's.
    fn call_tool<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        session: &'a str,
        name: &'a str,
        args: &'a serde_json::Value,
    ) -> Option<BoxFuture<'a, serde_json::Value>>;
    /// The policy's tools `workspace`'s agents use without asking.
    fn auto_tools(&self, state: &AppState, workspace: &str) -> Vec<String>;
    /// The note `session` (in `workspace`) carries as it starts, when it
    /// has not heard it yet.
    fn start_note<'a>(
        &'a self,
        state: &'a AppState,
        workspace: &'a str,
        session: &'a str,
    ) -> BoxFuture<'a, Option<StartNote>>;
    /// The note was delivered.
    fn note_told<'a>(&'a self, state: &'a AppState, note: &'a StartNote) -> BoxFuture<'a, ()>;

    // Workspaces.
    /// Whether registering a folder reads its identity marker.
    fn reads_folder_identity(&self, state: &AppState) -> bool;
    /// The user opened a registered workspace here.
    fn workspace_known(&self, state: &AppState, workspace: &str);
    /// After a folder registered (`Some(write_marker)`) or a registered
    /// workspace opened again (`None`).
    fn workspace_opened<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        workspace: &'a crate::workspaces::Workspace,
        registered: Option<bool>,
    ) -> BoxFuture<'a, ()>;

    // Lifecycle.
    /// After the state exists, before serving.
    fn started(&self, state: &Arc<AppState>) -> anyhow::Result<()>;
    /// Restore deferred this entry (it is held, not restored).
    fn held_at_boot(&self, state: &AppState, entry: &crate::ledger::LedgerEntry);
    /// Restore finished.
    fn restored(&self, state: &Arc<AppState>);
    /// A deferred session may resume now.
    fn may_resume(&self, state: &AppState, entry: &crate::ledger::LedgerEntry) -> bool;
    /// The last check before a deferred session respawns.
    fn resume_check(
        &self,
        state: &AppState,
        entry: &crate::ledger::LedgerEntry,
    ) -> anyhow::Result<()>;
    /// The shutdown signal arrived.
    fn stopping(&self, state: &AppState);
    /// Daemon shutdown, after the live agents were stopped.
    fn shutdown<'a>(&'a self, state: &'a Arc<AppState>) -> BoxFuture<'a, ()>;
    /// Whether `workspace`'s files are served by another daemon right now,
    /// so a window watches its feed instead of this disk.
    fn routed(&self, state: &AppState, workspace: &str) -> bool;
    /// Of `paths`, the ones outside a routed project's root: this computer's
    /// own files, still watched here.
    fn outside_project(&self, state: &AppState, workspace: &str, paths: &[String]) -> Vec<String>;
    /// Start the feed of a routed project for one window; `None` when the
    /// policy serves no feeds.
    fn project_feed(
        &self,
        state: &Arc<AppState>,
        workspace: &str,
        files: Vec<String>,
        dirs: Vec<String>,
    ) -> Option<Box<dyn ProjectFeed>>;
    /// Serve a session socket from the daemon that owns the session, when
    /// the policy routes it there: `true` once the relay ran to its end,
    /// `false` when the session is this daemon's own.
    fn proxy_socket<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        session: &'a str,
        kind: &'a str,
        options: &'a crate::ws::SocketOptions,
        auth: serde_json::Value,
        socket: &'a mut axum::extract::ws::WebSocket,
    ) -> BoxFuture<'a, bool>;
    /// Deferred sessions of `workspace` are resuming here: rows another
    /// daemon served for it are no longer current.
    fn workspace_resuming(&self, state: &AppState, workspace: &str);
    /// Sessions another daemon serves that wait on a permission decision
    /// (`(session, workspace)`), so a relayed alert stays up until answered.
    fn routed_decisions(&self, state: &AppState) -> Vec<(String, String)>;
    /// Routes served under `/api/v1`, behind the bearer check.
    fn routes(&self, state: &Arc<AppState>) -> axum::Router<Arc<AppState>>;
    /// The policy's own middleware around the authenticated API routes
    /// (inside the bearer check): identity without an extension.
    fn api_layers(
        &self,
        state: &Arc<AppState>,
        api: axum::Router<Arc<AppState>>,
    ) -> axum::Router<Arc<AppState>>;
    /// The same around the ticket and WebSocket routes (outside it).
    fn ticket_layers(
        &self,
        state: &Arc<AppState>,
        routes: axum::Router<Arc<AppState>>,
    ) -> axum::Router<Arc<AppState>>;
    /// The outermost layer over the whole app.
    fn outer_layers(&self, app: axum::Router) -> axum::Router;
}

/// No extension: admit everything, record nothing, run nothing.
pub(crate) struct Inert {
    fence: fence::Fence,
}
impl Inert {
    pub(crate) fn new(fence: fence::Fence) -> Self {
        Self { fence }
    }
}
impl WorkspacePolicy for Inert {
    fn composed(&self, _: &AppState) -> bool {
        false
    }
    fn active(&self, _: &AppState) -> bool {
        false
    }
    fn allows(&self, _: &AppState, workspace: &str, _: Need) -> bool {
        !self.fence.fenced(workspace)
    }
    fn reserve(
        &self,
        state: &AppState,
        workspace: &str,
        kind: LaunchKind,
    ) -> anyhow::Result<Option<Reservation>> {
        let need = match kind {
            LaunchKind::Agent => Need::Execute,
            LaunchKind::Shell => Need::Shell,
        };
        if self.allows(state, workspace, need) {
            Ok(None)
        } else {
            Err(Changed.into())
        }
    }
    fn admit_launch<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        workspace: &'a str,
        kind: LaunchKind,
    ) -> BoxFuture<'a, anyhow::Result<(Launch, Option<Reservation>)>> {
        Box::pin(async move {
            self.reserve(state, workspace, kind)?;
            Ok((Launch::inert(), None))
        })
    }
    fn hold_session<'a>(
        &'a self,
        _: &'a AppState,
        _: &str,
        _: &str,
        _: Option<&str>,
        _: bool,
    ) -> anyhow::Result<Hold<'a>> {
        Ok(Hold::none())
    }
    fn check_import(&self, _: &AppState, _: &str, _: Option<&str>) -> anyhow::Result<()> {
        Ok(())
    }
    fn capture(&self, state: &AppState, workspace: &str) -> anyhow::Result<Admission> {
        let admission = Admission::inert(workspace);
        admission.check(state)?;
        Ok(admission)
    }
    fn launch_context(&self, _: &AppState, _: &str) -> LaunchContext {
        LaunchContext::default()
    }
    fn launch_env<'a>(
        &'a self,
        _: &'a AppState,
        _: &'a str,
        _: &'a mut Vec<(String, String)>,
        _: &'a mut Vec<String>,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn updates_managed(&self, _: &AppState) -> bool {
        false
    }
    fn codex_notify_args<'a>(
        &'a self,
        _: &'a AppState,
        _: &'a str,
        _: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, Vec<String>> {
        Box::pin(async { Vec::new() })
    }
    fn session_retired(&self, _: &AppState, _: &str) {}
    fn session_pause(
        &self,
        _: &AppState,
        _: &str,
        _: Option<&crate::ledger::LedgerEntry>,
    ) -> Option<serde_json::Value> {
        None
    }
    fn owner(&self, _: &AppState, _: &str) -> Option<&'static str> {
        None
    }
    fn refusal(&self, _: &AppState, _: &str, watching: bool) -> serde_json::Value {
        if watching {
            serde_json::json!({"type":"error","code":"read_only","reason":"watching",
                "message":"You're watching. Take control to type."})
        } else {
            serde_json::json!({"type":"error","code":"read_only","reason":"elsewhere",
                "message":"This project is not available here right now. That was not sent."})
        }
    }
    fn acted(&self, _: &AppState, _: &str) {}
    fn decorate_sessions(
        &self,
        _: &AppState,
        _: &mut Vec<(u64, serde_json::Value)>,
    ) -> Vec<serde_json::Value> {
        Vec::new()
    }
    fn decorate_workspace(&self, _: &AppState, _: &str, _: &mut serde_json::Value) {}
    fn health(&self, _: &AppState, _: &mut serde_json::Value) {}
    fn tools(&self, _: &AppState, _: &str) -> Vec<serde_json::Value> {
        Vec::new()
    }
    fn call_tool<'a>(
        &'a self,
        _: &'a Arc<AppState>,
        _: &'a str,
        _: &'a str,
        _: &'a serde_json::Value,
    ) -> Option<BoxFuture<'a, serde_json::Value>> {
        None
    }
    fn auto_tools(&self, _: &AppState, _: &str) -> Vec<String> {
        Vec::new()
    }
    fn start_note<'a>(
        &'a self,
        _: &'a AppState,
        _: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, Option<StartNote>> {
        Box::pin(async { None })
    }
    fn note_told<'a>(&'a self, _: &'a AppState, _: &'a StartNote) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
    fn reads_folder_identity(&self, _: &AppState) -> bool {
        false
    }
    fn workspace_known(&self, _: &AppState, _: &str) {}
    fn workspace_opened<'a>(
        &'a self,
        _: &'a Arc<AppState>,
        _: &'a crate::workspaces::Workspace,
        _: Option<bool>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
    fn started(&self, _: &Arc<AppState>) -> anyhow::Result<()> {
        Ok(())
    }
    fn held_at_boot(&self, _: &AppState, _: &crate::ledger::LedgerEntry) {}
    fn restored(&self, _: &Arc<AppState>) {}
    fn may_resume(&self, _: &AppState, entry: &crate::ledger::LedgerEntry) -> bool {
        !self.fence.fenced(&entry.workspace_id)
    }
    fn resume_check(&self, _: &AppState, _: &crate::ledger::LedgerEntry) -> anyhow::Result<()> {
        Ok(())
    }
    fn stopping(&self, _: &AppState) {}
    fn shutdown<'a>(&'a self, _: &'a Arc<AppState>) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
    fn routed(&self, _: &AppState, _: &str) -> bool {
        false
    }
    fn outside_project(&self, _: &AppState, _: &str, paths: &[String]) -> Vec<String> {
        paths.to_vec()
    }
    fn project_feed(
        &self,
        _: &Arc<AppState>,
        _: &str,
        _: Vec<String>,
        _: Vec<String>,
    ) -> Option<Box<dyn ProjectFeed>> {
        None
    }
    fn proxy_socket<'a>(
        &'a self,
        _: &'a Arc<AppState>,
        _: &'a str,
        _: &'a str,
        _: &'a crate::ws::SocketOptions,
        _: serde_json::Value,
        _: &'a mut axum::extract::ws::WebSocket,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn workspace_resuming(&self, _: &AppState, _: &str) {}
    fn routed_decisions(&self, _: &AppState) -> Vec<(String, String)> {
        Vec::new()
    }
    fn routes(&self, _: &Arc<AppState>) -> axum::Router<Arc<AppState>> {
        axum::Router::new()
    }
    fn api_layers(
        &self,
        _: &Arc<AppState>,
        api: axum::Router<Arc<AppState>>,
    ) -> axum::Router<Arc<AppState>> {
        api
    }
    fn ticket_layers(
        &self,
        _: &Arc<AppState>,
        routes: axum::Router<Arc<AppState>>,
    ) -> axum::Router<Arc<AppState>> {
        routes
    }
    fn outer_layers(&self, app: axum::Router) -> axum::Router {
        app
    }
}
