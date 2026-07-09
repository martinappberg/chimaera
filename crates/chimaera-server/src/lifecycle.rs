use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use tokio::net::TcpListener;

use crate::{app, lock, AppState, ServerConfig};
use crate::{git, ledger, recents, runtimes, update};

/// Bind on 127.0.0.1, write the manifest, and serve until SIGINT/SIGTERM.
pub async fn run(cfg: ServerConfig) -> anyhow::Result<()> {
    // A predecessor that stopped gracefully left a handoff: rebind its port
    // with its token so ssh forwards stay valid and every client heals with
    // a plain reconnect — the "update without losing your windows" half of
    // the restart story (the ledger is the sessions half). An explicit
    // conflicting --port wins over the handoff; a crash never leaves one.
    let (listener, token) = match chimaera_core::Handoff::consume()
        .filter(|h| cfg.port.is_none() || cfg.port == Some(h.port))
    {
        Some(handoff) => match rebind(handoff.port).await {
            Some(listener) => (listener, handoff.token),
            None => {
                tracing::warn!(
                    port = handoff.port,
                    "handoff port still busy; starting fresh"
                );
                (
                    fresh_listener(cfg.port).await?,
                    chimaera_core::generate_token(),
                )
            }
        },
        None => (
            fresh_listener(cfg.port).await?,
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
    };
    manifest.write().context("failed to write manifest")?;

    println!("chimaera daemon listening on 127.0.0.1:{port}");
    println!("http://127.0.0.1:{port}/#token={token}");

    let state = Arc::new(AppState::new(
        token,
        hostname,
        pid,
        port,
        chimaera_core::data_dir(),
        chimaera_core::config_dir(),
    ));

    // Theming shims: regenerated at every daemon start (and after installs /
    // uninstalls / settings edits) so they always match this build's resolution
    // and the current managed-install + explicit-path picture.
    runtimes::regenerate_shims(&state);

    // Backstop poll for out-of-band git changes (external editor, terminal
    // `git` commands); event-driven refresh covers the rest. Idle-cheap.
    tokio::spawn(git::backstop_poll(state.clone()));

    // Session ledger: consume what the previous daemon left (resurrect /
    // retire), then keep sessions.json reconciled until shutdown. Flip
    // `restored` false HERE, before the listener accepts: the spawned task
    // may not have run yet when the first client connects, and that client's
    // sessions snapshot must wait out the resurrection (see AppState).
    state.restored.send_replace(false);
    tokio::spawn(ledger::run(state.clone()));

    // Release awareness (GET /api/v1/update + the `update` ws frame).
    tokio::spawn(update::run_checker(state.clone()));

    // `state.clone()` (not a move) so the post-serve ledger snapshot + handoff
    // below still own it after graceful shutdown returns.
    axum::serve(listener, app(state.clone()))
        .with_graceful_shutdown(shutdown_signal(state.clone()))
        .await
        .context("server error")?;

    // Graceful stop = planned: flush the ledger (the reconciler's last write
    // may be a few seconds stale) and leave a handoff so a successor within
    // the freshness window keeps this port + token. Sessions die with this
    // process — the ledger written here is exactly what resurrects them.
    let (entries, links) = ledger::snapshot(&state);
    lock(&state.ledger).write_if_changed(&entries, &links);

    // Chat sessions are daemon-owned drivers that die with this process, and
    // the ledger does not yet resurrect them (sv-11: real resurrection is a
    // follow-up in `ledger` — snapshot()/restore() cover only PTY sessions).
    // So at a graceful stop (update / restart), retire their conversations
    // into Recents here, so a survivor is offered for manual resume instead of
    // vanishing. Idempotent: a session already retired by an in-band
    // `close-all`/`shutdown` has no AgentRecord left, and `retire` no-ops.
    for info in state.chat.list() {
        recents::retire(&state, &info.id, None, None);
    }

    if let Err(err) = chimaera_core::Handoff::new(port, state.token.clone()).write() {
        tracing::warn!(%err, "failed to write restart handoff");
    }

    chimaera_core::Manifest::remove().context("failed to remove manifest")?;
    tracing::info!("chimaera daemon stopped");
    Ok(())
}

async fn fresh_listener(port: Option<u16>) -> anyhow::Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", port.unwrap_or(0)))
        .await
        .context("failed to bind 127.0.0.1")
}

/// Try the handoff port for ~5s: the predecessor releases it at exit, but
/// its teardown can lag the successor's start.
async fn rebind(port: u16) -> Option<TcpListener> {
    for _ in 0..20 {
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)).await {
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
}
