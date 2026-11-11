use std::path::Path;

pub fn run() -> anyhow::Result<()> {
    check_dir_writable("data dir", &chimaera_core::data_dir());
    check_dir_writable("runtime dir", &chimaera_core::runtime_dir());
    check_in_path("ssh");
    check_in_path("claude");
    Ok(())
}

fn check_dir_writable(label: &str, dir: &Path) {
    let probe = dir.join(".doctor-probe");
    match std::fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            println!("ok   {label} writable ({})", dir.display());
        }
        Err(e) => println!("warn {label} not writable ({}): {e}", dir.display()),
    }
}

fn check_in_path(bin: &str) {
    if chimaera_core::paths::find_on_path(bin, None).is_some() {
        println!("ok   {bin} found in PATH");
    } else {
        println!("warn {bin} not found in PATH");
    }
}
