//! Whether an agent was connected in the cloud at the last provider catalog
//! read, and which ones, so the Pro page can say so while the cloud is asleep
//! and quitting offers "Continue in the cloud" only for agents that could
//! continue there. Only catalog reads the page already makes update it:
//! nothing wakes the cloud to find out. Kept per account in a small file so an
//! app restart does not forget it. A hint only, never permission to move or
//! resume work: only the user's own choice moves anything.
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Fact {
    account: String,
    connected: bool,
    /// The agent providers (`claude`, `codex`) signed in at that read. A
    /// file written before this field existed names none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    signed_in: Vec<String>,
}

#[derive(Default)]
pub(in crate::shell) struct Agents {
    /// The outer `None` means the file has not been read yet.
    memory: Mutex<Option<Option<Fact>>>,
}

fn path() -> PathBuf {
    chimaera_core::config_dir().join("pro-agents.json")
}

impl Agents {
    /// The last fact for this account; `None` when unknown.
    pub(in crate::shell) fn get(&self, account: &str) -> Option<bool> {
        self.fact(account).map(|fact| fact.connected)
    }

    /// The agent providers signed in at the last catalog read for this
    /// account; none when unknown.
    pub(in crate::shell) fn signed_in(&self, account: &str) -> Vec<String> {
        self.fact(account)
            .map(|fact| fact.signed_in)
            .unwrap_or_default()
    }

    fn fact(&self, account: &str) -> Option<Fact> {
        let mut memory = super::lock(&self.memory);
        memory
            .get_or_insert_with(|| read(&path()))
            .as_ref()
            .filter(|fact| fact.account == account)
            .cloned()
    }

    /// Records what the latest catalog said. Not confirmed either way
    /// forgets the old fact rather than keeping a stale one.
    pub(in crate::shell) fn record(&self, account: &str, catalog: &serde_json::Value) {
        let next = from_catalog(catalog).map(|connected| Fact {
            account: account.to_owned(),
            connected,
            signed_in: signed_in_agents(catalog),
        });
        {
            let mut memory = super::lock(&self.memory);
            if memory.as_ref() == Some(&next) {
                return;
            }
            *memory = Some(next.clone());
        }
        // Written only when the fact changes: a few bytes, rarely.
        let _ = write(&path(), next.as_ref());
    }
}

/// What a provider catalog says about agent connections: one signed-in agent
/// is enough; every agent row definitely not signed in means none; anything
/// else (an incomplete catalog, an agent whose sign-in could not be checked)
/// is unknown.
pub(in crate::shell) fn from_catalog(value: &serde_json::Value) -> Option<bool> {
    if value["available"] != true {
        return None;
    }
    let states: Vec<&str> = value["providers"]
        .as_array()?
        .iter()
        .filter(|provider| provider["category"] == "agent")
        .map(|provider| provider["state"].as_str().unwrap_or("unknown"))
        .collect();
    if states.contains(&"signed_in") {
        return Some(true);
    }
    states
        .iter()
        .all(|state| matches!(*state, "missing" | "needs_sign_in" | "unavailable"))
        .then_some(false)
}

/// The agent providers a catalog shows signed in (bounded, each named once).
fn signed_in_agents(value: &serde_json::Value) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for provider in value["providers"].as_array().into_iter().flatten() {
        let id = provider["id"].as_str().unwrap_or_default();
        if provider["category"] == "agent"
            && provider["state"] == "signed_in"
            && !id.is_empty()
            && id.len() <= 32
            && !ids.iter().any(|known| known == id)
            && ids.len() < 16
        {
            ids.push(id.to_owned());
        }
    }
    ids
}

fn read(path: &Path) -> Option<Fact> {
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

fn write(path: &Path, fact: Option<&Fact>) -> std::io::Result<()> {
    let Some(fact) = fact else {
        return match std::fs::remove_file(path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
    };
    super::replace_small_file(path, &serde_json::to_vec(fact)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog(states: &[(&str, &str)]) -> serde_json::Value {
        json!({"available": true, "providers": states.iter().map(|(category, state)| json!({"id": "x", "category": category, "state": state})).collect::<Vec<_>>()})
    }

    #[test]
    fn one_signed_in_agent_is_enough_and_only_definite_rows_mean_none() {
        assert_eq!(
            from_catalog(&catalog(&[
                ("agent", "needs_sign_in"),
                ("agent", "signed_in")
            ])),
            Some(true)
        );
        assert_eq!(
            from_catalog(&catalog(&[
                ("agent", "needs_sign_in"),
                ("agent", "missing"),
                ("repository", "signed_in")
            ])),
            Some(false)
        );
        assert_eq!(
            from_catalog(&catalog(&[
                ("agent", "needs_sign_in"),
                ("agent", "unknown")
            ])),
            None
        );
        assert_eq!(from_catalog(&json!({"available": false})), None);
        assert_eq!(from_catalog(&json!({"available": true})), None);
    }

    #[test]
    fn remembers_which_agent_providers_are_signed_in() {
        let catalog = json!({"available": true, "providers": [
            {"id": "claude", "category": "agent", "state": "signed_in"},
            {"id": "codex", "category": "agent", "state": "needs_sign_in"},
            {"id": "github", "category": "repository", "state": "signed_in"},
            {"id": "claude", "category": "agent", "state": "signed_in"},
        ]});
        assert_eq!(signed_in_agents(&catalog), vec!["claude".to_string()]);
        assert!(signed_in_agents(&json!({"available": true})).is_empty());
    }

    #[test]
    fn a_fact_belongs_to_one_account_and_survives_a_reread() {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-agents-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("pro-agents.json");
        let fact = Fact {
            account: "a".into(),
            connected: true,
            signed_in: vec!["claude".into()],
        };
        write(&file, Some(&fact)).unwrap();
        assert_eq!(read(&file), Some(fact));
        // A file from before providers were remembered names none.
        std::fs::write(&file, br#"{"account":"a","connected":true}"#).unwrap();
        assert_eq!(read(&file).map(|fact| fact.signed_in), Some(Vec::new()));
        write(&file, None).unwrap();
        assert_eq!(read(&file), None);
        write(&file, None).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
