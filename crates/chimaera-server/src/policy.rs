//! The one hook shared daemon code consults about an extension. A composed
//! extension installs its own [`WorkspacePolicy`]; without one the daemon
//! uses [`Inert`]: everything is admitted, nothing is recorded, no timer or
//! connection starts and no state file is written. The only thing an
//! extension-less daemon still honours is the durable fence an earlier
//! composed daemon may have left ([`fence`]), so a project that runs on
//! another machine is not run twice, while its files stay readable here.
//!
//! Shared code never names the extension: it asks this module. The trait is
//! wide on purpose (one hook per decision shared code cannot make itself),
//! so it is laid out in groups. Each group names the shared modules that
//! call it; a new hook joins the group whose callers it serves.
//!
//! | Group | Decides | Called from |
//! |---|---|---|
//! | Composition | whether an extension is here, its routes and layers, `/health` | `router`, `api`, `lifecycle` |
//! | Admission | whether work may start or continue in a workspace | `chat`, `spawn`, `comms`, `exec`, `ws`, `ledger`, `download` |
//! | Launch | what a child process is told and given | `spawn`, `chat`, `recents`, `update`, `launcher` |
//! | Restore | which deferred sessions come back after a restart | `ledger` |
//! | Presentation | what rows, sockets and refusals say about work elsewhere | `ws`, `session_view`, `api/sessions`, `api/workspaces`, `plugins` |
//! | Routed projects | a project served by another daemon, seen from here | `ws`, `notices` |
//! | Viewer scope | a forwarded viewer bound to one project and epoch | `workspace_scope` |
//! | Agents | tools and notes the extension gives a project's agents | `mcp`, `plugins`, `agents`, `chat` |
//! | Workspaces | registering and opening folders | `api/workspaces` |
//!
//! The handles the admission group hands back live in [`admission`]; the
//! inert implementation in [`inert`].
use std::{any::Any, future::Future, pin::Pin, sync::Arc};

use crate::AppState;

pub mod admission;
pub mod fence;
pub mod inert;

pub use admission::{
    Admission, AdmissionToken, Changed, Held, Hold, Installer, InstallerToken, Launch,
    LaunchContext, LaunchKind, LaunchToken, Reservation,
};
pub use inert::Inert;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a caller wants to do in a workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    /// Start or drive an agent.
    Execute,
    /// Start or type into a plain shell.
    Shell,
    /// Bring back a session a previous daemon life left.
    Restore,
}

/// A frame a project that runs on another daemon sends the window that
/// watches it, in place of this daemon's own file, git and timeline
/// watching.
pub enum ProjectFrame {
    /// A path-only invalidation, already in this window's paths.
    Fs(serde_json::Value),
    /// The project's Git epoch where it runs.
    Git(u64),
    /// The project's Timeline epoch where it runs.
    Timeline(u64),
}

/// The live view of a routed project for one window
/// ([`WorkspacePolicy::routed`]): ends when the project stops being served
/// elsewhere or its owner refuses it.
pub trait ProjectFeed: Send {
    fn workspace(&self) -> &str;
    /// The window's mounted previews and listed folders, in its own paths.
    fn watch(&self, files: Vec<String>, dirs: Vec<String>);
    /// `None` once the feed ended.
    fn next(&mut self) -> BoxFuture<'_, Option<ProjectFrame>>;
}

/// A note an agent is told as it starts (once per change of machine), and
/// what the policy remembers once it has been delivered.
pub struct StartNote {
    pub text: String,
    /// Identifies the note's substance within one agent process, so two
    /// carriers racing at one start deliver it once.
    pub digest: u64,
    /// The policy's own record of it, handed back in
    /// [`WorkspacePolicy::note_told`].
    pub record: Box<dyn Any + Send + Sync>,
}

/// The extension hook. Implementations are trusted in-process. Every
/// method must be cheap and must not block: shared code calls them on the
/// reactor and on hot paths. Asynchronous hooks return a [`BoxFuture`] so
/// the trait stays object-safe.
pub trait WorkspacePolicy: Send + Sync + 'static {
    // ----------------------------------------------------------------------
    // Composition: is an extension here, and what does it add to the app?
    // ----------------------------------------------------------------------

    /// Whether an extension is composed here at all: only then does the
    /// daemon serve extension routes or add extension fields and frames.
    fn composed(&self, state: &AppState) -> bool;
    /// Whether this composition is doing paid work right now; shared code
    /// skips bookkeeping only an active extension reads (input stamps).
    fn active(&self, state: &AppState) -> bool;
    /// After the state exists, before serving.
    fn started(&self, state: &Arc<AppState>) -> anyhow::Result<()>;
    /// The shutdown signal arrived.
    fn stopping(&self, state: &AppState);
    /// Daemon shutdown, after the live agents were stopped.
    fn shutdown<'a>(&'a self, state: &'a Arc<AppState>) -> BoxFuture<'a, ()>;
    /// Additive `/health` fields.
    fn health(&self, state: &AppState, body: &mut serde_json::Value);
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

    // ----------------------------------------------------------------------
    // Admission: may this work start or continue in this workspace?
    // ----------------------------------------------------------------------

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
    /// The admission a local command in `workspace` commits under, when the
    /// policy ties commands to a live lease; `None` admits by ownership alone.
    fn capture_command(
        &self,
        state: &AppState,
        workspace: &str,
    ) -> anyhow::Result<Option<Admission>>;
    /// Run a reserved request to its end under its reservation: a
    /// disconnected observer never cancels it.
    fn run_reserved<'a>(
        &'a self,
        reservation: Reservation,
        operation: BoxFuture<'a, axum::response::Response>,
    ) -> BoxFuture<'a, axum::response::Response>;

    // ----------------------------------------------------------------------
    // Launch: what a child process is told and given.
    // ----------------------------------------------------------------------

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

    // ----------------------------------------------------------------------
    // Restore: which deferred sessions come back after a restart.
    // ----------------------------------------------------------------------

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
    /// Deferred sessions of `workspace` are resuming here: rows another
    /// daemon served for it are no longer current.
    fn workspace_resuming(&self, state: &AppState, workspace: &str);

    // ----------------------------------------------------------------------
    // Presentation: what rows, sockets and refusals say about work elsewhere.
    // ----------------------------------------------------------------------

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

    // ----------------------------------------------------------------------
    // Routed projects: a project served by another daemon, seen from here.
    // ----------------------------------------------------------------------

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
    /// Sessions another daemon serves that wait on a permission decision
    /// (`(session, workspace)`), so a relayed alert stays up until answered.
    fn routed_decisions(&self, state: &AppState) -> Vec<(String, String)>;

    // ----------------------------------------------------------------------
    // Viewer scope: a forwarded viewer bound to one project and epoch.
    // ----------------------------------------------------------------------

    /// Admit a viewer bound to `workspace` at `epoch`: the admission its
    /// reads prove and its writes reserve under.
    fn scope_admission(
        &self,
        state: &AppState,
        workspace: &str,
        epoch: u64,
    ) -> anyhow::Result<Admission>;
    /// Whether `workspace` is served to a viewer at `epoch` right now.
    fn scope_check(&self, state: &AppState, workspace: &str, epoch: u64) -> anyhow::Result<()>;
    /// A scope this machine cannot admit yet only because its own renewal of
    /// exactly `epoch` is still out.
    fn scope_renewing(&self, state: &AppState, workspace: &str, epoch: u64) -> bool;
    /// Wait for that renewal (bounded); `true` once a fresh proof exists. The
    /// caller checks the scope again.
    fn await_scope_renewal<'a>(
        &'a self,
        state: &'a AppState,
        workspace: &'a str,
        epoch: u64,
    ) -> BoxFuture<'a, bool>;

    // ----------------------------------------------------------------------
    // Agents: tools and notes the extension gives a project's agents.
    // ----------------------------------------------------------------------

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

    // ----------------------------------------------------------------------
    // Workspaces: registering and opening folders.
    // ----------------------------------------------------------------------

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
}
