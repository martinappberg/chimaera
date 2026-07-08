//! The chimaera native shell: a Tauri 2 wrapper around the same daemon and
//! web UI the browser uses. Windows load `http://127.0.0.1:{port}` straight
//! from a daemon (local, or an ssh tunnel to a remote one), so the shell
//! adds native affordances — real windows per workspace, a menu bar, remote
//! host management — without forking the UI.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod askpass;
mod daemon;
mod menu;
mod shell;
mod update;
mod windows;

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
    // detached and the daemon outlives every window.
    if std::env::args().any(|a| a == "--daemon") {
        daemon::run_headless();
        return;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    shell::run();
}
