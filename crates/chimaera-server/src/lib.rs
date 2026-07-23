mod agent_state;
mod agent_updates;
mod agents;
mod api;
mod assets;
mod chat;
mod compute;
mod compute_jobs;
mod download;
mod environment;
mod exec;
mod fs;
mod fs_watch;
mod git;
mod launcher;
mod ledger;
mod lifecycle;
mod links;
mod mcp;
mod naming;
mod persist;
mod proxy;
mod quickopen;
mod recents;
mod router;
mod runtimes;
mod session_view;
mod settings;
mod spawn;
mod state;
mod update;
mod upload;
mod view_state;
mod workspaces;
mod ws;

/// Configuration for the chimaera daemon.
pub struct ServerConfig {
    /// Port to bind on 127.0.0.1. `None` lets the OS assign a free port.
    pub port: Option<u16>,
    /// Bind 0.0.0.0 instead of loopback. OPT-IN, for Mode 2 rung A only:
    /// a compute-node daemon on a cluster without ssh-to-node, where the
    /// login node forwards straight to the node's port and the per-job
    /// bearer token is the gate. Loopback stays the default everywhere —
    /// this deliberately amends the "never accepts non-loopback" security
    /// note (architecture.md § Security notes).
    pub routable_bind: bool,
}

pub use lifecycle::run;
pub(crate) use router::app;
pub(crate) use state::{lock, AppState};

#[cfg(test)]
mod tests;
