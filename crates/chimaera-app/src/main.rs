//! The chimaera native shell: a Tauri 2 wrapper around the same daemon and
//! web UI the browser uses. Windows load `http://127.0.0.1:{port}` straight
//! from a daemon (local, or an ssh tunnel to a remote one), so the shell
//! adds native affordances — real windows per workspace, a menu bar, remote
//! host management — without forking the UI.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod appearance;
mod askpass;
mod command_manifest;
mod daemon;
mod http;
mod menu;
mod notify;
mod shell;
#[cfg(feature = "ssh-agent-prototype")]
#[allow(dead_code)] // No native Connect path advertises signing before live acceptance.
mod ssh_agent;
mod tray;
mod update;
mod windows;
mod wsl;

fn main() {
    // Triple role. `--askpass <prompt>` is the tiny SSH_ASKPASS helper ssh
    // runs to prompt for a password / 2FA: it relays to the running app over
    // a socket and prints the answer, no Tauri init. Checked first — it must
    // stay lightweight and never spawn a daemon or a window.
    if std::env::args().any(|a| a == "--askpass") {
        askpass::run_helper();
        return;
    }

    // `chimaera-app --daemon` IS the local daemon (headless, no Tauri init),
    // so the .app is self-contained — the shell spawns its own executable
    // detached and the daemon outlives every window. On Windows the daemon
    // is the Linux musl binary inside WSL2, never this exe — the flag must
    // fail loudly, not silently fall through to a GUI launch.
    if std::env::args().any(|a| a == "--daemon") {
        #[cfg(unix)]
        daemon::run_headless();
        #[cfg(windows)]
        {
            eprintln!(
                "chimaera --daemon does not exist on Windows: the daemon runs inside \
                 WSL2 (wsl -d <distro> -- ~/.chimaera/bin/chimaera serve)"
            );
            std::process::exit(2);
        }
        #[cfg(unix)]
        return;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    #[cfg(unix)]
    raise_open_file_limit();
    shell::run();
}

/// macOS starts GUI apps with a 256-descriptor soft limit. Every forwarded
/// terminal, chat or file view through a Pro connection costs two sockets,
/// so a busy workbench can exhaust it and lose its connections. Raise the
/// soft limit toward the hard limit (macOS rejects values above 10240).
#[cfg(unix)]
fn raise_open_file_limit() {
    use nix::sys::resource::{getrlimit, setrlimit, Resource};
    const WANTED: u64 = 8192;
    if let Ok((soft, hard)) = getrlimit(Resource::RLIMIT_NOFILE) {
        let target = WANTED.min(hard);
        if soft < target && setrlimit(Resource::RLIMIT_NOFILE, target, hard).is_err() {
            tracing::debug!("could not raise the open-file limit from {soft}");
        }
    }
}
