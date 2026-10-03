//! A sign-out that could neither delete the saved sign-in (the credential
//! store refused) nor revoke it (the account was unreachable) still signs
//! this computer out at once; this finishes the rest on its own. A small
//! marker keeps that saved sign-in from being restored, including after a
//! restart. Revocation is retried while the account is unreachable; deletion
//! is tried once per revocation and once per launch, never in a loop, since
//! a locked credential store may answer with an OS prompt.
use super::{lock, Shell};
use chimaera_link::{Client, Tokens};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use tauri::{AppHandle, Manager};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Pending {
    endpoint: String,
    device: Option<String>,
}

fn path() -> PathBuf {
    chimaera_core::config_dir().join("pro-sign-out.json")
}

/// Remembers the unfinished sign-out. False when even that failed.
pub(super) fn record(endpoint: &str, device: Option<&str>) -> bool {
    record_at(&path(), endpoint, device).is_ok()
}

fn record_at(path: &Path, endpoint: &str, device: Option<&str>) -> std::io::Result<()> {
    let pending = Pending {
        endpoint: endpoint.to_owned(),
        device: device.map(str::to_owned),
    };
    let bytes = serde_json::to_vec(&pending)?;
    let tmp = path.with_extension(format!("{}.tmp", chimaera_core::generate_token()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(&bytes)?;
        // This file suppresses a still-valid saved session on restart. Both
        // its contents and the renamed directory entry must survive a crash.
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)?;
        sync_directory(path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    result
}

/// Never promise automatic completion when neither credential store nor the
/// restart marker accepted the sign-out. Memory is already signed out either way.
pub(super) fn completion(
    saved_removed: bool,
    revoked: bool,
    marker_saved: bool,
) -> Result<(), &'static str> {
    if saved_removed || revoked {
        Ok(())
    } else if marker_saved {
        Err(super::code::SIGN_OUT_PENDING)
    } else {
        Err(super::code::SIGN_OUT_UNPERSISTED)
    }
}

/// `Some(device)` while a sign-out for this account service is unfinished.
pub(super) fn pending(endpoint: &str) -> Option<Option<String>> {
    pending_at(&path(), endpoint)
}

fn pending_at(path: &Path, endpoint: &str) -> Option<Option<String>> {
    match read(path) {
        Ok(Some(pending)) if pending.endpoint == endpoint => Some(pending.device),
        Ok(_) => None,
        // Only a genuinely missing marker permits automatic restoration. A
        // damaged/unreadable one cannot identify a device to revoke, but still
        // suppresses restoring the saved pair until deletion or a new sign-in.
        Err(_) => Some(None),
    }
}

pub(super) fn clear() {
    let _ = clear_at(&path());
}

fn clear_at(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => sync_directory(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn sync_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    std::fs::File::open(
        path.parent()
            .ok_or_else(|| std::io::Error::other("missing marker directory"))?,
    )?
    .sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Revokes the signed-out device once the account answers (30 s, doubling
/// to ten minutes), then deletes the saved pair. Stops as soon as a newer
/// sign-in or sign-out changes `generation`: that one owns the saved pair.
pub(super) fn finish(
    app: AppHandle,
    endpoint: String,
    device: Option<String>,
    tokens: Option<Tokens>,
    generation: u64,
) {
    tauri::async_runtime::spawn(async move {
        let client = tokens.and_then(|tokens| Client::new(&endpoint, Some(tokens)).ok());
        if let (Some(client), Some(device)) = (client, device) {
            let mut pause = Duration::from_secs(30);
            loop {
                if app.state::<Shell>().pro.generation() != generation {
                    return;
                }
                match client.revoke_device(&device).await {
                    Ok(()) => break,
                    Err(error) if error.is::<chimaera_link::AuthorizationRevoked>() => break,
                    Err(_) => {
                        let state = app.state::<Shell>();
                        let _operation = state.pro.operation.lock().await;
                        if state.pro.generation() != generation || lock(&state.pro.client).is_some()
                        {
                            return;
                        }
                        // Disk space may have become available while offline.
                        // Never rewrite an old marker after a newer sign-in.
                        record(&endpoint, Some(&device));
                    }
                }
                tokio::time::sleep(pause).await;
                pause = (pause * 2).min(Duration::from_secs(600));
            }
        }
        let state = app.state::<Shell>();
        let _operation = state.pro.operation.lock().await;
        if state.pro.generation() != generation || lock(&state.pro.client).is_some() {
            return;
        }
        let deleted = matches!(
            tokio::task::spawn_blocking(move || super::save_tokens(&endpoint, None)).await,
            Ok(Ok(()))
        );
        if deleted {
            clear();
        }
    });
}

fn read(path: &Path) -> std::io::Result<Option<Pending>> {
    let mut bytes = Vec::new();
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A dangling symlink is an existing unreadable marker too.
            return match std::fs::symlink_metadata(path) {
                Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => Ok(None),
                _ => Err(error),
            };
        }
        Err(error) => return Err(error),
    };
    file.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(std::io::Error::other("sign-out marker exceeds limit"));
    }
    Ok(Some(serde_json::from_slice(&bytes)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marker_names_its_account_service_and_device() {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-sign-out-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("pro-sign-out.json");
        let pending = Pending {
            endpoint: "https://account.example.invalid".into(),
            device: Some("device-1".into()),
        };
        record_at(&file, &pending.endpoint, pending.device.as_deref()).unwrap();
        assert_eq!(read(&file).unwrap(), Some(pending.clone()));
        clear_at(&file).unwrap();
        clear_at(&file).unwrap();
        assert_eq!(read(&file).unwrap(), None);
        record_at(&file, &pending.endpoint, pending.device.as_deref()).unwrap();
        std::fs::write(&file, b"not json").unwrap();
        assert!(read(&file).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn failed_marker_write_cannot_claim_restart_safe_signout() {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-sign-out-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // A directory at the destination makes rename fail even as root.
        let file = dir.join("pro-sign-out.json");
        std::fs::create_dir(&file).unwrap();
        let saved = record_at(&file, "https://account.example.invalid", Some("device-1")).is_ok();
        assert!(!saved);
        assert!(read(&file).is_err());
        assert_eq!(
            completion(false, false, saved),
            Err(super::super::code::SIGN_OUT_UNPERSISTED)
        );
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "failed write left a temporary marker"
        );
        assert_eq!(
            completion(false, false, true),
            Err(super::super::code::SIGN_OUT_PENDING)
        );
        assert_eq!(completion(true, false, false), Ok(()));
        assert_eq!(completion(false, true, false), Ok(()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn only_missing_or_verified_other_account_marker_permits_restoration() {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-sign-out-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("pro-sign-out.json");
        let endpoint = "https://account.example.invalid";
        assert_eq!(pending_at(&file, endpoint), None);
        record_at(&file, endpoint, Some("device-1")).unwrap();
        assert_eq!(pending_at(&file, endpoint), Some(Some("device-1".into())));
        assert_eq!(pending_at(&file, "https://another.example.invalid"), None);
        for bytes in [b"not json".as_slice(), &[b'x'; 4097]] {
            std::fs::write(&file, bytes).unwrap();
            assert_eq!(pending_at(&file, endpoint), Some(None));
        }
        clear_at(&file).unwrap();
        // Reading a directory fails on supported hosts without permissions or
        // process privilege assumptions, exercising the unreadable path.
        std::fs::create_dir(&file).unwrap();
        assert_eq!(pending_at(&file, endpoint), Some(None));
        #[cfg(unix)]
        {
            std::fs::remove_dir(&file).unwrap();
            std::os::unix::fs::symlink(dir.join("missing-target"), &file).unwrap();
            assert_eq!(pending_at(&file, endpoint), Some(None));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
