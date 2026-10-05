use std::sync::Arc;

use axum::routing::{delete, get, post, put};
use axum::{middleware, Router};
use tower_http::trace::TraceLayer;

use crate::AppState;
use crate::{
    agent_probe, agents, api, chat, compute, download, drafts, environment, fs, git, launcher,
    links, mcp, notebook, notices, plugins, proxy, quickopen, recents, runtimes, settings,
    timeline, update, upload, view_state, voice, ws,
};

/// Build the axum router (factored out so tests can drive it with `oneshot`).
pub(crate) fn app(state: Arc<AppState>) -> Router {
    // Consumes the chat manager's hook signals for the daemon's lifetime
    // (no-op when already running — tests may build several routers).
    chat::spawn_signal_task(state.clone());
    // Finished Slurm jobs → the Timeline (idempotent; idle without a queue).
    crate::episodes::spawn_jobs_task(state.clone());
    // Plugins' file events and watch sweep (idle without listeners).
    plugins::files::spawn_worker(state.clone());
    // Switched-on plugins compiled ahead of their first use.
    plugins::runtime::warm(&state, None, std::time::Duration::from_secs(3));
    // What an install cut short (a stop, a reboot) left in a tool's folder.
    {
        let state = state.clone();
        tokio::task::spawn_blocking(move || plugins::toolchain::sweep_all_leftovers(&state));
    }
    let api = Router::new()
        .route("/health", get(api::health))
        .route(
            "/pro/configure",
            post(crate::pro::configure).delete(crate::pro::disconnect),
        )
        .route(
            "/pro/configure/workspace",
            post(crate::pro::configure_workspace),
        )
        .route(
            "/pro/configure/execution",
            post(crate::pro::configure_execution),
        )
        .route(
            "/pro/execution/recover",
            post(crate::pro::recover_execution),
        )
        .route("/pro/status", get(crate::pro::status))
        .route("/pro/privacy", put(crate::pro::privacy))
        .route(
            "/pro/projects",
            get(crate::pro::project_list).put(crate::pro::projects),
        )
        .route("/pro/projects/open", post(crate::pro::open_project))
        .route("/pro/projects/copy", post(crate::pro::copy_project))
        .route("/pro/projects/takeover", post(crate::pro::takeover_project))
        // Both versions a return kept (`pro/kept.rs`): list, compare, choose.
        .route("/pro/projects/{id}/kept", get(crate::pro::kept_list))
        .route("/pro/projects/{id}/kept/file", get(crate::pro::kept_file))
        .route(
            "/pro/projects/{id}/kept/resolve",
            post(crate::pro::kept_resolve),
        )
        .route(
            "/pro/projects/{id}/kept/resolve_all",
            post(crate::pro::kept_resolve_all),
        )
        .route(
            "/pro/profile",
            get(crate::pro::profile).put(crate::pro::put_profile),
        )
        .route("/pro/sleep", post(crate::pro::sleep))
        .route("/pro/wake", post(crate::pro::wake))
        .route("/pro/power", put(crate::pro::power))
        .route("/pro/handoff", post(crate::pro::handoff))
        .route(
            "/pro/drain",
            post(crate::pro::drain).delete(crate::pro::cancel_drain),
        )
        .route("/pro/hydrate", post(crate::pro::hydrate))
        .route("/pro/cloud", get(crate::cloud::info))
        .route("/pro/cloud/providers", get(crate::cloud::providers::list))
        .route(
            "/pro/cloud/providers/{id}/connect",
            post(crate::cloud::providers::start),
        )
        .route(
            "/pro/cloud/providers/{id}/disconnect",
            post(crate::cloud::providers::disconnect),
        )
        .route(
            "/pro/cloud/connections/{id}",
            get(crate::cloud::providers::get),
        )
        .route(
            "/pro/cloud/connections/{id}/cancel",
            post(crate::cloud::providers::cancel),
        )
        .route(
            "/pro/cloud/connections/{id}/input",
            post(crate::cloud::providers::submit),
        )
        .route("/pro/cloud/project", post(crate::cloud::project))
        .route("/pro/bundles/{id}", get(crate::bundle::snapshot_route))
        .route(
            "/pro/bundles/{id}/export",
            post(crate::bundle::export_route),
        )
        .route(
            "/pro/bundles",
            post(crate::bundle::import_route).layer(axum::extract::DefaultBodyLimit::max(
                crate::bundle::MAX_ARCHIVE as usize,
            )),
        )
        .route(
            "/pro/placements",
            get(crate::session_proxy::inventory)
                .post(crate::session_proxy::register)
                .delete(crate::session_proxy::remove),
        )
        .route(
            "/workspaces",
            get(api::list_workspaces).post(api::create_workspace),
        )
        .route("/workspaces/{id}", delete(api::delete_workspace))
        .route("/workspaces/{id}/open", post(api::open_workspace))
        // The workspace Mastermind: PUT creates-and-binds (re-PUT retires the
        // old one — that is also how the mode changes), DELETE unbinds.
        .route(
            "/workspaces/{id}/mastermind",
            put(api::put_mastermind).delete(api::delete_mastermind),
        )
        .route("/workspaces/{id}/timeline", get(timeline::get_timeline))
        // Session history: every past session's record, and cost totals
        // across workspaces (`history`).
        .route(
            "/workspaces/{id}/history",
            get(crate::history::routes::list),
        )
        .route(
            "/workspaces/{id}/same-file",
            get(crate::history::routes::same_file),
        )
        .route("/activity", get(crate::history::routes::get_activity))
        .route(
            "/activity/csv",
            get(crate::history::routes::get_activity_csv),
        )
        // Agent communication (`comms`): the USER sends one Timeline
        // message to its addressee, hands a session its whole inbox, or
        // answers a wake request (their clicks); the unread counts and the
        // requests for a workspace.
        .route(
            "/workspaces/{id}/timeline/{seq}/deliver",
            post(crate::comms::deliver),
        )
        .route("/workspaces/{id}/comms", get(crate::comms::get_comms))
        .route(
            "/workspaces/{id}/comms/deliver",
            post(crate::comms::post_deliver),
        )
        .route(
            "/workspaces/{id}/comms/wakes/{wid}",
            post(crate::comms::post_wake),
        )
        // Workbench plugins: the catalog, per-workspace status, and the
        // per-workspace switch (`Workspace.plugins_on`; off by default).
        .route("/plugins", get(plugins::list_plugins))
        // Installed plugins (`plugins::installed`): install from a release
        // or a local directory, install a first-party plugin's pinned
        // release, update, Use previous, Remove — each a visible,
        // checksum-verified change the user asked for — and Check now
        // (`plugins::releases`).
        .route("/plugins/install", post(plugins::installed::install_route))
        .route(
            "/plugins/{pid}/install",
            post(plugins::installed::pinned_install_route),
        )
        .route("/plugins/{pid}", delete(plugins::installed::remove_route))
        .route(
            "/plugins/{pid}/update",
            post(plugins::installed::update_route),
        )
        .route(
            "/plugins/{pid}/rollback",
            post(plugins::installed::rollback_route),
        )
        .route("/plugins/{pid}/check", post(plugins::releases::check_route))
        // Trust (`plugins::trust`): trust what an installed build can do, or
        // run a soft-blocked one anyway; withdraw every answer; skip an
        // update that asks for more. The activity log (`plugins::activity`).
        .route(
            "/plugins/{pid}/trust",
            post(plugins::trust::trust_route).delete(plugins::trust::untrust_route),
        )
        .route("/plugins/{pid}/skip", post(plugins::trust::skip_route))
        .route(
            "/plugins/{pid}/activity",
            get(plugins::activity::activity_route),
        )
        // What a plugin's release says before it is installed
        // (`plugins::preview`): nothing is written.
        .route(
            "/plugins/{pid}/details",
            get(plugins::preview::details_route),
        )
        .route("/plugins/preview", post(plugins::preview::preview_route))
        .route("/workspaces/{id}/plugins", get(plugins::workspace_plugins))
        .route(
            "/workspaces/{id}/plugins/{pid}",
            put(plugins::put_workspace_plugin),
        )
        .route(
            "/workspaces/{id}/plugins/{pid}/trust-hooks",
            post(agent_probe::trust_hooks),
        )
        .route(
            "/workspaces/{id}/plugins/{pid}/install",
            post(plugins::install_requirement),
        )
        .route(
            "/workspaces/{id}/plugins/{pid}/setup",
            post(plugins::setup_workspace),
        )
        // The platform (0.2): screens (`plugins::screens`), the UI's reads
        // (0.1's promised query route), file menu items, data surfaces
        // (`plugins::surfaces`), output folders (`plugins::output`) and
        // declared settings (`plugins::pdata`).
        .route(
            "/workspaces/{id}/plugins/{pid}/views/{view}",
            get(plugins::screens::render_route),
        )
        .route(
            "/workspaces/{id}/plugins/{pid}/views/{view}/actions",
            post(plugins::screens::action_route),
        )
        .route(
            "/workspaces/{id}/plugins/{pid}/file-actions/{action}",
            post(plugins::screens::file_action_route),
        )
        .route(
            "/workspaces/{id}/plugins/{pid}/query/{name}",
            get(plugins::screens::query_route),
        )
        .route(
            "/workspaces/{id}/surfaces/{kind}/{version}",
            get(plugins::surfaces::route),
        )
        .route(
            "/workspaces/{id}/plugins/{pid}/output",
            get(plugins::output::folder_route),
        )
        .route(
            "/workspaces/{id}/plugins/{pid}/output/save",
            post(plugins::output::save_route),
        )
        .route(
            "/plugins/{pid}/output",
            get(plugins::output::usage_route).delete(plugins::output::clear_route),
        )
        .route(
            "/plugins/{pid}/settings",
            get(plugins::pdata::get_route).put(plugins::pdata::put_route),
        )
        // Programs and tools (the privileged tier): a plugin's side
        // programs (`plugins::toolchain`) and its jobs (`plugins::jobs`).
        .route("/plugins/{pid}/tools", get(plugins::toolchain::list_route))
        .route(
            "/plugins/{pid}/tools/{tool}/install",
            post(plugins::toolchain::install_route),
        )
        .route(
            "/plugins/{pid}/tools/{tool}",
            delete(plugins::toolchain::remove_route),
        )
        .route(
            "/workspaces/{id}/jobs/{job}",
            get(plugins::jobs::status_route).delete(plugins::jobs::cancel_route),
        )
        // What each agent CLI reports it has here (asked of the agents).
        .route(
            "/workspaces/{id}/agent-plugins",
            get(agent_probe::agent_plugins),
        )
        .route("/workspaces/{id}/skills", get(agent_probe::skills))
        .route(
            "/workspaces/{id}/agent-extensions/action",
            post(agent_probe::actions::run),
        )
        .route(
            "/workspaces/{id}/connections",
            get(agent_probe::connections::list),
        )
        .route(
            "/workspaces/{id}/connections/login",
            post(agent_probe::connections::login),
        )
        .route(
            "/workspaces/{id}/connections/login/{attempt}",
            get(agent_probe::connections::status).delete(agent_probe::connections::cancel),
        )
        .route(
            "/workspaces/{id}/connections/login/{attempt}/callback",
            post(agent_probe::connections::input),
        )
        .route(
            "/workspaces/{id}/connections/login/{attempt}/check",
            post(agent_probe::connections::check),
        )
        .route(
            "/workspaces/{id}/knowledge",
            get(crate::knowledge::get_knowledge),
        )
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
        .route("/sessions/{id}/resume", post(api::resume_manual_session))
        // Streamed to disk with its own per-file/per-session caps (see
        // `upload`); the DefaultBodyLimit override only lifts axum's 2MB
        // buffered-body default out of the way of multi-MB screenshots.
        .route(
            "/sessions/{id}/upload",
            post(upload::upload).layer(axum::extract::DefaultBodyLimit::max(
                upload::MAX_SESSION_UPLOAD_FILE_BYTES as usize + 64 * 1024,
            )),
        )
        .route("/sessions/{id}/journal", get(api::session_journal))
        // The agent's own edits per file, live or ended (`history::edits`).
        .route(
            "/sessions/{id}/edits",
            get(crate::history::routes::session_edits),
        )
        .route("/sessions/{id}/view", post(chat::switch_view))
        .route("/sessions/{id}/rewind", post(chat::rewind_session))
        .route("/sessions/{id}/fork", post(chat::fork_session))
        .route("/sessions/{id}/git", get(git::session_git))
        .route("/links", get(links::list_links).put(links::put_link))
        .route("/links/{terminal_id}", delete(links::delete_link))
        .route("/agents", get(launcher::list_agents))
        .route(
            "/agents/{id}/install",
            post(runtimes::install_agent).delete(runtimes::uninstall_agent),
        )
        .route("/agents/{id}/update", post(runtimes::update_agent))
        .route(
            "/agents/{id}/setup",
            get(crate::agent_setup::get).post(crate::agent_setup::start),
        )
        .route(
            "/agents/{id}/setup/{operation}",
            delete(crate::agent_setup::cancel),
        )
        .route("/agents/claude/sessions", get(launcher::claude_resumables))
        .route("/recents", get(recents::list_recents))
        // Archive hides a conversation from Recents (never deletes it); it
        // stays in All sessions under "Archived" (`recents_archive`).
        .route("/recents/archive", post(crate::recents_archive::archive))
        .route(
            "/recents/unarchive",
            post(crate::recents_archive::unarchive),
        )
        .route(
            "/recents/archived",
            get(crate::recents_archive::list_archived),
        )
        .route("/update", get(update::get_update))
        .route("/voice", get(voice::availability))
        // The native shell's notice long-poll (agent finished / needs you).
        .route("/notices", get(notices::get_notices))
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
        .route("/fs/notebook", get(notebook::notebook))
        .route("/fs/quickopen", get(quickopen::quickopen))
        .route("/fs/validate", post(fs::validate))
        .route("/fs/resolve_targets", post(crate::embed::resolve_targets))
        // The portable-dialect checker (the reading view's issues chip; the
        // MCP `check_document` tool runs the same code) and the opt-in
        // "teach agents" installs behind Settings.
        .route("/fs/check_document", get(crate::doc_check::check_document))
        .route("/agent-docs", get(crate::agent_docs::status))
        .route("/agent-docs/install", post(crate::agent_docs::install))
        // The draft mirror (unsaved editor text; see `drafts`). The body
        // limit only makes room for JSON escaping — the 1 MiB text cap is
        // judged on the decoded text.
        .route(
            "/fs/drafts",
            get(drafts::list_drafts).put(drafts::put_draft).layer(
                axum::extract::DefaultBodyLimit::max(drafts::MAX_DRAFT_BODY_BYTES),
            ),
        )
        .route(
            "/fs/draft",
            get(drafts::get_draft).delete(drafts::delete_draft),
        )
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
                upload::MAX_DIR_UPLOAD_BYTES as usize + 64 * 1024,
            )),
        )
        .route("/compute", get(compute::get_compute))
        // A cluster workspace job's own stop (scancel of SLURM_JOB_ID); 404
        // on any daemon not running inside a Slurm job.
        .route("/compute/self", delete(compute::delete_self))
        .route("/git/status", get(git::status))
        .route("/git/diff", get(git::diff))
        .route("/git/branches", get(git::branches))
        .route("/git/repos", get(git::repos))
        .route("/git/log", get(git::log))
        .route("/git/show", get(git::show))
        .route("/git/compare", get(git::compare))
        .route(
            "/git/worktrees",
            get(git::worktrees)
                .post(git::create_worktree)
                .delete(git::remove_worktree),
        )
        .route("/fs/ticket", post(fs::create_ticket))
        // Browser-pane proxy sessions: mint/list/revoke (+ the pane's
        // keep-alive health probe). The data plane rides /proxy below.
        .route("/proxy", get(proxy::list_proxies).post(proxy::create_proxy))
        .route("/proxy/{id}", delete(proxy::delete_proxy))
        .route("/proxy/{id}/health", get(proxy::proxy_health))
        .merge(
            state
                .daemon_extension
                .as_ref()
                .map_or_else(Router::new, |extension| {
                    Router::new().nest_service(
                        "/extensions",
                        extension.workspace_routes(
                            crate::workspace_maintenance::WorkspaceHost::new(state.clone()),
                        ),
                    )
                }),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            crate::session_proxy::api_proxy,
        ))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            crate::workspace_scope::middleware,
        ))
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
    // /proxy/{id} is unauthenticated like /raw: iframes cannot send bearer
    // headers, so the unguessable minted id (pinned to one host:port) is the
    // capability. `any()` — a proxied app uses every method.
    let ws = Router::new()
        .route("/ws/sessions/{id}", get(ws::session_ws))
        .route("/ws/chat/{id}", get(ws::chat_ws))
        .route("/ws/events", get(ws::events_ws))
        .route("/ws/voice", get(voice::voice_ws))
        .route("/raw/{ticket}", get(fs::raw))
        // An HTML report's relative assets, confined to its folder.
        .route("/raw/{ticket}/{*rest}", get(fs::raw_asset))
        .route("/download/{ticket}", get(download::download))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            crate::workspace_scope::ticket_middleware,
        ))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            crate::session_proxy::ticket_proxy,
        ))
        // Three spellings because `{*path}` refuses an EMPTY tail: the bare
        // form redirects to the slashed form, the slashed form IS the app's
        // root document, and the wildcard carries everything deeper.
        .route("/proxy/{id}", axum::routing::any(proxy::data_plane))
        .route("/proxy/{id}/", axum::routing::any(proxy::data_plane))
        .route("/proxy/{id}/{*path}", axum::routing::any(proxy::data_plane))
        .with_state(state.clone());

    Router::new()
        .nest("/api/v1", api)
        .merge(ws)
        // The fallback serves embedded UI assets, rescues absolute-path
        // requests from proxied apps (cookie/Referer), and applies the SPA
        // index.html rules — see proxy::fallback.
        .fallback_service(axum::routing::any(proxy::fallback).with_state(state))
        .layer(middleware::from_fn(crate::workspace_scope::reject_unbound))
        .layer(TraceLayer::new_for_http())
}
