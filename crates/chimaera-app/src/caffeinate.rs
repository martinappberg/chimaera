//! Device-local persistence and power assertion for Caffeinate.
//!
//! This intentionally does not use daemon settings: a window may be showing a
//! remote host, while Caffeinate always controls the Mac running the native
//! shell. The small JSON file follows the app's own isolated data home so dev
//! instances cannot arm or disarm the real app.

use std::io::ErrorKind;
use std::path::PathBuf;

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// Bump when the first-enable explanation changes materially enough that the
/// user should see and approve it again.
const CONSENT_VERSION: u32 = 1;

pub const CONSENT_REQUIRED: &str = "caffeinate needs first-time approval";

#[derive(Clone, Serialize)]
pub struct CaffeinateState {
    pub enabled: bool,
    pub consent_required: bool,
}

#[derive(Default, Deserialize, Serialize)]
struct Preferences {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    consent_version: u32,
}

/// The single held assertion plus its persisted user intent.
pub struct Caffeinate {
    path: PathBuf,
    prefs: Preferences,
    guard: Option<keepawake::KeepAwake>,
}

impl Caffeinate {
    pub fn load_default() -> Self {
        Self::load(chimaera_core::data_dir().join("caffeinate.json"))
    }

    fn load(path: PathBuf) -> Self {
        let prefs = match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str(&contents) {
                Ok(prefs) => prefs,
                Err(err) => {
                    tracing::warn!(path = %path.display(), %err,
                        "corrupt caffeinate.json; starting disabled");
                    Preferences::default()
                }
            },
            Err(err) if err.kind() == ErrorKind::NotFound => Preferences::default(),
            Err(err) => {
                tracing::warn!(path = %path.display(), %err,
                    "failed to read caffeinate.json; starting disabled");
                Preferences::default()
            }
        };
        Self {
            path,
            prefs,
            guard: None,
        }
    }

    pub fn state(&self) -> CaffeinateState {
        CaffeinateState {
            // Report the real assertion, never merely the persisted wish.
            enabled: self.guard.is_some(),
            consent_required: self.prefs.consent_version != CONSENT_VERSION,
        }
    }

    /// Re-arm a previously enabled mode on app launch. Failure leaves the
    /// persisted intent intact so a later launch or explicit click can retry,
    /// but the UI reports disabled because no assertion is actually held.
    pub fn restore(&mut self) -> Result<CaffeinateState, String> {
        if self.prefs.enabled && self.prefs.consent_version == CONSENT_VERSION {
            self.guard = Some(create_assertion()?);
        }
        Ok(self.state())
    }

    /// Apply an explicit user change. `acknowledge` is accepted only on the
    /// enabling edge, after the first-use explanation's confirm button.
    pub fn set(&mut self, enabled: bool, acknowledge: bool) -> Result<CaffeinateState, String> {
        if enabled && self.prefs.consent_version != CONSENT_VERSION && !acknowledge {
            return Err(CONSENT_REQUIRED.to_string());
        }
        if enabled == self.guard.is_some()
            && (!enabled || self.prefs.consent_version == CONSENT_VERSION)
        {
            return Ok(self.state());
        }

        if enabled {
            // Acquire first, persist second, publish last: a write failure must
            // not leave a live assertion the UI was told failed to enable.
            let guard = create_assertion()?;
            let previous = Preferences {
                enabled: self.prefs.enabled,
                consent_version: self.prefs.consent_version,
            };
            self.prefs.enabled = true;
            if acknowledge {
                self.prefs.consent_version = CONSENT_VERSION;
            }
            if let Err(err) = self.save() {
                self.prefs = previous;
                return Err(format!("could not save Caffeinate: {err:#}"));
            }
            self.guard = Some(guard);
        } else {
            let previous = self.prefs.enabled;
            self.prefs.enabled = false;
            if let Err(err) = self.save() {
                self.prefs.enabled = previous;
                return Err(format!("could not save Caffeinate: {err:#}"));
            }
            self.guard = None;
        }
        Ok(self.state())
    }

    fn save(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&self.prefs)?)
            .with_context(|| format!("failed to write {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("failed to rename into {}", self.path.display()))?;
        Ok(())
    }
}

fn create_assertion() -> Result<keepawake::KeepAwake, String> {
    keepawake::Builder::default()
        // The screen may dim, turn off, and lock normally. Caffeinate keeps
        // the work alive; it is not a presentation/display-lock override.
        .display(false)
        .idle(true)
        .sleep(true)
        .app_name("Chimaera")
        .app_reverse_domain("com.chimaera.app")
        .reason("Caffeinate")
        .create()
        .map_err(|e| format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_default_and_round_trip() {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-caffeinate-test-{}-{}",
            std::process::id(),
            chimaera_core::generate_token()
        ));
        let path = dir.join("caffeinate.json");
        let mut mode = Caffeinate::load(path.clone());
        assert!(!mode.state().enabled);
        assert!(mode.state().consent_required);

        mode.prefs.enabled = true;
        mode.prefs.consent_version = CONSENT_VERSION;
        mode.save().unwrap();

        let loaded = Caffeinate::load(path);
        assert!(loaded.prefs.enabled);
        assert!(!loaded.state().consent_required);
        // Loading preferences alone never lies that the assertion is held.
        assert!(!loaded.state().enabled);

        std::fs::remove_dir_all(dir).ok();
    }
}
