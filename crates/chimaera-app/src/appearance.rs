//! Port-independent first-paint appearance cache for native webviews.
//!
//! Daemon pages use volatile loopback ports, so browser `localStorage` alone
//! cannot carry a theme through a daemon restart or tunnel re-home. The shell
//! keeps one bounded appearance snapshot per authoritative window host and
//! adds it to the next daemon URL before the document is parsed.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::PathBuf;

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

const MAX_CACHE_BYTES: u64 = 64 * 1024;
const MAX_ENTRIES: usize = 256;
const MAX_THEME_ID_BYTES: usize = 96;
const MAX_COLOR_BYTES: usize = 192;
const LOCAL_KEY: &str = "local";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppearanceMode {
    Light,
    Dark,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppearanceBootstrap {
    mode: AppearanceMode,
    theme_id: String,
    background: String,
    accent: Option<String>,
}

impl AppearanceBootstrap {
    pub fn validate(&self) -> anyhow::Result<()> {
        validate_text("theme id", &self.theme_id, MAX_THEME_ID_BYTES)?;
        validate_text("background", &self.background, MAX_COLOR_BYTES)?;
        if let Some(accent) = &self.accent {
            validate_text("accent", accent, MAX_COLOR_BYTES)?;
        }
        Ok(())
    }

    pub fn json(&self) -> String {
        serde_json::to_string(self).expect("appearance bootstrap is always serializable")
    }
}

fn validate_text(name: &str, value: &str, max_bytes: usize) -> anyhow::Result<()> {
    if value.is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        bail!("invalid {name}");
    }
    Ok(())
}

/// Small atomically persisted cache, keyed by the shell-owned host scope.
pub struct AppearanceCache {
    path: PathBuf,
    entries: BTreeMap<String, AppearanceBootstrap>,
}

impl AppearanceCache {
    pub fn load_default() -> Self {
        Self::load(chimaera_core::data_dir().join("appearance-bootstrap.json"))
    }

    fn load(path: PathBuf) -> Self {
        let entries = match std::fs::metadata(&path) {
            Ok(metadata) if metadata.len() > MAX_CACHE_BYTES => {
                tracing::warn!(
                    path = %path.display(),
                    "appearance cache exceeds its size cap; ignoring it"
                );
                BTreeMap::new()
            }
            Ok(_) => std::fs::read_to_string(&path)
                .ok()
                .and_then(|contents| {
                    serde_json::from_str::<BTreeMap<String, AppearanceBootstrap>>(&contents).ok()
                })
                .filter(|entries| {
                    entries.len() <= MAX_ENTRIES
                        && entries
                            .values()
                            .all(|appearance| appearance.validate().is_ok())
                })
                .unwrap_or_else(|| {
                    tracing::warn!(
                        path = %path.display(),
                        "invalid appearance cache; ignoring it"
                    );
                    BTreeMap::new()
                }),
            Err(error) if error.kind() == ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "failed to read appearance cache; ignoring it"
                );
                BTreeMap::new()
            }
        };
        Self { path, entries }
    }

    pub fn get(&self, alias: Option<&str>) -> Option<AppearanceBootstrap> {
        self.entries.get(&cache_key(alias)).cloned()
    }

    pub fn set(
        &mut self,
        alias: Option<&str>,
        appearance: AppearanceBootstrap,
    ) -> anyhow::Result<()> {
        appearance.validate()?;
        let key = cache_key(alias);
        if self.entries.get(&key) == Some(&appearance) {
            return Ok(());
        }
        if !self.entries.contains_key(&key) && self.entries.len() >= MAX_ENTRIES {
            bail!("appearance cache is full");
        }

        let mut next = self.entries.clone();
        next.insert(key, appearance);
        self.save(&next)?;
        self.entries = next;
        Ok(())
    }

    fn save(&self, entries: &BTreeMap<String, AppearanceBootstrap>) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(entries)?)
            .with_context(|| format!("failed to write {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("failed to rename into {}", self.path.display()))?;
        Ok(())
    }
}

fn cache_key(alias: Option<&str>) -> String {
    alias.map_or_else(|| LOCAL_KEY.to_string(), |alias| format!("host:{alias}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dark() -> AppearanceBootstrap {
        AppearanceBootstrap {
            mode: AppearanceMode::Dark,
            theme_id: "chimaera-dark".to_string(),
            background: "#17171c".to_string(),
            accent: Some("#ff00ff".to_string()),
        }
    }

    #[test]
    fn cache_round_trips_per_host_and_rejects_oversized_values() {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-appearance-test-{}-{}",
            std::process::id(),
            chimaera_core::generate_token()
        ));
        let path = dir.join("appearance.json");
        let mut cache = AppearanceCache::load(path.clone());

        cache.set(None, dark()).unwrap();
        let mut remote = dark();
        remote.background = "#000000".to_string();
        cache.set(Some("cluster"), remote.clone()).unwrap();

        let loaded = AppearanceCache::load(path);
        assert_eq!(loaded.get(None), Some(dark()));
        assert_eq!(loaded.get(Some("cluster")), Some(remote));

        let mut invalid = dark();
        invalid.background = "x".repeat(MAX_COLOR_BYTES + 1);
        assert!(invalid.validate().is_err());
        std::fs::remove_dir_all(dir).ok();
    }
}
