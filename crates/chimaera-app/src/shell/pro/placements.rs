//! Desired project routes are separate from shared transports. The daemon can
//! outlive this shell, so retirement starts from its credential-free inventory.
//! A route is retired only on a definitive answer; a failed check keeps the
//! last verified route for a bounded time, so a keeper or account blip never
//! swaps a live view for a stale local copy.
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// How long a registered route survives failed checks. Longer than one
/// ownership lease (90 s) plus the takeover grace, so a real owner change
/// is always observed as a definitive answer before this expires.
pub(super) const ROUTE_STALENESS: Duration = Duration::from_secs(150);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Registered {
    pub host_id: String,
    pub workspace_id: String,
    pub epoch: u64,
}

/// One project's result for this pass.
pub(super) enum Observation {
    /// Verified live route to this host.
    Route(String),
    /// The account says this computer must not route the project anywhere:
    /// not owned, owned here, private, or removed.
    Retire,
    /// Could not verify now. `epoch` is the account's current epoch when the
    /// ownership read itself succeeded.
    Unverified {
        epoch: Option<u64>,
        error: anyhow::Error,
    },
}

/// When each project's route was last verified. Bounded by the daemon's
/// project list, which the caller prunes it to every pass.
#[derive(Default)]
pub(super) struct Verified(HashMap<String, Instant>);
impl Verified {
    pub fn confirm(&mut self, workspace: &str, now: Instant) {
        self.0.insert(workspace.to_owned(), now);
    }
    /// A route first seen in the daemon's inventory (e.g. after this shell
    /// restarted) gets the same bounded grace as one verified just now.
    pub fn fresh(&mut self, workspace: &str, now: Instant) -> bool {
        let since = *self.0.entry(workspace.to_owned()).or_insert(now);
        now.saturating_duration_since(since) <= ROUTE_STALENESS
    }
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.0.retain(|workspace, _| keep(workspace));
    }
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

    pub fn registered(&self, workspace: &str) -> Option<&Registered> {
        self.registered
            .iter()
            .find(|row| row.workspace_id == workspace)
    }

    /// `fresh`: the registered route is within its staleness bound and its
    /// shared transport is still open.
    pub fn observe(&mut self, workspace: &str, observation: Observation, fresh: bool) {
        match observation {
            Observation::Route(host) => {
                self.desired.insert(workspace.to_owned(), host);
            }
            Observation::Retire => {}
            Observation::Unverified { epoch, error } => {
                // A new epoch means ownership moved: the old route is wrong
                // even if the new owner cannot be reached yet.
                let kept = self
                    .registered(workspace)
                    .filter(|row| fresh && epoch.is_none_or(|epoch| epoch == row.epoch))
                    .map(|row| row.host_id.clone());
                if let Some(host) = kept {
                    self.desired.insert(workspace.to_owned(), host);
                }
                self.failed(error);
            }
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

/// The scope acknowledgment a suspended owner can give. A frozen machine cannot
/// answer the full project check, so its transport answers the scoped health
/// probe itself (marked `sleeping`) from the account's placement, and only for
/// a daemon that acknowledged scoping while it was awake. Anything else, or an
/// owner that is actually awake but did not acknowledge, fails.
pub(super) fn sleeping_acknowledgment(
    headers: &ureq::http::HeaderMap,
    workspace: &str,
    epoch: u64,
) -> Result<()> {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    anyhow::ensure!(
        header("x-chimaera-scope-version") == Some("1")
            && header("x-chimaera-workspace") == Some(workspace)
            && header("x-chimaera-epoch") == Some(epoch.to_string().as_str())
            && header("x-chimaera-worker-state") == Some("sleeping"),
        "suspended project owner not vouched for"
    );
    Ok(())
}

/// Verifies a suspended owner through its transport's scoped health probe
/// (see [`sleeping_acknowledgment`]). Passive: it never wakes the owner.
pub(super) async fn verify_sleeping_owner(
    port: u16,
    token: &str,
    workspace: &str,
    epoch: u64,
) -> Result<()> {
    let token = token.to_owned();
    let workspace = workspace.to_owned();
    tokio::task::spawn_blocking(move || {
        let response = crate::http::agent()
            .get(&format!("http://127.0.0.1:{port}/api/v1/health"))
            .header("Authorization", &format!("Bearer {token}"))
            .header("X-Chimaera-Workspace", &workspace)
            .header("X-Chimaera-Epoch", &epoch.to_string())
            .config()
            .timeout_global(Some(Duration::from_secs(10)))
            .max_redirects(0)
            .build()
            .call()?;
        sleeping_acknowledgment(response.headers(), &workspace, epoch)
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_a_scoped_sleeping_answer_vouches_for_a_suspended_owner() {
        let headers = |pairs: &[(&'static str, &str)]| {
            let mut map = ureq::http::HeaderMap::new();
            for (name, value) in pairs {
                map.insert(*name, value.parse().unwrap());
            }
            map
        };
        let scoped = [
            ("x-chimaera-scope-version", "1"),
            ("x-chimaera-workspace", "w-a"),
            ("x-chimaera-epoch", "4"),
        ];
        let mut sleeping = scoped.to_vec();
        sleeping.push(("x-chimaera-worker-state", "sleeping"));
        sleeping_acknowledgment(&headers(&sleeping), "w-a", 4).unwrap();
        assert!(sleeping_acknowledgment(&headers(&sleeping), "w-a", 5).is_err());
        assert!(sleeping_acknowledgment(&headers(&sleeping), "w-b", 4).is_err());
        assert!(
            sleeping_acknowledgment(&headers(&scoped), "w-a", 4).is_err(),
            "an awake owner goes through the full check"
        );
        assert!(sleeping_acknowledgment(
            &headers(&[("x-chimaera-worker-state", "sleeping")]),
            "w-a",
            4
        )
        .is_err());
    }
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
        sync.observe("w-a", Observation::Retire, true);
        sync.observe("w-b", Observation::Route("worker-shared".into()), true);
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

    fn unverified(epoch: Option<u64>) -> Observation {
        Observation::Unverified {
            epoch,
            error: anyhow::anyhow!("scope probe failed"),
        }
    }

    #[test]
    fn a_failed_check_keeps_the_last_route_until_stale_or_ownership_moves() {
        // A keeper or account blip: keep viewing the owner, report the failure.
        let mut sync = Reconciliation::new(snapshot()).unwrap();
        sync.observe("w-a", unverified(None), true);
        sync.observe("w-b", Observation::Route("worker-shared".into()), true);
        assert!(!sync.retired_workspaces().iter().any(|(id, _)| id == "w-a"));
        assert!(sync.desired_hosts().contains("worker-shared"));
        assert!(
            sync.finish().is_err(),
            "failure remains visible for the bounded retry"
        );
        // Same epoch read, owner unreachable: still the same owner.
        let mut sync = Reconciliation::new(snapshot()).unwrap();
        sync.observe("w-a", unverified(Some(4)), true);
        assert!(!sync.retired_workspaces().iter().any(|(id, _)| id == "w-a"));
        // Past the staleness bound the route is retired.
        let mut sync = Reconciliation::new(snapshot()).unwrap();
        sync.observe("w-a", unverified(None), false);
        assert!(sync.retired_workspaces().iter().any(|(id, _)| id == "w-a"));
        // A newer epoch is a definitive ownership change.
        let mut sync = Reconciliation::new(snapshot()).unwrap();
        sync.observe("w-a", unverified(Some(5)), true);
        assert!(sync.retired_workspaces().iter().any(|(id, _)| id == "w-a"));
        // A definitive answer retires regardless of freshness.
        let mut sync = Reconciliation::new(snapshot()).unwrap();
        sync.observe("w-a", Observation::Retire, true);
        sync.observe("w-b", Observation::Route("worker-shared".into()), true);
        assert!(sync.retired_workspaces().iter().any(|(id, _)| id == "w-a"));
        assert!(!sync.retired_workspaces().iter().any(|(id, _)| id == "w-b"));
        sync.finish().unwrap();
    }

    #[test]
    fn staleness_starts_at_first_sighting_and_is_pruned_with_the_project() {
        let start = Instant::now();
        let mut verified = Verified::default();
        assert!(verified.fresh("w-a", start));
        assert!(verified.fresh("w-a", start + ROUTE_STALENESS));
        assert!(!verified.fresh("w-a", start + ROUTE_STALENESS + Duration::from_secs(1)));
        verified.confirm("w-a", start + ROUTE_STALENESS + Duration::from_secs(1));
        assert!(verified.fresh("w-a", start + ROUTE_STALENESS + Duration::from_secs(2)));
        verified.retain(|workspace| workspace != "w-a");
        assert!(verified.0.is_empty());
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
