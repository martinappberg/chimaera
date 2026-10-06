mod activity;
mod agent_docs;
mod agent_probe;
mod agent_setup;
mod agent_state;
mod agent_updates;
mod agents;
mod api;
mod assets;
mod browser_open;
mod bundle;
mod chat;
mod cloud;
mod codex_notify;
mod comms;
mod compute;
pub mod daemon_extension;
mod doc_check;
mod download;
mod drafts;
mod embed;
mod environment;
mod episodes;
mod exec;
mod fs;
mod fs_watch;
mod git;
mod history;
mod job_host;
mod knowledge;
mod launcher;
mod ledger;
mod lifecycle;
mod links;
mod mcp;
mod naming;
mod notebook;
mod notices;
mod persist;
mod plugins;
mod pro;
mod proxy;
mod quickopen;
mod recents;
mod recents_archive;
mod router;
mod runtime_retention;
mod runtimes;
mod session_proxy;
mod session_view;
mod settings;
mod spawn;
mod state;
mod subagents;
mod timeline;
mod update;
mod upload;
mod view_state;
mod voice;
mod workspace_maintenance;
mod workspace_scope;
mod workspaces;
mod ws;

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
pub(crate) use router::app;
pub(crate) use state::{lock, AppState};

#[cfg(all(
    target_os = "linux",
    feature = "provider-authority-prototype",
    feature = "daemon-extension-fixture"
))]
pub use lifecycle::run_with_provider_fixture;
#[cfg(all(
    unix,
    feature = "provider-authority-prototype",
    feature = "daemon-extension-fixture"
))]
pub use pro::execution::provider_fixture_host as provider_fixture;

#[cfg(test)]
mod tests;
