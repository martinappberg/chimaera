#[path = "src/command_manifest.rs"]
mod command_manifest;

fn main() {
    // Registering the commands here generates `allow-*` permissions that the
    // runtime daemon-window capability grants to daemon-served remote URLs.
    // The same manifest feeds the runtime daemon capability, eliminating one
    // hand-maintained edge of the command/permission contract.
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(command_manifest::DAEMON_UI_COMMANDS),
    ))
    .expect("failed to run tauri-build");
}
