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
    io::Read,
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
    let pending = Pending {
        endpoint: endpoint.to_owned(),
        device: device.map(str::to_owned),
    };
    serde_json::to_vec(&pending)
        .map_err(std::io::Error::from)
        .and_then(|bytes| super::replace_small_file(&path(), &bytes))
        .is_ok()
}

/// `Some(device)` while a sign-out for this account service is unfinished.
pub(super) fn pending(endpoint: &str) -> Option<Option<String>> {
    read(&path())
        .filter(|pending| pending.endpoint == endpoint)
        .map(|pending| pending.device)
}

pub(super) fn clear() {
    let _ = std::fs::remove_file(path());
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
                    Err(_) => {}
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

fn read(path: &Path) -> Option<Pending> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(4097)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 4096 {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
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
        super::super::replace_small_file(&file, &serde_json::to_vec(&pending).unwrap()).unwrap();
        assert_eq!(read(&file), Some(pending));
        std::fs::write(&file, b"not json").unwrap();
        assert_eq!(read(&file), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
