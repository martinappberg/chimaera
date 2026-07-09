mod agent_state;
mod agents;
mod api;
mod assets;
mod chat;
mod exec;
mod fs;
mod git;
mod launcher;
mod ledger;
mod lifecycle;
mod links;
mod mcp;
mod naming;
mod quickopen;
mod recents;
mod router;
mod runtimes;
mod session_view;
mod settings;
mod spawn;
mod state;
mod update;
mod view_state;
mod workspaces;
mod ws;

/// Configuration for the chimaera daemon.
pub struct ServerConfig {
    /// Port to bind on 127.0.0.1. `None` lets the OS assign a free port.
    pub port: Option<u16>,
}

pub use lifecycle::run;
pub(crate) use router::app;
pub(crate) use state::{lock, AppState};

#[cfg(test)]
mod tests;
