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
) -> Result<Box<ClusterSnapshot>, ()> {
    match bounded(
        deadline,
        client.cluster_operation(HOST, &ClusterOperation::Overview { refresh }),
    )
    .await?
    .map_err(|_| ())?
    {
        ClusterReply::Overview { overview }
            if overview.jobs.len() == 2
                && overview.jobs.iter().filter(|job| job.id == BATCH).count() == 1
                && overview
                    .jobs
                    .iter()
                    .filter(|job| job.id == ATTACHED)
                    .count()
                    == 1
                && overview.workspaces.is_empty()
                && !overview.state_unreadable =>
        {
            Ok(overview)
        }
        _ => Err(()),
    }
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

async fn running(client: &Client, deadline: Instant) -> Result<(), ()> {
    let state = snapshot(client, deadline, true).await?;
    if state.degraded
        || job(&state, BATCH, false)? != ClusterJobState::Running
        || job(&state, ATTACHED, true)? != ClusterJobState::Running
    {
        return Err(());
    }
    health(client, &state, BATCH, deadline).await?;
    health(client, &state, ATTACHED, deadline).await
}

#[derive(Clone, Copy)]
enum StopStage {
    Arguments,
    ClientBuild,
    Me,
    Capabilities,
    StopReply,
    Snapshot,
    PendingProjection,
    BatchState,
    BatchHealth,
    RouteDenial,
}
impl StopStage {
    fn label(self) -> &'static str {
        match self {
            Self::Arguments => "arguments",
            Self::ClientBuild => "client-build",
            Self::Me => "me",
            Self::Capabilities => "capabilities",
            Self::StopReply => "stop-reply",
            Self::Snapshot => "snapshot",
            Self::PendingProjection => "pending-projection",
            Self::BatchState => "batch-state",
            Self::BatchHealth => "batch-health",
            Self::RouteDenial => "route-denial",
        }
    }
}

pub(super) async fn run(
    endpoint: String,
    phase: String,
    original_remaining_ms: u64,
) -> Result<(), ()> {
    let original_start = Instant::now();
    let report = phase == "stop-attached";
    let mut stage = StopStage::Arguments;
    let result = observe(
        endpoint,
        phase,
        original_remaining_ms,
        &mut stage,
        original_start,
    )
    .await;
    if report && result.is_err() {
        use std::io::Write;
        // Fixed fixture labels only. A closed diagnostic sink cannot replace the
        // original error, and no HTTP body/status/identity is rendered.
        let _ = writeln!(
            std::io::stdout().lock(),
            "C1_STOP_REFUSED {}",
            stage.label()
        );
    }
    result
}

async fn observe(
    endpoint: String,
    phase: String,
    original_remaining_ms: u64,
    stage: &mut StopStage,
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
    *stage = StopStage::ClientBuild;
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
    *stage = StopStage::Me;
    bounded(deadline, client.me()).await?.map_err(|_| ())?;
    *stage = StopStage::Capabilities;
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
                match mutate(&client, operation, deadline).await? {
                    ClusterReply::Job {
                        job_id,
                        slurm_job_id,
                        attached: actual,
                    } if job_id == id
                        && actual == attached
                        && (attached || slurm_job_id.is_some()) =>
                    {
                        ()
                    }
                    _ => return Err(()),
                }
            }
            let mut ready = None;
            for read in 0..4 {
                let state = snapshot(&client, deadline, true).await?;
                if startup_ready(&state)? {
                    ready = Some(state);
                    break;
                }
                if read < 3 {
                    bounded(deadline, tokio::time::sleep(Duration::from_secs(1))).await?;
                }
            }
            let state = ready.ok_or(())?;
            health(&client, &state, BATCH, deadline).await?;
            health(&client, &state, ATTACHED, deadline).await?;
            println!("C1_LINK_STARTED");
        }
        "running" => {
            running(&client, deadline).await?;
            println!("C1_LINK_RUNNING");
        }
        "stop-attached" => {
            *stage = StopStage::StopReply;
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
            *stage = StopStage::Snapshot;
            let state = snapshot(&client, deadline, true).await?;
            *stage = StopStage::PendingProjection;
            if !state
                .jobs
                .iter()
                .find(|job| job.id == ATTACHED)
                .is_some_and(pending_stop)
            {
                return Err(());
            }
            *stage = StopStage::BatchState;
            if job(&state, BATCH, false)? != ClusterJobState::Running {
                return Err(());
            }
            *stage = StopStage::BatchHealth;
            health(&client, &state, BATCH, deadline).await?;
            *stage = StopStage::RouteDenial;
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
            let state = snapshot(&client, deadline, true).await?;
            if !ended(&state, ATTACHED) || job(&state, BATCH, false)? != ClusterJobState::Running {
                return Err(());
            }
            health(&client, &state, BATCH, deadline).await?;
            println!("C1_LINK_ATTACHED_ENDED");
        }
        "jobs-ended" => {
            let state = snapshot(&client, deadline, true).await?;
            if !ended(&state, ATTACHED) {
                return Err(());
            }
            if job(&state, BATCH, false)? == ClusterJobState::Running {
                println!("C1_LINK_BATCH_PENDING");
            } else if ended(&state, BATCH) && state.routes.is_empty() {
                println!("C1_LINK_JOBS_ENDED");
            } else {
                return Err(());
            }
        }
        "finished" => {
            // Ordinary UI polling is passive. No refresh or target-health call
            // may reseed the normal login grace after the quiet observation.
            let state = snapshot(&client, deadline, false).await?;
            if !ended(&state, ATTACHED) || !ended(&state, BATCH) || !state.routes.is_empty() {
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
}
