//! The app half of the one-click update chain.
//!
//! An update is two swaps that must happen in order: the signed app bundle
//! (tauri-plugin-updater, restarts the process) and then the local daemon
//! (respawned from the NEW bundle's executable — the daemon binary IS the
//! app binary, so replacing it first is impossible). The click and the
//! restart are different processes, so consent is carried across by an
//! intent file: `begin_update` writes it, the next launch consumes it and —
//! only then — replaces a busy daemon without a second ask. The daemon's own
//! restart handoff + session ledger make that replacement state-safe.
//!
//! Periodic awareness lives here too: a slow loop re-checks the updater
//! endpoint and broadcasts `app-update` to every window, so the toast shows
//! up wherever you are working, not just on a freshly opened home screen.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::UpdaterExt;

/// Re-check cadence, matching the daemon's own release checker.
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// First check waits out startup (daemon ensure, window restore).
const INITIAL_DELAY: Duration = Duration::from_secs(20);

/// An update intent older than this is orphaned (the install failed after
/// the write, or the restart never happened) and is discarded unacted.
const INTENT_MAX_AGE_SECS: u64 = 10 * 60;

#[derive(Serialize, Deserialize)]
struct UpdateIntent {
    written_at: u64,
}

fn intent_path() -> PathBuf {
    chimaera_core::data_dir().join("update-intent.json")
}

/// Record that the user asked for the full update chain (called by
/// `begin_update` right before the app installs + restarts).
pub fn write_intent() -> anyhow::Result<()> {
    let intent = UpdateIntent {
        written_at: unix_now(),
    };
    let path = intent_path();
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&intent)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Discard a written intent (the install failed; the chain will not run).
pub fn clear_intent() {
    std::fs::remove_file(intent_path()).ok();
}

/// Whether a fresh update intent is pending — consumed either way, so an
/// intent can never act twice (or linger and fire weeks later).
pub fn consume_intent() -> bool {
    let path = intent_path();
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return false;
    };
    std::fs::remove_file(&path).ok();
    let Ok(intent) = serde_json::from_str::<UpdateIntent>(&contents) else {
        return false;
    };
    unix_now().saturating_sub(intent.written_at) <= INTENT_MAX_AGE_SECS
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One updater check. `None` = up to date, or the endpoint is unreachable —
/// a missing release must never surface as an error on a timer.
pub async fn poll_app_update(app: &AppHandle) -> Option<String> {
    let updater = app.updater().ok()?;
    match updater.check().await {
        Ok(Some(update)) => Some(update.version),
        Ok(None) => None,
        Err(e) => {
            tracing::debug!("update check unavailable: {e}");
            None
        }
    }
}

/// Broadcast `app-update` to every window whenever a newer signed build
/// exists. Windows decide presentation (the toast) and snoozing; the shell
/// only reports.
pub fn spawn_update_watch(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(INITIAL_DELAY).await;
        loop {
            if let Some(version) = poll_app_update(&app).await {
                let _ = app.emit("app-update", version);
            }
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intent_consumes_once_and_expires() {
        // Point the data dir at a private HOME for this test only.
        let dir = std::env::temp_dir().join(format!("chimaera-intent-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("CHIMAERA_HOME", &dir);

        assert!(!consume_intent(), "no intent yet");
        write_intent().unwrap();
        assert!(consume_intent(), "fresh intent acts");
        assert!(!consume_intent(), "consume-once");

        let stale = UpdateIntent { written_at: 1_000 };
        std::fs::write(intent_path(), serde_json::to_vec(&stale).unwrap()).unwrap();
        assert!(!consume_intent(), "stale intent discarded");
        assert!(!intent_path().exists());

        write_intent().unwrap();
        clear_intent();
        assert!(!consume_intent(), "cleared intent never acts");

        std::env::remove_var("CHIMAERA_HOME");
        std::fs::remove_dir_all(&dir).ok();
    }
}
