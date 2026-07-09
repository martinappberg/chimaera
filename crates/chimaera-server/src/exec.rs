//! Running a command inside a live shell session, with the server's exec
//! policy attached. Transport-neutral: both the REST endpoint
//! (`api::exec_session`) and the MCP `run_in_terminal` tool call
//! [`run_exec`], so it lives here rather than under `api/` — `mcp` must not
//! depend on the HTTP layer.

use std::sync::Arc;

use crate::AppState;

/// Foreground commands that forward keystrokes to a shell somewhere else
/// (remote or containerized), making sentinel-typing over a `running` phase
/// safe. Anything else running in the foreground (sleep, vim, tail) refuses.
const SENTINEL_FOREGROUNDS: &[&str] = &[
    "ssh",
    "mosh",
    "mosh-client",
    "et",
    "docker",
    "podman",
    "kubectl",
    "oc",
    "singularity",
    "apptainer",
];

/// Run an exec with the server's policy attached: sentinel over a *running*
/// phase only when the foreground forwards keystrokes elsewhere (ssh and
/// friends), and the stage (queued/executing) mirrored into session
/// snapshots for the UI's linked-terminal chips. Shared by the REST
/// endpoint and the MCP `run_in_terminal` tool.
pub(crate) async fn run_exec(
    state: &Arc<AppState>,
    id: &str,
    command: String,
    timeout_ms: Option<u64>,
    queue_timeout_ms: Option<u64>,
) -> Result<chimaera_pty::ExecOutcome, chimaera_pty::ExecError> {
    let allow_sentinel_over_running = state
        .sessions
        .foreground_pid(id)
        .and_then(crate::naming::comm_name)
        .is_some_and(|comm| SENTINEL_FOREGROUNDS.contains(&comm.as_str()));

    let (stage_tx, mut stage_rx) = tokio::sync::watch::channel(chimaera_pty::ExecStage::Queued);
    let mirror = {
        let state = state.clone();
        let id = id.to_string();
        tokio::spawn(async move {
            loop {
                let stage = *stage_rx.borrow_and_update();
                crate::lock(&state.exec_status).insert(id.clone(), stage);
                state.changes.notify_waiters();
                if stage_rx.changed().await.is_err() {
                    return;
                }
            }
        })
    };

    let outcome = state
        .sessions
        .exec(
            id,
            chimaera_pty::ExecOptions {
                command,
                queue_timeout: std::time::Duration::from_millis(
                    queue_timeout_ms.unwrap_or(15_000).min(600_000),
                ),
                timeout: std::time::Duration::from_millis(
                    timeout_ms.unwrap_or(30_000).min(3_600_000),
                ),
                allow_sentinel_over_running,
                stage: Some(stage_tx),
            },
        )
        .await;

    mirror.abort();
    crate::lock(&state.exec_status).remove(id);
    state.changes.notify_waiters();
    outcome
}
