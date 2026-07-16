use std::sync::Arc;

use axum::routing::{delete, get, post};
use axum::{middleware, Router};
use tower_http::trace::TraceLayer;

use crate::AppState;
use crate::{
    agents, api, assets, chat, compute, compute_jobs, download, environment, fs, git, launcher,
    links, mcp, quickopen, recents, runtimes, settings, update, upload, view_state, ws,
};

/// Build the axum router (factored out so tests can drive it with `oneshot`).
pub(crate) fn app(state: Arc<AppState>) -> Router {
    // Consumes the chat manager's hook signals for the daemon's lifetime
    // (no-op when already running — tests may build several routers).
    chat::spawn_signal_task(state.clone());
    let api = Router::new()
        .route("/health", get(api::health))
        .route(
            "/workspaces",
            get(api::list_workspaces).post(api::create_workspace),
        )
        .route("/workspaces/{id}", delete(api::delete_workspace))
        .route("/workspaces/{id}/open", post(api::open_workspace))
        .route(
            "/sessions",
            get(api::list_sessions)
                .post(api::create_session)
                .delete(api::delete_all_sessions),
        )
        .route(
            "/sessions/{id}",
            delete(api::delete_session).patch(api::rename_session),
        )
        // In-band graceful shutdown: end every session, then stop the daemon.
        // The only way (besides an OS signal) to bring the daemon down — the
        // app drives it through the tunnel to shut a remote host down.
        .route("/shutdown", post(api::shutdown))
        .route("/sessions/{id}/exec", post(api::exec_session))
        // Streamed to disk with its own per-file/per-session caps (see
        // `upload`); the DefaultBodyLimit override only lifts axum's 2MB
        // buffered-body default out of the way of multi-MB screenshots.
        .route(
            "/sessions/{id}/upload",
            post(upload::upload).layer(axum::extract::DefaultBodyLimit::max(
                upload::MAX_UPLOAD_BYTES as usize + 64 * 1024,
            )),
        )
        .route("/sessions/{id}/journal", get(api::session_journal))
        .route("/sessions/{id}/view", post(chat::switch_view))
        .route("/sessions/{id}/rewind", post(chat::rewind_session))
        .route("/links", get(links::list_links).put(links::put_link))
        .route("/links/{terminal_id}", delete(links::delete_link))
        .route("/agents", get(launcher::list_agents))
        .route(
            "/agents/{id}/install",
            post(runtimes::install_agent).delete(runtimes::uninstall_agent),
        )
        .route("/agents/claude/sessions", get(launcher::claude_resumables))
        .route("/recents", get(recents::list_recents))
        .route("/update", get(update::get_update))
        .route(
            "/view-state/{key}",
            get(view_state::get_view_state).put(view_state::put_view_state),
        )
        .route(
            "/settings",
            get(settings::get_settings).put(settings::put_settings),
        )
        .route(
            "/environment",
            get(environment::get_environment).put(environment::put_environment),
        )
        .route("/fs/home", get(fs::home))
        .route("/fs/dirs", get(fs::dirs))
        .route("/fs/list", get(fs::list))
        .route("/fs/file", get(fs::file).put(fs::put_file))
        .route("/fs/markdown", get(fs::markdown))
        .route("/fs/table", get(fs::table))
        .route("/fs/xlsx", get(fs::xlsx))
        .route("/fs/quickopen", get(quickopen::quickopen))
        .route("/fs/validate", post(fs::validate))
        .route("/fs/mkdir", post(fs::mkdir))
        .route("/fs/create", post(fs::create))
        .route("/fs/rename", post(fs::rename))
        .route("/fs/copy", post(fs::copy))
        .route("/fs/move", post(fs::move_))
        .route("/fs/delete", post(fs::delete))
        // OS-desktop drop into a chosen folder; same streaming + body-limit
        // override as the session upload route.
        .route(
            "/fs/upload",
            post(upload::upload_to_dir).layer(axum::extract::DefaultBodyLimit::max(
                upload::MAX_UPLOAD_BYTES as usize + 64 * 1024,
            )),
        )
        .route("/compute", get(compute::get_compute))
        .route(
            "/compute/sessions",
            get(compute_jobs::list_compute_sessions).post(compute_jobs::launch_compute_session),
        )
        .route(
            "/compute/sessions/{job_id}",
            delete(compute_jobs::cancel_compute_session),
        )
        .route("/git/status", get(git::status))
        .route("/git/diff", get(git::diff))
        .route(
            "/git/worktrees",
            get(git::worktrees)
                .post(git::create_worktree)
                .delete(git::remove_worktree),
        )
        .route("/fs/ticket", post(fs::create_ticket))
        .route_layer(middleware::from_fn_with_state(state.clone(), api::auth))
        // Registered after route_layer, so hook ingestion is NOT behind bearer
        // auth: claude's hooks cannot know the daemon token, so the random
        // per-session key embedded in the hook URL authorizes them instead.
        .route("/agent-events/{id}", post(agents::ingest))
        // Same key-in-URL auth story as agent-events: claude's MCP client
        // cannot know the daemon bearer token.
        .route("/mcp/{id}", post(mcp::mcp))
        .with_state(state.clone());

    // The WS routes stay outside the bearer-header middleware: browsers cannot
    // set headers on a WebSocket, so they authenticate via their first frame.
    // /raw/{ticket} is also unauthenticated: iframes and img tags cannot send
    // Authorization headers, so a short-lived single-path ticket (minted via
    // the bearer-authed POST /api/v1/fs/ticket) authorizes each fetch instead.
    // /download/{ticket} rides the same ticket story: an <a href> download
    // navigation cannot send headers either.
    let ws = Router::new()
        .route("/ws/sessions/{id}", get(ws::session_ws))
        .route("/ws/chat/{id}", get(ws::chat_ws))
        .route("/ws/events", get(ws::events_ws))
        .route("/raw/{ticket}", get(fs::raw))
        .route("/download/{ticket}", get(download::download))
        .with_state(state);

    Router::new()
        .nest("/api/v1", api)
        .merge(ws)
        .fallback(assets::static_handler)
        .layer(TraceLayer::new_for_http())
}
