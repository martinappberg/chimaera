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
    },
    /// Check the local environment for common problems.
    Doctor,
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
            chimaera_server::run(chimaera_server::ServerConfig { port }).await
        }
        Command::Status { host } => status::run(host.as_deref()).await,
        Command::Kill => kill::run().await,
        Command::Connect {
            host,
            local_port,
            binary,
            no_open,
        } => connect::run(&host, local_port, binary.as_deref(), no_open).await,
        Command::Doctor => doctor::run(),
    }
}
