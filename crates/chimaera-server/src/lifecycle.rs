use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use tokio::net::TcpListener;

use crate::{app, lock, AppState, ServerConfig};
use crate::{git, ledger, recents, runtimes, update};

/// Bind on 127.0.0.1, write the manifest, and serve until SIGINT/SIGTERM.
pub async fn run(cfg: ServerConfig) -> anyhow::Result<()> {
    // One daemon per state dir: the manifest is the registry, and a second
    // daemon over the same ledger respawns every session AGAIN (duplicate
    // agent processes), while each failed-connect retry piles one more daemon
    // onto a shared login node. Refuse before touching anything — including
    // the handoff, which is consume-once. Only a manifest whose pid is alive
    // AND whose port answers HTTP counts: a crash leftover or a recycled pid
    // must not block startup. Best-effort (not a lock) — it closes the retry
    // pile-up, not a deliberate simultaneous double-start race.
    if let Ok(Some(m)) = chimaera_core::Manifest::load() {
        if m.is_alive() && port_answers_http(m.port).await {
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
    let (listener, token) = match chimaera_core::Handoff::consume()
        .filter(|h| cfg.port.is_none() || cfg.port == Some(h.port))
    {
        Some(handoff) => match listener_after_handoff(handoff.port).await? {
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

async fn fresh_listener(port: Option<u16>) -> anyhow::Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", port.unwrap_or(0)))
        .await
        .context("failed to bind 127.0.0.1")
}

/// Acquire the startup listener when a handoff was consumed: rebind the
/// handoff port, or — if it's STILL busy after `rebind`'s ~5s — an OS-assigned
/// port. Returns `(listener, reused)`; `reused` = keep the handoff token.
///
/// Never retries the requested port in the fallback: `rebind` already spent
/// its budget on it, so re-binding it would just fail and take the daemon
/// down. This is why the fallback binds `None`, not the requested port —
/// staying up on a fresh port beats dying on a transient clash.
async fn listener_after_handoff(handoff_port: u16) -> anyhow::Result<(TcpListener, bool)> {
    match rebind(handoff_port).await {
        Some(listener) => Ok((listener, true)),
        None => Ok((fresh_listener(None).await?, false)),
    }
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

        let (listener, reused) = listener_after_handoff(busy_port)
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

    /// A free handoff port is rebound and its token reused.
    #[tokio::test]
    async fn handoff_reuses_a_free_port() {
        let free = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = free.local_addr().unwrap().port();
        drop(free); // release it so rebind can take it

        let (listener, reused) = listener_after_handoff(port).await.expect("rebind");
        assert!(reused, "a free handoff port is reused");
        assert_eq!(listener.local_addr().unwrap().port(), port);
    }
}
