//! Packs `web/` into the tar `web_bundle()` returns (`OUT_DIR/web.tar`):
//! plain ustar, folders and files only, sorted, with fixed times and modes,
//! so the bundle — and the component's sha256 — is the same on every build.
//! No dependency: a ustar header is 512 bytes of fields.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

fn octal(field: &mut [u8], value: u64) {
    let digits = field.len() - 1;
    let text = format!("{value:0digits$o}");
    field[..digits].copy_from_slice(text.as_bytes());
    field[digits] = 0;
}

fn header(name: &str, size: u64, dir: bool) -> [u8; 512] {
    assert!(name.len() < 100, "{name}: a bundle path is under 100 bytes");
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    octal(&mut h[100..108], if dir { 0o755 } else { 0o644 });
    octal(&mut h[108..116], 0);
    octal(&mut h[116..124], 0);
    octal(&mut h[124..136], size);
    octal(&mut h[136..148], 0);
    h[156] = if dir { b'5' } else { b'0' };
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
    let text = format!("{sum:06o}\0 ");
    h[148..156].copy_from_slice(text.as_bytes());
    h
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Option<PathBuf>)>) {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .expect("web/ is readable")
        .map(|e| e.expect("a web/ entry").path())
        .collect();
    entries.sort();
    for path in entries {
        let rel = path
            .strip_prefix(root)
            .expect("under web/")
            .to_string_lossy()
            .replace('\\', "/");
        if path.is_dir() {
            out.push((format!("{rel}/"), None));
            walk(root, &path, out);
        } else {
            out.push((rel, Some(path)));
        }
    }
}

fn main() {
    let root = PathBuf::from("web");
    println!("cargo:rerun-if-changed=web");
    let mut entries = Vec::new();
    walk(&root, &root, &mut entries);
    let mut tar = Vec::new();
    for (name, file) in entries {
        match file {
            None => tar.extend_from_slice(&header(&name, 0, true)),
            Some(path) => {
                let bytes = fs::read(&path).expect("a web/ file is readable");
                tar.extend_from_slice(&header(&name, bytes.len() as u64, false));
                tar.extend_from_slice(&bytes);
                tar.resize(tar.len().div_ceil(512) * 512, 0);
            }
        }
    }
    tar.extend_from_slice(&[0u8; 1024]);
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("web.tar");
    fs::File::create(out)
        .and_then(|mut f| f.write_all(&tar))
        .expect("OUT_DIR is writable");
}
