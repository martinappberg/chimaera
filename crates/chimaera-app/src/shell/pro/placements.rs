//! Desired project routes are separate from shared transports. The daemon can
//! outlive this shell, so retirement starts from its credential-free inventory.
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Registered {
    pub host_id: String,
    pub workspace_id: String,
    epoch: u64,
}

pub(super) struct Reconciliation {
    registered: Vec<Registered>,
    desired: HashMap<String, String>,
    failure: Option<anyhow::Error>,
}

impl Reconciliation {
    pub fn new(value: serde_json::Value) -> Result<Self> {
        let registered: Vec<Registered> = serde_json::from_value(value)?;
        anyhow::ensure!(
            registered.len() <= 4096,
            "placement inventory exceeds limit"
        );
        let valid = |id: &str| {
            !id.is_empty()
                && id.len() <= 128
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
        };
        let mut seen = HashSet::new();
        for row in &registered {
            anyhow::ensure!(
                valid(&row.host_id)
                    && valid(&row.workspace_id)
                    && row.epoch > 0
                    && seen.insert(&row.workspace_id),
                "invalid placement inventory"
            );
        }
        Ok(Self {
            registered,
            desired: HashMap::new(),
            failure: None,
        })
    }

    pub fn observe(&mut self, workspace: &str, result: Result<Option<String>>) {
        match result {
            Ok(Some(host)) => {
                self.desired.insert(workspace.to_owned(), host);
            }
            Ok(None) => {}
            Err(error) => self.failed(error),
        }
    }

    pub fn failed(&mut self, error: anyhow::Error) {
        if self.failure.is_none() {
            self.failure = Some(error);
        }
    }

    pub fn retired_workspaces(&self) -> Vec<(String, String)> {
        self.registered
            .iter()
            .filter(|row| !self.desired.contains_key(&row.workspace_id))
            .map(|row| (row.workspace_id.clone(), row.host_id.clone()))
            .collect()
    }

    pub fn desired_hosts(&self) -> HashSet<String> {
        self.desired.values().cloned().collect()
    }

    pub fn known_hosts(&self) -> HashSet<String> {
        self.registered
            .iter()
            .map(|row| row.host_id.clone())
            .collect()
    }

    pub fn finish(self) -> Result<()> {
        self.failure.map_or(Ok(()), |error| {
            Err(error).context("project route refresh incomplete")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> serde_json::Value {
        serde_json::json!([
            {"host_id":"worker-shared","workspace_id":"w-a","epoch":4},
            {"host_id":"worker-shared","workspace_id":"w-b","epoch":9},
            {"host_id":"device-old","workspace_id":"w-removed","epoch":3}
        ])
    }

    #[test]
    fn restart_inventory_retires_one_workspace_without_dropping_shared_transport() {
        // A fresh shell has no in-memory tunnel registry. The daemon inventory
        // still retires a removed project and a now-local/private/unowned one.
        let mut sync = Reconciliation::new(snapshot()).unwrap();
        sync.observe("w-a", Ok(None));
        sync.observe("w-b", Ok(Some("worker-shared".into())));
        assert_eq!(
            sync.retired_workspaces(),
            vec![
                ("w-a".into(), "worker-shared".into()),
                ("w-removed".into(), "device-old".into()),
            ]
        );
        assert_eq!(
            sync.desired_hosts(),
            HashSet::from(["worker-shared".into()])
        );
        assert_eq!(
            sync.known_hosts(),
            HashSet::from(["worker-shared".into(), "device-old".into()])
        );
        sync.finish().unwrap();
    }

    #[test]
    fn failed_probe_is_retired_but_does_not_discard_healthy_sibling() {
        let mut sync = Reconciliation::new(snapshot()).unwrap();
        sync.observe("w-a", Err(anyhow::anyhow!("scope probe failed")));
        sync.observe("w-b", Ok(Some("worker-shared".into())));
        assert!(sync.retired_workspaces().iter().any(|(id, _)| id == "w-a"));
        assert!(!sync.retired_workspaces().iter().any(|(id, _)| id == "w-b"));
        assert!(sync.desired_hosts().contains("worker-shared"));
        assert!(
            sync.finish().is_err(),
            "failure remains visible for the bounded retry"
        );
    }

    #[test]
    fn inventory_is_bounded_unambiguous_and_never_accepts_transport_secrets() {
        for value in [
            serde_json::json!([{"host_id":"worker-a","workspace_id":"w-a","epoch":0}]),
            serde_json::json!([{"host_id":"worker-a","workspace_id":"w/a","epoch":4}]),
            serde_json::json!([{"host_id":"worker-a","workspace_id":"w-a","epoch":4,"token":"unexpected"}]),
            serde_json::json!([
                {"host_id":"worker-a","workspace_id":"w-a","epoch":4},
                {"host_id":"worker-b","workspace_id":"w-a","epoch":5}
            ]),
        ] {
            assert!(Reconciliation::new(value).is_err());
        }
    }
}
