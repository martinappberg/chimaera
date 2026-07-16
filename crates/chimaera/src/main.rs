mod compute;
mod connect;
mod daemonize;
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
        /// Detach into a new session and return, so the daemon outlives the
        /// shell (or SSH channel) that started it. `connect` uses this to start
        /// a daemon on a remote host without relying on the host having
        /// util-linux `setsid`/`nohup` — the portable path that works on any
        /// POSIX remote (Linux, macOS, the BSDs).
        #[arg(long)]
        daemonize: bool,
        /// Bind 0.0.0.0 instead of loopback — Mode 2 rung A only (a
        /// compute-node daemon reached by a direct login-node forward on
        /// clusters without ssh-to-node); the bearer token is the gate.
        #[arg(long)]
        bind_routable: bool,
    },
    /// Show daemon status, locally or on a remote ssh host. A dev build
    /// reports the dev daemon (~/.chimaera-dev) on both ends — dev-ness is
    /// the build's property, not a flag.
    Status {
        /// Remote ssh host to check instead of the local machine.
        host: Option<String>,
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
        ///
        /// A dev build (never release-stamped) always targets the isolated
        /// dev daemon in ~/.chimaera-dev on the host: it deploys your
        /// locally built binary (`just dist`) under its own CHIMAERA_HOME,
        /// next to — never touching — the real ~/.chimaera daemon, and never
        /// downloads a release. Releases always target ~/.chimaera. There is
        /// no flag: dev-ness is the build's property.
        #[arg(long)]
        update_daemon: bool,
    },
    /// Check the local environment for common problems.
    Doctor,
    /// Print the shell-integration snippet (for remote hosts' rc files).
    ShellIntegration,
    /// Mode 2: chimaera sessions running AS Slurm jobs on a cluster.
    Compute {
        #[command(subcommand)]
        cmd: ComputeCmd,
    },
}

#[derive(Subcommand)]
enum ComputeCmd {
    /// List compute sessions (chimaera-named Slurm jobs) on a host.
    List { host: String },
    /// Submit a chimaera daemon as a Slurm job on a host.
    Launch {
        host: String,
        /// Display name (slugged into the job name `chimaera-<slug>`).
        #[arg(long, default_value = "session")]
        name: String,
        /// Walltime, e.g. 4:00:00 or 1-00:00:00.
        #[arg(long, default_value = "2:00:00")]
        time: String,
        #[arg(long)]
        partition: Option<String>,
        #[arg(long)]
        cpus: Option<u32>,
        #[arg(long)]
        mem: Option<String>,
        /// GPUs etc., e.g. gpu:1.
        #[arg(long)]
        gres: Option<String>,
        /// Workspace id whose environment prelude applies.
        #[arg(long)]
        workspace: Option<String>,
        /// Launch with a routable bind (rung A clusters only; token-gated).
        #[arg(long)]
        routable: bool,
    },
    /// Tunnel to a running compute session and open its UI.
    Connect {
        host: String,
        job_id: String,
        /// Do not open the UI in a browser.
        #[arg(long)]
        no_open: bool,
    },
    /// scancel a compute session.
    Cancel { host: String, job_id: String },
}

/// Parse a `$PORT`-style listen port. An unset, empty, or unparsable value
/// yields `None` — the daemon then binds an OS-assigned free port.
fn parse_port(raw: Option<String>) -> Option<u16> {
    raw?.trim().parse().ok()
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Detach BEFORE the async runtime exists. `fork` is only safe while the
    // process is single-threaded, and the tokio runtime spawns worker threads —
    // so the parent must exit (inside `detach`) before we build the runtime.
    // Only the new session leader returns here to serve.
    if let Command::Serve {
        daemonize: true, ..
    } = &cli.command
    {
        daemonize::detach()?;
    }

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(dispatch(cli.command))
}

async fn dispatch(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Serve {
            port,
            bind_routable,
            ..
        } => {
            // `--port` wins; else honor $PORT (twelve-factor) so autoPort dev
            // tooling and PaaS can assign it; else the OS picks a free port.
            let port = port.or_else(|| parse_port(std::env::var("PORT").ok()));
            chimaera_server::run(chimaera_server::ServerConfig {
                port,
                routable_bind: bind_routable,
            })
            .await
        }
        Command::Status { host } => status::run(host.as_deref()).await,
        Command::Kill => kill::run().await,
        Command::Connect {
            host,
            local_port,
            binary,
            no_open,
            update_daemon,
        } => connect::run(&host, local_port, binary.as_deref(), no_open, update_daemon).await,
        Command::Doctor => doctor::run(),
        Command::ShellIntegration => {
            print!("{}", chimaera_core::shellint::snippet());
            Ok(())
        }
        Command::Compute { cmd } => match cmd {
            ComputeCmd::List { host } => compute::list(&host).await,
            ComputeCmd::Launch {
                host,
                name,
                time,
                partition,
                cpus,
                mem,
                gres,
                workspace,
                routable,
            } => {
                compute::launch(
                    &host,
                    &name,
                    &time,
                    partition.as_deref(),
                    cpus,
                    mem.as_deref(),
                    gres.as_deref(),
                    workspace.as_deref(),
                    routable,
                )
                .await
            }
            ComputeCmd::Connect {
                host,
                job_id,
                no_open,
            } => compute::connect(&host, &job_id, no_open).await,
            ComputeCmd::Cancel { host, job_id } => compute::cancel(&host, &job_id).await,
        },
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
                ..
            } => {
                assert_eq!(host, "cluster");
                assert!(update_daemon);
            }
            _ => panic!("expected connect"),
        }
    }

    /// Dev-ness is the build's property, not a flag — the old `--dev`
    /// switches must be gone so nothing can mix a dev client with a real
    /// home (or vice versa).
    #[test]
    fn dev_flags_no_longer_parse() {
        assert!(Cli::try_parse_from(["chimaera", "connect", "cluster", "--dev"]).is_err());
        assert!(Cli::try_parse_from(["chimaera", "status", "cluster", "--dev"]).is_err());
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

    /// `connect` starts a remote daemon with `serve --daemonize`; the flag must
    /// parse, and a plain `serve` must stay foreground (dev preview, native app,
    /// `just` all run it that way).
    #[test]
    fn serve_daemonize_flag_parses_and_defaults_off() {
        let bg = Cli::try_parse_from(["chimaera", "serve", "--daemonize"]).unwrap();
        match bg.command {
            Command::Serve { daemonize, .. } => assert!(daemonize),
            _ => panic!("expected serve"),
        }
        let fg = Cli::try_parse_from(["chimaera", "serve"]).unwrap();
        match fg.command {
            Command::Serve { daemonize, .. } => assert!(!daemonize),
            _ => panic!("expected serve"),
        }
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
