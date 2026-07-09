use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::AppState;

/// Stop every structured chat driver too — [`SessionManager::kill_all`] only
/// covers PTY sessions, so "close all" and shutdown would otherwise leave chat
/// agents running (and billing) and uncounted. Alive drivers get the polite
/// stop (the exit path retires them); dead-but-registered ones are retired
/// directly. Returns how many rows were ended.
fn kill_all_chat(state: &Arc<AppState>) -> usize {
    let mut killed = 0;
    for info in state.chat.list() {
        if info.alive {
            if state.chat.kill(&info.id) {
                killed += 1;
            }
        } else {
            state.chat.remove(&info.id);
            crate::recents::retire(state, &info.id, None, None);
            killed += 1;
        }
    }
    killed
}

/// DELETE /api/v1/sessions — end every live session; the daemon stays up.
/// This is "kill everything here" without the teardown: the caller can start
/// fresh work immediately, no reconnect. Returns how many were ended.
pub(crate) async fn delete_all_sessions(State(state): State<Arc<AppState>>) -> Response {
    let killed = state.sessions.kill_all() + kill_all_chat(&state);
    if killed > 0 {
        state.changes.notify_waiters();
    }
    Json(json!({ "killed": killed })).into_response()
}

/// POST /api/v1/shutdown — end every session, then stop the daemon.
///
/// The kill has to complete BEFORE the process exits: a session that ignores
/// SIGHUP is force-killed on a detached thread (see `kill_all`) that would die
/// with the daemon, so exiting immediately could orphan it and reparent it to
/// init. So we SIGHUP everything now, reply at once (the caller's tunnel is
/// about to drop with the daemon), and let a background task outlast the
/// escalation grace before tripping the graceful-shutdown future.
pub(crate) async fn shutdown(State(state): State<Arc<AppState>>) -> Response {
    let killed = state.sessions.kill_all() + kill_all_chat(&state);
    if killed > 0 {
        state.changes.notify_waiters();
    }
    tracing::info!("in-band shutdown requested; ending {killed} session(s) then stopping");
    tokio::spawn(async move {
        tokio::time::sleep(
            chimaera_pty::KILL_ESCALATION_GRACE + std::time::Duration::from_millis(500),
        )
        .await;
        state.shutdown.notify_one();
    });
    Json(json!({ "killed": killed, "shutdown": true })).into_response()
}
