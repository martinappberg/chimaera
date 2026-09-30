//! What the app remembers about an account's cloud, so the Pro page can
//! answer at once while the cloud sleeps and quitting offers "Continue in the
//! cloud" only for agents that could continue there: whether an agent was
//! connected at the last provider catalog read and which ones, that read's
//! provider rows (shown until a live read answers), and whether this
//! account's cloud has ever been ready (so a later `preparing`, a service
//! update say, never reads as first-time setup). Only reads the page already
//! makes update it: nothing wakes the cloud to find out. Kept per account in
//! a small file so an app restart does not forget it. A hint only, never
//! permission to move or resume work: only the user's own choice moves
//! anything.
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Fact {
    account: String,
    /// Whether an agent was connected at the last catalog read; absent when
    /// that read could not tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    connected: Option<bool>,
    /// The agent providers (`claude`, `codex`) signed in at that read. A
    /// file written before this field existed names none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    signed_in: Vec<String>,
    /// That read's provider rows, bounded (`rows`). Additive: an older file
    /// has none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    providers: Vec<Row>,
    /// This account's cloud has been ready at least once. Additive: an older
    /// file has only a catalog fact, which implies it (`ready_once`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    ready_once: bool,
}

impl Fact {
    fn for_account(account: &str) -> Self {
        Self {
            account: account.to_owned(),
            ..Self::default()
        }
    }

    /// A catalog read needs a ready cloud, so a remembered one implies it.
    fn ready_once(&self) -> bool {
        self.ready_once || self.connected.is_some() || !self.providers.is_empty()
    }

    fn is_empty(&self) -> bool {
        self.connected.is_none()
            && self.signed_in.is_empty()
            && self.providers.is_empty()
            && !self.ready_once
    }
}

/// One provider row as the last catalog read showed it, in the catalog's own
/// shape (`GET /api/v1/pro/cloud/providers`), so the page renders it exactly
/// like a live row until a live read replaces it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(in crate::shell) struct Row {
    id: String,
    label: String,
    category: String,
    state: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    methods: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    disconnect_supported: Option<bool>,
}

/// What `pro_cloud_status` reports from memory for one account.
#[derive(Debug, Default, PartialEq)]
pub(in crate::shell) struct Remembered {
    pub agents_connected: Option<bool>,
    pub ready_once: bool,
    pub providers: Vec<Row>,
}

/// One account at a time: another account's first read or readiness
/// replaces the file, so switching accounts starts that one afresh.
#[derive(Default)]
pub(in crate::shell) struct Agents {
    /// The outer `None` means the file has not been read yet.
    memory: Mutex<Option<Option<Fact>>>,
    /// Tests keep their own file; the app uses `pro-agents.json`.
    file: Option<PathBuf>,
}

fn path() -> PathBuf {
    chimaera_core::config_dir().join("pro-agents.json")
}

impl Agents {
    #[cfg(test)]
    fn at(file: PathBuf) -> Self {
        Self {
            memory: Mutex::default(),
            file: Some(file),
        }
    }

    fn file(&self) -> PathBuf {
        self.file.clone().unwrap_or_else(path)
    }

    /// Everything remembered for this account (nothing for another one).
    pub(in crate::shell) fn remembered(&self, account: &str) -> Remembered {
        self.fact(account)
            .map(|fact| Remembered {
                agents_connected: fact.connected,
                ready_once: fact.ready_once(),
                providers: fact.providers,
            })
            .unwrap_or_default()
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
            .get_or_insert_with(|| read(&self.file()))
            .as_ref()
            .filter(|fact| fact.account == account)
            .cloned()
    }

    /// Records that this account's cloud is set up (the account said it is
    /// ready or asleep, or it has a registered cloud daemon). Written once.
    pub(in crate::shell) fn mark_ready(&self, account: &str) {
        self.update(account, |fact| fact.ready_once = true);
    }

    /// Records what the latest catalog said. Not confirmed either way
    /// forgets the old agent fact rather than keeping a stale one; the rows
    /// are always the latest read's.
    pub(in crate::shell) fn record(&self, account: &str, catalog: &serde_json::Value) {
        self.update(account, |fact| {
            let connected = from_catalog(catalog);
            fact.signed_in = if connected.is_some() {
                signed_in_agents(catalog)
            } else {
                Vec::new()
            };
            fact.connected = connected;
            fact.providers = rows(catalog);
            fact.ready_once |= catalog["available"] == true;
        });
    }

    /// Applies `change` to this account's fact (a fresh one for a new
    /// account) and writes the file only when something changed: a few
    /// bytes, rarely.
    fn update(&self, account: &str, change: impl FnOnce(&mut Fact)) {
        let next = {
            let mut memory = super::lock(&self.memory);
            let current = memory.get_or_insert_with(|| read(&self.file()));
            let mut next = current
                .clone()
                .filter(|fact| fact.account == account)
                .unwrap_or_else(|| Fact::for_account(account));
            // An older file's implied readiness is kept explicitly.
            next.ready_once = next.ready_once();
            change(&mut next);
            let next = (!next.is_empty()).then_some(next);
            if *current == next {
                return;
            }
            *current = next.clone();
            next
        };
        let _ = write(&self.file(), next.as_ref());
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

/// A catalog's provider rows, for showing until a live read answers:
/// well-formed rows only, each id once, bounded like the file. None when the
/// catalog is not available.
fn rows(value: &serde_json::Value) -> Vec<Row> {
    if value["available"] != true {
        return Vec::new();
    }
    let token = |text: &str, max: usize| {
        !text.is_empty()
            && text.len() <= max
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    };
    let mut rows: Vec<Row> = Vec::new();
    for provider in value["providers"].as_array().into_iter().flatten() {
        let id = provider["id"].as_str().unwrap_or_default();
        let label = provider["label"].as_str().unwrap_or_default().trim();
        let category = provider["category"].as_str().unwrap_or_default();
        if rows.len() >= 16
            || !token(id, 32)
            || rows.iter().any(|row| row.id == id)
            || label.is_empty()
            || label.chars().count() > 64
            || label.chars().any(char::is_control)
            || !matches!(category, "agent" | "repository")
        {
            continue;
        }
        // A state this app does not know (a newer daemon's) reads as unknown.
        let state = provider["state"]
            .as_str()
            .filter(|state| {
                matches!(
                    *state,
                    "missing" | "needs_sign_in" | "signed_in" | "unavailable"
                )
            })
            .unwrap_or("unknown");
        let methods = provider["methods"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|method| method.as_str().filter(|method| token(method, 32)))
            .take(8)
            .map(str::to_owned)
            .collect();
        rows.push(Row {
            id: id.to_owned(),
            label: label.to_owned(),
            category: category.to_owned(),
            state: state.to_owned(),
            methods,
            disconnect_supported: provider["disconnect_supported"].as_bool(),
        });
    }
    rows
}

fn read(path: &Path) -> Option<Fact> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(16 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 16 * 1024 {
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

    fn scratch(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-agents-{name}-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("pro-agents.json");
        (dir, file)
    }

    #[test]
    fn a_ready_cloud_is_remembered_per_account_across_reads_and_restarts() {
        let (dir, file) = scratch("ready");
        let agents = Agents::at(file.clone());
        assert_eq!(agents.remembered("a"), Remembered::default());
        agents.mark_ready("a");
        assert!(agents.remembered("a").ready_once);
        // An app restart reads it back.
        assert!(Agents::at(file.clone()).remembered("a").ready_once);
        // A catalog that cannot tell forgets the agent fact, never readiness.
        agents.record(
            "a",
            &json!({"available": true, "providers": [{"id": "claude", "label": "Claude Code", "category": "agent", "state": "unknown"}]}),
        );
        let remembered = agents.remembered("a");
        assert!(remembered.ready_once);
        assert_eq!(remembered.agents_connected, None);
        agents.record("a", &json!({"available": false}));
        assert!(Agents::at(file.clone()).remembered("a").ready_once);
        // Another account starts afresh, and the first one's memory is gone.
        assert!(!agents.remembered("b").ready_once);
        agents.mark_ready("b");
        assert!(agents.remembered("b").ready_once);
        assert!(!Agents::at(file.clone()).remembered("a").ready_once);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_catalog_read_means_the_cloud_was_ready_and_its_rows_are_kept() {
        let (dir, file) = scratch("rows");
        let agents = Agents::at(file.clone());
        agents.record(
            "a",
            &json!({"available": true, "providers": [
                {"id": "claude", "label": "Claude Code", "category": "agent", "state": "signed_in", "methods": ["browser"], "disconnect_supported": true, "installed": true, "reason": null, "checked_at": 1},
                {"id": "codex", "label": "Codex", "category": "agent", "state": "needs_sign_in", "methods": ["device_code"]},
                {"id": "github", "label": "GitHub", "category": "repository", "state": "a_newer_state"},
                {"id": "claude", "label": "Again", "category": "agent", "state": "missing"},
                {"id": "../x", "label": "Bad id", "category": "agent", "state": "missing"},
                {"id": "future", "label": "Future", "category": "something_else", "state": "missing"},
                {"id": "blank", "label": " ", "category": "agent", "state": "missing"},
            ]}),
        );
        let remembered = Agents::at(file.clone()).remembered("a");
        assert!(remembered.ready_once);
        assert_eq!(remembered.agents_connected, Some(true));
        let rows = serde_json::to_value(&remembered.providers).unwrap();
        assert_eq!(
            rows,
            json!([
                {"id": "claude", "label": "Claude Code", "category": "agent", "state": "signed_in", "methods": ["browser"], "disconnect_supported": true},
                {"id": "codex", "label": "Codex", "category": "agent", "state": "needs_sign_in", "methods": ["device_code"]},
                {"id": "github", "label": "GitHub", "category": "repository", "state": "unknown"},
            ])
        );
        // Another account never sees them.
        assert!(agents.remembered("b").providers.is_empty());
        // A pre-rows file (a catalog fact only) implies a ready cloud.
        std::fs::write(&file, br#"{"account":"a","connected":false}"#).unwrap();
        let older = Agents::at(file.clone()).remembered("a");
        assert!(older.ready_once);
        assert_eq!(older.agents_connected, Some(false));
        assert!(older.providers.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
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
            connected: Some(true),
            signed_in: vec!["claude".into()],
            ..Fact::default()
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
