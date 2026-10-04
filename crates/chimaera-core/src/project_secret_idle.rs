//! Historical v1 parking-record validation and retained provider launch identity.
//! The namespace-idle transport is retired; these types decode existing records.
//! Parsing establishes bounded structure only, never idle or execution proof.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const REQUEST_MAX: usize = 16 * 1024;
pub const REPLY_MAX: usize = 32 * 1024;
pub const DEADLINE_MAX_MS: u64 = 30_000;
pub const LEADERS_MAX: usize = 64;
pub const REVISION_MAX: u64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidMessage;
impl std::fmt::Display for InvalidMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid project secret idle message")
    }
}
impl std::error::Error for InvalidMessage {}
type Result<T> = std::result::Result<T, InvalidMessage>;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootIdentity {
    pub device: u64,
    pub inode: u64,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub account_id: String,
    pub workspace_id: String,
    pub root_identity: RootIdentity,
    pub registration_revision: u64,
    pub launch_generation: u64,
    pub os_boot_id: String,
}
impl Binding {
    pub fn validate(&self) -> Result<()> {
        valid(
            stable_id(&self.account_id)
                && stable_id(&self.workspace_id)
                && self.root_identity.inode > 0
                && self.registration_revision > 0
                && self.launch_generation > 0
                // Keep the existing inherited cleanup binding's boot grammar.
                && self.os_boot_id.len() == 36
                && self.os_boot_id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'),
        )
    }
}

/// Exact immutable maintenance identity, independent of a channel request ID.
#[derive(Clone, PartialEq, Eq)]
pub struct AttemptIdentity {
    pub binding: Binding,
    pub attempt_id: String,
    pub operation_id: String,
    pub pending_id: String,
    pub expected_applied_revision: u64,
}

macro_rules! message {
    ($name:ident { $($field:ident: $ty:ty),* $(,)? }) => {
        #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $name {
            pub version: u16,
            pub request_id: u64,
            pub binding: Binding,
            pub attempt_id: String,
            pub operation_id: String,
            pub pending_id: String,
            pub expected_applied_revision: u64,
            $(pub $field: $ty,)*
        }
        impl $name {
            pub fn identity(&self) -> AttemptIdentity {
                AttemptIdentity {
                    binding: self.binding.clone(),
                    attempt_id: self.attempt_id.clone(),
                    operation_id: self.operation_id.clone(),
                    pending_id: self.pending_id.clone(),
                    expected_applied_revision: self.expected_applied_revision,
                }
            }
            fn validate_common(&self) -> Result<()> {
                self.binding.validate()?;
                valid(self.version == 1 && self.request_id > 0
                    && canonical_uuid(&self.attempt_id)
                    && canonical_uuid(&self.operation_id)
                    && canonical_uuid(&self.pending_id)
                    && self.expected_applied_revision <= REVISION_MAX)
            }
        }
    };
}
message!(Prepare { expires_in_ms: u64 });
message!(Inspect {});
message!(Abort { fence_id: String });
message!(Busy { reason: BusyReason });
message!(Prepared { fence_id: String, remaining_ms: u64, leaders: Vec<Leader> });
message!(Aborted { fence_id: String });
message!(Expired { fence_id: String });
message!(RecoveryRequired {
    reason: RecoveryReason
});
message!(Conflict {
    reason: ConflictReason
});
message!(NotFound {});

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ready {
    pub version: u16,
    pub binding: Binding,
    pub channel_nonce: String,
    pub project_secrets_idle: u16,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Leader {
    pub session_id: String,
    pub namespace_pid: u32,
    pub start_ticks: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BusyReason {
    ActiveTurn,
    PendingInput,
    BackgroundWork,
    PermissionWait,
    ExternalInput,
    TerminalWork,
    SetupOrMutation,
    LifecycleOrTransfer,
    Unresumable,
    ProcessUnknown,
    LimitReached,
    Expired,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryReason {
    ParkingCleanupUnknown,
    ParkRecordUnknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictReason {
    IdentityChanged,
    BindingChanged,
    AttemptInProgress,
    OutcomeUnknown,
}

// No Debug on messages: Ready and prepared/rollback replies carry authority.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Prepare(Prepare),
    Inspect(Inspect),
    Abort(Abort),
}
impl Request {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value: Self = decode(bytes, REQUEST_MAX)?;
        value.validate()?;
        Ok(value)
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        encode(self, REQUEST_MAX)
    }
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Prepare(v) => {
                v.validate_common()?;
                valid((1..=DEADLINE_MAX_MS).contains(&v.expires_in_ms))
            }
            Self::Inspect(v) => v.validate_common(),
            Self::Abort(v) => {
                v.validate_common()?;
                valid(nonce(&v.fence_id))
            }
        }
    }
    pub fn identity(&self) -> AttemptIdentity {
        match self {
            Self::Prepare(v) => v.identity(),
            Self::Inspect(v) => v.identity(),
            Self::Abort(v) => v.identity(),
        }
    }
    pub fn request_id(&self) -> u64 {
        match self {
            Self::Prepare(v) => v.request_id,
            Self::Inspect(v) => v.request_id,
            Self::Abort(v) => v.request_id,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    Ready(Ready),
    Busy(Busy),
    Prepared(Prepared),
    Aborted(Aborted),
    Expired(Expired),
    RecoveryRequired(RecoveryRequired),
    Conflict(Conflict),
    NotFound(NotFound),
}
impl Reply {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value: Self = decode(bytes, REPLY_MAX)?;
        value.validate()?;
        Ok(value)
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        encode(self, REPLY_MAX)
    }
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Ready(v) => {
                v.binding.validate()?;
                valid(v.version == 1 && v.project_secrets_idle == 1 && nonce(&v.channel_nonce))
            }
            Self::Busy(v) => v.validate_common(),
            Self::Prepared(v) => {
                v.validate_common()?;
                valid(
                    nonce(&v.fence_id)
                        && (1..=DEADLINE_MAX_MS).contains(&v.remaining_ms)
                        && v.leaders.len() <= LEADERS_MAX,
                )?;
                let mut sessions = BTreeSet::new();
                let mut pids = BTreeSet::new();
                for leader in &v.leaders {
                    valid(
                        stable_id(&leader.session_id)
                            && leader.namespace_pid > 0
                            && leader.start_ticks > 0
                            && sessions.insert(leader.session_id.as_str())
                            && pids.insert(leader.namespace_pid),
                    )?;
                }
                Ok(())
            }
            Self::Aborted(v) => {
                v.validate_common()?;
                valid(nonce(&v.fence_id))
            }
            Self::Expired(v) => {
                v.validate_common()?;
                valid(nonce(&v.fence_id))
            }
            Self::RecoveryRequired(v) => v.validate_common(),
            Self::Conflict(v) => v.validate_common(),
            Self::NotFound(v) => v.validate_common(),
        }
    }
    /// Ready is support only; it has no retained attempt or request correlation.
    pub fn identity(&self) -> Option<AttemptIdentity> {
        match self {
            Self::Ready(_) => None,
            Self::Busy(v) => Some(v.identity()),
            Self::Prepared(v) => Some(v.identity()),
            Self::Aborted(v) => Some(v.identity()),
            Self::Expired(v) => Some(v.identity()),
            Self::RecoveryRequired(v) => Some(v.identity()),
            Self::Conflict(v) => Some(v.identity()),
            Self::NotFound(v) => Some(v.identity()),
        }
    }
    pub fn request_id(&self) -> Option<u64> {
        match self {
            Self::Ready(_) => None,
            Self::Busy(v) => Some(v.request_id),
            Self::Prepared(v) => Some(v.request_id),
            Self::Aborted(v) => Some(v.request_id),
            Self::Expired(v) => Some(v.request_id),
            Self::RecoveryRequired(v) => Some(v.request_id),
            Self::Conflict(v) => Some(v.request_id),
            Self::NotFound(v) => Some(v.request_id),
        }
    }
}

fn decode<T: for<'de> Deserialize<'de>>(bytes: &[u8], max: usize) -> Result<T> {
    valid(!bytes.is_empty() && bytes.len() <= max)?;
    serde_json::from_slice(bytes).map_err(|_| InvalidMessage)
}
fn encode<T: Serialize>(value: &T, max: usize) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value).map_err(|_| InvalidMessage)?;
    valid(!bytes.is_empty() && bytes.len() <= max)?;
    Ok(bytes)
}
fn valid(condition: bool) -> Result<()> {
    condition.then_some(()).ok_or(InvalidMessage)
}
fn stable_id(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn canonical_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
}
fn nonce(value: &str) -> bool {
    // A 32-byte unpadded encoding has two zero tail bits, not an arbitrary
    // final base64 digit. Randomness belongs to the trusted sender.
    value.len() == 43
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && value
            .as_bytes()
            .last()
            .is_some_and(|b| b"AEIMQUYcgkosw048".contains(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn binding() -> Value {
        json!({
            "account_id":"account-fixture", "workspace_id":"workspace-fixture",
            "root_identity":{"device":0,"inode":1},
            "registration_revision":1,"launch_generation":2,
            "os_boot_id":"11111111-2222-3333-4444-555555555555"
        })
    }
    fn attempt(kind: &str) -> Value {
        json!({
            "version":1,"type":kind,"request_id":1,"binding":binding(),
            "attempt_id":"11111111-1111-1111-1111-111111111111",
            "operation_id":"22222222-2222-2222-2222-222222222222",
            "pending_id":"33333333-3333-3333-3333-333333333333",
            "expected_applied_revision":0
        })
    }
    fn bytes(value: &Value) -> Vec<u8> {
        serde_json::to_vec(value).unwrap()
    }
    fn authority() -> String {
        "A".repeat(43)
    }
    fn leader() -> Value {
        json!({"session_id":"session-fixture","namespace_pid":42,"start_ticks":7})
    }

    #[test]
    fn exact_closed_shapes_roundtrip_without_a_consume_command() {
        for (kind, extra) in [
            ("prepare", json!({"expires_in_ms":30000})),
            ("inspect", json!({})),
            ("abort", json!({"fence_id":authority()})),
        ] {
            let mut value = attempt(kind);
            value
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            let request = Request::decode(&bytes(&value)).unwrap();
            assert_eq!(request.request_id(), 1);
            assert_eq!(bytes(&value).len(), request.encode().unwrap().len());
            assert!(request == Request::decode(&request.encode().unwrap()).unwrap());
            value["unrecognized"] = json!(true);
            assert!(Request::decode(&bytes(&value)).is_err());
        }
        for (kind, extra) in [
            ("busy", json!({"reason":"terminal_work"})),
            (
                "prepared",
                json!({"fence_id":authority(),"remaining_ms":1,"leaders":[leader()]}),
            ),
            ("aborted", json!({"fence_id":authority()})),
            ("expired", json!({"fence_id":authority()})),
            ("recovery_required", json!({"reason":"park_record_unknown"})),
            ("conflict", json!({"reason":"binding_changed"})),
            ("not_found", json!({})),
        ] {
            let mut value = attempt(kind);
            value
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            let reply = Reply::decode(&bytes(&value)).unwrap();
            assert_eq!(reply.request_id(), Some(1));
            assert!(reply == Reply::decode(&reply.encode().unwrap()).unwrap());
            assert!(
                reply.identity().unwrap()
                    == Request::decode(&bytes(&attempt("inspect")))
                        .unwrap()
                        .identity()
            );
            value["unrecognized"] = json!(true);
            assert!(Reply::decode(&bytes(&value)).is_err());
        }
        let ready = json!({"version":1,"type":"ready","binding":binding(),
            "channel_nonce":authority(),"project_secrets_idle":1});
        let reply = Reply::decode(&bytes(&ready)).unwrap();
        assert!(reply.identity().is_none() && reply.request_id().is_none());
        assert!(Request::decode(&bytes(&attempt("consume"))).is_err());
    }

    #[test]
    fn duplicates_trailing_json_unknown_variants_and_nested_fields_are_rejected() {
        let inspect = String::from_utf8(bytes(&attempt("inspect"))).unwrap();
        for value in [
            inspect.replacen("\"request_id\":1", "\"request_id\":1,\"request_id\":2", 1),
            inspect.replacen(
                "\"type\":\"inspect\"",
                "\"type\":\"inspect\",\"type\":\"inspect\"",
                1,
            ),
            inspect.replacen("\"inode\":1", "\"inode\":1,\"inode\":2", 1),
            format!("{inspect} {{}}"),
        ] {
            assert!(Request::decode(value.as_bytes()).is_err());
        }
        let mut nested = attempt("inspect");
        nested["binding"]["environment"] = json!("never allowed");
        assert!(Request::decode(&bytes(&nested)).is_err());
        let mut busy = attempt("busy");
        busy["reason"] = json!("future_reason");
        assert!(Reply::decode(&bytes(&busy)).is_err());
        assert!(Reply::decode(&bytes(&attempt("future_reply"))).is_err());
        assert_eq!(
            Request::decode(b"{secret-value").err().unwrap().to_string(),
            "invalid project secret idle message"
        );
    }

    #[test]
    fn identity_nonce_deadline_and_frame_bounds_are_enforced() {
        for (field, invalid) in [
            ("version", json!(2)),
            ("request_id", json!(0)),
            ("attempt_id", json!("11111111-1111-1111-1111-11111111111A")),
            ("pending_id", json!(null)),
            ("expected_applied_revision", json!(REVISION_MAX + 1)),
        ] {
            let mut v = attempt("inspect");
            v[field] = invalid;
            assert!(Request::decode(&bytes(&v)).is_err(), "{field}");
        }
        for ttl in [0, DEADLINE_MAX_MS + 1] {
            let mut v = attempt("prepare");
            v["expires_in_ms"] = json!(ttl);
            assert!(Request::decode(&bytes(&v)).is_err());
        }
        let mut abort = attempt("abort");
        abort["fence_id"] = json!("B".repeat(43));
        assert!(Request::decode(&bytes(&abort)).is_err());
        assert!(Request::decode(&[]).is_err());
        assert!(Request::decode(&vec![b' '; REQUEST_MAX + 1]).is_err());
        assert!(Reply::decode(&vec![b' '; REPLY_MAX + 1]).is_err());
        let mut request = Request::decode(&bytes(&attempt("inspect"))).unwrap();
        if let Request::Inspect(v) = &mut request {
            v.binding.workspace_id = "x".repeat(129);
        }
        assert!(request.encode().is_err());
    }

    #[test]
    fn prepared_leaders_are_bounded_and_empty_list_is_structure_only() {
        let mut v = attempt("prepared");
        v["fence_id"] = json!(authority());
        v["remaining_ms"] = json!(30000);
        v["leaders"] = json!([]);
        assert!(Reply::decode(&bytes(&v)).is_ok());
        v["leaders"] = json!([leader(), leader()]);
        assert!(Reply::decode(&bytes(&v)).is_err());
        v["leaders"] = json!([leader()]);
        v["leaders"][0]["namespace_pid"] = json!(0);
        assert!(Reply::decode(&bytes(&v)).is_err());
        v["leaders"] = json!([leader()]);
        v["leaders"][0]["start_ticks"] = json!(0);
        assert!(Reply::decode(&bytes(&v)).is_err());
        v["leaders"] = json!((0..=LEADERS_MAX)
            .map(
                |i| json!({"session_id":format!("session-{i}"),"namespace_pid":i+1,"start_ticks":1})
            )
            .collect::<Vec<_>>());
        assert!(Reply::decode(&bytes(&v)).is_err());
    }
}
