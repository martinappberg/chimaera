use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use tokio::net::TcpListener;

use crate::{agent_updates, git, ledger, proxy, recents, runtimes, settings, update};
use crate::{app, lock, AppState, ServerConfig};

/// Bind on 127.0.0.1, write the manifest, and serve until SIGINT/SIGTERM.
pub async fn run(cfg: ServerConfig) -> anyhow::Result<()> {
    run_selected(
        cfg,
        None,
        #[cfg(all(
            unix,
            feature = "provider-authority-prototype",
            feature = "daemon-extension-fixture"
        ))]
        None,
    )
    .await
}

/// Trusted composed assembly. Optional policy is supplied after CLI daemonization;
/// public startup restrictions and the actual task owners remain the same.
pub async fn run_with_extension(
    cfg: ServerConfig,
    runtime: Arc<dyn crate::daemon_extension::Runtime>,
) -> anyhow::Result<()> {
    run_selected(
        cfg,
        Some(runtime),
        #[cfg(all(
            unix,
            feature = "provider-authority-prototype",
            feature = "daemon-extension-fixture"
        ))]
        None,
    )
    .await
}

/// Nondefault closed fixture composition; ordinary daemon entrypoints never
/// receive this callback. Actual inherited startup is consumed by the same host.
#[cfg(all(
    target_os = "linux",
    feature = "provider-authority-prototype",
    feature = "daemon-extension-fixture"
))]
pub async fn run_with_provider_fixture(
    cfg: ServerConfig,
    start: fn(crate::provider_fixture::Context) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    anyhow::ensure!(!cfg.routable_bind, "Fixture requires loopback binding");
    run_selected(cfg, None, Some(start)).await
}

async fn run_selected(
    cfg: ServerConfig,
    runtime: Option<Arc<dyn crate::daemon_extension::Runtime>>,
    #[cfg(all(
        unix,
        feature = "provider-authority-prototype",
        feature = "daemon-extension-fixture"
    ))]
    provider_fixture: Option<fn(crate::provider_fixture::Context) -> anyhow::Result<()>>,
) -> anyhow::Result<()> {
    // Consume the trusted launcher's one-shot pipe before restore or helpers
    // can inherit it. An opted-in idle descriptor is protected and its Linux
    // proc/ptrace gate verified here, before any startup child. Ordinary device
    // and cluster startup have no such channel.
    let supervisor_cleanup = crate::pro::read_supervisor_cleanup().await?;
    // A cluster workspace job: its data dir is the workspace's folder on the
    // shared filesystem, and the manifest there is the workspace's lease. A
    // previous job of the same workspace may still be shutting down (a
    // "continue on a new node" handoff) — wait it out before reading a
    // single store from that dir.
    let own_job = std::env::var("SLURM_JOB_ID")
        .ok()
        .filter(|j| !j.trim().is_empty());
    // Only a cluster workspace job holds a lease: a daemon someone starts by
    // hand inside an allocation keeps today's single-daemon check below.
    let workspace_job = std::env::var_os(chimaera_core::cluster::ENV_CLUSTER_WORKSPACE)
        .is_some_and(|v| !v.is_empty());
    let previous_job = match own_job.as_deref() {
        Some(own) if workspace_job => wait_for_previous_job(own).await,
        _ => None,
    };

    // One daemon per state dir: the manifest is the registry, and a second
    // daemon over the same ledger respawns every session AGAIN (duplicate
    // agent processes), while each failed-connect retry piles one more daemon
    // onto a shared login node. Refuse before touching anything — including
    // the handoff, which is consume-once. Only a manifest whose pid is alive
    // AND whose port answers HTTP counts: a crash leftover or a recycled pid
    // must not block startup. Best-effort (not a lock) — it closes the retry
    // pile-up, not a deliberate simultaneous double-start race.
    if let Ok(Some(m)) = chimaera_core::Manifest::load() {
        // The previous job's record, left standing by a job the lease wait
        // proved ended: its pid and port say nothing here (another node's,
        // or a dead process on this one that a recycled pid could
        // impersonate), so it is simply overwritten below.
        let left_by_previous_job = previous_job.is_some() && m.slurm_job_id == previous_job;
        if left_by_previous_job {
            tracing::info!(
                job = m.slurm_job_id.as_deref().unwrap_or(""),
                node = %m.hostname,
                "taking over this workspace from its previous job"
            );
        } else if !m.written_here() {
            // On a home shared across nodes the record may be another
            // node's, whose pid and port can't be checked from here. Not
            // refused: a renamed host (a laptop's DHCP-assigned name) looks
            // the same and must still start. `chimaera connect` only lands
            // here after proving that node's daemon gone — by probing it
            // there, or because the node's name no longer resolves on this
            // one.
            tracing::warn!(
                node = %m.hostname,
                pid = m.pid,
                "the manifest was written on another node; this daemon takes over the registry — a daemon still running there is no longer reachable through it"
            );
        }
        if !left_by_previous_job && m.is_alive() && port_answers_http(m.port).await {
            anyhow::bail!(
                "a chimaera daemon for {} is already running (pid {}, \
                 http://127.0.0.1:{}) — refusing to start a second",
                chimaera_core::data_dir().display(),
                m.pid,
                m.port
            );
        }
    }

    // A predecessor that stopped gracefully left a handoff: rebind its port
    // with its token so ssh forwards stay valid and every client heals with
    // a plain reconnect — the "update without losing your windows" half of
    // the restart story (the ledger is the sessions half). An explicit
    // conflicting --port wins over the handoff; a crash never leaves one.
    // A cluster workspace never restarts in place — job-host starts its
    // chimaera fresh on every open — so a handoff there is one a close left
    // (in this job or another, maybe on another node): consumed, never
    // honored. Each open gets its own token.
    let handoff = chimaera_core::Handoff::consume().filter(|_| {
        if workspace_job {
            tracing::info!(
                "a closed workspace's restart handoff was dropped: this open gets its own token"
            );
        }
        !workspace_job
    });
    let (listener, token) = match handoff.filter(|h| cfg.port.is_none() || cfg.port == Some(h.port))
    {
        Some(handoff) => match listener_after_handoff(handoff.port, cfg.routable_bind).await? {
            (listener, true) => (listener, handoff.token),
            (listener, false) => {
                tracing::warn!(
                    port = handoff.port,
                    "handoff port still busy; started fresh on an OS-assigned port"
                );
                (listener, chimaera_core::generate_token())
            }
        },
        None => (
            fresh_listener(cfg.port, cfg.routable_bind).await?,
            chimaera_core::generate_token(),
        ),
    };
    let port = listener.local_addr()?.port();

    let hostname = hostname::get()
        .context("failed to read hostname")?
        .to_string_lossy()
        .into_owned();
    let pid = std::process::id();
    let started_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();

    let manifest = chimaera_core::Manifest {
        hostname: hostname.clone(),
        port,
        token: token.clone(),
        pid,
        version: chimaera_core::VERSION.to_string(),
        started_at,
        build: Some(chimaera_core::BUILD_ID.to_string()),
        slurm_job_id: own_job.clone(),
        runtime_leases: true,
    };
    manifest.write().context("failed to write manifest")?;

    println!("chimaera daemon listening on 127.0.0.1:{port}");
    println!("http://127.0.0.1:{port}/#token={token}");

    let mut state = AppState::new(
        token,
        hostname,
        pid,
        port,
        chimaera_core::data_dir(),
        chimaera_core::config_dir(),
    );
    state.daemon_extension = runtime;
    let managed_root = chimaera_core::managed_agents_dir();
    if state.managed_root != managed_root {
        state.legacy_managed_root = Some(std::mem::replace(&mut state.managed_root, managed_root));
    }
    let state = Arc::new(state);

    // A cluster workspace job registers the workspace it was started for
    // (under the cluster's id) before anything — the ledger's resurrection
    // included — looks the workspace up.
    {
        let state = state.clone();
        let _ =
            tokio::task::spawn_blocking(move || crate::workspaces::seed_cluster_workspace(&state))
                .await;
    }

    crate::pro::stage_supervisor_cleanup(&state, supervisor_cleanup)?;
    #[cfg(all(
        unix,
        feature = "provider-authority-prototype",
        feature = "daemon-extension-fixture"
    ))]
    if let Some(start) = provider_fixture {
        start(crate::provider_fixture::Context::from_state(state.clone())?)?;
    }

    // Theming shims: regenerated at every daemon start (and after installs /
    // uninstalls / settings edits) so they always match this build's resolution
    // and the current managed-install + explicit-path picture.
    runtimes::regenerate_shims(&state);

    // Backstop poll for out-of-band git changes (external editor, terminal
    // `git` commands); event-driven refresh covers the rest. Idle-cheap.
    tokio::spawn(git::backstop_poll(state.clone()));
    tokio::spawn(git::track_sessions(state.clone()));

    // Settings hand-edit watcher: one off-reactor stat every couple of
    // seconds, so the events bus never stats settings.json on the reactor.
    tokio::spawn(settings::watch_external_edits(state.clone()));

    // Session ledger: consume what the previous daemon left (resurrect /
    // retire), then keep sessions.json reconciled until shutdown. Flip
    // `restored` false HERE, before the listener accepts: the spawned task
    // may not have run yet when the first client connects, and that client's
    // sessions snapshot must wait out the resurrection (see AppState).
    state.restored.send_replace(false);
    // Session history: close what a daemon that died without a graceful stop
    // left open — BEFORE the ledger resurrects those sessions under new
    // records — then keep checkpointing and sweeping.
    crate::history::boot_close(&state).await;
    crate::history::spawn_task(state.clone());
    tokio::spawn(ledger::run(state.clone()));
    crate::runtime_retention::boot(state.clone());

    // Release awareness (GET /api/v1/update + the `update` ws frame), and
    // the same question for the agent CLIs the daemon launches.
    tokio::spawn(update::run_checker(state.clone()));
    tokio::spawn(agent_updates::run_checker(state.clone()));

    // Idle sweep for browser-pane proxy sessions (kills their relay children).
    tokio::spawn(proxy::sweeper(state.clone()));

    // The notice feed's edge detector (agent finished / needs you).
    tokio::spawn(crate::notices::run(state.clone()));

    // Uploads left by sessions that ended while no daemon was watching
    // (crashes, unclean stops) — swept once restore has decided which
    // sessions still exist.
    crate::upload::spawn_boot_prune(state.clone());
    // Transfer leftovers (staging copies, Git locks, temporary archives) from
    // a previous daemon life that never finished them.
    crate::pro::sweep_leftovers(&state);

    // `state.clone()` (not a move) so the post-serve ledger snapshot + handoff
    // below still own it after graceful shutdown returns.
    axum::serve(listener, app(state.clone()))
        .with_graceful_shutdown(shutdown_signal(state.clone()))
        .await
        .context("server error")?;

    // Graceful stop = planned: flush the ledger (the reconciler's last write
    // may be a few seconds stale) and leave a handoff so a successor within
    // the freshness window keeps this port + token. Sessions die with this
    // process — the ledger written here is exactly what resurrects them, and
    // `ledger::snapshot` now covers chat sessions too, so a successor brings
    // them back (resumable ones live, the rest into Recents at boot). We must
    // NOT retire the LIVE chats here: that removes their workspace mapping, so
    // the reconciler's next snapshot would drop them and they'd never resurrect.
    let (entries, links) = ledger::snapshot(&state);
    lock(&state.ledger).write_if_changed(&entries, &links);

    // A relay child (`ssh -N -L`) outlives its parent unless killed — never
    // strand one on a login node.
    proxy::shutdown_relays(&state);

    // Dead-but-visible chats (a ProtocolError entry the ChatManager keeps in
    // the registry with alive=false) are excluded from the snapshot above, so
    // resurrection never touches them. They're still resumable conversations
    // (codex has no transcript-store backstop), so retire them into Recents now
    // — exactly what the old blanket retire loop did — or they'd vanish on the
    // restart. Only the dead ones: retiring a live chat would strip the mapping
    // the reconciler already captured.
    for info in state.chat.list() {
        if !info.alive {
            recents::retire_with_resume(
                &state,
                &info.id,
                None,
                None,
                chimaera_agent::model::SessionUi::Chat,
                info.native_session_id,
            );
        }
    }

    // Every open history record closes as `retired` (the daemon ended it; a
    // resurrected session continues under a new record), and the writes
    // land before the process goes.
    {
        let state = state.clone();
        let _ =
            tokio::task::spawn_blocking(move || crate::history::close_all_for_exit(&state)).await;
    }

    // Only now, with the ledger flushed and the dead chats settled, end the
    // live chat agents cleanly so their own teardown stops their background
    // work (see `chat::stop_all_for_exit`); they resurrect from the ledger.
    crate::chat::stop_all_for_exit(&state).await;
    // Managed agents are proven stopped here, so a same-boot successor (an
    // update or restart) does not treat their launch evidence as a crash.
    crate::pro::shutdown(&state).await;
    // Plugins' programs end with the daemon, their whole process groups (a
    // build is started again on the next save; nothing resumes it).
    state.plugin_platform.jobs.kill_all();

    if let Err(err) = chimaera_core::Handoff::new(port, state.token.clone()).write() {
        tracing::warn!(%err, "failed to write restart handoff");
    }

    // Only our own record: on a home shared across nodes a daemon on another
    // node may own the file by now, and unlinking it would hide that live
    // daemon from every client.
    if !manifest
        .remove_if_owned()
        .context("failed to remove manifest")?
    {
        tracing::warn!("the manifest now belongs to another daemon; left in place");
    }
    tracing::info!("chimaera daemon stopped");
    Ok(())
}

/// Lease poll cadence: the manifest is a cheap stat, the scheduler is asked
/// at most once a minute (the etiquette every scheduler check here keeps).
const LEASE_FILE_CHECK: Duration = Duration::from_secs(15);
const LEASE_SQUEUE_FLOOR: Duration = Duration::from_secs(60);

/// The workspace lease, for a daemon running inside Slurm job `own`: when
/// the data dir's manifest names a DIFFERENT job, that job may still be
/// shutting down (or, during a "continue on a new node", still serving until
/// the app stops it) — two daemons over one data dir would both write its
/// ledger and journals. So wait until the manifest is gone (the previous
/// daemon's graceful stop removes it last) or Slurm says that job is no
/// longer live. Returns the job waited on — the last holder, whose record
/// may still stand (the caller overwrites it, and drops the handoff its
/// graceful stop left) — or `None` when no other job held the workspace.
///
/// Bounded per step and cheap: a stat every 15 s, one 5 s-capped `squeue -j`
/// per minute, logged at info. Overall it is bounded by this job's own time
/// limit — taking over a live workspace is never the fallback; the person
/// who started both jobs stops one.
async fn wait_for_previous_job(own: &str) -> Option<String> {
    let path = chimaera_core::Manifest::path();
    // Only resolved once another job is found holding the lease: the common
    // boot (no manifest, or our own) runs no login shell for it. Stopping
    // the job this one replaces is job-host's (once, for the whole job).
    let squeue = || async {
        let bindir = std::env::var_os("CHIMAERA_SLURM_BINDIR").map(PathBuf::from);
        match crate::compute::detect_tools(bindir.as_deref()).await {
            Some(crate::compute::Detection::Slurm { squeue, .. }) => Some(squeue),
            _ => None,
        }
    };
    lease_wait(own, &path, squeue, LEASE_FILE_CHECK, LEASE_SQUEUE_FLOOR).await
}

/// The record at `path` when it names a Slurm job other than `own`.
async fn other_jobs_manifest(path: &Path, own: &str) -> Option<chimaera_core::Manifest> {
    let (path, own) = (path.to_path_buf(), own.to_string());
    tokio::task::spawn_blocking(move || {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str::<chimaera_core::Manifest>(&text)
            .ok()
            .filter(|m| m.slurm_job_id.as_deref().is_some_and(|j| j != own))
    })
    .await
    .ok()
    .flatten()
}

/// [`wait_for_previous_job`] with its inputs explicit (tests drive it with
/// a stand-in squeue and short cadences).
async fn lease_wait<F, Fut>(
    own: &str,
    manifest_path: &Path,
    find_squeue: F,
    file_check: Duration,
    squeue_floor: Duration,
) -> Option<String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Option<PathBuf>>,
{
    let mut manifest = other_jobs_manifest(manifest_path, own).await?;
    let mut waiting_on = manifest.slurm_job_id.clone()?;
    tracing::info!(job = %waiting_on, node = %manifest.hostname,
        "this workspace's previous job still holds it; waiting for it to end");
    let squeue = find_squeue().await;
    if squeue.is_none() {
        tracing::warn!("no squeue found; waiting for the previous job's manifest to go away");
    }
    let mut asked: Option<Instant> = None;
    loop {
        if asked.is_none_or(|at| at.elapsed() >= squeue_floor) {
            asked = Some(Instant::now());
            let live = match &squeue {
                Some(squeue) => crate::compute::job_is_live(squeue, &waiting_on).await,
                // No scheduler to ask: a record written on this node by a
                // process that is gone is the one thing still provable.
                None => (manifest.written_here() && !manifest.is_alive()).then_some(false),
            };
            match live {
                Some(false) => {
                    tracing::info!(job = %waiting_on, "the previous job has ended; taking over this workspace");
                    return Some(waiting_on);
                }
                Some(true) => {
                    tracing::info!(job = %waiting_on, "the previous job is still live; asking again in a minute");
                }
                None => {
                    tracing::info!(job = %waiting_on, "could not ask Slurm about the previous job; asking again in a minute");
                }
            }
        }
        tokio::time::sleep(file_check).await;
        let Some(next) = other_jobs_manifest(manifest_path, own).await else {
            tracing::info!(job = %waiting_on, "the previous job released this workspace");
            return Some(waiting_on);
        };
        if next.slurm_job_id.as_deref() != Some(waiting_on.as_str()) {
            // Another job took the lease meanwhile: that one is now the
            // holder to wait out, asked about right away.
            waiting_on = next.slurm_job_id.clone()?;
            tracing::info!(job = %waiting_on, "another job now holds this workspace; waiting for it instead");
            asked = None;
        }
        manifest = next;
    }
}

/// Whether an HTTP server answers on `127.0.0.1:port` within 2s. Any status
/// counts — even a 401 had to come from a live server. The manifest's pid
/// check alone can't be trusted on a long-lived login node (pids recycle), so
/// only a served response proves the manifest's daemon is really there.
async fn port_answers_http(port: u16) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let attempt = async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .ok()?;
        stream
            .write_all(
                format!(
                    "GET /api/v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .ok()?;
        let mut buf = [0u8; 5];
        stream.read_exact(&mut buf).await.ok()?;
        (&buf == b"HTTP/").then_some(())
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), attempt)
        .await
        .ok()
        .flatten()
        .is_some()
}

/// Loopback is the rule; 0.0.0.0 is `--bind-routable`, which a cluster
/// workspace job's `chimaera serve` passes (reached by a plain `ssh -L`
/// through the login node; token-gated).
/// Every listener bind — fresh or handoff-rebind — must resolve its host
/// here, or a restart silently demotes a routable daemon to loopback-only.
fn bind_host(routable: bool) -> &'static str {
    if routable {
        "0.0.0.0"
    } else {
        "127.0.0.1"
    }
}

async fn fresh_listener(port: Option<u16>, routable: bool) -> anyhow::Result<TcpListener> {
    let host = bind_host(routable);
    TcpListener::bind((host, port.unwrap_or(0)))
        .await
        .with_context(|| format!("failed to bind {host}"))
}

/// Acquire the startup listener when a handoff was consumed: rebind the
/// handoff port, or — if it's STILL busy after `rebind`'s ~5s — an OS-assigned
/// port. Returns `(listener, reused)`; `reused` = keep the handoff token.
///
/// Never retries the requested port in the fallback: `rebind` already spent
/// its budget on it, so re-binding it would just fail and take the daemon
/// down. This is why the fallback binds `None`, not the requested port —
/// staying up on a fresh port beats dying on a transient clash.
async fn listener_after_handoff(
    handoff_port: u16,
    routable: bool,
) -> anyhow::Result<(TcpListener, bool)> {
    match rebind(handoff_port, routable).await {
        Some(listener) => Ok((listener, true)),
        None => Ok((fresh_listener(None, routable).await?, false)),
    }
}

/// Try the handoff port for ~5s: the predecessor releases it at exit, but
/// its teardown can lag the successor's start.
async fn rebind(port: u16, routable: bool) -> Option<TcpListener> {
    for _ in 0..20 {
        if let Ok(listener) = TcpListener::bind((bind_host(routable), port)).await {
            return Some(listener);
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    None
}

/// Resolve when SIGINT (ctrl-c) or SIGTERM is received, or when an in-band
/// `POST /shutdown` signals `state.shutdown`.
async fn shutdown_signal(state: Arc<AppState>) {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(%err, "failed to install ctrl-c handler");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(err) => {
                tracing::error!(%err, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
        _ = state.shutdown.notified() => {},
    }
    tracing::info!("shutdown signal received");
    // Release long-held requests before axum starts draining them.
    state
        .stopping
        .store(true, std::sync::atomic::Ordering::Relaxed);
    #[cfg(all(unix, feature = "provider-authority-prototype"))]
    crate::pro::retire_provider_startup(&state);
    state.changes.notify_waiters();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// When the handoff port stays busy through `rebind`, the fallback must
    /// bind an OS-assigned port and report `reused=false` — NOT retry the busy
    /// port (the old `fresh_listener(cfg.port)` bug, which took the daemon
    /// down when an explicit `--port` equalled the handoff port). ~5s: `rebind`
    /// exhausts its retry budget against the occupied port first.
    #[tokio::test]
    async fn handoff_falls_back_to_a_fresh_port_when_the_port_stays_busy() {
        let occupied = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let busy_port = occupied.local_addr().unwrap().port();

        let (listener, reused) = listener_after_handoff(busy_port, false)
            .await
            .expect("must stay up on a fresh port, not error");
        assert!(!reused, "a busy handoff port cannot be reused");
        assert_ne!(
            listener.local_addr().unwrap().port(),
            busy_port,
            "must fall back to a different (OS-assigned) port"
        );
    }

    /// The single-instance guard keys on this probe: an HTTP answer means a
    /// live daemon (refuse to double-start), while an accept-only listener or
    /// a closed port means the manifest is stale (a crash leftover, a recycled
    /// pid) and startup must proceed.
    #[tokio::test]
    async fn port_answers_http_requires_a_response_not_just_an_accept() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // Accepts and holds the socket open, never writing: not a daemon.
        let silent = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let silent_port = silent.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((s, _)) = silent.accept().await {
                held.push(s);
            }
        });
        assert!(!port_answers_http(silent_port).await);

        // Answers any bytes with an HTTP status line: a live daemon.
        let http = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let http_port = http.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = http.accept().await {
                let mut buf = [0u8; 512];
                let _ = s.read(&mut buf).await;
                let _ = s
                    .write_all(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n")
                    .await;
            }
        });
        assert!(port_answers_http(http_port).await);

        // Nothing listening: stale manifest, start normally.
        let free = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let free_port = free.local_addr().unwrap().port();
        drop(free);
        assert!(!port_answers_http(free_port).await);
    }

    fn lease_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("chimaera-lease-{label}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_manifest(path: &Path, job: Option<&str>) {
        let m = chimaera_core::Manifest {
            hostname: "elsewhere".into(),
            port: 1,
            token: "t".into(),
            pid: 1,
            version: "0".into(),
            started_at: 0,
            build: None,
            slurm_job_id: job.map(str::to_string),
            runtime_leases: false,
        };
        std::fs::write(path, serde_json::to_vec(&m).unwrap()).unwrap();
    }

    /// A stand-in squeue that answers with whatever `state` holds (empty =
    /// not listed) and logs each question.
    fn fake_squeue(dir: &Path) -> PathBuf {
        let squeue = dir.join("squeue");
        std::fs::write(
            &squeue,
            format!(
                "#!/bin/sh\necho \"$@\" >> '{log}'\ncat '{state}' 2>/dev/null\n",
                log = dir.join("asks").display(),
                state = dir.join("state").display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&squeue, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        squeue
    }

    const FAST: Duration = Duration::from_millis(20);

    /// No manifest, our own, or a non-job daemon's: nothing to wait for,
    /// and squeue is never even looked for.
    #[tokio::test]
    async fn lease_wait_is_free_without_another_jobs_manifest() {
        let dir = lease_dir("free");
        let path = dir.join("manifest.json");
        let never = || async { panic!("squeue looked up without a previous job") };
        assert_eq!(lease_wait("200", &path, never, FAST, FAST).await, None);
        write_manifest(&path, Some("200"));
        assert_eq!(lease_wait("200", &path, never, FAST, FAST).await, None);
        write_manifest(&path, None);
        assert_eq!(lease_wait("200", &path, never, FAST, FAST).await, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A previous job that is still live is waited out — squeue asked at
    /// most once per floor — and taken over once Slurm says it ended.
    #[tokio::test]
    async fn lease_wait_holds_until_the_previous_job_ends() {
        let dir = lease_dir("ends");
        let path = dir.join("manifest.json");
        write_manifest(&path, Some("100"));
        let squeue = fake_squeue(&dir);
        std::fs::write(dir.join("state"), "RUNNING\n").unwrap();
        let state = dir.join("state");
        let flip = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            std::fs::write(state, "CANCELLED by 1000\n").unwrap();
        });
        let squeue_path = squeue.clone();
        let got = lease_wait(
            "200",
            &path,
            || async move { Some(squeue_path) },
            FAST,
            Duration::from_millis(150),
        )
        .await;
        flip.await.unwrap();
        assert_eq!(got.as_deref(), Some("100"));
        let asks = std::fs::read_to_string(dir.join("asks")).unwrap();
        assert!(asks.lines().all(|l| l == "-h -j 100 -o %T"), "{asks}");
        // ~400 ms at one ask per 150 ms: a handful, never one per file check.
        assert!(asks.lines().count() <= 5, "{asks}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The previous daemon's graceful stop removes its manifest last: that
    /// releases the lease even while squeue still lists the job.
    #[tokio::test]
    async fn lease_wait_ends_when_the_manifest_goes() {
        let dir = lease_dir("released");
        let path = dir.join("manifest.json");
        write_manifest(&path, Some("100"));
        let squeue = fake_squeue(&dir);
        std::fs::write(dir.join("state"), "COMPLETING\n").unwrap();
        let gone = path.clone();
        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            std::fs::remove_file(gone).unwrap();
        });
        let got = lease_wait(
            "200",
            &path,
            || async move { Some(squeue) },
            FAST,
            Duration::from_secs(60),
        )
        .await;
        release.await.unwrap();
        assert_eq!(got.as_deref(), Some("100"), "the job it waited on");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A free handoff port is rebound and its token reused.
    #[tokio::test]
    async fn handoff_reuses_a_free_port() {
        let free = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = free.local_addr().unwrap().port();
        drop(free); // release it so rebind can take it

        let (listener, reused) = listener_after_handoff(port, false).await.expect("rebind");
        assert!(reused, "a free handoff port is reused");
        assert_eq!(listener.local_addr().unwrap().port(), port);
        assert!(
            listener.local_addr().unwrap().ip().is_loopback(),
            "without --bind-routable a rebind stays loopback"
        );
    }

    /// A --bind-routable daemon consuming a handoff must come back routable:
    /// `rebind` honors the flag like `fresh_listener` does, instead of
    /// hardcoding loopback (which silently demoted a cluster workspace job's
    /// daemon to unreachable-from-the-login-forward after a restart).
    #[tokio::test]
    async fn handoff_rebind_honors_routable_bind() {
        let free = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = free.local_addr().unwrap().port();
        drop(free);

        let (listener, reused) = listener_after_handoff(port, true).await.expect("rebind");
        assert!(reused, "a free handoff port is reused");
        let addr = listener.local_addr().unwrap();
        assert_eq!(addr.port(), port);
        assert!(
            addr.ip().is_unspecified(),
            "routable rebind must bind 0.0.0.0, got {addr}"
        );
    }
}
