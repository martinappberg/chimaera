#[path = "src/command_manifest.rs"]
mod command_manifest;

fn main() {
    // Registering the commands here generates `allow-*` permissions that the
    // runtime daemon-window capability grants to daemon-served remote URLs.
    // The same manifest feeds the runtime daemon capability, eliminating one
    // hand-maintained edge of the command/permission contract.
    // AppManifest keeps a 'static command slice for code generation. The
    // build-script process is one-shot, so leaking this tiny combined slice is
    // the cleanest way to retain the two meaningful source groups above.
    let commands: &'static [&'static str] = Box::leak(
        command_manifest::DAEMON_UI_COMMANDS
            .iter()
            .chain(command_manifest::WSL_SETUP_COMMANDS)
            .copied()
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    );
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(commands)),
    )
    .expect("failed to run tauri-build");
}
