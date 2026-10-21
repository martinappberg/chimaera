use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use tokio::net::TcpListener;

use crate::{agent_updates, git, ledger, proxy, recents, runtimes, settings, update};
use crate::{app, AppState, ServerConfig};

/// Bind on 127.0.0.1, write the manifest, and serve until SIGINT/SIGTERM.
pub async fn run(cfg: ServerConfig) -> anyhow::Result<()> {
    // A cluster workspace job: its data dir is the workspace's folder on the
    // shared filesystem, and the lease file there keeps it open in one job
    // at a time. A previous job of the same workspace may still be shutting
    // down (a "continue on a new node" handoff) — wait it out before reading
    // a single store from that dir.
    let own_job = std::env::var("SLURM_JOB_ID")
        .ok()
        .filter(|j| !j.trim().is_empty());
    // Only a cluster workspace job holds a lease: a daemon someone starts by
    // hand inside an allocation keeps today's single-daemon check below.
    let workspace_job = std::env::var_os(chimaera_core::cluster::ENV_CLUSTER_WORKSPACE)
        .is_some_and(|v| !v.is_empty());
    let lease = match own_job.as_deref() {
        Some(own) if workspace_job => Some(take_workspace_lease(own).await?),
        _ => None,
    };
    // A manifest names its job too: a holder from a build that predates the
    // lease file is waited out the old way (and the job just taken over
    // from is not asked about twice).
    let previous_job = match (own_job.as_deref(), &lease) {
        (Some(own), Some((_, taken_from))) => wait_for_previous_job(own, taken_from.as_deref())
            .await
            .or_else(|| taken_from.clone()),
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
    // The one exception is `final_flush`'s own fallback: a ledger it still
    // can't read is left alone, and then the live agents retire to Recents.
    ledger::final_flush(&state).await;

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
    // The lease goes last: whoever takes the workspace next finds the
    // manifest already gone.
    if let Some((own, _)) = &lease {
        let own = own.clone();
        let _ =
            tokio::task::spawn_blocking(move || release_lease(&lease_path(), |h| *h == own)).await;
    }
    tracing::info!("chimaera daemon stopped");
    Ok(())
}

/// Lease poll cadence: the manifest is a cheap stat, the scheduler is asked
/// at most once a minute (the etiquette every scheduler check here keeps).
const LEASE_FILE_CHECK: Duration = Duration::from_secs(15);
const LEASE_SQUEUE_FLOOR: Duration = Duration::from_secs(60);

/// The workspace lease file, next to the manifest in the data dir.
pub(crate) const LEASE_FILE: &str = "lease.json";

fn lease_path() -> PathBuf {
    chimaera_core::data_dir().join(LEASE_FILE)
}

/// Who holds a cluster workspace. Created whole by one atomic exclusive
/// create and never edited; a takeover compares the entire record.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct LeaseHolder {
    pub(crate) slurm_job_id: String,
    pub(crate) node: String,
    pub(crate) pid: u32,
    /// Random per acquisition, so no later record ever equals an earlier one
    /// (a compare can't mistake a new holder for the dead one it replaced).
    pub(crate) nonce: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LeaseOutcome {
    /// Ours now; `replaced` is the dead holder taken over from, if any.
    Acquired { replaced: Option<LeaseHolder> },
    /// Someone else's; `None` when the record can't be read as one.
    Held(Option<LeaseHolder>),
}

/// The record at `path`, when there is one (a lease is a few hundred bytes;
/// anything bigger isn't one).
pub(crate) fn read_lease(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut bytes = Vec::new();
    file.take(4096).read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

/// The holder [`read_lease`] names, if the record parses as one.
pub(crate) fn lease_holder(path: &Path) -> Option<LeaseHolder> {
    parse_holder(&read_lease(path).ok()??)
}

/// A record is a holder only with a nonce that can name its takeover marker
/// (ours are 16 hex; anything else is foreign, never taken over).
fn parse_holder(bytes: &[u8]) -> Option<LeaseHolder> {
    serde_json::from_slice::<LeaseHolder>(bytes)
        .ok()
        .filter(|h| (1..=64).contains(&h.nonce.len()))
        .filter(|h| h.nonce.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Take the lease at `path` for `own`, exclusively: of any number of
/// contenders — on this node or another sharing the filesystem — exactly one
/// is `Acquired`. A record already there is taken over only when `is_live`
/// says its holder is gone (`Some(false)`), and only that very record is
/// removed ([`remove_record`]). Blocking (shared-filesystem I/O): call it off
/// the reactor.
pub(crate) fn acquire_lease(
    path: &Path,
    own: &LeaseHolder,
    is_live: impl Fn(&LeaseHolder) -> Option<bool>,
) -> std::io::Result<LeaseOutcome> {
    let own_bytes = serde_json::to_vec(own).map_err(std::io::Error::other)?;
    let mut replaced = None;
    let mut last = None;
    // Each round either wins, sees a live holder, or lost a race to another
    // contender; a few rounds settle any real contention.
    for _ in 0..8 {
        if create_exclusive(path, &own_bytes)? {
            return Ok(LeaseOutcome::Acquired { replaced });
        }
        let Some(current) = read_lease(path)? else {
            continue; // released between the create and the read
        };
        let Some(holder) = parse_holder(&current) else {
            return Ok(LeaseOutcome::Held(None));
        };
        if holder == *own {
            return Ok(LeaseOutcome::Acquired { replaced });
        }
        if is_live(&holder) != Some(false) {
            return Ok(LeaseOutcome::Held(Some(holder)));
        }
        match remove_record(path, &holder, &current, own, &is_live)? {
            Removal::Removed => replaced = Some(holder.clone()),
            Removal::Changed => {}
            // Another contender is taking it over: that one is who to wait on.
            Removal::Busy(claimant) => {
                return Ok(LeaseOutcome::Held(claimant.or(Some(holder))));
            }
        }
        last = Some(holder);
    }
    Ok(LeaseOutcome::Held(last))
}

/// Remove the lease at `path` when `is_ours` says the record there is ours
/// (a daemon's graceful stop; job-host after a crashed child) — through the
/// same takeover marker as a takeover, so it never races one.
pub(crate) fn release_lease(path: &Path, is_ours: impl Fn(&LeaseHolder) -> bool) {
    let Ok(Some(current)) = read_lease(path) else {
        return;
    };
    let Some(holder) = parse_holder(&current).filter(|h| is_ours(h)) else {
        return;
    };
    let me = LeaseHolder {
        slurm_job_id: std::env::var("SLURM_JOB_ID").unwrap_or_default(),
        node: chimaera_core::this_node().unwrap_or_default(),
        pid: std::process::id(),
        nonce: chimaera_core::generate_token()[..16].to_string(),
    };
    if let Err(e) = remove_record(path, &holder, &current, &me, |_| None) {
        tracing::warn!(path = %path.display(), "could not release the workspace lease: {e}");
    }
}

#[derive(Debug)]
enum Removal {
    Removed,
    /// The lease no longer holds that record (or a stale marker was just
    /// cleared): look again.
    Changed,
    /// Another contender holds the marker for that record.
    Busy(Option<LeaseHolder>),
}

/// The takeover marker for `holder`'s record: `.lease.json.takeover-<nonce>`.
fn marker_path(path: &Path, holder: &LeaseHolder) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.takeover-{}", holder.nonce))
}

/// Remove the record `expected` (`expected_bytes` on disk) from `path`, as
/// `claimant`. Every removal of a record — takeover or release — first
/// creates that record's takeover marker exclusively, so exactly one actor
/// ever removes a given record, and the lease can only change away from it
/// through that actor: after the marker is ours, a lease that still reads
/// `expected` is safe to unlink. A marker left by a claimant that is proven
/// gone (`is_live` says `Some(false)`) is cleared; a live one's is waited on.
fn remove_record(
    path: &Path,
    expected: &LeaseHolder,
    expected_bytes: &[u8],
    claimant: &LeaseHolder,
    is_live: impl Fn(&LeaseHolder) -> Option<bool>,
) -> std::io::Result<Removal> {
    let marker = marker_path(path, expected);
    let claimant_bytes = serde_json::to_vec(claimant).map_err(std::io::Error::other)?;
    if !create_exclusive(&marker, &claimant_bytes)? {
        let Some(bytes) = read_lease(&marker)? else {
            return Ok(Removal::Changed); // its claimant just finished
        };
        let other = parse_holder(&bytes);
        if let Some(other) = other.as_ref().filter(|o| is_live(o) == Some(false)) {
            // A claimant that died mid-takeover. Cleared by compare: a breaker
            // racing another breaker of the same dead claimant is the one
            // window left, and it needs that crash first.
            if read_lease(&marker)?.is_some_and(|now| now == bytes) {
                tracing::info!(job = %other.slurm_job_id, "clearing a takeover its claimant never finished");
                let _ = std::fs::remove_file(&marker);
            }
            return Ok(Removal::Changed);
        }
        return Ok(Removal::Busy(other));
    }
    let removed = match read_lease(path) {
        Ok(Some(now)) if now == expected_bytes => match std::fs::remove_file(path) {
            Ok(()) => Ok(Removal::Removed),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Removal::Changed),
            Err(e) => Err(e),
        },
        Ok(_) => Ok(Removal::Changed),
        Err(e) => Err(e),
    };
    let _ = std::fs::remove_file(&marker);
    removed
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    path.with_file_name(format!(
        ".{name}.{}.{suffix}",
        &chimaera_core::generate_token()[..16]
    ))
}

/// Create `path` holding `bytes`, or `false` when it already exists. A
/// hard link of a complete temp file is atomic and exclusive on NFS too,
/// and no reader ever sees a half-written record.
fn create_exclusive(path: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    use std::io::{ErrorKind, Write};
    let tmp = sibling(path, "tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options.open(&tmp).and_then(|mut f| f.write_all(bytes));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let linked = std::fs::hard_link(&tmp, path);
    // NFS can answer a retransmitted LINK with EEXIST for a link that took:
    // the temp's own link count is the truth.
    #[cfg(unix)]
    let took = || {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(&tmp).is_ok_and(|m| m.nlink() == 2)
    };
    #[cfg(not(unix))]
    let took = || false;
    let result = match linked {
        Ok(()) => Ok(true),
        Err(_) if took() => Ok(true),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(false),
        // A filesystem without hard links: an exclusive create of the
        // record itself (a reader may then briefly see it empty — Held).
        Err(e) if e.kind() == ErrorKind::Unsupported || e.raw_os_error() == Some(1) => {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            match options.open(path) {
                Ok(mut f) => f.write_all(bytes).map(|_| true),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(false),
                Err(e) => Err(e),
            }
        }
        Err(e) => Err(e),
    };
    let _ = std::fs::remove_file(&tmp);
    result
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    i32::try_from(pid)
        .is_ok_and(|p| nix::sys::signal::kill(nix::unistd::Pid::from_raw(p), None).is_ok())
}

#[cfg(not(unix))]
fn pid_alive(pid: u32) -> bool {
    chimaera_core::proc::pid_alive(pid)
}

/// What is provable about `holder` without the scheduler: `Some(false)` for
/// one in `proven_dead` (the scheduler said so) and for a record written on
/// this node by a process that is gone — or that names this very process,
/// which didn't write it (a recycled pid). `None` otherwise.
fn local_verdict(holder: &LeaseHolder, proven_dead: &[LeaseHolder]) -> Option<bool> {
    if proven_dead.contains(holder) {
        return Some(false);
    }
    let here = chimaera_core::this_node()
        .is_some_and(|node| chimaera_core::same_node(&node, &holder.node));
    (here && (holder.pid == std::process::id() || !pid_alive(holder.pid))).then_some(false)
}

/// Take this workspace's lease for Slurm job `own`, waiting out a live
/// holder. Returns our record and the job taken over from, if any.
async fn take_workspace_lease(own: &str) -> anyhow::Result<(LeaseHolder, Option<String>)> {
    let holder = LeaseHolder {
        slurm_job_id: own.to_string(),
        node: chimaera_core::this_node().unwrap_or_default(),
        pid: std::process::id(),
        nonce: chimaera_core::generate_token()[..16].to_string(),
    };
    let squeue = || async {
        let bindir = std::env::var_os("CHIMAERA_SLURM_BINDIR").map(PathBuf::from);
        match crate::compute::detect_tools(bindir.as_deref()).await {
            Some(crate::compute::Detection::Slurm { squeue, .. }) => Some(squeue),
            _ => None,
        }
    };
    let path = tokio::task::spawn_blocking(lease_path).await?;
    let taken_from =
        lease_acquire_wait(&holder, &path, squeue, LEASE_FILE_CHECK, LEASE_SQUEUE_FLOOR)
            .await
            .context("could not take this workspace's lease")?;
    Ok((holder, taken_from))
}

/// [`take_workspace_lease`] with its inputs explicit (tests drive it with a
/// stand-in squeue and short cadences). Same etiquette as [`lease_wait`]: a
/// stat every `file_check`, the scheduler asked about a holder at most once
/// per `squeue_floor`, and only once the file shows another job holds it.
async fn lease_acquire_wait<F, Fut>(
    own: &LeaseHolder,
    path: &Path,
    find_squeue: F,
    file_check: Duration,
    squeue_floor: Duration,
) -> anyhow::Result<Option<String>>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Option<PathBuf>>,
{
    let mut find_squeue = Some(find_squeue);
    let mut squeue: Option<PathBuf> = None;
    // A dead holder, and maybe a claimant that died taking it over.
    let mut proven_dead: Vec<LeaseHolder> = Vec::new();
    let mut asked: Option<(Instant, String)> = None;
    let mut reported: Option<Option<LeaseHolder>> = None;
    loop {
        let (p, o, dead) = (path.to_path_buf(), own.clone(), proven_dead.clone());
        let outcome =
            tokio::task::spawn_blocking(move || acquire_lease(&p, &o, |h| local_verdict(h, &dead)))
                .await??;
        let holder = match outcome {
            LeaseOutcome::Acquired { replaced } => {
                if let Some(old) = &replaced {
                    tracing::info!(job = %old.slurm_job_id, node = %old.node,
                        "the previous holder has ended; took over this workspace");
                }
                return Ok(replaced.map(|h| h.slurm_job_id));
            }
            LeaseOutcome::Held(holder) => holder,
        };
        if reported.as_ref() != Some(&holder) {
            match &holder {
                Some(h) => tracing::info!(job = %h.slurm_job_id, node = %h.node,
                    "another job holds this workspace; waiting for it to end"),
                None => tracing::warn!(path = %path.display(),
                    "this workspace's lease can't be read; waiting — remove it if no job has this workspace open"),
            }
            reported = Some(holder.clone());
        }
        // Our own job is live by definition: only the node check above can
        // free a record an earlier start in this job left.
        if let Some(h) = holder.filter(|h| h.slurm_job_id != own.slurm_job_id) {
            let due = asked
                .as_ref()
                .is_none_or(|(at, job)| *job != h.slurm_job_id || at.elapsed() >= squeue_floor);
            if due {
                if let Some(find) = find_squeue.take() {
                    squeue = find().await;
                    if squeue.is_none() {
                        tracing::warn!("no squeue found; waiting for the lease to be released");
                    }
                }
                asked = Some((Instant::now(), h.slurm_job_id.clone()));
                if let Some(squeue) = &squeue {
                    match crate::compute::job_is_live(squeue, &h.slurm_job_id).await {
                        Some(false) => {
                            if proven_dead.len() >= 4 {
                                proven_dead.remove(0);
                            }
                            proven_dead.push(h);
                            continue;
                        }
                        Some(true) => tracing::info!(job = %h.slurm_job_id,
                            "the holder is still live; asking again in a minute"),
                        None => tracing::info!(job = %h.slurm_job_id,
                            "could not ask Slurm about the holder; asking again in a minute"),
                    }
                }
            }
        }
        tokio::time::sleep(file_check).await;
    }
}

/// The manifest-side wait, for a daemon running inside Slurm job `own`: when
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
///
/// Kept for holders from builds that predate the lease file; `proven_ended`
/// is a job the lease wait already proved gone (not asked about again).
async fn wait_for_previous_job(own: &str, proven_ended: Option<&str>) -> Option<String> {
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
    lease_wait(
        own,
        &path,
        proven_ended,
        squeue,
        LEASE_FILE_CHECK,
        LEASE_SQUEUE_FLOOR,
    )
    .await
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
    proven_ended: Option<&str>,
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
    if proven_ended == Some(waiting_on.as_str()) {
        return Some(waiting_on);
    }
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

/// POST /api/v1/stop — stop the daemon the way SIGTERM does: no session is
/// ended, so the graceful path's final ledger flush and restart handoff
/// carry them to the next start. The in-band stop for hosts without SIGTERM
/// (Windows); `POST /shutdown` is the one that ends every session first.
/// The signal trips just after the reply leaves, so the caller reads it
/// before the listener closes.
pub(crate) async fn stop(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    tracing::info!("in-band stop requested; sessions carry over to the next start");
    tokio::spawn(async move {
        tokio::time::sleep(STOP_REPLY_GRACE).await;
        state.shutdown.notify_one();
    });
    axum::Json(serde_json::json!({ "stopping": true })).into_response()
}

/// How long `stop` lets its reply flush before the graceful drain begins.
const STOP_REPLY_GRACE: Duration = Duration::from_millis(200);

/// Resolve when SIGINT (ctrl-c) or SIGTERM is received, or when an in-band
/// `POST /shutdown` or `POST /stop` signals `state.shutdown`.
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
    state.changes.notify_waiters();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `POST /stop` answers, trips the graceful-shutdown signal, and ends
    /// no session: the graceful path hands them to the next start.
    #[tokio::test]
    async fn stop_route_signals_shutdown_and_keeps_sessions() {
        use http_body_util::BodyExt;
        use tower::ServiceExt;
        let dir = std::env::temp_dir().join(format!(
            "chimaera-stop-route-{}-{}",
            std::process::id(),
            chimaera_core::generate_token()
        ));
        let state = Arc::new(AppState::new(
            "test-token".into(),
            "testhost".into(),
            4242,
            0,
            dir.join("data"),
            dir.join("data/config"),
        ));
        let cwd = dir.join("cwd");
        std::fs::create_dir_all(&cwd).unwrap();
        let session = state
            .sessions
            .spawn(chimaera_pty::SpawnOpts {
                cwd,
                name: None,
                cols: 80,
                rows: 24,
                command: if cfg!(windows) {
                    None
                } else {
                    Some(vec!["sleep".into(), "600".into()])
                },
                id: None,
                env: Vec::new(),
                env_remove: Vec::new(),
                scrollback: None,
            })
            .expect("spawn session");

        let res = app(state.clone())
            .oneshot(
                axum::http::Request::builder()
                    .method(axum::http::Method::POST)
                    .uri("/api/v1/stop")
                    .header(axum::http::header::AUTHORIZATION, "Bearer test-token")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body, serde_json::json!({ "stopping": true }));

        tokio::time::timeout(Duration::from_secs(5), state.shutdown.notified())
            .await
            .expect("the stop route trips the graceful shutdown");
        let info = state
            .sessions
            .get(&session.id)
            .expect("session still registered");
        assert!(info.alive, "the stop route must end no session");

        state.sessions.kill_all();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Unauthenticated, the stop route is refused like every `/api/v1` route.
    #[tokio::test]
    async fn stop_route_requires_the_bearer_token() {
        use tower::ServiceExt;
        let dir = std::env::temp_dir().join(format!(
            "chimaera-stop-auth-{}-{}",
            std::process::id(),
            chimaera_core::generate_token()
        ));
        let state = Arc::new(AppState::new(
            "test-token".into(),
            "testhost".into(),
            4242,
            0,
            dir.join("data"),
            dir.join("data/config"),
        ));
        let res = app(state.clone())
            .oneshot(
                axum::http::Request::builder()
                    .method(axum::http::Method::POST)
                    .uri("/api/v1/stop")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::UNAUTHORIZED);
        assert!(
            tokio::time::timeout(Duration::from_millis(500), state.shutdown.notified())
                .await
                .is_err(),
            "a refused stop must not signal shutdown"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

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
        assert_eq!(
            lease_wait("200", &path, None, never, FAST, FAST).await,
            None
        );
        write_manifest(&path, Some("200"));
        assert_eq!(
            lease_wait("200", &path, None, never, FAST, FAST).await,
            None
        );
        write_manifest(&path, None);
        assert_eq!(
            lease_wait("200", &path, None, never, FAST, FAST).await,
            None
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A previous job that is still live is waited out — squeue asked at
    /// most once per floor — and taken over once Slurm says it ended.
    // The stand-in squeue is a /bin/sh script, and these need its answers
    // (unanswered, the wait holds by design).
    #[cfg(unix)]
    #[tokio::test]
    async fn lease_wait_holds_until_the_previous_job_ends() {
        let dir = lease_dir("ends");
        let path = dir.join("manifest.json");
        write_manifest(&path, Some("100"));
        let squeue = fake_squeue(&dir);
        std::fs::write(dir.join("state"), "RUNNING|chimaera-w~1\n").unwrap();
        let state = dir.join("state");
        let flip = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            std::fs::write(state, "CANCELLED by 1000|chimaera-w~1\n").unwrap();
        });
        let squeue_path = squeue.clone();
        let got = lease_wait(
            "200",
            &path,
            None,
            || async move { Some(squeue_path) },
            FAST,
            Duration::from_millis(150),
        )
        .await;
        flip.await.unwrap();
        assert_eq!(got.as_deref(), Some("100"));
        let asks = std::fs::read_to_string(dir.join("asks")).unwrap();
        assert!(asks.lines().all(|l| l == own_asks("100")), "{asks}");
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
        std::fs::write(dir.join("state"), "COMPLETING|chimaera-w~1\n").unwrap();
        let gone = path.clone();
        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            std::fs::remove_file(gone).unwrap();
        });
        let got = lease_wait(
            "200",
            &path,
            None,
            || async move { Some(squeue) },
            FAST,
            Duration::from_secs(60),
        )
        .await;
        release.await.unwrap();
        assert_eq!(got.as_deref(), Some("100"), "the job it waited on");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The arguments every holder question carries: this user's jobs only,
    /// with the name that proves the job one of ours.
    #[cfg(unix)]
    fn own_asks(job: &str) -> String {
        let user = format!("-u {} ", nix::unistd::Uid::effective().as_raw());
        format!("-h {user}-j {job} -o %T|%j")
    }

    /// A job the lease wait already proved ended is not waited on again.
    #[tokio::test]
    async fn lease_wait_skips_a_job_already_proven_ended() {
        let dir = lease_dir("proven");
        let path = dir.join("manifest.json");
        write_manifest(&path, Some("100"));
        let never = || async { panic!("squeue looked up for a proven-ended job") };
        let got = lease_wait("200", &path, Some("100"), never, FAST, FAST).await;
        assert_eq!(got.as_deref(), Some("100"));
        std::fs::remove_dir_all(&dir).ok();
    }

    fn holder(job: &str, nonce: &str) -> LeaseHolder {
        LeaseHolder {
            slurm_job_id: job.into(),
            node: "elsewhere".into(),
            pid: 1,
            nonce: nonce.into(),
        }
    }

    /// Two jobs opening one closed workspace at the same moment (two
    /// clients, or two nodes behind one shared home): exactly one gets it,
    /// and every other contender sees that one as the holder.
    #[test]
    fn lease_acquire_is_exclusive_under_concurrency() {
        let dir = lease_dir("exclusive");
        let path = dir.join(LEASE_FILE);
        let _ = std::fs::remove_file(&path);
        let start = Arc::new(std::sync::Barrier::new(16));
        let runs: Vec<_> = (0..16)
            .map(|i| {
                let (path, start) = (path.clone(), start.clone());
                std::thread::spawn(move || {
                    let own = holder(&format!("{}", 100 + i), &format!("n{i}"));
                    start.wait();
                    (own.clone(), acquire_lease(&path, &own, |_| None).unwrap())
                })
            })
            .collect();
        let results: Vec<_> = runs.into_iter().map(|r| r.join().unwrap()).collect();
        let winners: Vec<&LeaseHolder> = results
            .iter()
            .filter(|(_, o)| matches!(o, LeaseOutcome::Acquired { replaced: None }))
            .map(|(own, _)| own)
            .collect();
        assert_eq!(winners.len(), 1, "{results:?}");
        let winner = winners[0].clone();
        for (own, outcome) in &results {
            if *own != winner {
                assert_eq!(*outcome, LeaseOutcome::Held(Some(winner.clone())));
            }
        }
        assert_eq!(lease_holder(&path), Some(winner));
        // No temp or set-aside file is left behind.
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.'))
            .collect();
        assert!(left.is_empty(), "{left:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A holder is replaced only when it is proven gone — never while it is
    /// live or unknown — and of many contenders taking over the same dead
    /// holder, exactly one does.
    #[test]
    fn lease_takeover_only_from_a_dead_holder() {
        let dir = lease_dir("takeover");
        let path = dir.join(LEASE_FILE);
        let _ = std::fs::remove_file(&path);
        let old = holder("100", "old");
        assert_eq!(
            acquire_lease(&path, &old, |_| None).unwrap(),
            LeaseOutcome::Acquired { replaced: None }
        );
        // Taking it again as its holder is idempotent.
        assert_eq!(
            acquire_lease(&path, &old, |_| None).unwrap(),
            LeaseOutcome::Acquired { replaced: None }
        );
        let new = holder("200", "new");
        for live in [Some(true), None] {
            assert_eq!(
                acquire_lease(&path, &new, |_| live).unwrap(),
                LeaseOutcome::Held(Some(old.clone())),
                "{live:?}"
            );
        }
        assert_eq!(lease_holder(&path), Some(old.clone()));
        assert_eq!(
            acquire_lease(&path, &new, |h| (*h == old).then_some(false)).unwrap(),
            LeaseOutcome::Acquired {
                replaced: Some(old.clone())
            }
        );
        assert_eq!(lease_holder(&path), Some(new.clone()));

        // Many contenders taking over one dead holder, round after round
        // (each round's winner is the next round's dead holder): exactly one
        // winner every time, and nothing left beside the lease.
        let mut dead = new;
        for round in 0..50 {
            let start = Arc::new(std::sync::Barrier::new(8));
            let runs: Vec<_> = (0..8)
                .map(|i| {
                    let (path, start, dead) = (path.clone(), start.clone(), dead.clone());
                    std::thread::spawn(move || {
                        let own = holder(
                            &format!("{}", 1000 + round * 8 + i),
                            &format!("r{round}t{i}"),
                        );
                        start.wait();
                        let outcome = acquire_lease(&path, &own, |h| (*h == dead).then_some(false));
                        (own, outcome.unwrap())
                    })
                })
                .collect();
            let results: Vec<_> = runs.into_iter().map(|r| r.join().unwrap()).collect();
            let winners: Vec<&LeaseHolder> = results
                .iter()
                .filter(|(_, o)| matches!(o, LeaseOutcome::Acquired { .. }))
                .map(|(own, _)| own)
                .collect();
            assert_eq!(winners.len(), 1, "round {round}: {results:?}");
            assert_eq!(
                lease_holder(&path).as_ref(),
                Some(winners[0]),
                "round {round}"
            );
            assert!(results.iter().all(|(_, o)| matches!(
                o,
                LeaseOutcome::Acquired { .. } | LeaseOutcome::Held(Some(_))
            )));
            let left: Vec<String> = std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n != LEASE_FILE)
                .collect();
            assert!(left.is_empty(), "round {round}: {left:?}");
            dead = winners[0].clone();
        }

        // A takeover its claimant never finished: waited on while that
        // claimant may be live, cleared once it is proven gone.
        let claimant = holder("600", "claim");
        std::fs::write(
            marker_path(&path, &dead),
            serde_json::to_vec(&claimant).unwrap(),
        )
        .unwrap();
        let taker = holder("700", "taker");
        assert_eq!(
            acquire_lease(&path, &taker, |h| (*h == dead).then_some(false)).unwrap(),
            LeaseOutcome::Held(Some(claimant.clone()))
        );
        assert_eq!(lease_holder(&path), Some(dead.clone()));
        assert_eq!(
            acquire_lease(&path, &taker, |h| (*h == dead || *h == claimant)
                .then_some(false))
            .unwrap(),
            LeaseOutcome::Acquired {
                replaced: Some(dead.clone())
            }
        );
        assert!(!marker_path(&path, &dead).exists());

        // An unreadable record is held (by whom is unknown), never taken.
        std::fs::write(&path, b"{garbage").unwrap();
        assert_eq!(
            acquire_lease(&path, &holder("400", "x"), |_| Some(false)).unwrap(),
            LeaseOutcome::Held(None)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A stop removes only its own record.
    #[test]
    fn lease_release_removes_only_its_own_record() {
        let dir = lease_dir("release");
        let path = dir.join(LEASE_FILE);
        let _ = std::fs::remove_file(&path);
        let own = holder("100", "a");
        acquire_lease(&path, &own, |_| None).unwrap();
        release_lease(&path, |h| *h == holder("100", "b"));
        assert_eq!(lease_holder(&path), Some(own.clone()));
        release_lease(&path, |h| *h == own);
        assert!(!path.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The async wait: a live holder is waited out (asked about at most once
    /// per floor, as this user's chimaera job), then taken over.
    // The stand-in squeue is a /bin/sh script, and these need its answers
    // (unanswered, the wait holds by design).
    #[cfg(unix)]
    #[tokio::test]
    async fn lease_acquire_wait_takes_over_once_slurm_says_the_holder_ended() {
        let dir = lease_dir("acquire-wait");
        let path = dir.join(LEASE_FILE);
        let _ = std::fs::remove_file(&path);
        let old = holder("100", "old");
        acquire_lease(&path, &old, |_| None).unwrap();
        let squeue = fake_squeue(&dir);
        std::fs::write(dir.join("state"), "RUNNING|chimaera-w~1\n").unwrap();
        let state = dir.join("state");
        let flip = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            std::fs::write(state, "").unwrap();
        });
        let own = LeaseHolder {
            node: "here".into(),
            ..holder("200", "own")
        };
        let got = lease_acquire_wait(
            &own,
            &path,
            || async move { Some(squeue) },
            FAST,
            Duration::from_millis(100),
        )
        .await
        .unwrap();
        flip.await.unwrap();
        assert_eq!(got.as_deref(), Some("100"));
        assert_eq!(lease_holder(&path), Some(own));
        let asks = std::fs::read_to_string(dir.join("asks")).unwrap();
        assert!(asks.lines().all(|l| l == own_asks("100")), "{asks}");
        assert!(asks.lines().count() <= 6, "{asks}");
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
