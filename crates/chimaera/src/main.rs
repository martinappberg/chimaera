mod compute;
mod connect;
mod daemonize;
mod doctor;
mod kill;
mod plugin;
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

// Parsed once per run: a variant's size is irrelevant.
#[allow(clippy::large_enum_variant)]
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
        /// Bind 0.0.0.0 instead of loopback — what a cluster workspace job
        /// runs with, so a plain `ssh -L` through the login node reaches it;
        /// the bearer token is the gate.
        #[arg(long)]
        bind_routable: bool,
    },
    /// Fixed supervisor-only provider control; never a daemon/project route.
    #[cfg(feature = "provider-authority-prototype")]
    #[command(hide = true)]
    PersonalProviderControl {
        #[arg(long)]
        startup_fd: i32,
        #[arg(long)]
        control_fd: i32,
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
        /// Run the daemon on the login node of a cluster (a host whose login
        /// shell reaches a batch scheduler) anyway. Most clusters don't allow
        /// servers on login nodes — use this only if yours says it's fine;
        /// otherwise use `chimaera compute`, which runs each workspace as a
        /// job. Saved per host once used (the app shares the setting).
        #[arg(long)]
        login_node: bool,
    },
    /// Check the local environment for common problems.
    Doctor,
    /// Print the shell-integration snippet (for remote hosts' rc files).
    ShellIntegration,
    /// Clusters: start Slurm jobs and open workspaces inside them; nothing
    /// runs on the login node.
    Compute {
        #[command(subcommand)]
        cmd: ComputeCmd,
    },
    /// Workbench plugins on the daemon running here: list them (the
    /// installed ones and the first-party ones you can install), install
    /// one, update or remove an installed one.
    Plugin {
        #[command(subcommand)]
        cmd: PluginCmd,
    },
    /// A cluster job's main process (what the job script runs, on the job's
    /// compute node): opens workspaces inside the job on the app's request
    /// and closes them all when the job ends.
    #[command(hide = true)]
    JobHost {
        /// The job's folder (`…/cluster/j/<id>`).
        #[arg(long)]
        job_dir: PathBuf,
    },
    /// Read-only look at a cluster folder, for the app's ssh commands: print
    /// JSON and exit. Never writes, locks or starts anything — it is the one
    /// chimaera command run on a cluster's login node.
    #[command(hide = true)]
    Browse {
        /// Print the cluster folder's state (workspaces, jobs, leases).
        #[arg(long, conflicts_with = "dir")]
        state: bool,
        /// List one folder's subfolders (`~` and `$VARS` expand here).
        #[arg(long)]
        dir: Option<String>,
        /// The cluster folder.
        #[arg(long)]
        cluster_dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum PluginCmd {
    /// Every plugin with its version, whether it is a Chimaera plugin and
    /// verified, where it came from, and any update.
    List,
    /// Install a plugin: a Chimaera plugin by its id (the release chimaera
    /// pins), any plugin from its GitHub release, or a local build with
    /// --path (checksum-verified whenever there is a SHA256SUMS).
    Add {
        /// A Chimaera plugin's id (`mycelium`), or a repository:
        /// owner/repo or its https://github.com/owner/repo URL.
        #[arg(required_unless_present = "path", conflicts_with = "path")]
        plugin: Option<String>,
        /// A release version of a repository (default: its latest).
        #[arg(long, conflicts_with = "path")]
        version: Option<String>,
        /// A local build instead: a directory holding plugin.wasm and
        /// plugin.toml (and SHA256SUMS, if it should be verified).
        #[arg(long)]
        path: Option<std::path::PathBuf>,
        /// Trust what a plugin the maintainers haven't verified can do,
        /// without asking (for scripts). The list is still printed.
        #[arg(long)]
        trust: bool,
    },
    /// Update an installed plugin to its latest release.
    Update {
        id: String,
        /// Allow an update that asks for more, without asking.
        #[arg(long)]
        trust: bool,
    },
    /// Remove an installed plugin (every version of it).
    Remove { id: String },
    /// Trust what an installed plugin can do (one waiting for your trust
    /// stays off until you do).
    Trust {
        id: String,
        /// Don't ask (the list is still printed).
        #[arg(long)]
        yes: bool,
    },
    /// Withdraw your trust: the plugin goes off everywhere until trusted again.
    Untrust { id: String },
    /// What a plugin.toml can do: its tier, capability digest and the
    /// card's Can list (no daemon needed; plugin authors and the lock's
    /// maintainers use it).
    Caps {
        manifest: std::path::PathBuf,
        /// Print the JSON the lock and the card read.
        #[arg(long)]
        json: bool,
    },
    /// A plugin's activity log (installs, updates, trust, blocks), newest first.
    Activity { id: String },
}

#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum ComputeCmd {
    /// The cluster's jobs and the workspaces open in them.
    Jobs { host: String },
    /// Add a folder on the cluster as a workspace ($SCRATCH/x and ~/x expand there).
    Add {
        host: String,
        path: String,
        #[arg(long)]
        name: Option<String>,
    },
    /// Start a job; workspaces open inside it.
    Start {
        host: String,
        /// Time limit, e.g. 4:00:00 or 2-00:00:00 (required — every job states one).
        #[arg(long)]
        time: String,
        #[arg(long)]
        partition: Option<String>,
        #[arg(long)]
        account: Option<String>,
        #[arg(long)]
        qos: Option<String>,
        #[arg(long)]
        constraint: Option<String>,
        #[arg(long)]
        cpus: Option<u32>,
        /// Memory per node, e.g. 16G.
        #[arg(long)]
        mem: Option<String>,
        #[arg(long)]
        gpus: Option<u32>,
        /// Workspaces (id or name) to open when it starts.
        #[arg(long, value_delimiter = ',')]
        open: Vec<String>,
        /// What to call the job (default: its partition and time).
        #[arg(long)]
        name: Option<String>,
        /// Startup commands for this job only (after the cluster's and each
        /// workspace's).
        #[arg(long)]
        startup: Option<String>,
        /// Hold the job in this terminal instead of submitting it (for
        /// partitions that take only interactive jobs); it stops when this
        /// command ends.
        #[arg(long)]
        attached: bool,
        /// Remember this setup under a name.
        #[arg(long)]
        save_as: Option<String>,
    },
    /// Open a workspace's UI: where it's open, else in the running job (or
    /// the one named). Holds the ssh forward until Ctrl-C.
    Open {
        host: String,
        workspace: String,
        /// The job to open it in when several run.
        #[arg(long)]
        job: Option<String>,
        /// Print the URL without opening a browser.
        #[arg(long)]
        no_open: bool,
    },
    /// Close a workspace in its job (it saves its chats first).
    Close { host: String, workspace: String },
    /// Move an open workspace to another running job; its chats come along.
    Move {
        host: String,
        workspace: String,
        /// The job to move it to.
        #[arg(long)]
        to: String,
    },
    /// Start a new job with this one's setup; when it runs it stops this one
    /// and takes its workspaces over.
    Continue {
        host: String,
        job: String,
        /// A new time limit for the new job.
        #[arg(long)]
        time: Option<String>,
    },
    /// Stop a job (every workspace in it saves its chats first).
    Stop { host: String, job: String },
}

/// Parse a `$PORT`-style listen port. An unset, empty, or unparsable value
/// yields `None` — the daemon then binds an OS-assigned free port.
fn parse_port(raw: Option<String>) -> Option<u16> {
    raw?.trim().parse().ok()
}

/// `chimaera browse`: one JSON document on stdout, or an error on stderr and
/// exit 2 (the app shows the message as is).
fn browse(
    state: bool,
    dir: Option<&str>,
    cluster_dir: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    let out = if state {
        let cluster_dir =
            cluster_dir.ok_or_else(|| anyhow::anyhow!("--state needs --cluster-dir"))?;
        serde_json::to_string(&chimaera_core::cluster::browse_state(cluster_dir))?
    } else if let Some(dir) = dir {
        match chimaera_core::cluster::browse_dir(dir, cluster_dir) {
            Ok(listing) => serde_json::to_string(&listing)?,
            Err(message) => {
                eprintln!("{message}");
                std::process::exit(2);
            }
        }
    } else {
        anyhow::bail!("browse needs --state or --dir");
    };
    println!("{out}");
    Ok(())
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
    // log_internal_errors(false): a failed stderr write must drop the log line,
    // never escalate. The fmt() builder's default (true) reports the failure via
    // eprintln!, which panics on the SAME dead stderr — killing whatever task
    // logged. A daemon that outlives its launch channel (ssh pipe, terminal)
    // with stderr unredirected would otherwise turn every logging request
    // handler into an empty reply once that channel dies.
    // `browse` prints JSON for a machine and exits: no logging, no runtime.
    if let Command::Browse {
        state,
        dir,
        cluster_dir,
    } = &cli.command
    {
        return browse(*state, dir.as_deref(), cluster_dir.as_deref());
    }

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .log_internal_errors(false)
        .init();

    // A login node has 64-192 cores; tokio's default of one worker per core
    // (plus up to 512 blocking threads) is the wrong shape for a daemon that
    // measures <1 core steady-state — dozens of idle worker stacks and
    // per-thread allocator heaps (87 threads observed on a 64-core node with
    // six sessions), and a spiky blocking pool under repaint bursts. Four
    // workers carry the async side (PTY reads run on their own threads); the
    // blocking pool stays generous because file walks, snapshot renders and
    // NFS stats park there and one wedged stat must not starve the rest;
    // the cap is a ceiling, not a preallocation (threads spawn on demand and
    // retire after 10 s idle), so the margin is free. The flip side: any
    // filesystem call still inline in an async handler (the session-create
    // prelude/settings writes are the known ones) now stalls a quarter of the
    // reactor instead of a sixty-fourth — keep moving those off. The app's
    // `--daemon` builds the same shape (`chimaera-app/src/daemon.rs`).
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .max_blocking_threads(128)
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
        #[cfg(feature = "provider-authority-prototype")]
        Command::PersonalProviderControl {
            startup_fd,
            control_fd,
        } => {
            use std::os::fd::FromRawFd;
            anyhow::ensure!(
                startup_fd >= 3 && control_fd >= 3 && startup_fd != control_fd,
                "Provider control descriptors refused"
            );
            // Ownership is transferred once; the library verifies pipe/socket
            // types before any child exists and closes enrollment before serving.
            let startup = unsafe { std::os::fd::OwnedFd::from_raw_fd(startup_fd) };
            let control = unsafe { std::os::fd::OwnedFd::from_raw_fd(control_fd) };
            chimaera_server::run_personal_provider_control(startup, control)
                .await
                .map_err(anyhow::Error::from)
        }
        Command::Status { host } => status::run(host.as_deref()).await,
        Command::Kill => kill::run().await,
        Command::Connect {
            host,
            local_port,
            binary,
            no_open,
            update_daemon,
            login_node,
        } => {
            connect::run(
                &host,
                local_port,
                binary.as_deref(),
                no_open,
                update_daemon,
                login_node,
            )
            .await
        }
        Command::Doctor => doctor::run(),
        Command::ShellIntegration => {
            print!("{}", chimaera_core::shellint::snippet());
            Ok(())
        }
        Command::Compute { cmd } => match cmd {
            ComputeCmd::Jobs { host } => compute::jobs(&host).await,
            ComputeCmd::Add { host, path, name } => {
                compute::add(&host, &path, name.as_deref()).await
            }
            ComputeCmd::Start {
                host,
                time,
                partition,
                account,
                qos,
                constraint,
                cpus,
                mem,
                gpus,
                open,
                name,
                startup,
                attached,
                save_as,
            } => {
                let spec = chimaera_core::slurm::LaunchSpec {
                    time,
                    partition,
                    account,
                    qos,
                    constraint,
                    cpus,
                    mem,
                    gpus,
                };
                compute::start(
                    &host,
                    compute::StartArgs {
                        spec,
                        name: name.as_deref(),
                        open,
                        run_startup: startup.as_deref(),
                        attached,
                        save_as: save_as.as_deref(),
                    },
                )
                .await
            }
            ComputeCmd::Open {
                host,
                workspace,
                job,
                no_open,
            } => compute::open(&host, &workspace, job.as_deref(), no_open).await,
            ComputeCmd::Close { host, workspace } => compute::close(&host, &workspace).await,
            ComputeCmd::Move {
                host,
                workspace,
                to,
            } => compute::move_ws(&host, &workspace, &to).await,
            ComputeCmd::Continue { host, job, time } => {
                compute::continue_job(&host, &job, time).await
            }
            ComputeCmd::Stop { host, job } => compute::stop(&host, &job).await,
        },
        Command::JobHost { job_dir } => chimaera_server::run_job_host(job_dir).await,
        Command::Browse { .. } => unreachable!("browse returns before the runtime starts"),
        Command::Plugin { cmd } => match cmd {
            PluginCmd::List => plugin::list().await,
            PluginCmd::Add {
                plugin,
                version,
                path,
                trust,
            } => {
                plugin::add(
                    plugin.as_deref(),
                    version.as_deref(),
                    path.as_deref(),
                    trust,
                )
                .await
            }
            PluginCmd::Update { id, trust } => plugin::update(&id, trust).await,
            PluginCmd::Remove { id } => plugin::remove(&id).await,
            PluginCmd::Trust { id, yes } => plugin::trust(&id, yes).await,
            PluginCmd::Untrust { id } => plugin::untrust(&id).await,
            PluginCmd::Caps { manifest, json } => plugin::caps(&manifest, json),
            PluginCmd::Activity { id } => plugin::activity(&id).await,
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

    #[cfg(feature = "provider-authority-prototype")]
    #[test]
    fn personal_control_accepts_only_inherited_descriptor_flags() {
        let cli = Cli::try_parse_from([
            "chimaera",
            "personal-provider-control",
            "--startup-fd",
            "3",
            "--control-fd",
            "4",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::PersonalProviderControl {
                startup_fd: 3,
                control_fd: 4
            }
        ));
        assert!(Cli::try_parse_from([
            "chimaera",
            "personal-provider-control",
            "--startup-fd",
            "3",
            "--control-fd",
            "4",
            "--home",
            "/tmp"
        ])
        .is_err());
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
    fn plugin_subcommands_parse() {
        let cli = Cli::try_parse_from([
            "chimaera",
            "plugin",
            "add",
            "acme/latex",
            "--version",
            "0.2.0",
        ])
        .unwrap();
        match cli.command {
            Command::Plugin {
                cmd:
                    PluginCmd::Add {
                        plugin,
                        version,
                        path,
                        trust,
                    },
            } => {
                assert_eq!(plugin.as_deref(), Some("acme/latex"));
                assert_eq!(version.as_deref(), Some("0.2.0"));
                assert_eq!(path, None);
                assert!(!trust, "asking is the default");
            }
            _ => panic!("expected plugin add"),
        }
        let cli = Cli::try_parse_from(["chimaera", "plugin", "add", "--path", "target/x"]).unwrap();
        match cli.command {
            Command::Plugin {
                cmd: PluginCmd::Add { plugin, path, .. },
            } => {
                assert_eq!(plugin, None);
                assert_eq!(path, Some(std::path::PathBuf::from("target/x")));
            }
            _ => panic!("expected plugin add --path"),
        }
        for args in [
            &["chimaera", "plugin", "list"][..],
            &["chimaera", "plugin", "add", "mycelium"],
            &["chimaera", "plugin", "update", "latex"],
            &["chimaera", "plugin", "update", "latex", "--trust"],
            &["chimaera", "plugin", "add", "acme/x", "--trust"],
            &["chimaera", "plugin", "remove", "latex"],
            &["chimaera", "plugin", "trust", "latex"],
            &["chimaera", "plugin", "trust", "latex", "--yes"],
            &["chimaera", "plugin", "untrust", "latex"],
            &["chimaera", "plugin", "caps", "plugin.toml", "--json"],
            &["chimaera", "plugin", "activity", "latex"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok(), "{args:?}");
        }
        for args in [
            &["chimaera", "plugin", "add"][..],
            &["chimaera", "plugin", "add", "acme/x", "--path", "d"],
            &[
                "chimaera",
                "plugin",
                "add",
                "--path",
                "d",
                "--version",
                "1.0.0",
            ],
        ] {
            assert!(Cli::try_parse_from(args).is_err(), "{args:?}");
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
