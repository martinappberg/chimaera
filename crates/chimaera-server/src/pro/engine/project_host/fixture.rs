//! Closed setup/observations for the original paid cases; never ordinary API.
use super::*;
use crate::pro::engine::coordinator_host::fixture::Harness;
use serde_json::Value;

pub enum Seed {
    Configuration {
        value: Value,
        generation: u64,
        worker: bool,
        configured: bool,
    },
    Ownership(Option<Ownership>),
    CopyAndPreference(Value),
    ExecutionObservation {
        baton: Value,
        accept: bool,
    },
    SleepAndReturn {
        sleep_generation: u64,
        sleeping: bool,
        parked: bool,
        release_pending: bool,
        awake_since: u64,
        power_suitable: bool,
        backoff: Option<(u64, u64)>,
    },
    DeferredSession(Value),
    MoveRequest {
        acted_ms: Option<u64>,
    },
}
pub enum Request {
    Sleep(Option<Value>),
    Wake,
    Disconnect,
    Drain(Value),
    Handoff { workspace: String, epoch: u64 },
    Hydrate(Value),
    Configure(Value),
    ConfigureLegacy(Value),
    ConfigureWorkspace(Value),
    OpenWorkspace { id: String },
}
pub struct Reservation(tokio::sync::OwnedMutexGuard<()>);
impl Reservation {
    pub fn held(&self) -> bool {
        let _ = &self.0;
        true
    }
}
pub enum TerminalKind {
    Shell,
    UnsavedAgent,
}
pub enum Operation {
    Reconcile,
    Snapshot {
        clean: bool,
    },
    SleepFlush(Sleep),
    Hydrate {
        epoch: u64,
        fork: bool,
        destination: Option<PathBuf>,
    },
    LocateReturn,
}
pub struct ProjectObservation {
    pub ownership: Option<Ownership>,
    pub preference: Value,
    pub status: Value,
    pub answered: bool,
    pub installing: bool,
    pub parked: bool,
    pub release_pending: bool,
    pub opened: bool,
    pub return_backoff: Option<(u64, u64)>,
    pub sleeping: bool,
    pub may_execute: bool,
    pub may_write: bool,
    pub fenced: bool,
    pub lease_valid: bool,
    pub recovery_context: bool,
    pub resuming: bool,
    pub quiescent: bool,
    pub other_computer: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Renewal {
    Renewed,
    Refused,
    TimedOut,
}
pub struct HostObservation {
    pub generation: u64,
    pub configured: bool,
    pub worker: bool,
    pub sleep_generation: u64,
    pub jobs_busy: bool,
}
pub struct SessionObservation {
    pub terminal: Option<Value>,
    pub chat: Option<Value>,
    pub deferred: Value,
}
/// Original test root and key are immutable; no mutable registry escapes.
#[derive(Clone)]
pub struct Scenario {
    harness: Arc<Harness>,
    key: String,
}
fn bounded(value: &Value) -> Result<()> {
    ensure!(
        serde_json::to_vec(value)?.len() <= 64 * 1024,
        "fixture record exceeds bound"
    );
    Ok(())
}
fn configuration(value: &Value) -> Result<Configure> {
    bounded(value)?;
    let config: Configure = serde_json::from_value(value.clone())?;
    let endpoint: std::net::SocketAddr = config
        .endpoint
        .strip_prefix("http://")
        .context("fixture account must be loopback")?
        .parse()?;
    ensure!(
        endpoint.ip() == std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        "fixture account must be loopback"
    );
    if !config.keeper_url.is_empty() {
        let keeper: std::net::SocketAddr = config
            .keeper_url
            .strip_prefix("http://")
            .context("fixture keeper must be loopback")?
            .parse()?;
        ensure!(
            keeper.ip() == std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            "fixture keeper must be loopback"
        );
    }
    ensure!(
        config.delegation.access_token.len() <= 8192,
        "fixture delegation exceeds bound"
    );
    Ok(config)
}
pub fn managed_capability() -> Value {
    serde_json::to_value(execution::wire::ExecutionCapability::managed())
        .expect("fixed capability serializes")
}
pub fn release_error_code(error: &anyhow::Error) -> &'static str {
    crate::pro::routes::error_code(error)
}
pub struct RestartObservation {
    pub parked: bool,
    pub ownership: Option<Ownership>,
}
/// One original loopback listener task; explicit close reaps that exact task.
pub struct LoopbackServer {
    address: std::net::SocketAddr,
    task: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
}
impl LoopbackServer {
    pub fn address(&self) -> std::net::SocketAddr {
        self.address
    }
    pub async fn close(mut self) -> Result<()> {
        if let Some(task) = self.task.take() {
            task.abort();
            match task.await {
                Err(error) if error.is_cancelled() => {}
                Err(error) => return Err(error.into()),
                Ok(result) => result?,
            }
        }
        Ok(())
    }
}
impl Drop for LoopbackServer {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
pub struct FinishedConversation {
    pub session: String,
    pub native: String,
    pub native_bytes: String,
    pub journal: String,
    pub ledger: Value,
}
pub struct FinishedObservation {
    pub alive: bool,
    pub native_id: Option<String>,
    pub workspace: Option<String>,
    pub finished: bool,
    pub journal: String,
    pub native: String,
}
pub struct AdoptionObservation {
    pub local_root: Option<PathBuf>,
    pub account_matches: bool,
    pub adoption_pending: bool,
    pub legacy_pending: bool,
    pub eligible: bool,
    pub registered: bool,
    pub adoptions_empty: bool,
    pub preference: Value,
}
pub fn companion_child(data: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    ensure!(
        data.is_absolute()
            && data.as_os_str().len() <= 4096
            && data.file_name() == Some(std::ffi::OsStr::new("data")),
        "invalid fixture child data"
    );
    let app = data.parent().context("fixture child app missing")?;
    ensure!(
        app.file_name() == Some(std::ffi::OsStr::new("app")),
        "invalid fixture app suffix"
    );
    let root = app.parent().context("fixture child root missing")?;
    let metadata = std::fs::symlink_metadata(root)?;
    ensure!(
        metadata.is_dir()
            && metadata.uid() == rustix::process::geteuid().as_raw()
            && metadata.mode() & 0o777 == 0o700,
        "fixture child anchor untrusted"
    );
    crate::pro::companion::fixture_child(data)
}
pub fn open_error_code(error: &anyhow::Error) -> &'static str {
    crate::pro::projects::open_error_code(error)
}
impl Harness {
    async fn fixed_request(
        &self,
        path: &str,
        body: Value,
    ) -> Result<crate::pro::engine::coordinator_host::fixture::Reply> {
        self.fixed_request_method(path, Some(body), axum::http::Method::POST)
            .await
    }
    async fn fixed_request_method(
        &self,
        path: &str,
        body: Option<Value>,
        method: axum::http::Method,
    ) -> Result<crate::pro::engine::coordinator_host::fixture::Reply> {
        use tower::ServiceExt;
        if let Some(body) = &body {
            bounded(body)?;
        }
        let bytes = body
            .map(|body| serde_json::to_vec(&body))
            .transpose()?
            .unwrap_or_default();
        let response = crate::app(self.state.clone())
            .oneshot(
                axum::http::Request::builder()
                    .method(method)
                    .uri(path)
                    .header("Authorization", format!("Bearer {}", self.state.token))
                    .header("Content-Type", "application/json")
                    .body(axum::body::Body::from(bytes))?,
            )
            .await?;
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await?;
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)?
        };
        Ok(crate::pro::engine::coordinator_host::fixture::Reply { status, body })
    }
    pub async fn request(
        &self,
        request: Request,
    ) -> Result<crate::pro::engine::coordinator_host::fixture::Reply> {
        let (path, body) = match request {
            Request::Sleep(body) => {
                return self
                    .fixed_request_method("/api/v1/pro/sleep", body, axum::http::Method::POST)
                    .await;
            }
            // The daemon hears a wake from the OS itself (`sleep_watch`); the
            // fixture calls the same function.
            Request::Wake => {
                crate::pro::routes::woke(&self.state).await;
                return Ok(crate::pro::engine::coordinator_host::fixture::Reply {
                    status: axum::http::StatusCode::NO_CONTENT,
                    body: Value::Null,
                });
            }
            Request::Disconnect => {
                return self
                    .fixed_request_method(
                        "/api/v1/pro/configure",
                        Some(Value::Null),
                        axum::http::Method::DELETE,
                    )
                    .await;
            }
            Request::Drain(body) => ("/api/v1/pro/drain", body),
            Request::Handoff { workspace, epoch } => {
                ensure!(crate::pro::valid_id(&workspace), "invalid fixture handoff");
                (
                    "/api/v1/pro/handoff",
                    json!({"workspace_id":workspace,"expected_epoch":epoch}),
                )
            }
            Request::Hydrate(body) => ("/api/v1/pro/hydrate", body),
            Request::ConfigureWorkspace(body) => {
                configuration(&body)?;
                let root = body
                    .get("workspace_root")
                    .and_then(Value::as_str)
                    .context("fixture workspace root missing")?;
                let root = PathBuf::from(root);
                ensure!(
                    root.starts_with(self.fixture_root()) && root != self.fixture_root(),
                    "fixture workspace root outside anchor"
                );
                ("/api/v1/pro/configure/workspace", body)
            }
            Request::ConfigureLegacy(body) => {
                configuration(&body)?;
                ("/api/v1/pro/configure", body)
            }
            Request::Configure(body) => {
                configuration(&body)?;
                ("/api/v1/pro/configure/execution", body)
            }
            Request::OpenWorkspace { id } => {
                ensure!(crate::pro::valid_id(&id), "invalid fixture workspace");
                ensure!(
                    lock(&self.state.workspaces).get(&id).is_some(),
                    "fixture workspace missing"
                );
                return self
                    .fixed_request(&format!("/api/v1/workspaces/{id}/open"), Value::Null)
                    .await;
            }
        };
        self.fixed_request(path, body).await
    }
    pub async fn copy_project(&self, body: Value) -> Result<Value> {
        bounded(&body)?;
        Box::pin(crate::pro::projects::copy(
            &self.state,
            serde_json::from_value(body)?,
        ))
        .await
    }
    pub async fn project_list(&self) -> Result<Value> {
        let cache = crate::pro::projects::list(&self.state).await;
        let value = serde_json::to_value(cache)?;
        ensure!(
            serde_json::to_vec(&value)?.len() <= 1024 * 1024,
            "fixture listing exceeds cap"
        );
        Ok(value)
    }
    pub fn configuration_value(&self) -> Result<Value> {
        let config = lock(&self.state.pro.runtime)
            .clone()
            .context("fixture configuration missing")?;
        ensure!(
            !config.recovery,
            "fixture cannot project recovery credentials"
        );
        let value = json!({"account_id":config.account_id,"role":match config.role { Role::Device=>"device",Role::Worker=>"worker" },"endpoint":config.endpoint,"keeper_url":config.keeper_url,"delegation":config.delegation,"hours_exhausted":config.hours_exhausted,"execution":config.execution});
        bounded(&value)?;
        Ok(value)
    }
    pub fn seed_projects_root(&self, root: PathBuf) -> Result<()> {
        ensure!(
            root.starts_with(self.fixture_root()) && root != self.fixture_root(),
            "fixture legacy root outside anchor"
        );
        *lock(&self.state.pro.projects_root) = Some(root);
        Ok(())
    }
    pub fn chats_empty(&self) -> bool {
        self.state.chat.list().is_empty()
    }
    pub fn stop_coordinator(&self) {
        self.state.stopping.store(true, Ordering::Release);
    }
    pub async fn resume_unverified(&self) {
        crate::pro::resume_unverified(&self.state).await;
    }
    pub async fn periodic_pass(&self) -> Result<()> {
        ensure!(
            lock(&self.state.pro.mirror_task).is_none(),
            "fixture pass already present"
        );
        let jobs = self.state.pro.jobs.clone();
        let task = tokio::spawn(async move {
            let _guard = jobs.lock_owned().await;
            tokio::time::sleep(Duration::from_secs(3600)).await;
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        *lock(&self.state.pro.mirror_task) = Some(task);
        Ok(())
    }
    pub async fn serve_loopback(&self) -> Result<LoopbackServer> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let app = crate::app(self.state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await });
        Ok(LoopbackServer {
            address,
            task: Some(task),
        })
    }
    /// Original serving identity, only for this owned loopback listener. No
    /// process HOME or production Manifest is read or rewritten.
    pub fn serving_manifest(&self, server: &LoopbackServer) -> Result<chimaera_core::Manifest> {
        ensure!(
            server.address.ip().is_loopback() && server.address.port() != 0,
            "fixture listener unavailable"
        );
        Ok(chimaera_core::Manifest {
            hostname: self.state.hostname.clone(),
            port: server.address.port(),
            token: self.state.token.clone(),
            pid: self.state.pid,
            version: chimaera_core::VERSION.into(),
            started_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
            build: Some(chimaera_core::BUILD_ID.into()),
            slurm_job_id: None,
            runtime_leases: false,
        })
    }
    pub async fn jobs_reservation(&self) -> Reservation {
        Reservation(self.state.pro.jobs.clone().lock_owned().await)
    }
    pub fn detached_running(&self) -> usize {
        crate::pro::detached::running(&self.state)
    }
    /// Explicit unregistered account marker, captured in this same disposable
    /// host. It never imports a WorkspaceStore row or grants execution.
    pub fn marker_scenario(self: &Arc<Self>, workspace: &str) -> Result<Scenario> {
        ensure!(crate::pro::valid_id(workspace), "invalid fixture marker");
        Ok(Scenario {
            harness: self.clone(),
            key: workspace.into(),
        })
    }
    pub fn seed_configuration(
        &self,
        value: Value,
        generation: u64,
        worker: bool,
        configured: bool,
    ) -> Result<()> {
        let config = configuration(&value)?;
        *lock(&self.state.pro.runtime) = Some(config);
        self.state
            .pro
            .generation
            .store(generation, Ordering::Release);
        self.state.pro.worker.store(worker, Ordering::Release);
        self.state
            .pro
            .configured
            .store(configured, Ordering::Release);
        Ok(())
    }
    pub async fn renew_delegation(&self, value: Value, original_generation: u64) -> Result<bool> {
        let config = configuration(&value)?;
        Ok(super::super::renew_delegation(&self.state, &config, original_generation).await)
    }
    pub async fn add_project(self: &Arc<Self>, root: PathBuf) -> Result<Value> {
        let anchor = self.fixture_root().to_path_buf();
        let root = tokio::task::spawn_blocking(move || {
            let root = root.canonicalize()?;
            ensure!(root.starts_with(&anchor), "fixture project escapes root");
            Ok::<_, anyhow::Error>(root)
        })
        .await??;
        let workspace = lock(&self.state.workspaces).add(root)?;
        Ok(serde_json::to_value(workspace)?)
    }
    pub fn projects(&self) -> Result<Vec<Value>> {
        let rows = lock(&self.state.workspaces).list();
        ensure!(rows.len() <= 128, "fixture project list exceeds bound");
        rows.into_iter()
            .map(|row| Ok(serde_json::to_value(row)?))
            .collect()
    }
    pub async fn capture_project(self: &Arc<Self>, record: Value) -> Result<Scenario> {
        bounded(&record)?;
        let workspace: crate::workspaces::Workspace = serde_json::from_value(record)?;
        ensure!(
            crate::pro::valid_id(&workspace.id),
            "invalid fixture project"
        );
        let anchor = self.fixture_root().to_path_buf();
        let workspace = tokio::task::spawn_blocking(move || {
            let canonical = workspace.root.canonicalize()?;
            ensure!(
                canonical.starts_with(&anchor),
                "fixture project escapes root"
            );
            ensure!(
                canonical == workspace.root,
                "fixture project must be canonical"
            );
            Ok::<_, anyhow::Error>(workspace)
        })
        .await??;
        let key = workspace.id.clone();
        lock(&self.state.workspaces).import_exact(workspace)?;
        self.scenario(&key)
    }
    pub fn scenario(self: &Arc<Self>, workspace: &str) -> Result<Scenario> {
        ensure!(crate::pro::valid_id(workspace), "invalid fixture project");
        ensure!(
            lock(&self.state.workspaces).get(workspace).is_some()
                || lock(&self.state.pro.adoptions).contains_key(workspace),
            "fixture project is not captured"
        );
        Ok(Scenario {
            harness: self.clone(),
            key: workspace.into(),
        })
    }
}
impl Scenario {
    pub fn original_owner(&self, value: Value, original_generation: u64) -> Result<ProjectOwner> {
        Ok(ProjectOwner {
            state: self.harness.state.clone(),
            config: configuration(&value)?,
            workspace: self.key.clone(),
            generation: original_generation,
        })
    }
    pub fn accept_configuration(
        &self,
        value: Value,
        baton: Value,
        original_generation: u64,
    ) -> Result<()> {
        let config = configuration(&value)?;
        bounded(&baton)?;
        let baton: Baton = serde_json::from_value(baton)?;
        ensure!(
            baton.workspace_id == self.key,
            "fixture grant project changed"
        );
        execution::accept(
            &self.harness.state,
            &config,
            &baton,
            original_generation,
            execution::RequestStart::now(),
        )
    }
    pub fn release_owner(
        &self,
        value: Value,
        original_generation: u64,
        epoch: u64,
        current: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<policy_host::ReleaseOwner> {
        let project = self.original_owner(value, original_generation)?;
        Ok(policy_host::ReleaseOwner::fixture(project, epoch, current))
    }
    pub fn handback_owner(
        &self,
        value: Value,
        host: Value,
        holder: String,
        epoch: u64,
    ) -> Result<policy_host::HandbackOwner> {
        bounded(&host)?;
        policy_host::HandbackOwner::capture(
            self.harness.state.clone(),
            configuration(&value)?,
            self.key.clone(),
            serde_json::from_value(host)?,
            holder,
            epoch,
        )
    }
    pub fn move_request_owner(
        &self,
        value: Value,
        original_generation: u64,
        observed: Value,
        deadline: tokio::time::Instant,
        current: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<policy_host::MoveRequestOwner> {
        bounded(&observed)?;
        let observed: Baton = serde_json::from_value(observed)?;
        ensure!(
            observed.workspace_id == self.key,
            "fixture move project changed"
        );
        Ok(policy_host::MoveRequestOwner::fixture(
            self.original_owner(value, original_generation)?,
            observed,
            deadline,
            current,
        ))
    }
    pub fn advance_generation(&self) -> u64 {
        self.harness
            .state
            .pro
            .generation
            .fetch_add(1, Ordering::AcqRel)
            + 1
    }
    pub fn id(&self) -> &str {
        &self.key
    }
    pub fn seed(&self, seed: Seed) -> Result<()> {
        let state = &self.harness.state;
        let key = &self.key;
        match seed {
            Seed::Configuration {
                value,
                generation,
                worker,
                configured,
            } => {
                let config = configuration(&value)?;
                *lock(&state.pro.runtime) = Some(config);
                state.pro.generation.store(generation, Ordering::Release);
                state.pro.worker.store(worker, Ordering::Release);
                state.pro.configured.store(configured, Ordering::Release);
            }
            Seed::Ownership(value) => {
                let mut ownership = lock(&state.pro.ownership);
                match value {
                    Some(value) => {
                        ownership.insert(key.clone(), value);
                    }
                    None => {
                        ownership.remove(key);
                    }
                }
            }
            Seed::CopyAndPreference(value) => {
                bounded(&value)?;
                let preference: crate::pro::Preference = serde_json::from_value(value)?;
                preference.profile.validate()?;
                lock(&state.pro.preferences).insert(key.clone(), preference);
            }
            Seed::ExecutionObservation { baton, accept } => {
                bounded(&baton)?;
                let baton: Baton = serde_json::from_value(baton)?;
                ensure!(&baton.workspace_id == key, "fixture grant project changed");
                let config = lock(&state.pro.runtime)
                    .clone()
                    .context("fixture configuration missing")?;
                if accept {
                    execution::accept(
                        state,
                        &config,
                        &baton,
                        state.pro.generation.load(Ordering::Acquire),
                        execution::RequestStart::now(),
                    )?;
                } else {
                    execution::observe(state, &config, &baton)?;
                }
            }
            Seed::SleepAndReturn {
                sleep_generation,
                sleeping,
                parked,
                release_pending,
                awake_since,
                power_suitable,
                backoff,
            } => {
                state
                    .pro
                    .sleep_generation
                    .store(sleep_generation, Ordering::Release);
                // The fixture's computer has reached the account since then (0: long ago).
                state
                    .pro
                    .reachable_since
                    .store(awake_since.max(1), Ordering::Release);
                state
                    .pro
                    .power_suitable
                    .store(power_suitable, Ordering::Release);
                for (record, value) in [
                    (&state.pro.sleeping, sleeping),
                    (&state.pro.parked, parked),
                    (&state.pro.release_pending, release_pending),
                ] {
                    if value {
                        lock(record).insert(key.clone());
                    } else {
                        lock(record).remove(key);
                    }
                }
                let mut records = lock(&state.pro.return_backoff);
                if let Some(backoff) = backoff {
                    records.insert(key.clone(), backoff);
                } else {
                    records.remove(key);
                }
            }
            Seed::DeferredSession(value) => {
                bounded(&value)?;
                let entry = crate::ledger::LedgerEntry::from_json(&value)
                    .context("invalid fixture ledger entry")?;
                ensure!(
                    &entry.workspace_id == key && crate::pro::valid_id(&entry.id),
                    "fixture session project changed"
                );
                lock(&state.deferred_sessions).insert(entry.id.clone(), entry);
            }
            Seed::MoveRequest { acted_ms } => {
                crate::pro::moves::seed_actor_fixture(state, key, acted_ms);
            }
        }
        Ok(())
    }
    pub fn project(&self) -> Result<ProjectObservation> {
        let state = &self.harness.state;
        let key = &self.key;
        let preference = serde_json::to_value(lock(&state.pro.preferences).get(key))?;
        let status = serde_json::to_value(lock(&state.pro.status).get(key))?;
        ensure!(
            serde_json::to_vec(&preference)?.len() <= 1024 * 1024
                && serde_json::to_vec(&status)?.len() <= 1024 * 1024,
            "fixture observation exceeds bound"
        );
        let ownership = lock(&state.pro.ownership).get(key).cloned();
        let answered = lock(&state.pro.answered).contains(key);
        let installing = lock(&state.pro.installing).contains(key);
        let parked = lock(&state.pro.parked).contains(key);
        let release_pending = lock(&state.pro.release_pending).contains(key);
        let opened = lock(&state.pro.opened_here).contains(key);
        let return_backoff = lock(&state.pro.return_backoff).get(key).copied();
        let sleeping = lock(&state.pro.sleeping).contains(key);
        Ok(ProjectObservation {
            ownership,
            preference,
            status,
            answered,
            installing,
            parked,
            release_pending,
            opened,
            return_backoff,
            sleeping,
            may_execute: crate::pro::may_execute(state, key),
            may_write: crate::pro::may_write(state, key),
            fenced: execution::fenced(state, key),
            lease_valid: execution::lease_valid(state, key),
            recovery_context: execution::recovery_context(state, key),
            resuming: execution::resuming(state, key),
            quiescent: execution::quiescent(state, key),
            other_computer: crate::pro::moves::other_computer(state, key),
        })
    }
    pub fn shadow_path(&self) -> PathBuf {
        self.harness
            .state
            .pro
            .root
            .join(&self.key)
            .join("working-tree.git")
    }
    pub fn owned_epoch(&self) -> Option<u64> {
        crate::pro::owned_epoch(&self.harness.state, &self.key)
    }
    pub async fn workspace_identity(&self) -> Result<Option<String>> {
        let workspace = lock(&self.harness.state.workspaces)
            .get(&self.key)
            .context("fixture project missing")?;
        tokio::task::spawn_blocking(move || {
            Ok(crate::workspaces::identity::read(&workspace.root).map(|marker| marker.id))
        })
        .await?
    }
    pub fn adoption(&self) -> Result<AdoptionObservation> {
        let state = &self.harness.state;
        let workspace = lock(&state.workspaces).get(&self.key);
        let preference = serde_json::to_value(lock(&state.pro.preferences).get(&self.key))?;
        bounded(&preference)?;
        let adoptions_empty = lock(&state.pro.adoptions).is_empty();
        Ok(AdoptionObservation {
            local_root: crate::pro::projects::local_root(state, &self.key),
            account_matches: crate::pro::projects::account_matches(state, &self.key),
            adoption_pending: crate::pro::projects::adoption_pending(state, &self.key),
            legacy_pending: lock(&state.pro.legacy_pending).contains(&self.key),
            eligible: workspace
                .as_ref()
                .is_some_and(|workspace| super::super::eligible(state, workspace)),
            registered: workspace.is_some(),
            adoptions_empty,
            preference,
        })
    }
    pub fn host(&self) -> HostObservation {
        let state = &self.harness.state;
        HostObservation {
            generation: state.pro.generation.load(Ordering::Acquire),
            configured: state.pro.configured.load(Ordering::Acquire),
            worker: state.pro.worker.load(Ordering::Acquire),
            sleep_generation: state.pro.sleep_generation.load(Ordering::Acquire),
            jobs_busy: state.pro.jobs.try_lock().is_err(),
        }
    }
    pub fn defer_boot_session(&self, id: &str) -> Result<()> {
        ensure!(!id.is_empty() && id.len() <= 512, "invalid fixture session");
        {
            let deferred = lock(&self.harness.state.deferred_sessions);
            let entry = deferred
                .get(id)
                .context("fixture deferred session missing")?;
            ensure!(
                entry.workspace_id == self.key,
                "fixture session changed workspace"
            );
        }
        crate::pro::defer_boot_session(&self.harness.state, id);
        Ok(())
    }
    pub fn session_workspace(&self, id: &str) -> Option<String> {
        lock(&self.harness.state.session_workspaces)
            .get(id)
            .cloned()
    }
    pub fn park(&self, expected_sleep_generation: u64) -> bool {
        crate::pro::park(&self.harness.state, &self.key, expected_sleep_generation)
    }
    pub async fn persist(&self) -> Result<()> {
        crate::pro::persist(&self.harness.state).await
    }
    /// Read only: the final wake assertion must not cause the write it verifies.
    pub async fn restarted(&self) -> Result<RestartObservation> {
        let root = self.harness.state.pro.root.clone();
        let key = self.key.clone();
        tokio::task::spawn_blocking(move || {
            let restarted = crate::pro::ProState::new(root);
            let parked = lock(&restarted.parked).contains(&key);
            let ownership = lock(&restarted.ownership).get(&key).cloned();
            Ok(RestartObservation { parked, ownership })
        })
        .await?
    }
    pub fn acted_here(&self) {
        crate::pro::moves::acted_here(&self.harness.state, &self.key);
    }
    pub fn forget_moves(&self) {
        crate::pro::moves::forget(&self.harness.state);
    }
    pub fn resumed(&self) -> Vec<String> {
        execution::resumed_fixture(&self.harness.state, &self.key)
    }
    pub fn thawed(&self) {
        execution::thawed_fixture(&self.harness.state, &self.key);
    }
    pub fn ticked(&self) {
        execution::ticked(&self.harness.state, std::time::Instant::now());
    }
    pub fn renewing(&self, epoch: u64) -> bool {
        execution::renewing(&self.harness.state, &self.key, epoch)
    }
    pub async fn await_renewal(&self, epoch: u64, cap: Duration) -> Result<Renewal> {
        ensure!(
            cap <= Duration::from_secs(20),
            "fixture renewal cap exceeds bound"
        );
        Ok(
            match execution::await_renewal(&self.harness.state, &self.key, epoch, cap).await {
                execution::Renewal::Renewed => Renewal::Renewed,
                execution::Renewal::Refused => Renewal::Refused,
                execution::Renewal::TimedOut => Renewal::TimedOut,
            },
        )
    }
    pub async fn finished_conversation_archive(
        &self,
        input: FinishedConversation,
    ) -> Result<PathBuf> {
        ensure!(
            crate::pro::valid_id(&input.session)
                && input.native.len() == 36
                && input
                    .native
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() || byte == b'-'),
            "invalid finished fixture identity"
        );
        ensure!(
            input.native_bytes.len() <= 64 * 1024 && input.journal.len() <= 64 * 1024,
            "finished fixture bytes exceed cap"
        );
        bounded(&input.ledger)?;
        let entry = crate::ledger::LedgerEntry::from_json(&input.ledger)
            .context("invalid finished fixture ledger")?;
        ensure!(
            entry.id == input.session && entry.workspace_id == self.key,
            "finished fixture identity mismatch"
        );
        let workspace = lock(&self.harness.state.workspaces)
            .get(&self.key)
            .context("fixture project missing")?;
        ensure!(
            entry.cwd == workspace.root,
            "finished fixture root mismatch"
        );
        let state = self.harness.state.clone();
        let session = input.session.clone();
        tokio::task::spawn_blocking(move || {
            let native = state
                .claude_projects_dir
                .join(crate::launcher::encode_cwd(&workspace.root))
                .join(format!("{}.jsonl", input.native));
            std::fs::create_dir_all(native.parent().context("fixture native parent missing")?)?;
            std::fs::write(native, input.native_bytes)?;
            std::fs::create_dir_all(state.chat.journal_dir())?;
            std::fs::write(
                state
                    .chat
                    .journal_dir()
                    .join(format!("{}.jsonl", input.session)),
                input.journal,
            )?;
            crate::ledger::defer(&state, entry)
        })
        .await??;
        crate::bundle::export(
            self.harness.state.clone(),
            &session,
            crate::bundle::ExportMode::Snapshot,
        )
        .await
    }
    pub async fn finished_conversation_cli(&self, script: PathBuf) -> Result<()> {
        ensure!(
            script.starts_with(self.harness.fixture_root()),
            "finished fixture script outside anchor"
        );
        tokio::task::spawn_blocking({
            let script = script.clone();
            move || {
                use std::os::unix::fs::MetadataExt;
                let metadata = std::fs::symlink_metadata(&script)?;
                ensure!(
                    metadata.is_file()
                        && metadata.uid() == rustix::process::geteuid().as_raw()
                        && metadata.nlink() == 1
                        && metadata.len() <= 8192
                        && metadata.mode() & 0o111 != 0,
                    "finished fixture script untrusted"
                );
                Ok::<_, anyhow::Error>(())
            }
        })
        .await??;
        lock(&self.harness.state.agent_bins).insert(
            crate::agents::AgentKind::Claude,
            crate::launcher::AgentDetection {
                path: Ok(script),
                version: Some("2.1.283".into()),
                managed: false,
                explicit: true,
                mtime: None,
            },
        );
        Ok(())
    }
    pub async fn finished_conversation(&self, id: &str) -> Result<FinishedObservation> {
        ensure!(crate::pro::valid_id(id), "invalid finished fixture session");
        let state = &self.harness.state;
        let chat = state.chat.get(id);
        let workspace = lock(&state.session_workspaces).get(id).cloned();
        ensure!(
            workspace
                .as_deref()
                .is_none_or(|workspace| workspace == self.key),
            "finished fixture session changed workspace"
        );
        let finished = lock(&state.agents)
            .get(id)
            .is_some_and(|agent| agent.state == crate::agent_state::AgentState::Finished);
        let path = state.chat.journal_dir().join(format!("{id}.jsonl"));
        let journal = tokio::task::spawn_blocking(move || {
            use std::io::Read;
            let mut bytes = Vec::new();
            let file = match std::fs::File::open(path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(String::new())
                }
                Err(error) => return Err(error.into()),
            };
            file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() <= 1024 * 1024,
                "finished fixture journal exceeds cap"
            );
            Ok::<_, anyhow::Error>(String::from_utf8(bytes)?)
        })
        .await??;
        let native_id = chat
            .as_ref()
            .and_then(|chat| chat.native_session_id.clone());
        let native = if let Some(native_id) = &native_id {
            ensure!(
                native_id.len() <= 128
                    && native_id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
                "invalid fixture native identity"
            );
            let root = lock(&state.workspaces)
                .get(&self.key)
                .context("fixture native project missing")?
                .root;
            let path = state
                .claude_projects_dir
                .join(crate::launcher::encode_cwd(&root))
                .join(format!("{native_id}.jsonl"));
            tokio::task::spawn_blocking(move || {
                use std::io::Read;
                let mut bytes = Vec::new();
                let file = match std::fs::File::open(path) {
                    Ok(file) => file,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Ok(String::new())
                    }
                    Err(error) => return Err(error.into()),
                };
                file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
                ensure!(
                    bytes.len() <= 1024 * 1024,
                    "fixture native bytes exceed cap"
                );
                Ok::<_, anyhow::Error>(String::from_utf8(bytes)?)
            })
            .await??
        } else {
            String::new()
        };
        Ok(FinishedObservation {
            alive: chat.as_ref().is_some_and(|chat| chat.alive),
            native_id,
            workspace,
            finished,
            journal,
            native,
        })
    }
    pub async fn mid_turn_chat(&self) -> Result<String> {
        use std::os::unix::fs::MetadataExt;
        let path = self.harness.fixture_root().join("claude");
        tokio::task::spawn_blocking({
            let path = path.clone();
            move || {
                let metadata = std::fs::symlink_metadata(&path)?;
                ensure!(
                    metadata.is_file()
                        && metadata.uid() == rustix::process::geteuid().as_raw()
                        && metadata.nlink() == 1
                        && metadata.len() <= 8192
                        && metadata.mode() & 0o111 != 0,
                    "fixture agent script is not captured"
                );
                Ok::<_, anyhow::Error>(())
            }
        })
        .await??;
        lock(&self.harness.state.agent_bins).insert(
            crate::agents::AgentKind::Claude,
            crate::launcher::AgentDetection {
                path: Ok(path),
                version: Some("9.9.9-fake".into()),
                managed: false,
                explicit: false,
                mtime: None,
            },
        );
        let response = self
            .harness
            .fixed_request(
                "/api/v1/sessions",
                json!({"workspace_id":self.key,"kind":"agent","ui":"chat"}),
            )
            .await?;
        ensure!(
            response.status == axum::http::StatusCode::OK,
            "fixture chat creation refused"
        );
        let id = response.body["id"]
            .as_str()
            .context("fixture chat identity missing")?
            .to_owned();
        self.harness
            .state
            .chat
            .command(
                &id,
                chimaera_agent::model::AgentCommand::Send {
                    blocks: vec![chimaera_agent::model::ContentBlock::Text {
                        text: "keep working".into(),
                    }],
                },
            )
            .await?;
        Ok(id)
    }
    pub fn sleeping_terminal(&self, kind: TerminalKind) -> Result<String> {
        let workspace = lock(&self.harness.state.workspaces)
            .get(&self.key)
            .context("fixture project not registered")?;
        let session = self.harness.state.sessions.spawn(chimaera_pty::SpawnOpts {
            cwd: workspace.root,
            name: None,
            cols: 80,
            rows: 24,
            command: Some(vec!["/bin/sleep".into(), "300".into()]),
            id: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            scrollback: None,
        })?;
        if matches!(kind, TerminalKind::UnsavedAgent) {
            lock(&self.harness.state.agents).insert(
                session.id.clone(),
                crate::agent_state::AgentRecord::new(
                    "k".into(),
                    crate::agent_state::AgentKind::Claude,
                ),
            );
        }
        lock(&self.harness.state.session_workspaces).insert(session.id.clone(), self.key.clone());
        Ok(session.id)
    }
    pub async fn kill_session(&self, id: &str) -> Result<()> {
        let state = &self.harness.state;
        ensure!(
            lock(&state.session_workspaces).get(id) == Some(&self.key),
            "fixture session project changed"
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let chat = state.chat.get(id);
        let terminal = state.sessions.get(id);
        ensure!(
            chat.is_some() || terminal.is_some(),
            "fixture session absent"
        );
        let group = if chat.is_some() {
            state.chat.process_group(id)
        } else {
            terminal.as_ref().and_then(|info| info.pid)
        };
        let mut chat_owner = if chat.is_some() {
            Some(
                tokio::time::timeout_at(deadline, state.chat.pause_commands(id))
                    .await
                    .context("fixture chat capture timed out")??,
            )
        } else {
            None
        };
        if chat.as_ref().is_some_and(|info| info.alive) {
            chat_owner
                .as_mut()
                .context("fixture chat owner absent")?
                .fence();
        } else if terminal.is_some() {
            state.sessions.kill(id)?;
        }
        loop {
            ensure!(
                lock(&state.session_workspaces)
                    .get(id)
                    .is_none_or(|workspace| workspace == &self.key),
                "fixture session project changed"
            );
            let current_chat = state.chat.get(id);
            let current_terminal = state.sessions.get(id);
            ensure!(
                current_chat.as_ref().is_none_or(|info| chat
                    .as_ref()
                    .is_some_and(|original| info.created_at_ms == original.created_at_ms
                        && info.cwd == original.cwd)),
                "fixture chat owner changed"
            );
            ensure!(
                current_terminal
                    .as_ref()
                    .is_none_or(|info| terminal
                        .as_ref()
                        .is_some_and(|original| info.pid == original.pid
                            && info.created_at == original.created_at
                            && info.cwd == original.cwd)),
                "fixture terminal owner changed"
            );
            let group_absent =
                group.is_none_or(|group| !crate::cloud::providers::process::group_alive(group));
            if group_absent
                && chat_owner
                    .as_ref()
                    .is_none_or(|owner| !owner.cleanup_pending())
                && current_terminal.is_none()
                && current_chat.as_ref().is_none_or(|info| !info.alive)
            {
                // The retained original chat handle and alive=false prove driver and
                // journal settlement. The normal delete path also retires a
                // dead protocol-error entry, which intentionally stays visible.
                if current_chat.is_some() {
                    let response = crate::api::delete_session(
                        axum::extract::State(state.clone()),
                        axum::extract::Path(id.to_owned()),
                    )
                    .await;
                    ensure!(
                        response.status().is_success(),
                        "fixture dead chat retirement refused"
                    );
                } else if chat.is_none() && lock(&state.agents).contains_key(id) {
                    // A fixture PTY AgentRecord has no watcher. Retire its
                    // informational identity only after the original wait
                    // thread removed the process and its group is absent.
                    let state = state.clone();
                    let id = id.to_owned();
                    tokio::task::spawn_blocking(move || {
                        crate::recents::retire(
                            &state,
                            &id,
                            None,
                            None,
                            chimaera_agent::model::SessionUi::Term,
                        )
                    })
                    .await?;
                }
                if !state.chat.contains(id)
                    && state.sessions.get(id).is_none()
                    && !lock(&state.agents).contains_key(id)
                {
                    return Ok(());
                }
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "fixture session retirement timed out"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    pub fn bind_session(&self, id: &str) -> Result<()> {
        ensure!(crate::pro::valid_id(id), "invalid fixture session");
        lock(&self.harness.state.session_workspaces).insert(id.into(), self.key.clone());
        Ok(())
    }
    pub fn session(&self, id: &str) -> Result<SessionObservation> {
        ensure!(crate::pro::valid_id(id), "invalid fixture session");
        let state = &self.harness.state;
        ensure!(
            lock(&state.session_workspaces)
                .get(id)
                .is_none_or(|workspace| workspace == &self.key),
            "fixture session project changed"
        );
        let terminal = state
            .sessions
            .get(id)
            .map(|info| json!({"id":info.id,"alive":info.alive}));
        let chat = state
            .chat
            .get(id)
            .map(|info| json!({"id":info.id,"alive":info.alive}));
        let deferred = lock(&state.deferred_sessions)
            .get(id)
            .map(crate::ledger::LedgerEntry::to_json)
            .unwrap_or(serde_json::Value::Null);
        bounded(&deferred)?;
        Ok(SessionObservation {
            terminal,
            chat,
            deferred,
        })
    }
    pub fn cache_quiescent(&self) -> Result<()> {
        transport::cache_quiescent(&self.key)
    }
    pub async fn snapshot_phase(
        &self,
        value: Value,
        clean: bool,
        phase: &mut &'static str,
    ) -> Result<()> {
        let config = configuration(&value)?;
        super::super::snapshot_inner(&self.harness.state, &config, &self.key, clean, None, phase)
            .await
    }
    pub async fn run_with_configuration(
        &self,
        value: Value,
        operation: Operation,
    ) -> Result<Option<u64>> {
        let config = configuration(&value)?;
        self.run_captured(config, operation).await
    }
    pub fn expire(&self, original_generation: u64) -> Vec<String> {
        execution::expire(&self.harness.state, original_generation)
    }
    pub async fn renew_delegation(&self) -> Result<bool> {
        let state = &self.harness.state;
        let config = lock(&state.pro.runtime)
            .clone()
            .context("fixture configuration missing")?;
        let generation = state.pro.generation.load(Ordering::Acquire);
        Ok(super::super::renew_delegation(state, &config, generation).await)
    }
    pub async fn run(&self, operation: Operation) -> Result<Option<u64>> {
        let state = &self.harness.state;
        let config = lock(&state.pro.runtime)
            .clone()
            .context("fixture configuration missing")?;
        self.run_captured(config, operation).await
    }
    async fn run_captured(&self, config: Configure, operation: Operation) -> Result<Option<u64>> {
        let state = &self.harness.state;
        match operation {
            Operation::Reconcile => {
                super::super::reconcile_generation(
                    state,
                    &config,
                    &self.key,
                    state.pro.generation.load(Ordering::Acquire),
                )
                .await
            }
            Operation::Snapshot { clean } => {
                super::super::snapshot(state, &config, &self.key, clean).await?;
                Ok(None)
            }
            Operation::SleepFlush(sleep) => {
                super::super::sleep_flush(state, &config, &self.key, sleep).await?;
                Ok(None)
            }
            Operation::Hydrate {
                epoch,
                fork,
                destination,
            } => {
                super::super::hydrate(
                    state,
                    &config,
                    &self.key,
                    epoch,
                    fork,
                    destination.as_deref(),
                )
                .await?;
                Ok(None)
            }
            Operation::LocateReturn => {
                super::super::lazy_handback(state, &config).await?;
                Ok(None)
            }
        }
    }
}

// Fixed effects used only by relocated actual-Runtime tests.
#[path = "fixture_mcp.rs"]
mod mcp;
#[path = "fixture_retry.rs"]
mod retry;
