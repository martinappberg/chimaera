//! Pure redacted PROJECT_SECRETS wire. Parsing is structure/correlation only;
//! neither a catalog, receipt nor acknowledgment grants execution authority.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
pub const BODY_MAX: usize = 1024 * 1024;
pub const REVISION_MAX: u64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidMessage;
impl std::fmt::Display for InvalidMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid project secret status")
    }
}
impl std::error::Error for InvalidMessage {}
type Result<T> = std::result::Result<T, InvalidMessage>;
fn valid(b: bool) -> Result<()> {
    if b {
        Ok(())
    } else {
        Err(InvalidMessage)
    }
}
fn required<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> std::result::Result<Option<T>, D::Error> {
    Option::deserialize(d)
}
fn id(s: &str) -> bool {
    (1..=128).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
fn uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
}
fn name(s: &str) -> bool {
    (1..=128).contains(&s.len())
        && s.bytes()
            .enumerate()
            .all(|(i, b)| b == b'_' || b.is_ascii_uppercase() || i > 0 && b.is_ascii_digit())
}
fn names(list: &[String], max: usize) -> bool {
    list.len() <= max && list.iter().all(|n| name(n)) && list.windows(2).all(|p| p[0] < p[1])
}
fn revision(n: u64) -> bool {
    (1..=REVISION_MAX).contains(&n)
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamePolicy {
    pub max_name_bytes: u16,
    pub max_value_bytes: u16,
    pub max_names: u16,
    pub reserved_names: Vec<String>,
    pub reserved_prefixes: Vec<String>,
}
impl NamePolicy {
    pub fn validate(&self) -> Result<()> {
        valid(
            self.max_name_bytes == 128
                && self.max_value_bytes == 8192
                && self.max_names == 32
                && names(&self.reserved_names, 128)
                && names(&self.reserved_prefixes, 128),
        )
    }
    pub fn permits(&self, candidate: &str) -> bool {
        self.validate().is_ok()
            && name(candidate)
            && !self.reserved_names.iter().any(|n| n == candidate)
            && !self
                .reserved_prefixes
                .iter()
                .any(|p| candidate.starts_with(p))
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pending {
    pub operation_id: String,
    pub base_revision: u64,
    pub names: Vec<String>,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Ready,
    Applying,
    Unavailable,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub workspace_id: String,
    pub revision: u64,
    pub applied_names: Vec<String>,
    #[serde(deserialize_with = "required")]
    pub pending: Option<Pending>,
    pub state: State,
}
impl Project {
    pub fn validate(&self) -> Result<()> {
        valid(id(&self.workspace_id) && revision(self.revision) && names(&self.applied_names, 32))?;
        if let Some(p) = &self.pending {
            valid(
                uuid(&p.operation_id)
                    && p.base_revision == self.revision
                    && !p.names.is_empty()
                    && names(&p.names, 32)
                    && self
                        .applied_names
                        .iter()
                        .chain(&p.names)
                        .collect::<BTreeSet<_>>()
                        .len()
                        <= 32,
            )?;
        }
        Ok(())
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub version: u16,
    pub project_secrets: u16,
    pub name_policy: NamePolicy,
    pub projects: Vec<Project>,
    #[serde(deserialize_with = "required")]
    pub next: Option<String>,
}
impl Catalog {
    pub fn validate(&self) -> Result<()> {
        valid(self.version == 1 && self.project_secrets == 1 && self.projects.len() <= 64)?;
        self.name_policy.validate()?;
        for p in &self.projects {
            p.validate()?;
        }
        valid(
            self.projects
                .windows(2)
                .all(|p| p[0].workspace_id < p[1].workspace_id)
                && self.next.as_ref().is_none_or(|next| {
                    id(next)
                        && self
                            .projects
                            .last()
                            .is_some_and(|p| &p.workspace_id == next)
                }),
        )
    }
    /// A previous cursor is position only, never a registration/idle proof.
    pub fn validate_after(&self, after: Option<&str>) -> Result<()> {
        self.validate()?;
        valid(after.is_none_or(|after| {
            id(after)
                && self
                    .projects
                    .iter()
                    .all(|p| p.workspace_id.as_str() > after)
        }))
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Queued,
    Applying,
    Applied,
    Canceled,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub version: u16,
    pub operation_id: String,
    pub workspace_id: String,
    pub base_revision: u64,
    #[serde(deserialize_with = "required")]
    pub result_revision: Option<u64>,
    pub names: Vec<String>,
    pub outcome: Outcome,
}
impl Receipt {
    pub fn validate(&self) -> Result<()> {
        valid(
            self.version == 1
                && uuid(&self.operation_id)
                && id(&self.workspace_id)
                && revision(self.base_revision)
                && !self.names.is_empty()
                && names(&self.names, 32)
                && match self.outcome {
                    Outcome::Queued | Outcome::Canceled => self.result_revision.is_none(),
                    Outcome::Applying | Outcome::Applied => self
                        .result_revision
                        .is_some_and(|r| revision(r) && r > self.base_revision),
                },
        )
    }
    pub fn validate_for(&self, operation: &str, workspace: &str, base: u64) -> Result<()> {
        self.validate()?;
        valid(
            self.operation_id == operation
                && self.workspace_id == workspace
                && self.base_revision == base,
        )
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrationAck {
    pub version: u16,
    pub account_id: String,
    pub holder_id: String,
    pub process_boot: String,
    pub registration_generation: u64,
    pub worker_credential_digest: String,
}
impl RegistrationAck {
    pub fn validate(&self) -> Result<()> {
        valid(
            self.version == 1
                && id(&self.account_id)
                && id(&self.holder_id)
                && uuid(&self.process_boot)
                && self.registration_generation > 0
                && self.worker_credential_digest.len() == 64
                && self
                    .worker_credential_digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        )
    }
}
// No secret-bearing request type implements this codec. Validation before
// serialization bounds caller-constructed collections as well as parsed input.
macro_rules! codec {
    ($ty:ty, $max:expr) => {
        impl $ty {
            pub fn decode(bytes: &[u8]) -> Result<Self> {
                valid(!bytes.is_empty() && bytes.len() <= $max)?;
                let value: Self = serde_json::from_slice(bytes).map_err(|_| InvalidMessage)?;
                value.validate()?;
                Ok(value)
            }
            pub fn encode(&self) -> Result<Vec<u8>> {
                self.validate()?;
                let bytes = serde_json::to_vec(self).map_err(|_| InvalidMessage)?;
                valid(bytes.len() <= $max)?;
                Ok(bytes)
            }
        }
    };
}
codec!(Catalog, BODY_MAX);
codec!(Receipt, BODY_MAX);
codec!(RegistrationAck, 8192);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn catalog() -> Catalog {
        Catalog {
            version: 1,
            project_secrets: 1,
            name_policy: NamePolicy {
                max_name_bytes: 128,
                max_value_bytes: 8192,
                max_names: 32,
                reserved_names: vec!["HOME".into()],
                reserved_prefixes: vec!["CHIMAERA_".into()],
            },
            projects: vec![Project {
                workspace_id: "project-one".into(),
                revision: 1,
                applied_names: vec!["SERVICE_TOKEN".into()],
                pending: None,
                state: State::Ready,
            }],
            next: None,
        }
    }
    #[test]
    fn closed_nullable_status_and_cursor_cannot_silently_change_meaning() {
        let c = catalog();
        assert!(Catalog::decode(&c.encode().unwrap()).unwrap() == c);
        let mut value = serde_json::to_value(&c).unwrap();
        value.as_object_mut().unwrap().remove("next");
        assert!(Catalog::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        value["next"] = json!(null);
        value["projects"][0]["secret_value"] = json!("should-never-be-exposed");
        assert!(Catalog::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        let mut c = catalog();
        c.next = Some("project-two".into());
        assert!(c.validate().is_err());
        c.next = Some("project-one".into());
        assert!(c.validate_after(Some("project-one")).is_err());
        assert!(c.validate_after(Some("project-before")).is_ok());
    }
    #[test]
    fn receipt_requires_exact_original_identity_and_positive_application_revision() {
        let mut r = Receipt {
            version: 1,
            operation_id: "11111111-1111-4111-8111-111111111111".into(),
            workspace_id: "project-one".into(),
            base_revision: 7,
            result_revision: None,
            names: vec!["SERVICE_TOKEN".into()],
            outcome: Outcome::Queued,
        };
        assert!(r.validate_for(&r.operation_id, "project-one", 7).is_ok());
        assert!(r.validate_for(&r.operation_id, "project-two", 7).is_err());
        r.outcome = Outcome::Applying;
        assert!(r.validate().is_err());
        r.result_revision = Some(8);
        assert!(r.validate().is_ok());
        assert!(r.outcome != Outcome::Applied);
        r.outcome = Outcome::Canceled;
        assert!(r.validate().is_err());
    }
    #[test]
    fn maximum_catalog_page_fits_wire_bound_and_rejects_more_rows() {
        let mut c = catalog();
        let names: Vec<_> = (0..32)
            .map(|n| format!("N{n:03}{}", "A".repeat(124)))
            .collect();
        let policy: Vec<_> = (0..128)
            .map(|n| format!("P{n:03}{}", "Z".repeat(124)))
            .collect();
        c.name_policy.reserved_names = policy.clone();
        c.name_policy.reserved_prefixes = policy;
        c.projects = (0..64)
            .map(|n| Project {
                workspace_id: format!("W{n:03}{}", "X".repeat(124)),
                revision: REVISION_MAX,
                applied_names: names.clone(),
                pending: Some(Pending {
                    operation_id: "11111111-1111-4111-8111-111111111111".into(),
                    base_revision: REVISION_MAX,
                    names: names.clone(),
                }),
                state: State::Ready,
            })
            .collect();
        c.next = c.projects.last().map(|p| p.workspace_id.clone());
        assert!(c.encode().unwrap().len() < BODY_MAX);
        c.projects.push(c.projects[0].clone());
        assert!(c.encode().is_err());
    }
    #[test]
    fn negotiated_name_policy_and_pending_union_remain_bounded() {
        let mut c = catalog();
        assert!(c.name_policy.permits("SERVICE_TOKEN"));
        assert!(!c.name_policy.permits("HOME"));
        assert!(!c.name_policy.permits("CHIMAERA_TOKEN"));
        assert!(!c.name_policy.permits("1TOKEN"));
        c.projects[0].pending = Some(Pending {
            operation_id: "11111111-1111-4111-8111-111111111111".into(),
            base_revision: 2,
            names: vec!["SERVICE_TOKEN".into()],
        });
        assert!(c.validate().is_err());
        c.name_policy.max_value_bytes = 8193;
        assert!(!c.name_policy.permits("SERVICE_TOKEN"));
    }
}
