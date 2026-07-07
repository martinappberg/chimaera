fn main() {
    // Registering the commands here generates `allow-*` permissions that the
    // daemon-ui capability grants to the daemon-served (remote-url) windows.
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "list_hosts",
            "add_host",
            "remove_host",
            "connect_host",
            "disconnect_host",
            "remote_workspaces",
            "open_window",
        ]),
    ))
    .expect("failed to run tauri-build");
}
