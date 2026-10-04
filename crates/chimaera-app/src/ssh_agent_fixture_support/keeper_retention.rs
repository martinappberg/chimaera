//! C1 observers use the real authenticated Link cluster routes; no SSH selection.
//! Explicit fixture-only observer dispatch; runtime acceptance waits for A/B.
use chimaera_link::{
    Client, ClusterJobState, ClusterOperation, ClusterOperationState, ClusterReply,
    ClusterSnapshot, Tokens,
};
use futures_util::{SinkExt, StreamExt};
use std::{future::Future, time::Duration};
use tokio::time::{timeout_at, Instant};
use tokio_tungstenite::tungstenite::Message;
use zeroize::Zeroizing;

const HOST: &str = "fixture-host";
const BATCH: &str = "j-00000001";
const ATTACHED: &str = "j-00000002";
const HTTP_MAX: usize = 64 * 1024;

async fn bounded<T>(deadline: Instant, future: impl Future<Output = T>) -> Result<T, ()> {
    timeout_at(
        deadline.min(Instant::now() + Duration::from_secs(5)),
        future,
    )
    .await
    .map_err(|_| ())
}

// A lost mutation response only permits reads of this exact immutable operation.
// No retry can mint another allocation or re-submit a cancellation.
async fn mutate(
    client: &Client,
    operation: ClusterOperation,
    deadline: Instant,
) -> Result<ClusterReply, ()> {
    if let Ok(Ok(reply)) = bounded(deadline, client.cluster_operation(HOST, &operation)).await {
        return Ok(reply);
    }
    for read in 0..3 {
        match bounded(deadline, client.cluster_operation_state(HOST, &operation))
            .await?
            .map_err(|_| ())?
        {
            ClusterOperationState::Completed { reply } => return Ok(*reply),
            ClusterOperationState::Pending if read < 2 => {
                bounded(deadline, tokio::time::sleep(Duration::from_secs(1))).await?;
            }
            ClusterOperationState::Pending => return Err(()),
            ClusterOperationState::Uncertain | ClusterOperationState::Unknown => return Err(()),
        }
    }
    Err(())
}

async fn snapshot(
    client: &Client,
    deadline: Instant,
    refresh: bool,
    stage: &mut ObserverStage,
) -> Result<Box<ClusterSnapshot>, ()> {
    *stage = ObserverStage::SnapshotRequest;
    let result = bounded(
        deadline,
        client.cluster_operation(HOST, &ClusterOperation::Overview { refresh }),
    )
    .await;
    let reply = match result {
        Err(()) => {
            *stage = ObserverStage::SnapshotTimeout;
            return Err(());
        }
        Ok(Err(error)) => {
            *stage = snapshot_error(&error);
            return Err(());
        }
        Ok(Ok(reply)) => reply,
    };
    snapshot_projection(reply, stage)
}

fn snapshot_error(error: &anyhow::Error) -> ObserverStage {
    use chimaera_link::ClusterErrorCode;
    let Some(error) = error.downcast_ref::<chimaera_link::ClusterRequestError>() else {
        return ObserverStage::SnapshotClientError;
    };
    // Only fixed typed classes; never render an anyhow/HTTP error or body.
    let status = match error.status {
        400 => "bad-request",
        401 => "unauthorized",
        403 => "forbidden",
        404 => "not-found",
        409 => "conflict",
        429 => "limited",
        503 => "unavailable",
        _ => "other",
    };
    let code = match error.code {
        ClusterErrorCode::UnsupportedClusterPolicy => "unsupported-policy",
        ClusterErrorCode::OperationChanged => "operation-changed",
        ClusterErrorCode::ClusterRequiresJob => "requires-job",
        ClusterErrorCode::JobsHeld => "jobs-held",
        ClusterErrorCode::JobUnavailable => "job-unavailable",
        ClusterErrorCode::JobsChanged => "jobs-changed",
        ClusterErrorCode::RolloutPending => "rollout-pending",
        ClusterErrorCode::Unknown => "unknown",
    };
    ObserverStage::SnapshotRequestError { status, code }
}

fn snapshot_projection(
    reply: ClusterReply,
    stage: &mut ObserverStage,
) -> Result<Box<ClusterSnapshot>, ()> {
    *stage = ObserverStage::SnapshotReplyVariant;
    let ClusterReply::Overview { overview } = reply else {
        return Err(());
    };
    *stage = ObserverStage::SnapshotJobCount;
    if overview.jobs.len() != 2 {
        return Err(());
    }
    *stage = ObserverStage::SnapshotJobIdentity;
    if overview.jobs.iter().filter(|job| job.id == BATCH).count() != 1
        || overview
            .jobs
            .iter()
            .filter(|job| job.id == ATTACHED)
            .count()
            != 1
    {
        return Err(());
    }
    *stage = ObserverStage::SnapshotWorkspaces;
    if !overview.workspaces.is_empty() {
        return Err(());
    }
    *stage = ObserverStage::SnapshotStateUnreadable;
    if overview.state_unreadable {
        return Err(());
    }
    Ok(overview)
}

fn job(snapshot: &ClusterSnapshot, id: &str, attached: bool) -> Result<ClusterJobState, ()> {
    let row = snapshot.jobs.iter().find(|job| job.id == id).ok_or(())?;
    observed_job(row, attached)
}

// An accepted stop can remove the attached queue-only scheduler ID. This
// presentation check never substitutes for the separate original-resource
// UNCERTAIN receipt; running/start observations still require a scheduler ID.
fn pending_stop(row: &chimaera_link::ClusterJobView) -> bool {
    row.id == ATTACHED
        && row.attached
        && row.state == ClusterJobState::Running
        && row.stopping
        && row.stopped_by_user
        && row.ended.is_none()
        && row.ended_at_ms.is_none()
}

fn observed_job(
    row: &chimaera_link::ClusterJobView,
    attached: bool,
) -> Result<ClusterJobState, ()> {
    if row.attached != attached || row.slurm_job_id.is_none() {
        return Err(());
    }
    Ok(row.state)
}

// Allocation identity is asynchronous for an attached StartJob. Only startup
// polling tolerates these pre-running states; positive HELD/running checks keep
// using job(), which requires the actual scheduler identity.
fn startup_job(row: &chimaera_link::ClusterJobView, attached: bool) -> Result<bool, ()> {
    if row.attached != attached {
        return Err(());
    }
    match row.state {
        ClusterJobState::Waiting | ClusterJobState::Starting => Ok(false),
        ClusterJobState::Running if row.slurm_job_id.is_some() => Ok(true),
        _ => Err(()),
    }
}

fn startup_ready(snapshot: &ClusterSnapshot) -> Result<bool, ()> {
    let batch = snapshot.jobs.iter().find(|job| job.id == BATCH).ok_or(())?;
    let attached = snapshot
        .jobs
        .iter()
        .find(|job| job.id == ATTACHED)
        .ok_or(())?;
    // Evaluate both rows even when the first is still pending so malformed
    // positive evidence cannot hide behind ordinary allocation progress.
    let batch_ready = startup_job(batch, false)?;
    let attached_ready = startup_job(attached, true)?;
    Ok(!snapshot.degraded && batch_ready && attached_ready)
}

// The public overview's Ended row is presentation, not terminal authority.
// Each phase must still pass the paired private exact scheduler/journal/original
// resource receipt. Optional reason and the historic stop flag prove neither.
fn ended(snapshot: &ClusterSnapshot, id: &str) -> bool {
    snapshot
        .jobs
        .iter()
        .any(|job| job.id == id && job.state == ClusterJobState::Ended)
        && !snapshot.routes.iter().any(|route| route.job_id == id)
}

async fn health(
    client: &Client,
    snapshot: &ClusterSnapshot,
    id: &str,
    deadline: Instant,
) -> Result<(), ()> {
    let mut routes = snapshot
        .routes
        .iter()
        .filter(|route| route.job_id == id && route.workspace_id.is_none());
    let route = routes.next().ok_or(())?;
    if routes.next().is_some()
        || route.daemon.token.is_empty()
        || route.daemon.token.len() > 4096
        || !route
            .daemon
            .token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    {
        return Err(());
    }
    // Link's production socket config caps each frame/message at 64 KiB. Both
    // bearer-bearing HTTP buffers also remain zeroizing and are never printed.
    let mut socket = bounded(deadline, client.cluster_tcp(HOST, id, None))
        .await?
        .map_err(|_| ())?;
    let request = Zeroizing::new(format!("GET /api/v1/health HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n", route.daemon.token).into_bytes());
    let result = async {
        bounded(
            deadline,
            socket.send(Message::Binary(request.to_vec().into())),
        )
        .await?
        .map_err(|_| ())?;
        let mut response = Zeroizing::new(Vec::with_capacity(HTTP_MAX));
        loop {
            match bounded(deadline, socket.next()).await? {
                Some(Ok(Message::Binary(bytes))) if bytes.len() <= HTTP_MAX - response.len() => {
                    response.extend_from_slice(&bytes)
                }
                Some(Ok(Message::Ping(bytes))) if bytes.len() <= 125 => {
                    bounded(deadline, socket.send(Message::Pong(bytes)))
                        .await?
                        .map_err(|_| ())?;
                }
                Some(Ok(Message::Pong(bytes))) if bytes.len() <= 125 => (),
                Some(Ok(Message::Close(_))) | None => break,
                _ => return Err(()),
            }
        }
        let text = std::str::from_utf8(&response).map_err(|_| ())?;
        let (head, body) = text.split_once("\r\n\r\n").ok_or(())?;
        let mut lines = head.split("\r\n");
        if lines.next() != Some("HTTP/1.1 200 OK") || !body.is_empty() {
            return Err(());
        }
        let mut length = false;
        for line in lines {
            let (name, value) = line.split_once(':').ok_or(())?;
            if name.eq_ignore_ascii_case("content-length") {
                if length || value.trim() != "0" {
                    return Err(());
                }
                length = true;
            } else if !name.eq_ignore_ascii_case("connection") && !name.eq_ignore_ascii_case("date")
            {
                return Err(());
            }
        }
        if !length {
            return Err(());
        }
        Ok(())
    }
    .await;
    // Each observer retires its actual stream even on malformed HTTP. A viewer
    // close never claims to stop the underlying job/forward.
    let _ = bounded(deadline, socket.close(None)).await;
    result
}

async fn running(client: &Client, deadline: Instant, stage: &mut ObserverStage) -> Result<(), ()> {
    *stage = ObserverStage::Snapshot;
    let state = snapshot(client, deadline, true, stage).await?;
    *stage = ObserverStage::BatchState;
    if state.degraded || job(&state, BATCH, false)? != ClusterJobState::Running {
        return Err(());
    }
    *stage = ObserverStage::AttachedState;
    if job(&state, ATTACHED, true)? != ClusterJobState::Running {
        return Err(());
    }
    *stage = ObserverStage::BatchHealth;
    health(client, &state, BATCH, deadline).await?;
    *stage = ObserverStage::AttachedHealth;
    health(client, &state, ATTACHED, deadline).await
}

// Terminal presentation may lose its queue-only ID. This is not scheduler or
// resource proof: the runner still requires the paired private exact TIMEOUT,
// journal Terminal and original-resource absence before continuing.
fn jobs_ended(state: &ClusterSnapshot, stage: &mut ObserverStage) -> Result<bool, ()> {
    *stage = ObserverStage::AttachedTerminal;
    if !ended(state, ATTACHED) {
        return Err(());
    }
    *stage = ObserverStage::BatchTerminal;
    let batch = state.jobs.iter().find(|row| row.id == BATCH).ok_or(())?;
    if batch.state == ClusterJobState::Ended {
        if batch.attached {
            return Err(());
        }
        *stage = ObserverStage::RouteAbsence;
        return if ended(state, BATCH) && state.routes.is_empty() {
            Ok(true)
        } else {
            Err(())
        };
    }
    *stage = ObserverStage::BatchState;
    if job(state, BATCH, false)? == ClusterJobState::Running {
        Ok(false)
    } else {
        Err(())
    }
}

#[derive(Clone, Copy)]
enum ObserverStage {
    Arguments,
    ClientBuild,
    Me,
    Capabilities,
    StopReply,
    Snapshot,
    SnapshotRequest,
    SnapshotTimeout,
    SnapshotClientError,
    SnapshotRequestError {
        status: &'static str,
        code: &'static str,
    },
    SnapshotReplyVariant,
    SnapshotJobCount,
    SnapshotJobIdentity,
    SnapshotWorkspaces,
    SnapshotStateUnreadable,
    PendingProjection,
    BatchState,
    BatchHealth,
    RouteDenial,
    StartBatchReply,
    StartAttachedReply,
    StartupProjection,
    StartupDelay,
    AttachedState,
    AttachedHealth,
    AttachedTerminal,
    BatchTerminal,
    RouteAbsence,
}
impl ObserverStage {
    fn label(self) -> &'static str {
        match self {
            Self::Arguments => "arguments",
            Self::ClientBuild => "client-build",
            Self::Me => "me",
            Self::Capabilities => "capabilities",
            Self::StopReply => "stop-reply",
            Self::Snapshot => "snapshot",
            Self::SnapshotRequest => "snapshot-request",
            Self::SnapshotTimeout => "snapshot-timeout",
            Self::SnapshotClientError => "snapshot-client-error",
            Self::SnapshotRequestError { .. } => "snapshot-request-error",
            Self::SnapshotReplyVariant => "snapshot-reply-variant",
            Self::SnapshotJobCount => "snapshot-job-count",
            Self::SnapshotJobIdentity => "snapshot-job-identity",
            Self::SnapshotWorkspaces => "snapshot-workspaces",
            Self::SnapshotStateUnreadable => "snapshot-state-unreadable",
            Self::PendingProjection => "pending-projection",
            Self::BatchState => "batch-state",
            Self::BatchHealth => "batch-health",
            Self::RouteDenial => "route-denial",
            Self::StartBatchReply => "start-batch-reply",
            Self::StartAttachedReply => "start-attached-reply",
            Self::StartupProjection => "startup-projection",
            Self::StartupDelay => "startup-delay",
            Self::AttachedState => "attached-state",
            Self::AttachedHealth => "attached-health",
            Self::AttachedTerminal => "attached-terminal",
            Self::BatchTerminal => "batch-terminal",
            Self::RouteAbsence => "route-absence",
        }
    }
}

pub(super) async fn run(
    endpoint: String,
    phase: String,
    original_remaining_ms: u64,
) -> Result<(), ()> {
    let original_start = Instant::now();
    let report = match phase.as_str() {
        "start" => Some("start"),
        "running" => Some("running"),
        "stop-attached" => Some("stop-attached"),
        "attached-ended" => Some("attached-ended"),
        "jobs-ended" => Some("jobs-ended"),
        "finished" => Some("finished"),
        _ => None,
    };
    let mut stage = ObserverStage::Arguments;
    let result = observe(
        endpoint,
        phase,
        original_remaining_ms,
        &mut stage,
        original_start,
    )
    .await;
    if let Some(phase) = report.filter(|_| result.is_err()) {
        use std::io::Write;
        // Fixed fixture labels only. A closed diagnostic sink cannot replace the
        // original error; status classes/codes are typed, never raw HTTP values.
        if let ObserverStage::SnapshotRequestError { status, code } = stage {
            let _ = writeln!(
                std::io::stdout().lock(),
                "C1_LINK_REFUSED {} {} {} {}",
                phase,
                stage.label(),
                status,
                code
            );
        } else {
            let _ = writeln!(
                std::io::stdout().lock(),
                "C1_LINK_REFUSED {} {}",
                phase,
                stage.label()
            );
        }
    }
    result
}

async fn observe(
    endpoint: String,
    phase: String,
    original_remaining_ms: u64,
    stage: &mut ObserverStage,
    original_start: Instant,
) -> Result<(), ()> {
    // The runner retains the one original300s envelope. Every child phase is
    // clamped to its remaining allowance; this is never new Connect authority.
    if original_remaining_ms == 0 || original_remaining_ms > 300_000 {
        return Err(());
    }
    let original = original_start + Duration::from_millis(original_remaining_ms);
    let seconds = match phase.as_str() {
        "start" => 20,
        "running" | "stop-attached" | "attached-ended" | "jobs-ended" | "finished" => 10,
        _ => return Err(()),
    };
    let url = url::Url::parse(&endpoint).map_err(|_| ())?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(());
    }
    let deadline = original.min(Instant::now() + Duration::from_secs(seconds));
    *stage = ObserverStage::ClientBuild;
    let client = Client::new(
        &endpoint,
        Some(Tokens {
            access_token: "synthetic-route-device-token".into(),
            refresh_token: "synthetic-route-refresh-token".into(),
            token_type: "Bearer".into(),
            expires_in: 3600,
        }),
    )
    .map_err(|_| ())?;
    *stage = ObserverStage::Me;
    bounded(deadline, client.me()).await?.map_err(|_| ())?;
    *stage = ObserverStage::Capabilities;
    if !bounded(deadline, client.cluster_capabilities())
        .await?
        .map_err(|_| ())?
        .jobs_supported()
    {
        return Err(());
    }
    match phase.as_str() {
        "start" => {
            for (id, attached, operation_id, time) in [
                (BATCH, false, "c1-start-batch", "00:01:00"),
                (ATTACHED, true, "c1-start-attached", "00:10:00"),
            ] {
                let operation = ClusterOperation::StartJob {
                    operation_id: operation_id.into(),
                    job_id: id.into(),
                    name: None,
                    spec: chimaera_core::slurm::LaunchSpec {
                        partition: Some("fixture".into()),
                        time: time.into(),
                        ..Default::default()
                    },
                    open: Vec::new(),
                    startup: String::new(),
                    attached,
                    replaces: None,
                    save_as: None,
                };
                *stage = if attached {
                    ObserverStage::StartAttachedReply
                } else {
                    ObserverStage::StartBatchReply
                };
                match mutate(&client, operation, deadline).await? {
                    ClusterReply::Job {
                        job_id,
                        slurm_job_id,
                        attached: actual,
                    } if job_id == id
                        && actual == attached
                        && (attached || slurm_job_id.is_some()) => {}
                    _ => return Err(()),
                }
            }
            let mut ready = None;
            for read in 0..4 {
                *stage = ObserverStage::Snapshot;
                let state = snapshot(&client, deadline, true, stage).await?;
                *stage = ObserverStage::StartupProjection;
                if startup_ready(&state)? {
                    ready = Some(state);
                    break;
                }
                if read < 3 {
                    *stage = ObserverStage::StartupDelay;
                    bounded(deadline, tokio::time::sleep(Duration::from_secs(1))).await?;
                }
            }
            *stage = ObserverStage::StartupProjection;
            let state = ready.ok_or(())?;
            *stage = ObserverStage::BatchHealth;
            health(&client, &state, BATCH, deadline).await?;
            *stage = ObserverStage::AttachedHealth;
            health(&client, &state, ATTACHED, deadline).await?;
            println!("C1_LINK_STARTED");
        }
        "running" => {
            running(&client, deadline, stage).await?;
            println!("C1_LINK_RUNNING");
        }
        "stop-attached" => {
            *stage = ObserverStage::StopReply;
            match mutate(
                &client,
                ClusterOperation::StopJob {
                    operation_id: "c1-stop-attached".into(),
                    job_id: ATTACHED.into(),
                },
                deadline,
            )
            .await?
            {
                ClusterReply::StopPending { job_id } if job_id == ATTACHED => (),
                _ => return Err(()),
            }
            *stage = ObserverStage::Snapshot;
            let state = snapshot(&client, deadline, true, stage).await?;
            *stage = ObserverStage::PendingProjection;
            if !state
                .jobs
                .iter()
                .find(|job| job.id == ATTACHED)
                .is_some_and(pending_stop)
            {
                return Err(());
            }
            *stage = ObserverStage::BatchState;
            if job(&state, BATCH, false)? != ClusterJobState::Running {
                return Err(());
            }
            *stage = ObserverStage::BatchHealth;
            health(&client, &state, BATCH, deadline).await?;
            *stage = ObserverStage::RouteDenial;
            match bounded(deadline, client.cluster_tcp(HOST, ATTACHED, None)).await? {
                Err(error) if error.to_string() == "websocket upgrade rejected (503)" => (),
                Ok(mut socket) => {
                    let _ = bounded(deadline, socket.close(None)).await;
                    return Err(());
                }
                _ => return Err(()),
            }
            println!("C1_LINK_UNCERTAIN");
        }
        "attached-ended" => {
            *stage = ObserverStage::Snapshot;
            let state = snapshot(&client, deadline, true, stage).await?;
            *stage = ObserverStage::AttachedTerminal;
            if !ended(&state, ATTACHED) {
                return Err(());
            }
            *stage = ObserverStage::BatchState;
            if job(&state, BATCH, false)? != ClusterJobState::Running {
                return Err(());
            }
            *stage = ObserverStage::BatchHealth;
            health(&client, &state, BATCH, deadline).await?;
            println!("C1_LINK_ATTACHED_ENDED");
        }
        "jobs-ended" => {
            *stage = ObserverStage::Snapshot;
            let state = snapshot(&client, deadline, true, stage).await?;
            if jobs_ended(&state, stage)? {
                println!("C1_LINK_JOBS_ENDED");
            } else {
                println!("C1_LINK_BATCH_PENDING");
            }
        }
        "finished" => {
            // Ordinary UI polling is passive. No refresh or target-health call
            // may reseed the normal login grace after the quiet observation.
            *stage = ObserverStage::Snapshot;
            let state = snapshot(&client, deadline, false, stage).await?;
            *stage = ObserverStage::AttachedTerminal;
            if !ended(&state, ATTACHED) {
                return Err(());
            }
            *stage = ObserverStage::BatchTerminal;
            if !ended(&state, BATCH) {
                return Err(());
            }
            *stage = ObserverStage::RouteAbsence;
            if !state.routes.is_empty() {
                return Err(());
            }
            println!("C1_LINK_FINISHED");
        }
        _ => return Err(()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed_job(state: &str, scheduler_id: Option<&str>) -> chimaera_link::ClusterJobView {
        serde_json::from_value(serde_json::json!({
            "id": ATTACHED,
            "name": "fixture",
            "state": state,
            "slurm_job_id": scheduler_id,
            "attached": true,
            "stopped_by_user": false,
            "open": [],
            "spec": chimaera_core::slurm::LaunchSpec::default(),
            "startup": "",
            "submitted_ms": 0
        }))
        .unwrap_or_else(|_| panic!("fixed job parser fixture"))
    }

    #[test]
    fn pending_stop_is_not_running_identity_or_terminal_evidence() {
        for id in [None, Some("7002")] {
            let mut row = parsed_job("running", id);
            row.stopping = true;
            row.stopped_by_user = true;
            assert!(pending_stop(&row));
            assert_eq!(observed_job(&row, true).is_ok(), id.is_some());
            row.state = ClusterJobState::Ended;
            assert!(!pending_stop(&row));
            row.state = ClusterJobState::Running;
            row.ended = Some("CANCELLED".into());
            assert!(!pending_stop(&row));
            row.ended = None;
            row.ended_at_ms = Some(1);
            assert!(!pending_stop(&row));
            row.ended_at_ms = None;
            row.stopping = false;
            assert!(!pending_stop(&row));
            row.stopping = true;
            row.stopped_by_user = false;
            assert!(!pending_stop(&row));
            row.stopped_by_user = true;
            row.attached = false;
            assert!(!pending_stop(&row));
            row.attached = true;
            row.id = BATCH.into();
            assert!(!pending_stop(&row));
        }
    }

    #[test]
    fn terminal_presentation_is_not_a_scheduler_reason_receipt() {
        let mut row = parsed_job("ended", None);
        row.stopping = true;
        row.stopped_by_user = true;
        // These fields are optional presentation, even after verified cleanup.
        assert!(row.ended.is_none() && row.ended_at_ms.is_none());
        let mut state: ClusterSnapshot = serde_json::from_value(serde_json::json!({
            "scheduler": "slurm", "login_node": "", "home": "", "now_ms": 1,
            "jobs": [row], "workspaces": [], "other_jobs": {"running": 0, "waiting": 0},
            "degraded": false, "queue_at_ms": 1,
            "config": chimaera_core::cluster::ClusterConfig::default(),
            "startup": {"cluster": "", "workspaces": {}},
            "config_sum": "", "state_unreadable": false, "records": {}, "routes": []
        }))
        .unwrap_or_else(|_| panic!("fixed terminal parser fixture"));
        assert!(ended(&state, ATTACHED));
        assert!(!ended(&state, BATCH));
        state.jobs[0].state = ClusterJobState::Running;
        assert!(!ended(&state, ATTACHED));
        state.jobs[0].state = ClusterJobState::Ended;
        state.routes.push(chimaera_link::ClusterRoute {
            job_id: ATTACHED.into(),
            workspace_id: None,
            daemon: chimaera_link::Daemon {
                token: "synthetic".into(),
                build: "fixture".into(),
                sessions: 0,
            },
        });
        assert!(!ended(&state, ATTACHED));
    }

    #[test]
    fn jobs_ended_without_queue_id_is_only_terminal_presentation() {
        let attached = parsed_job("ended", None);
        let mut batch = parsed_job("ended", None);
        batch.id = BATCH.into();
        batch.attached = false;
        let mut state: ClusterSnapshot = serde_json::from_value(serde_json::json!({
            "scheduler": "slurm", "login_node": "", "home": "", "now_ms": 1,
            "jobs": [attached, batch], "workspaces": [], "other_jobs": {"running": 0, "waiting": 0},
            "degraded": false, "queue_at_ms": 1,
            "config": chimaera_core::cluster::ClusterConfig::default(),
            "startup": {"cluster": "", "workspaces": {}},
            "config_sum": "", "state_unreadable": false, "records": {}, "routes": []
        }))
        .unwrap_or_else(|_| panic!("fixed jobs-ended parser fixture"));
        let mut stage = ObserverStage::Arguments;
        assert_eq!(jobs_ended(&state, &mut stage), Ok(true));
        state.jobs[1].attached = true;
        assert_eq!(jobs_ended(&state, &mut stage), Err(()));
        assert_eq!(stage.label(), "batch-terminal");
        state.jobs[1].attached = false;
        state.jobs[1].state = ClusterJobState::Running;
        // Missing identity remains invalid for positive running/pending work.
        assert_eq!(jobs_ended(&state, &mut stage), Err(()));
        assert_eq!(stage.label(), "batch-state");
        state.jobs[1].slurm_job_id = Some("7001".into());
        assert_eq!(jobs_ended(&state, &mut stage), Ok(false));
        state.jobs[1].state = ClusterJobState::Ended;
        state.jobs[1].slurm_job_id = None;
        state.routes.push(chimaera_link::ClusterRoute {
            job_id: "j-99999999".into(),
            workspace_id: None,
            daemon: chimaera_link::Daemon {
                token: "synthetic".into(),
                build: "fixture".into(),
                sessions: 0,
            },
        });
        assert_eq!(jobs_ended(&state, &mut stage), Err(()));
        assert_eq!(stage.label(), "route-absence");
        state.routes[0].job_id = BATCH.into();
        assert_eq!(jobs_ended(&state, &mut stage), Err(()));
        assert_eq!(stage.label(), "route-absence");
        state.routes.clear();
        state.jobs[0].state = ClusterJobState::Running;
        assert_eq!(jobs_ended(&state, &mut stage), Err(()));
        assert_eq!(stage.label(), "attached-terminal");
    }

    #[test]
    fn asynchronous_allocation_identity_is_not_positive_running_evidence() {
        for state in ["waiting", "starting"] {
            let row = parsed_job(state, None);
            assert_eq!(startup_job(&row, true), Ok(false));
            // The ordinary post-start observer still rejects a missing ID.
            assert_eq!(observed_job(&row, true), Err(()));
        }
        assert_eq!(startup_job(&parsed_job("running", None), true), Err(()));
        assert_eq!(
            startup_job(&parsed_job("running", Some("7002")), true),
            Ok(true)
        );
        assert_eq!(
            startup_job(&parsed_job("running", Some("7002")), false),
            Err(())
        );
        // Unknown wire states must not silently become tolerated pending work.
        assert_eq!(startup_job(&parsed_job("pending", None), true), Err(()));
        assert_eq!(
            startup_job(&parsed_job("ended", Some("7002")), true),
            Err(())
        );
    }
    #[test]
    fn snapshot_diagnostics_keep_exact_projection_and_optional_terminal_id() {
        let mut attached = parsed_job("ended", None);
        attached.id = ATTACHED.into();
        let mut batch = parsed_job("ended", None);
        batch.id = BATCH.into();
        batch.attached = false;
        let state: ClusterSnapshot = serde_json::from_value(serde_json::json!({
            "scheduler": "slurm", "login_node": "", "home": "", "now_ms": 1,
            "jobs": [attached, batch], "workspaces": [], "other_jobs": {"running": 0, "waiting": 0},
            "degraded": false, "queue_at_ms": 1,
            "config": chimaera_core::cluster::ClusterConfig::default(),
            "startup": {"cluster": "", "workspaces": {}},
            "config_sum": "", "state_unreadable": false, "records": {}, "routes": []
        }))
        .unwrap_or_else(|_| panic!("fixed snapshot projection fixture"));
        let project = |state: ClusterSnapshot| {
            let reply = ClusterReply::Overview {
                overview: Box::new(state),
            };
            // Exercise the same actual Link DTO validation before projection.
            assert!(reply.validate().is_ok());
            let mut stage = ObserverStage::Arguments;
            let accepted = snapshot_projection(reply, &mut stage).is_ok();
            (accepted, stage.label())
        };
        assert!(project(state.clone()).0);
        let mut changed = state.clone();
        changed.jobs.pop();
        assert_eq!(project(changed), (false, "snapshot-job-count"));
        let mut changed = state.clone();
        changed.jobs[1].id = ATTACHED.into();
        assert_eq!(project(changed), (false, "snapshot-job-identity"));
        let mut changed = state.clone();
        changed.state_unreadable = true;
        assert_eq!(project(changed), (false, "snapshot-state-unreadable"));
        let mut changed = state.clone();
        changed.workspaces.push(
            serde_json::from_value(serde_json::json!({
                "id": "w-00000001", "state": "closed", "name": "fixture", "path": ""
            }))
            .unwrap_or_else(|_| panic!("fixed workspace projection fixture")),
        );
        assert_eq!(project(changed), (false, "snapshot-workspaces"));
        let mut stage = ObserverStage::Arguments;
        assert!(snapshot_projection(ClusterReply::Saved, &mut stage).is_err());
        assert_eq!(stage.label(), "snapshot-reply-variant");
    }

    #[test]
    fn snapshot_error_diagnostics_use_only_closed_typed_classes() {
        use chimaera_link::{ClusterErrorCode, ClusterRequestError};
        let statuses = [
            (400, "bad-request"),
            (401, "unauthorized"),
            (403, "forbidden"),
            (404, "not-found"),
            (409, "conflict"),
            (429, "limited"),
            (503, "unavailable"),
            (599, "other"),
        ];
        let codes = [
            (
                ClusterErrorCode::UnsupportedClusterPolicy,
                "unsupported-policy",
            ),
            (ClusterErrorCode::OperationChanged, "operation-changed"),
            (ClusterErrorCode::ClusterRequiresJob, "requires-job"),
            (ClusterErrorCode::JobsHeld, "jobs-held"),
            (ClusterErrorCode::JobUnavailable, "job-unavailable"),
            (ClusterErrorCode::JobsChanged, "jobs-changed"),
            (ClusterErrorCode::RolloutPending, "rollout-pending"),
            (ClusterErrorCode::Unknown, "unknown"),
        ];
        for (status, expected_status) in statuses {
            for (code, expected_code) in codes {
                let error = anyhow::Error::new(ClusterRequestError { status, code });
                match snapshot_error(&error) {
                    ObserverStage::SnapshotRequestError { status, code } => {
                        assert_eq!(status, expected_status);
                        assert_eq!(code, expected_code);
                    }
                    _ => panic!("typed snapshot error classification"),
                }
            }
        }
        let error = anyhow::anyhow!("synthetic unpublished payload");
        assert_eq!(snapshot_error(&error).label(), "snapshot-client-error");
    }
}
