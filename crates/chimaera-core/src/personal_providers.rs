//! Pure closed personal-provider control DTOs. No transport or runtime authority.
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub const BODY_MAX: usize = 64 * 1024;
pub const COMMAND_MAX: usize = 8 * 1024;
pub const COUNTER_MAX: u64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Unsupported,
    InvalidRequest,
    StateChanged,
    Unavailable,
    OperationUnavailable,
    LimitReached,
    SignInRequired,
    ContextChanged,
    Unconfirmed,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unsupported => "providers_unsupported",
            Self::InvalidRequest => "providers_invalid_request",
            Self::StateChanged => "providers_state_changed",
            Self::Unavailable => "providers_unavailable",
            Self::OperationUnavailable => "providers_operation_unavailable",
            Self::LimitReached => "providers_limit_reached",
            Self::SignInRequired => "providers_sign_in_required",
            Self::ContextChanged => "providers_context_changed",
            Self::Unconfirmed => "providers_unconfirmed",
        })
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Claude,
    Codex,
    Github,
}
impl Provider {
    pub fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Github => "github",
        }
    }
}
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Legacy,
    Personal,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModeReply {
    pub version: u16,
    pub context: String,
    pub mode: Mode,
}
impl ModeReply {
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
pub fn id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
pub fn operation_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
}
fn counter(value: u64) -> bool {
    value <= COUNTER_MAX
}
fn required<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    pub version: u32,
    pub account_id: String,
    pub holder_id: String,
    pub process_boot: String,
    pub registration_generation: u64,
    pub worker_credential_digest: String,
}
impl Registration {
    pub fn validate(&self) -> Result<(), Error> {
        if self.version == 1
            && id(&self.account_id)
            && id(&self.holder_id)
            && operation_id(&self.process_boot)
            && self.registration_generation > 0
            && counter(self.registration_generation)
            && context(&self.worker_credential_digest)
        {
            Ok(())
        } else {
            Err(Error::Unsupported)
        }
    }
}
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Disconnected,
    Connected,
    NeedsSignIn,
    RecoveryNeeded,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub provider: Provider,
    pub state: State,
    pub generation: u64,
    pub revision: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub providers_control: Registration,
    pub connections: [Connection; 3],
}
impl Catalog {
    pub fn validate(&self) -> Result<(), Error> {
        self.providers_control.validate()?;
        for (index, row) in self.connections.iter().enumerate() {
            if !counter(row.generation)
                || !counter(row.revision)
                || self.connections[..index]
                    .iter()
                    .any(|previous| previous.provider == row.provider)
                || row.state == State::Connected && row.revision == 0
                || row.state == State::Disconnected && row.revision != 0
            {
                return Err(Error::Unsupported);
            }
        }
        Ok(())
    }
    pub fn connection(&self, provider: Provider) -> Result<&Connection, Error> {
        self.validate()?;
        self.connections
            .iter()
            .find(|row| row.provider == provider)
            .ok_or(Error::Unsupported)
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogPage {
    pub version: u16,
    pub context: String,
    pub catalog: Catalog,
}
impl CatalogPage {
    pub fn validate(&self) -> Result<(), Error> {
        if self.version != 1 || !context(&self.context) {
            return Err(Error::Unsupported);
        }
        self.catalog.validate()
    }
}

/// Live one-shot input, never formatted, cloned or included in a digest.
pub struct Code(Zeroizing<String>);
impl Code {
    pub fn new(value: String) -> Result<Self, Error> {
        let value = Zeroizing::new(value);
        if value.is_empty()
            || value.len() > 4096
            || value.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(Error::InvalidRequest);
        }
        Ok(Self(value))
    }
}
impl<'de> Deserialize<'de> for Code {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(d)?).map_err(|_| serde::de::Error::custom("invalid code"))
    }
}
impl Serialize for Code {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Connect {},
    Disconnect {
        acknowledge_cloud_work: bool,
    },
    Cancel {
        attempt_id: String,
    },
    Submit {
        attempt_id: String,
        submission_nonce: String,
        code: Code,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub version: u32,
    pub operation_id: String,
    pub provider: Provider,
    pub expected_connection_generation: u64,
    pub command: Action,
}
impl Command {
    pub fn validate(&self) -> Result<(), Error> {
        if self.version != 1
            || !operation_id(&self.operation_id)
            || !counter(self.expected_connection_generation)
            || !match &self.command {
                Action::Connect {} => true,
                Action::Disconnect {
                    acknowledge_cloud_work,
                } => *acknowledge_cloud_work,
                Action::Cancel { attempt_id } => operation_id(attempt_id),
                Action::Submit {
                    attempt_id,
                    submission_nonce,
                    ..
                } => operation_id(attempt_id) && operation_id(submission_nonce),
            }
        {
            Err(Error::InvalidRequest)
        } else {
            Ok(())
        }
    }
    pub fn decode_owned(value: String) -> Result<Self, Error> {
        let value = Zeroizing::new(value);
        if value.len() > COMMAND_MAX {
            return Err(Error::InvalidRequest);
        }
        let command: Self = serde_json::from_str(&value).map_err(|_| Error::InvalidRequest)?;
        command.validate()?;
        Ok(command)
    }
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Connect,
    Disconnect,
}
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Preparing,
    Waiting,
    Verifying,
    Connected,
    Disconnected,
    Failed,
    Canceled,
    Expired,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum LoginAction {
    Browser {
        url: String,
        input: String,
    },
    DeviceCode {
        verification_url: String,
        user_code: String,
    },
}
impl LoginAction {
    pub fn url(&self) -> &str {
        match self {
            Self::Browser { url, .. } => url,
            Self::DeviceCode {
                verification_url, ..
            } => verification_url,
        }
    }
    fn valid(&self, provider: Provider) -> bool {
        let url = self.url();
        let shape = match self {
            Self::Browser { input, .. } => {
                provider == Provider::Claude && input == "authorization_code"
            }
            Self::DeviceCode { user_code, .. } => {
                provider != Provider::Claude
                    && (4..=32).contains(&user_code.len())
                    && user_code
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            }
        };
        if !shape || url.len() > 4096 {
            return false;
        }
        let Ok(url) = url::Url::parse(url) else {
            return false;
        };
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && crate::cloud_providers::provider_auth_origins(provider.id())
                .contains(&url.origin().ascii_serialization())
    }
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    Canceled,
    Expired,
    UnsupportedLogin,
    ControlUnavailable,
    CleanupFailed,
    AccountChanged,
    ProviderChanged,
    SignInFailed,
    PublicationFailed,
    InvalidAuthorizationCode,
    VerificationFailed,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    pub id: String,
    pub provider_id: Provider,
    pub operation: Operation,
    pub phase: Phase,
    pub expires_at: u64,
    #[serde(deserialize_with = "required")]
    pub action: Option<LoginAction>,
    #[serde(deserialize_with = "required")]
    pub error_code: Option<Failure>,
    pub control_version: u32,
    pub connection_generation: u64,
    pub credential_revision: u64,
    pub registration_generation: u64,
}
/// Nonsecret original identity survives an ambiguous send; polls cannot change it.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Original {
    pub context: String,
    pub operation_id: String,
    pub provider: Provider,
    pub operation: Operation,
    pub expected_connection_generation: u64,
    pub registration: Registration,
    #[serde(deserialize_with = "required")]
    pub attempt_id: Option<String>,
}
impl Original {
    pub fn validate(&self) -> Result<(), Error> {
        self.registration.validate()?;
        if !context(&self.context)
            || !operation_id(&self.operation_id)
            || !counter(self.expected_connection_generation)
            || self.attempt_id.as_ref().is_some_and(|id| !operation_id(id))
        {
            return Err(Error::InvalidRequest);
        }
        Ok(())
    }
}
impl Command {
    pub fn validate_original(&self, original: &Original) -> Result<(), Error> {
        self.validate()?;
        original.validate()?;
        let matches = self.provider == original.provider
            && self.expected_connection_generation == original.expected_connection_generation
            && match &self.command {
                Action::Connect {} => {
                    self.operation_id == original.operation_id
                        && original.operation == Operation::Connect
                        && original.attempt_id.is_none()
                }
                Action::Disconnect { .. } => {
                    self.operation_id == original.operation_id
                        && original.operation == Operation::Disconnect
                        && original.attempt_id.is_none()
                }
                Action::Submit { attempt_id, .. } => {
                    self.operation_id != original.operation_id
                        && original.operation == Operation::Connect
                        && original.attempt_id.as_ref() == Some(attempt_id)
                }
                Action::Cancel { attempt_id } => {
                    self.operation_id != original.operation_id
                        && original.attempt_id.as_ref() == Some(attempt_id)
                }
            };
        if matches {
            Ok(())
        } else {
            Err(Error::InvalidRequest)
        }
    }
}
impl Attempt {
    pub fn validate(&self, original: &Original) -> Result<(), Error> {
        original.validate()?;
        let committed = matches!(self.phase, Phase::Connected | Phase::Disconnected);
        let generation = if committed {
            original.expected_connection_generation.checked_add(1)
        } else {
            Some(original.expected_connection_generation)
        };
        if !operation_id(&self.id)
            || self.provider_id != original.provider
            || self.operation != original.operation
            || self.control_version != 1
            || self.registration_generation != original.registration.registration_generation
            || generation != Some(self.connection_generation)
            || !counter(self.connection_generation)
            || !counter(self.credential_revision)
            || !counter(self.expires_at)
            || original
                .attempt_id
                .as_ref()
                .is_some_and(|id| id != &self.id)
            || self.phase == Phase::Connected
                && (self.operation != Operation::Connect || self.credential_revision == 0)
            || self.phase == Phase::Disconnected
                && (self.operation != Operation::Disconnect || self.credential_revision != 0)
            || self.action.as_ref().is_some_and(|action| {
                self.phase != Phase::Waiting || !action.valid(self.provider_id)
            })
        {
            Err(Error::Unconfirmed)
        } else {
            Ok(())
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandResult {
    pub version: u16,
    pub context: String,
    pub operation_id: String,
    pub attempt: Attempt,
}
