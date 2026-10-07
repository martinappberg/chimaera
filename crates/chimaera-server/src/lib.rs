#[doc(hidden)]
pub mod activity;
mod agent_docs;
mod agent_probe;
mod agent_setup;
#[doc(hidden)]
pub mod agent_state;
mod agent_updates;
#[doc(hidden)]
pub mod agents;
#[doc(hidden)]
pub mod api;
mod assets;
mod browser_open;
mod bundle;
#[doc(hidden)]
pub mod chat;
mod cloud;
mod codex_notify;
#[doc(hidden)]
pub mod codex_rollout;
mod comms;
mod compute;
pub mod daemon_extension;
#[doc(hidden)]
pub mod doc_check;
#[doc(hidden)]
pub mod download;
#[doc(hidden)]
pub mod drafts;
#[doc(hidden)]
pub mod embed;
mod environment;
mod episodes;
#[doc(hidden)]
pub mod exec;
#[doc(hidden)]
pub mod fs;
mod fs_watch;
#[doc(hidden)]
pub mod git;
#[doc(hidden)]
pub mod history;
mod job_host;
mod knowledge;
#[doc(hidden)]
pub mod launcher;
#[doc(hidden)]
pub mod ledger;
mod lifecycle;
#[doc(hidden)]
pub mod links;
#[doc(hidden)]
pub mod mcp;
mod naming;
#[doc(hidden)]
pub mod notebook;
#[doc(hidden)]
pub mod notices;
#[doc(hidden)]
pub mod persist;
mod plugins;
#[doc(hidden)]
pub mod policy;
mod pro;
#[doc(hidden)]
pub mod process;
mod proxy;
mod quickopen;
#[doc(hidden)]
pub mod recents;
mod recents_archive;
#[doc(hidden)]
pub mod router;
mod runtime_retention;
#[doc(hidden)]
pub mod runtimes;
mod session_proxy;
#[doc(hidden)]
pub mod session_view;
#[doc(hidden)]
pub mod settings;
#[doc(hidden)]
pub mod spawn;
#[doc(hidden)]
pub mod state;
mod subagents;
mod timeline;
#[doc(hidden)]
pub mod update;
#[doc(hidden)]
pub mod upload;
mod view_state;
#[doc(hidden)]
pub mod voice;
mod workspace_maintenance;
mod workspace_scope;
#[doc(hidden)]
pub mod workspaces;
#[doc(hidden)]
pub mod ws;

/// Configuration for the chimaera daemon.
pub struct ServerConfig {
    /// Port to bind on 127.0.0.1. `None` lets the OS assign a free port.
    pub port: Option<u16>,
    /// Bind 0.0.0.0 instead of loopback (`--bind-routable`). What a cluster
    /// workspace job's `chimaera serve` passes: the app reaches the job with
    /// a plain `ssh -L <port>:<node>:<port>` through the login node, and the
    /// per-job bearer token is the gate. Loopback stays the default for every
    /// other daemon — this deliberately amends the "never accepts
    /// non-loopback" security note (architecture.md § Security notes).
    pub routable_bind: bool,
}

pub use job_host::run as run_job_host;
pub use lifecycle::{run, run_with_extension};
/// `chimaera plugin caps <plugin.toml>`: a manifest's tier, capability
/// digest and Can list, as the lock and the card record them.
pub use plugins::capabilities::describe_manifest as plugin_capabilities;
pub use router::app;
pub use state::{lock, AppState};

#[cfg(test)]
mod tests;
