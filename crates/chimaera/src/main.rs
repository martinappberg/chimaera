mod connect;
mod doctor;
mod kill;
mod status;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Chimaera: agent-native IDE daemon and remote-control CLI.
#[derive(Parser)]
#[command(name = "chimaera", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the chimaera daemon in the foreground.
    Serve {
        /// Port to listen on (defaults to an OS-assigned free port).
        #[arg(long)]
        port: Option<u16>,
    },
    /// Show daemon status, locally or on a remote ssh host.
    Status {
        /// Remote ssh host to check instead of the local machine.
        host: Option<String>,
        /// Inspect the host's isolated dev daemon (~/.chimaera-dev) instead
        /// of the real one. Requires a host; a local dev daemon is scoped by
        /// whatever CHIMAERA_HOME launched it — set that instead.
        #[arg(long, requires = "host")]
        dev: bool,
    },
    /// Stop the local daemon.
    Kill,
    /// Connect to a daemon on a remote ssh host, starting it if needed.
    Connect {
        /// Remote ssh host (resolved via your ~/.ssh/config).
        host: String,
        /// Local port for the tunnel (defaults to the remote port if free).
        #[arg(long)]
        local_port: Option<u16>,
        /// Path to a chimaera binary to install on the remote host if missing.
        #[arg(long)]
        binary: Option<PathBuf>,
        /// Do not open the UI in a browser.
        #[arg(long)]
        no_open: bool,
        /// Replace an outdated remote daemon even if it has live sessions
        /// (they end with it). At zero sessions outdated daemons are
        /// replaced automatically; the stop is always graceful.
        #[arg(long)]
        update_daemon: bool,
        /// Run against an isolated DEV daemon in ~/.chimaera-dev on the host:
        /// deploys your locally built binary (`just dist`) and starts it
        /// under its own CHIMAERA_HOME, next to — never touching — the real
        /// ~/.chimaera daemon. Never downloads a release.
        #[arg(long)]
        dev: bool,
    },
    /// Check the local environment for common problems.
    Doctor,
    /// Print the shell-integration snippet (for remote hosts' rc files).
    ShellIntegration,
}

/// Parse a `$PORT`-style listen port. An unset, empty, or unparsable value
/// yields `None` — the daemon then binds an OS-assigned free port.
fn parse_port(raw: Option<String>) -> Option<u16> {
    raw?.trim().parse().ok()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    match Cli::parse().command {
        Command::Serve { port } => {
            // `--port` wins; else honor $PORT (twelve-factor) so autoPort dev
            // tooling and PaaS can assign it; else the OS picks a free port.
            let port = port.or_else(|| parse_port(std::env::var("PORT").ok()));
            chimaera_server::run(chimaera_server::ServerConfig { port }).await
        }
        Command::Status { host, dev } => status::run(host.as_deref(), dev).await,
        Command::Kill => kill::run().await,
        Command::Connect {
            host,
            local_port,
            binary,
            no_open,
            update_daemon,
            dev,
        } => {
            connect::run(
                &host,
                local_port,
                binary.as_deref(),
                no_open,
                update_daemon,
                dev,
            )
            .await
        }
        Command::Doctor => doctor::run(),
        Command::ShellIntegration => {
            print!("{}", chimaera_core::shellint::snippet());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn connect_parses_update_daemon_flag() {
        let cli =
            Cli::try_parse_from(["chimaera", "connect", "cluster", "--update-daemon"]).unwrap();
        match cli.command {
            Command::Connect {
                host,
                update_daemon,
                dev,
                ..
            } => {
                assert_eq!(host, "cluster");
                assert!(update_daemon);
                assert!(!dev, "dev is opt-in");
            }
            _ => panic!("expected connect"),
        }
    }

    #[test]
    fn connect_parses_dev_flag() {
        let cli = Cli::try_parse_from(["chimaera", "connect", "cluster", "--dev"]).unwrap();
        match cli.command {
            Command::Connect { host, dev, .. } => {
                assert_eq!(host, "cluster");
                assert!(dev);
            }
            _ => panic!("expected connect"),
        }
    }

    /// `status --dev` reads the host's dev manifest; without a host there is
    /// no fixed dev home to read (local dev daemons are scoped by whatever
    /// CHIMAERA_HOME launched them), so clap rejects the combination.
    #[test]
    fn status_dev_requires_a_host() {
        assert!(Cli::try_parse_from(["chimaera", "status", "--dev"]).is_err());
        let cli = Cli::try_parse_from(["chimaera", "status", "cluster", "--dev"]).unwrap();
        match cli.command {
            Command::Status { host, dev } => {
                assert_eq!(host.as_deref(), Some("cluster"));
                assert!(dev);
            }
            _ => panic!("expected status"),
        }
    }

    #[test]
    fn parse_port_reads_valid_values_only() {
        assert_eq!(parse_port(Some("9700".into())), Some(9700));
        assert_eq!(parse_port(Some("  8080 ".into())), Some(8080));
        // Unset, empty, and unparsable all fall back to an OS-assigned port.
        assert_eq!(parse_port(None), None);
        assert_eq!(parse_port(Some("".into())), None);
        assert_eq!(parse_port(Some("notaport".into())), None);
        assert_eq!(parse_port(Some("99999".into())), None); // out of u16 range
    }

    #[test]
    fn connect_update_daemon_defaults_off() {
        let cli = Cli::try_parse_from(["chimaera", "connect", "cluster"]).unwrap();
        match cli.command {
            Command::Connect { update_daemon, .. } => assert!(!update_daemon),
            _ => panic!("expected connect"),
        }
    }
}
