fn main() {
    // Registering the commands here generates `allow-*` permissions that the
    // daemon-ui capability grants to the daemon-served (remote-url) windows.
    // Keep this list in lockstep with `generate_handler!` in shell.rs AND with
    // the grants in capabilities/daemon-ui.json — a command missing here has no
    // permission to generate, so the capability can't grant it and the webview's
    // invoke fails with "not allowed by ACL".
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
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
            "open_window",
            "report_window_scope",
            "check_app_update",
            "begin_update",
            "shell_build",
            "write_clipboard",
            "answer_askpass",
            "list_askpass",
            "wsl_status",
            "wsl_install",
            "wsl_update",
            "wsl_install_distro",
            "wsl_setup_daemon",
        ]),
    ))
    .expect("failed to run tauri-build");
}
