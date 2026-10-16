//! One command vocabulary shared by the Tauri build-time permission generator
//! and the runtime daemon-window capability. `generate_handler!` remains the
//! compiler-checked dispatch side; wizard grants remain static and local-only.

pub const DAEMON_UI_COMMANDS: &[&str] = &[
    "list_hosts",
    "add_host",
    "remove_host",
    "connect_host",
    "disconnect_host",
    "end_host_sessions",
    "shutdown_host",
    "local_state",
    "update_local_daemon",
    "remote_workspaces",
    "cluster_overview",
    "cluster_facts",
    "cluster_add_workspace",
    "cluster_remove_workspace",
    "cluster_list_dir",
    "cluster_start_job",
    "cluster_continue_job",
    "cluster_stop_job",
    "cluster_dismiss_job",
    "cluster_open",
    "cluster_close",
    "cluster_move",
    "cluster_queue_open",
    "set_not_cluster",
    "cluster_set_startup",
    "cluster_forget_setup",
    "cluster_set_agent_rules",
    "cluster_set_login_serve",
    "cluster_stop_login_daemon",
    "cluster_open_terminal",
    "open_window",
    "navigate_home",
    "open_detached_window",
    "drag_track",
    "drag_drop",
    "drag_cancel",
    "adopt_ack",
    "adopt_tab",
    "list_scope_windows",
    "report_window_scope",
    "report_window_view",
    "report_unsaved",
    "reply_unsaved",
    "take_pending_focus",
    "notification_permission",
    "request_notification_permission",
    "open_notification_settings",
    "test_notification",
    "check_app_update",
    "app_update_status",
    "begin_update",
    "shell_build",
    "write_clipboard",
    "copy_file_to_clipboard",
    "reveal_in_file_manager",
    "open_external",
    "answer_askpass",
    "list_askpass",
    "cache_appearance",
];

// The application crate consumes only the daemon list at runtime; this list is
// consumed by the separately compiled build script.
#[allow(dead_code)]
pub const WSL_SETUP_COMMANDS: &[&str] = &[
    "wsl_status",
    "wsl_install",
    "wsl_update",
    "wsl_install_distro",
    "wsl_setup_daemon",
];

/// Who may invoke a daemon-window command. Every daemon page holds the same
/// grants (remote hosts' pages included), so the authority lives in the
/// command: `OwnHost` commands call `shell::require_target` on the alias
/// they are handed, `LocalOnly` ones `shell::require_local`.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Authority {
    /// Acts only on the caller's own registered window, or on nothing that
    /// belongs to a host (OS settings, the clipboard, a browser URL), or
    /// moves the user to the genuine destination page without returning
    /// its endpoint (Home navigation, open a window).
    AnyWindow,
    /// Names a host; the caller must be local or registered on that host.
    OwnHost,
    /// Host and update management, and handing local paths to the OS.
    LocalOnly,
}

/// Every `DAEMON_UI_COMMANDS` entry, classified exactly once (a test holds
/// the two lists in lockstep, and that each non-`AnyWindow` command's body
/// performs its check).
#[cfg(test)]
pub const COMMAND_AUTHORITY: &[(&str, Authority)] = &[
    ("list_hosts", Authority::AnyWindow),
    ("add_host", Authority::LocalOnly),
    ("remove_host", Authority::LocalOnly),
    ("connect_host", Authority::OwnHost),
    ("disconnect_host", Authority::OwnHost),
    ("end_host_sessions", Authority::OwnHost),
    ("shutdown_host", Authority::LocalOnly),
    ("local_state", Authority::AnyWindow),
    ("update_local_daemon", Authority::LocalOnly),
    ("remote_workspaces", Authority::OwnHost),
    ("cluster_overview", Authority::OwnHost),
    ("cluster_facts", Authority::OwnHost),
    ("cluster_add_workspace", Authority::OwnHost),
    ("cluster_remove_workspace", Authority::OwnHost),
    ("cluster_list_dir", Authority::OwnHost),
    ("cluster_start_job", Authority::OwnHost),
    ("cluster_continue_job", Authority::OwnHost),
    ("cluster_stop_job", Authority::OwnHost),
    ("cluster_dismiss_job", Authority::OwnHost),
    ("cluster_open", Authority::OwnHost),
    ("cluster_close", Authority::OwnHost),
    ("cluster_move", Authority::OwnHost),
    ("cluster_queue_open", Authority::OwnHost),
    ("set_not_cluster", Authority::OwnHost),
    ("cluster_set_startup", Authority::OwnHost),
    ("cluster_forget_setup", Authority::OwnHost),
    ("cluster_set_agent_rules", Authority::OwnHost),
    ("cluster_set_login_serve", Authority::OwnHost),
    ("cluster_stop_login_daemon", Authority::OwnHost),
    ("cluster_open_terminal", Authority::OwnHost),
    ("open_window", Authority::OwnHost),
    ("navigate_home", Authority::AnyWindow),
    ("open_detached_window", Authority::AnyWindow),
    ("drag_track", Authority::AnyWindow),
    ("drag_drop", Authority::AnyWindow),
    ("drag_cancel", Authority::AnyWindow),
    ("adopt_ack", Authority::AnyWindow),
    ("adopt_tab", Authority::AnyWindow),
    ("list_scope_windows", Authority::AnyWindow),
    ("report_window_scope", Authority::AnyWindow),
    ("report_window_view", Authority::AnyWindow),
    ("report_unsaved", Authority::AnyWindow),
    ("reply_unsaved", Authority::AnyWindow),
    ("take_pending_focus", Authority::AnyWindow),
    ("notification_permission", Authority::AnyWindow),
    ("request_notification_permission", Authority::AnyWindow),
    ("open_notification_settings", Authority::AnyWindow),
    ("test_notification", Authority::AnyWindow),
    ("check_app_update", Authority::AnyWindow),
    ("app_update_status", Authority::AnyWindow),
    ("begin_update", Authority::LocalOnly),
    ("shell_build", Authority::AnyWindow),
    ("write_clipboard", Authority::AnyWindow),
    ("copy_file_to_clipboard", Authority::LocalOnly),
    ("reveal_in_file_manager", Authority::LocalOnly),
    ("open_external", Authority::AnyWindow),
    ("answer_askpass", Authority::AnyWindow),
    ("list_askpass", Authority::AnyWindow),
    ("cache_appearance", Authority::AnyWindow),
];
