//! The chimaera native shell: a Tauri 2 wrapper around the same daemon and
//! web UI the browser uses. Windows load `http://127.0.0.1:{port}` straight
//! from a daemon (local, or an ssh tunnel to a remote one), so the shell
//! adds native affordances — real windows per workspace, a menu bar, remote
//! host management — without forking the UI.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod askpass;
mod command_manifest;
mod daemon;
mod http;
mod menu;
mod shell;
mod tray;
mod update;
mod windows;
mod wsl;

fn main() {
    // Four roles, chosen by argv before any init: askpass, daemon, board CLI,
    // else the Tauri shell. `--askpass <prompt>` is the tiny SSH_ASKPASS helper
    // ssh runs to prompt for a password / 2FA: it relays to the running app
    // over a socket and prints the answer, no Tauri init. Checked first — it
    // must stay lightweight and never spawn a daemon or a window.
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

    // `chimaera board …` IS the board CLI here: the daemon writes a `chimaera`
    // shim exec'ing current_exe() onto every session's PATH, and in the native
    // app that binary is this GUI exe. Falling through to Tauri would swallow
    // the args silently (the single-instance plugin fronts the running window)
    // — so dispatch before ANY Tauri init and exit; this must work while
    // another app instance runs, without touching a window. On Windows the
    // shim target is the WSL2 musl binary, never this exe — fail loudly like
    // `--daemon`.
    if std::env::args().nth(1).as_deref() == Some("board") {
        #[cfg(unix)]
        board_cli();
        #[cfg(windows)]
        {
            eprintln!(
                "chimaera board does not run in the Windows app binary: the daemon \
                 (and its `chimaera` CLI) lives inside WSL2 — run it in a chimaera \
                 session, or as `wsl -d <distro> -- ~/.chimaera/bin/chimaera board …`"
            );
            std::process::exit(2);
        }
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

/// Parse and run `chimaera board …` exactly like the standalone binary: the
/// same clap tree under the same names (`chimaera_board::cli::BoardCmd` is the
/// one the `chimaera` crate mounts), so help text, parse errors, and exit
/// codes are byte-identical between the two deployments.
#[cfg(unix)]
fn board_cli() -> ! {
    use clap::Parser;

    /// Mirror of the standalone CLI's spine, board arm only. `bin_name` is
    /// pinned to `chimaera` — the name the shim (and the board note) uses —
    /// so usage lines match the standalone binary even when argv[0] is the
    /// .app executable's path.
    #[derive(Parser)]
    #[command(name = "chimaera", bin_name = "chimaera")]
    struct BoardCli {
        #[command(subcommand)]
        command: BoardCommand,
    }

    #[derive(clap::Subcommand)]
    enum BoardCommand {
        /// Boards: compose, render, and read back .board visual surfaces (the
        /// legacy .board.json extension still opens).
        Board {
            #[command(subcommand)]
            cmd: chimaera_board::cli::BoardCmd,
        },
    }

    // `exit()` prints help/version to stdout and errors to stderr with clap's
    // own exit codes — the same path the standalone `Cli::parse()` takes.
    let cli = BoardCli::try_parse().unwrap_or_else(|err| err.exit());
    let BoardCommand::Board { cmd } = cli.command;
    match chimaera_board::cli::run(cmd) {
        Ok(()) => std::process::exit(0),
        Err(err) => {
            // The standalone binary's `main() -> anyhow::Result<()>` reports
            // failures as `Error: {:?}` (message + Caused-by chain), exit 1.
            eprintln!("Error: {err:?}");
            std::process::exit(1);
        }
    }
}
