use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=CHIMAERA_UI_DIST");
    let directory = match env::var_os("CHIMAERA_UI_DIST") {
        Some(value) => {
            let path = PathBuf::from(value);
            assert!(path.is_absolute(), "CHIMAERA_UI_DIST must be absolute");
            path
        }
        None => PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../../web-ui/dist"),
    };
    // Track the supplied alias as well as its resolved target: retargeting an
    // assembly symlink must not keep embedding its predecessor's assets.
    println!("cargo:rerun-if-changed={}", directory.display());
    let directory = directory
        .canonicalize()
        .expect("build the web UI before compiling chimaera-server");
    assert!(
        directory.join("index.html").is_file(),
        "UI index.html missing"
    );
    let directory = directory.to_str().expect("UI path must be UTF-8");
    println!("cargo:rerun-if-changed={directory}");
    // This is a build input, never a runtime path or downloadable application.
    // A separately assembled client can embed its captured UI without editing
    // the public checkout or Cargo's immutable Git dependency cache.
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("ui_assets.rs"),
        format!("#[derive(RustEmbed)]\n#[folder = {directory:?}]\nstruct Assets;\n"),
    )
    .expect("write embedded UI declaration");
}
