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
    "remote_compute_sessions",
    "launch_compute_session",
    "cancel_compute_session",
    "connect_compute_session",
    "open_window",
    "report_window_scope",
    "check_app_update",
    "begin_update",
    "shell_build",
    "write_clipboard",
    "set_caffeinate",
    "caffeinate_state",
    "answer_askpass",
    "list_askpass",
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
