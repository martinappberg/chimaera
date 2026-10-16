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
//! Every check's outcome is also kept (`status`), so a window opened after
//! the broadcast — or one asking "am I up to date?" — reads the answer
//! instead of waiting six hours for the next one.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
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

static INSTALL_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// One app update at a time: two clicks (or two windows) must not run two
/// downloads and two installs over the same bundle. Released on drop, so a
/// failed attempt can be retried.
pub(crate) struct InstallClaim(());

impl Drop for InstallClaim {
    fn drop(&mut self) {
        INSTALL_IN_FLIGHT.store(false, Ordering::SeqCst);
    }
}

pub(crate) fn claim_install() -> Option<InstallClaim> {
    INSTALL_IN_FLIGHT
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
        .then_some(InstallClaim(()))
}

/// The refusal for `n` windows holding unsaved edits, or None to go ahead.
/// Asked before the download and again right before the relaunch: Tauri's
/// restart cannot be held by the unsaved-edits guard, and the download can
/// take minutes of editing.
pub(crate) fn unsaved_refusal(n: usize, installed: bool) -> Option<String> {
    let who = match n {
        0 => return None,
        1 => "A window has unsaved edits".to_string(),
        n => format!("{n} windows have unsaved edits"),
    };
    Some(if installed {
        format!(
            "The update is installed. {who} — save or discard them, then quit and reopen chimaera."
        )
    } else {
        format!("{who} — save or discard them, then update.")
    })
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// What the shell last learned from the signed-update endpoint — the
/// native half of "is there an update?", beside the daemon's
/// `GET /api/v1/update`. A failed check keeps the last known answer and says
/// why it failed, so "couldn't check" never reads as "up to date".
#[derive(Clone, Default, Serialize)]
pub struct AppUpdateStatus {
    /// This app's own version.
    pub current: String,
    /// A dev build never checks: its "update" would swap the build under test.
    pub dev: bool,
    /// The most recent check attempt (unix seconds), successful or not.
    pub checked_at: Option<u64>,
    /// A newer signed version, when the last good answer had one.
    pub available: Option<String>,
    /// Why the most recent attempt failed; cleared by the next success.
    pub error: Option<String>,
    /// The periodic re-check cadence, so the UI can say "every 6 hours".
    pub interval_secs: u64,
}

struct Known {
    checked_at: Option<u64>,
    available: Option<String>,
    error: Option<String>,
}

static KNOWN: Mutex<Known> = Mutex::new(Known {
    checked_at: None,
    available: None,
    error: None,
});

fn known() -> std::sync::MutexGuard<'static, Known> {
    KNOWN.lock().unwrap_or_else(|e| e.into_inner())
}

/// The last check's outcome, without checking.
pub fn status(app: &AppHandle) -> AppUpdateStatus {
    let known = known();
    AppUpdateStatus {
        current: app.package_info().version.to_string(),
        dev: chimaera_core::is_dev_build(),
        checked_at: known.checked_at,
        available: known.available.clone(),
        error: known.error.clone(),
        interval_secs: CHECK_INTERVAL.as_secs(),
    }
}

/// One updater check, recorded. A dev build is answered without the network
/// (it never offers a release); an unreachable endpoint is recorded as the
/// failure it is and logged at debug — never raised on a timer.
pub async fn check(app: &AppHandle) -> AppUpdateStatus {
    if chimaera_core::is_dev_build() {
        return status(app);
    }
    let result = match app.updater() {
        Ok(updater) => updater.check().await.map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    {
        let mut known = known();
        known.checked_at = Some(unix_now());
        match result {
            Ok(update) => {
                known.available = update.map(|u| u.version);
                known.error = None;
            }
            Err(e) => {
                tracing::debug!("update check unavailable: {e}");
                // Bounded: this rides IPC into every asking window.
                known.error = Some(e.chars().take(200).collect());
            }
        }
    }
    status(app)
}

/// Broadcast `app-update` to every window whenever a newer signed build
/// exists. Windows decide presentation (the toast) and snoozing; the shell
/// only reports. A dev build never polls (see `check`).
pub fn spawn_update_watch(app: AppHandle) {
    if chimaera_core::is_dev_build() {
        return;
    }
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(INITIAL_DELAY).await;
        loop {
            if let Some(version) = check(&app).await.available {
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
    fn one_install_at_a_time_and_a_finished_one_frees_the_claim() {
        let first = claim_install().expect("nothing in flight");
        assert!(claim_install().is_none(), "a second click is refused");
        drop(first);
        assert!(claim_install().is_some(), "a failed attempt can retry");
    }

    #[test]
    fn unsaved_edits_block_the_restart_even_after_the_install() {
        assert_eq!(unsaved_refusal(0, false), None);
        assert_eq!(unsaved_refusal(0, true), None);
        assert!(unsaved_refusal(1, false)
            .unwrap()
            .starts_with("A window has unsaved edits"));
        let after = unsaved_refusal(2, true).unwrap();
        assert!(
            after.contains("installed") && after.contains("2 windows"),
            "{after}"
        );
    }

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
