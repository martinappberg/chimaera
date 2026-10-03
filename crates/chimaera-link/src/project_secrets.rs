//! Personal selected-project control. Values are write-only and omit Debug.
pub use chimaera_core::project_secret_status::{
    Catalog, NamePolicy, Outcome, Pending, Project, Receipt, State,
};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub const COMMAND_MAX: usize = 64 * 1024;
pub(crate) const REVISION_MAX: u64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Unsupported,
    InvalidRequest,
    StateChanged,
    Unavailable,
    OperationUnavailable,
    LimitReached,
    SignInRequired,
    Unconfirmed,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unsupported => "project_secrets_unsupported",
            Self::InvalidRequest => "project_secrets_invalid_request",
            Self::StateChanged => "project_secrets_state_changed",
            Self::Unavailable => "project_secrets_unavailable",
            Self::OperationUnavailable => "project_secrets_operation_unavailable",
            Self::LimitReached => "project_secrets_limit_reached",
            Self::SignInRequired => "project_secrets_sign_in_required",
            Self::Unconfirmed => "project_secrets_unconfirmed",
        })
    }
}
impl std::error::Error for Error {}

/// Stale-view equality only; ordinary authentication grants every request.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlContext {
    pub version: u16,
    pub context: String,
}
impl ControlContext {
    pub fn validate(&self) -> Result<(), Error> {
        if self.version == 1 && context(&self.context) {
            Ok(())
        } else {
            Err(Error::Unsupported)
        }
    }
}
pub fn context(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogPage {
    pub version: u16,
    pub context: String,
    pub catalog: Catalog,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandResult {
    pub version: u16,
    pub context: String,
    pub receipt: Receipt,
}

/// Kept only until the single submission finishes. Never format or clone it.
pub struct Value(Zeroizing<String>);
impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = Value;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a bounded secret value")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Value, E> {
                if value.is_empty() || value.len() > 8192 || value.contains('\0') {
                    return Err(E::custom("invalid secret value"));
                }
                Ok(Value(Zeroizing::new(value.to_owned())))
            }
            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Value, E> {
                let value = Zeroizing::new(value);
                if value.is_empty() || value.len() > 8192 || value.contains('\0') {
                    return Err(E::custom("invalid secret value"));
                }
                Ok(Value(value))
            }
        }
        d.deserialize_string(Visitor)
    }
}
impl Serialize for Value {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}
fn required<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    Option::deserialize(d)
}
/// Struct variants, including no-extra-field actions, keep the envelope closed.
#[derive(Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Set {
        version: u16,
        operation_id: String,
        workspace_id: String,
        expected_revision: u64,
        #[serde(deserialize_with = "required")]
        expected_pending: Option<String>,
        name: String,
        value: Value,
    },
    Apply {
        version: u16,
        operation_id: String,
        workspace_id: String,
        expected_revision: u64,
        #[serde(deserialize_with = "required")]
        expected_pending: Option<String>,
    },
    Cancel {
        version: u16,
        operation_id: String,
        workspace_id: String,
        expected_revision: u64,
        #[serde(deserialize_with = "required")]
        expected_pending: Option<String>,
    },
    Remove {
        version: u16,
        operation_id: String,
        workspace_id: String,
        expected_revision: u64,
        #[serde(deserialize_with = "required")]
        expected_pending: Option<String>,
        name: String,
    },
}
pub(crate) fn id(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
pub(crate) fn uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
}
impl Command {
    /// IPC consumes its encoded value once; no parse source reaches the caller.
    pub fn decode_owned(payload: String) -> Result<Self, Error> {
        let payload = Zeroizing::new(payload);
        if payload.len() > COMMAND_MAX {
            return Err(Error::InvalidRequest);
        }
        let command: Self = serde_json::from_str(&payload).map_err(|_| Error::InvalidRequest)?;
        command.validate_shape()?;
        Ok(command)
    }
    pub fn identity(&self) -> (&str, &str, u64, Option<&str>) {
        match self {
            Self::Set {
                operation_id,
                workspace_id,
                expected_revision,
                expected_pending,
                ..
            }
            | Self::Apply {
                operation_id,
                workspace_id,
                expected_revision,
                expected_pending,
                ..
            }
            | Self::Cancel {
                operation_id,
                workspace_id,
                expected_revision,
                expected_pending,
                ..
            }
            | Self::Remove {
                operation_id,
                workspace_id,
                expected_revision,
                expected_pending,
                ..
            } => (
                operation_id,
                workspace_id,
                *expected_revision,
                expected_pending.as_deref(),
            ),
        }
    }
    pub fn validate_shape(&self) -> Result<(), Error> {
        let (operation, workspace, revision, pending) = self.identity();
        let version = match self {
            Self::Set { version, .. }
            | Self::Apply { version, .. }
            | Self::Cancel { version, .. }
            | Self::Remove { version, .. } => *version,
        };
        if version != 1
            || !uuid(operation)
            || !id(workspace)
            || !(1..=REVISION_MAX).contains(&revision)
            || pending.is_some_and(|p| !uuid(p) || p == operation)
        {
            return Err(Error::InvalidRequest);
        }
        match self {
            Self::Set { name, value, .. }
                if name.len() > 128
                    || value.0.is_empty()
                    || value.0.len() > 8192
                    || value.0.contains('\0') =>
            {
                Err(Error::InvalidRequest)
            }
            Self::Apply { .. } | Self::Cancel { .. } if pending.is_none() => {
                Err(Error::InvalidRequest)
            }
            _ => Ok(()),
        }
    }
    /// Fresh policy and exact shown state are required immediately before send.
    pub fn validate_catalog(&self, catalog: &Catalog) -> Result<Vec<String>, Error> {
        self.validate_shape()?;
        catalog.validate().map_err(|_| Error::Unsupported)?;
        let (_, workspace, revision, pending) = self.identity();
        let project = catalog
            .projects
            .iter()
            .find(|p| p.workspace_id == workspace)
            .ok_or(Error::Unavailable)?;
        if project.state != State::Ready
            || project.revision != revision
            || project.pending.as_ref().map(|p| p.operation_id.as_str()) != pending
        {
            return Err(Error::StateChanged);
        }
        let mut names = match self {
            Self::Set { name, .. } => {
                if !catalog.name_policy.permits(name) {
                    return Err(Error::InvalidRequest);
                }
                let mut names = project
                    .pending
                    .as_ref()
                    .map(|p| p.names.clone())
                    .unwrap_or_default();
                names.push(name.clone());
                names
            }
            Self::Apply { .. } | Self::Cancel { .. } => project
                .pending
                .as_ref()
                .ok_or(Error::StateChanged)?
                .names
                .clone(),
            Self::Remove { name, .. } => {
                if !project.applied_names.contains(name) {
                    return Err(Error::StateChanged);
                }
                vec![name.clone()]
            }
        };
        names.sort();
        names.dedup();
        if names.len() > 32
            || project
                .applied_names
                .iter()
                .chain(&names)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                > 32
        {
            return Err(Error::InvalidRequest);
        }
        Ok(names)
    }
    pub fn validate_receipt(&self, receipt: &Receipt, names: &[String]) -> Result<(), Error> {
        let (operation, workspace, revision, _) = self.identity();
        receipt
            .validate_for(operation, workspace, revision)
            .map_err(|_| Error::Unconfirmed)?;
        let outcome = match self {
            Self::Set { .. } => matches!(
                receipt.outcome,
                Outcome::Queued | Outcome::Applying | Outcome::Applied
            ),
            Self::Cancel { .. } => receipt.outcome == Outcome::Canceled,
            Self::Apply { .. } | Self::Remove { .. } => {
                matches!(receipt.outcome, Outcome::Applying | Outcome::Applied)
            }
        };
        if !outcome || receipt.names != names {
            return Err(Error::Unconfirmed);
        }
        Ok(())
    }
}
