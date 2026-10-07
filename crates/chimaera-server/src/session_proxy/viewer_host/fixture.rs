//! Nondefault real viewer/router fixture. No production authority setter.
use super::*;
use crate::daemon_extension::Runtime;
use anyhow::ensure;
use std::path::PathBuf;
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

/// Caller owns the private root and the synthetic loopback peer. The fixture
/// uses real placement validation, scope/roster reads and first-frame auth.
pub struct Harness {
    state: Arc<AppState>,
    workspace: String,
    client: Option<Upstream>,
    shutdown: Option<oneshot::Sender<()>>,
    server: Option<JoinHandle<std::io::Result<()>>>,
}
impl Harness {
    pub async fn new(
        root: PathBuf,
        peer: std::net::SocketAddr,
        factory: Option<fn() -> Arc<dyn Runtime>>,
    ) -> Result<Self> {
        ensure!(
            peer.ip() == std::net::Ipv4Addr::LOCALHOST && peer.port() != 0,
            "fixture peer refused"
        );
        let (mut state, workspace) = tokio::task::spawn_blocking(move || {
            use std::os::unix::fs::MetadataExt;
            let root = root.canonicalize()?;
            let meta = std::fs::symlink_metadata(&root)?;
            ensure!(
                meta.is_dir()
                    && meta.uid() == rustix::process::geteuid().as_raw()
                    && meta.mode() & 0o777 == 0o700,
                "fixture root refused"
            );
            let project = root.join("project");
            std::fs::create_dir(&project)?;
            let state = AppState::new(
                "fixture-viewer".into(),
                "fixture-device".into(),
                std::process::id(),
                0,
                root.join("data"),
                root.join("config"),
            );
            let workspace = crate::lock(&state.workspaces).add(project)?.id;
            Ok::<_, anyhow::Error>((state, workspace))
        })
        .await??;
        if let Some(runtime) = factory.map(|factory| factory()) {
            state.pro().set_runtime(runtime);
        }
        let state = Arc::new(state);
        let body = Registration {
            host_id: "worker-fixture".into(),
            endpoint: format!("http://{peer}"),
            token: "fixture-owner".into(),
            workspace_id: workspace.clone(),
            epoch: 4,
        };
        ensure!(
            register(State(state.clone()), Json(body)).await.status() == StatusCode::NO_CONTENT,
            "fixture placement refused"
        );
        poll_workspaces(&state.pro().session_proxy, &state.changes).await;
        ensure!(
            state.pro().session_proxy.for_session("s-fixture").is_some(),
            "fixture roster missing"
        );
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let router = axum::Router::new()
            .route("/ws/chat/{id}", axum::routing::get(crate::ws::chat_ws))
            .with_state(state.clone());
        let (shutdown, stopped) = oneshot::channel();
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
        });
        let connected = tokio::time::timeout(Duration::from_secs(5), async {
            let (mut client, _) =
                tokio_tungstenite::connect_async(format!("ws://{address}/ws/chat/s-fixture"))
                    .await?;
            bounded_send(
                &mut client,
                Up::Text(
                    json!({"type":"auth","token":"fixture-viewer","last_seq":0})
                        .to_string()
                        .into(),
                ),
            )
            .await?;
            Ok::<_, anyhow::Error>(client)
        })
        .await;
        let mut harness = Self {
            state,
            workspace,
            client: None,
            shutdown: Some(shutdown),
            server: Some(server),
        };
        match connected {
            Ok(Ok(client)) => {
                harness.client = Some(client);
                Ok(harness)
            }
            _ => {
                harness.close().await?;
                bail!("fixture viewer connect refused")
            }
        }
    }
    pub async fn send(&mut self, value: Value) -> Result<()> {
        let text = serde_json::to_string(&value)?;
        ensure!(text.len() <= 64 * 1024, "fixture frame over bound");
        bounded_send(
            self.client.as_mut().context("fixture viewer closed")?,
            Up::Text(text.into()),
        )
        .await
    }
    pub async fn next(&mut self) -> Result<Option<Value>> {
        let client = self.client.as_mut().context("fixture viewer closed")?;
        loop {
            match tokio::time::timeout(Duration::from_secs(5), client.next()).await? {
                Some(Ok(Up::Text(text))) => {
                    ensure!(text.len() <= 64 * 1024, "fixture result over bound");
                    return Ok(Some(serde_json::from_str(&text)?));
                }
                Some(Ok(Up::Close(_))) | None => return Ok(None),
                Some(Err(tokio_tungstenite::tungstenite::Error::ConnectionClosed | tokio_tungstenite::tungstenite::Error::AlreadyClosed)) => return Ok(None),
                Some(Err(tokio_tungstenite::tungstenite::Error::Protocol(tokio_tungstenite::tungstenite::error::ProtocolError::ResetWithoutClosingHandshake))) => return Ok(None),
                Some(Err(error)) => return Err(error.into()),
                _ => {}
            }
        }
    }
    pub fn retire(&self) {
        self.state
            .pro()
            .session_proxy
            .clear_workspace(&self.workspace);
    }
    /// Closing the client alone is not a cleanup receipt. Wait for the actual
    /// original upgrade task to finish before the caller may remove its root.
    pub async fn close(&mut self) -> Result<()> {
        if let Some(mut client) = self.client.take() {
            let _ = tokio::time::timeout(Duration::from_secs(2), client.close(None)).await;
        }
        self.state.stopping.store(true, Ordering::Release);
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(mut server) = self.server.take() {
            match tokio::time::timeout(Duration::from_secs(5), &mut server).await {
                Ok(result) => result??,
                Err(_) => {
                    server.abort();
                    let _ = server.await;
                    bail!("fixture viewer cleanup uncertain");
                }
            }
        }
        ensure!(
            self.state.sessions.list().is_empty() && crate::lock(&self.state.agents).is_empty(),
            "fixture created local work"
        );
        Ok(())
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.state.stopping.store(true, Ordering::Release);
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(server) = self.server.take() {
            server.abort();
        }
    }
}

/// Isolated test capacity; the same reservation implementation enforces it.
/// Never used by production admissions, whose budget is the one static owner.
pub struct FixtureBudget(HeldBudget);
impl FixtureBudget {
    pub fn new(limit: usize) -> Self {
        Self(HeldBudget::new(limit))
    }
    pub fn used(&self) -> usize {
        self.0.used.load(Ordering::Acquire)
    }
    pub fn reserve(&self, bytes: usize) -> Option<ViewerReservation<'_>> {
        self.0.reserve(bytes).then_some(ViewerReservation {
            budget: &self.0,
            bytes,
        })
    }
}
