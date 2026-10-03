//! Closed local provider correlation and framing. These types confer no authority.
//! Consumers must retain the sealed controller admission and authentic startup FD.
use crate::personal_providers::{id, operation_id, Registration, COUNTER_MAX};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub const CONTROL_MAX: usize = 128 * 1024;
pub const DATA_MAX: usize = 64 * 1024;
pub const BODY_MAX: u64 = 16 * 1024 * 1024;
pub const ACCESS_MAX: usize = 32 * 1024;
pub const STREAMS_GLOBAL: usize = 16;
pub const STREAMS_PROJECT: usize = 8;
pub const QUEUE_MAX: usize = 16;
pub const STARTUP_MAX: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Error {
    Unsupported,
    InvalidRequest,
    Inactive,
    StateChanged,
    Unavailable,
    NeedsSignIn,
    LimitReached,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unsupported => "provider_runtime_unsupported",
            Self::InvalidRequest => "provider_runtime_invalid_request",
            Self::Inactive => "provider_runtime_inactive",
            Self::StateChanged => "provider_runtime_state_changed",
            Self::Unavailable => "provider_runtime_unavailable",
            Self::NeedsSignIn => "provider_runtime_needs_sign_in",
            Self::LimitReached => "provider_runtime_limit_reached",
        })
    }
}
impl std::error::Error for Error {}
fn positive(value: u64) -> bool {
    value > 0 && value <= COUNTER_MAX
}
fn uuid(value: &str) -> bool {
    operation_id(value) && value != "00000000-0000-0000-0000-000000000000"
}
fn identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
}
fn required<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}

/// Correlation only. Deserialization never creates an Admission or activates a socket.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub version: u16,
    pub account_id: String,
    pub workspace_id: String,
    pub project_revision: u64,
    pub launch_generation: u64,
    pub enrollment: Registration,
}
impl Binding {
    pub fn validate(&self) -> Result<(), Error> {
        self.enrollment
            .validate()
            .map_err(|_| Error::InvalidRequest)?;
        if self.version != 1
            || !id(&self.account_id)
            || !id(&self.workspace_id)
            || self.account_id != self.enrollment.account_id
            || !positive(self.project_revision)
            || !positive(self.launch_generation)
            || !uuid(&self.enrollment.process_boot)
        {
            return Err(Error::InvalidRequest);
        }
        Ok(())
    }
}

/// Never Debug: only the trusted inherited-FD consumer may retain this value.
pub struct Capability(Zeroizing<String>);
impl Capability {
    pub fn new(value: String) -> Result<Self, Error> {
        let value = Zeroizing::new(value);
        if value.len() != 43
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            || !matches!(
                value.as_bytes()[42],
                b'A' | b'E'
                    | b'I'
                    | b'M'
                    | b'Q'
                    | b'U'
                    | b'Y'
                    | b'c'
                    | b'g'
                    | b'k'
                    | b'o'
                    | b's'
                    | b'w'
                    | b'0'
                    | b'4'
                    | b'8'
            )
        {
            return Err(Error::InvalidRequest);
        }
        Ok(Self(value))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for Capability {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(d)?)
            .map_err(|_| serde::de::Error::custom("invalid runtime capability"))
    }
}
impl Serialize for Capability {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.expose())
    }
}

/// Nonsecret cleanup metadata only. Validation does not inspect or own an FD.
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StartupDescriptor {
    pub version: u16,
    pub fd: i32,
}
impl StartupDescriptor {
    pub fn validate(&self, maintenance_fd: Option<i32>) -> Result<(), Error> {
        if self.version == 1 && (3..=255).contains(&self.fd) && maintenance_fd != Some(self.fd) {
            Ok(())
        } else {
            Err(Error::InvalidRequest)
        }
    }
}

/// Moved once from the trusted pipe. No Clone/Debug or serialized authority proof.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartupPayload {
    pub version: u16,
    pub binding: Binding,
    pub capability: Capability,
}
impl StartupPayload {
    pub fn validate(&self, expected: &Binding) -> Result<(), Error> {
        self.binding.validate()?;
        expected.validate()?;
        if self.version == 1 && self.binding == *expected {
            Ok(())
        } else {
            Err(Error::InvalidRequest)
        }
    }
    /// Decode once before the consumer obtains current enrollment from Ready.
    /// This validates structure only; the consumer must separately correlate it.
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.is_empty() || bytes.len() > STARTUP_MAX {
            return Err(Error::InvalidRequest);
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| Error::InvalidRequest)?;
        value.validate(&value.binding)?;
        Ok(value)
    }
    pub fn encode(&self) -> Result<Zeroizing<Vec<u8>>, Error> {
        self.validate(&self.binding)?;
        encode_bounded(self, STARTUP_MAX)
    }
}
pub struct AccessToken(Zeroizing<String>);
impl AccessToken {
    pub fn new(value: String) -> Result<Self, Error> {
        let value = Zeroizing::new(value);
        if value.is_empty()
            || value.len() > ACCESS_MAX
            || !value.bytes().all(|b| (33..=126).contains(&b))
        {
            return Err(Error::InvalidRequest);
        }
        Ok(Self(value))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for AccessToken {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(d)?)
            .map_err(|_| serde::de::Error::custom("invalid runtime access"))
    }
}
impl Serialize for AccessToken {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.expose())
    }
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeRoute {
    Messages,
    CountTokens,
}
/// Nonsecret compatibility metadata for the exact reviewed CLI, never auth.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaudeVersion {
    #[serde(rename = "2023-06-01")]
    V20230601,
}
impl ClaudeVersion {
    pub fn as_str(self) -> &'static str {
        "2023-06-01"
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaudeUserAgent {
    #[serde(rename = "claude-cli/2.1.287 (external, cli)")]
    Cli21287,
    #[serde(rename = "claude-cli/2.1.287 (external, sdk-cli)")]
    SdkCli21287,
}
impl ClaudeUserAgent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cli21287 => "claude-cli/2.1.287 (external, cli)",
            Self::SdkCli21287 => "claude-cli/2.1.287 (external, sdk-cli)",
        }
    }
    pub fn from_http(value: &str) -> Result<Self, Error> {
        match value {
            "claude-cli/2.1.287 (external, cli)" => Ok(Self::Cli21287),
            "claude-cli/2.1.287 (external, sdk-cli)" => Ok(Self::SdkCli21287),
            _ => Err(Error::InvalidRequest),
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaudeBeta {
    #[serde(rename = "claude-code-20250219")]
    ClaudeCode,
    #[serde(rename = "oauth-2025-04-20")]
    Oauth,
    #[serde(rename = "interleaved-thinking-2025-05-14")]
    InterleavedThinking,
    #[serde(rename = "redact-thinking-2026-02-12")]
    RedactThinking,
    #[serde(rename = "thinking-token-count-2026-05-13")]
    ThinkingTokenCount,
    #[serde(rename = "context-management-2025-06-27")]
    ContextManagement,
    #[serde(rename = "prompt-caching-scope-2026-01-05")]
    PromptCachingScope,
    #[serde(rename = "mid-conversation-system-2026-04-07")]
    MidConversationSystem,
    #[serde(rename = "per-turn-control-2026-07-01")]
    PerTurnControl,
    #[serde(rename = "mid-conversation-tool-changes-2026-07-01")]
    MidConversationToolChanges,
    #[serde(rename = "effort-2025-11-24")]
    Effort,
    #[serde(rename = "structured-outputs-2025-12-15")]
    StructuredOutputs,
    #[serde(rename = "dangerous-tool-use-2026-09-03")]
    DangerousToolUse,
    #[serde(rename = "thinking-display-updates-2026-08-18")]
    ThinkingDisplayUpdates,
    #[serde(rename = "afk-mode-2026-01-31")]
    AfkMode,
    #[serde(rename = "extended-cache-ttl-2025-04-11")]
    ExtendedCacheTtl,
    #[serde(rename = "token-counting-2024-11-01")]
    TokenCounting,
}
impl ClaudeBeta {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code-20250219",
            Self::Oauth => "oauth-2025-04-20",
            Self::InterleavedThinking => "interleaved-thinking-2025-05-14",
            Self::RedactThinking => "redact-thinking-2026-02-12",
            Self::ThinkingTokenCount => "thinking-token-count-2026-05-13",
            Self::ContextManagement => "context-management-2025-06-27",
            Self::PromptCachingScope => "prompt-caching-scope-2026-01-05",
            Self::MidConversationSystem => "mid-conversation-system-2026-04-07",
            Self::PerTurnControl => "per-turn-control-2026-07-01",
            Self::MidConversationToolChanges => "mid-conversation-tool-changes-2026-07-01",
            Self::Effort => "effort-2025-11-24",
            Self::StructuredOutputs => "structured-outputs-2025-12-15",
            Self::DangerousToolUse => "dangerous-tool-use-2026-09-03",
            Self::ThinkingDisplayUpdates => "thinking-display-updates-2026-08-18",
            Self::AfkMode => "afk-mode-2026-01-31",
            Self::ExtendedCacheTtl => "extended-cache-ttl-2025-04-11",
            Self::TokenCounting => "token-counting-2024-11-01",
        }
    }
    fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "claude-code-20250219" => Ok(Self::ClaudeCode),
            "oauth-2025-04-20" => Ok(Self::Oauth),
            "interleaved-thinking-2025-05-14" => Ok(Self::InterleavedThinking),
            "redact-thinking-2026-02-12" => Ok(Self::RedactThinking),
            "thinking-token-count-2026-05-13" => Ok(Self::ThinkingTokenCount),
            "context-management-2025-06-27" => Ok(Self::ContextManagement),
            "prompt-caching-scope-2026-01-05" => Ok(Self::PromptCachingScope),
            "mid-conversation-system-2026-04-07" => Ok(Self::MidConversationSystem),
            "per-turn-control-2026-07-01" => Ok(Self::PerTurnControl),
            "mid-conversation-tool-changes-2026-07-01" => Ok(Self::MidConversationToolChanges),
            "effort-2025-11-24" => Ok(Self::Effort),
            "structured-outputs-2025-12-15" => Ok(Self::StructuredOutputs),
            "dangerous-tool-use-2026-09-03" => Ok(Self::DangerousToolUse),
            "thinking-display-updates-2026-08-18" => Ok(Self::ThinkingDisplayUpdates),
            "afk-mode-2026-01-31" => Ok(Self::AfkMode),
            "extended-cache-ttl-2025-04-11" => Ok(Self::ExtendedCacheTtl),
            "token-counting-2024-11-01" => Ok(Self::TokenCounting),
            _ => Err(Error::InvalidRequest),
        }
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeRequestHeaders {
    pub version: ClaudeVersion,
    pub user_agent: ClaudeUserAgent,
    pub beta: Vec<ClaudeBeta>,
}
impl ClaudeRequestHeaders {
    pub fn from_http(
        version: &str,
        beta: &str,
        user_agent: &str,
        route: ClaudeRoute,
    ) -> Result<Self, Error> {
        if version != ClaudeVersion::V20230601.as_str()
            || beta.is_empty()
            || beta.len() > 512
            || beta.split(',').count() > 17
        {
            return Err(Error::InvalidRequest);
        }
        let headers = Self {
            version: ClaudeVersion::V20230601,
            user_agent: ClaudeUserAgent::from_http(user_agent)?,
            beta: beta
                .split(',')
                .map(ClaudeBeta::parse)
                .collect::<Result<_, _>>()?,
        };
        headers.validate(route)?;
        Ok(headers)
    }
    fn validate_atoms(&self) -> Result<usize, Error> {
        if self.beta.is_empty() || self.beta.len() > 17 {
            return Err(Error::InvalidRequest);
        }
        let mut seen = 0u32;
        let mut length = self.beta.len() - 1;
        for beta in &self.beta {
            let bit = 1u32 << (*beta as u32);
            if seen & bit != 0 {
                return Err(Error::InvalidRequest);
            }
            seen |= bit;
            length += beta.as_str().len();
        }
        if length > 512 {
            return Err(Error::InvalidRequest);
        }
        Ok(length)
    }
    pub fn validate(&self, route: ClaudeRoute) -> Result<(), Error> {
        self.validate_atoms()?;
        use ClaudeBeta::*;
        if !self.beta.contains(&ClaudeCode) || !self.beta.contains(&Oauth) {
            return Err(Error::InvalidRequest);
        }
        match route {
            ClaudeRoute::Messages if self.beta.contains(&TokenCounting) => {
                Err(Error::InvalidRequest)
            }
            ClaudeRoute::CountTokens if self.user_agent == ClaudeUserAgent::SdkCli21287 => {
                Err(Error::InvalidRequest)
            }
            ClaudeRoute::CountTokens
                if self.beta
                    != [
                        ClaudeCode,
                        Oauth,
                        InterleavedThinking,
                        ContextManagement,
                        TokenCounting,
                    ] =>
            {
                Err(Error::InvalidRequest)
            }
            _ => Ok(()),
        }
    }
    /// Bounded fixed atoms only; callers separately validate the admitted route.
    pub fn beta_header(&self) -> Result<String, Error> {
        let length = self.validate_atoms()?;
        let mut value = String::with_capacity(length);
        for beta in &self.beta {
            if !value.is_empty() {
                value.push(',');
            }
            value.push_str(beta.as_str());
        }
        Ok(value)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Ready {},
    CodexAccess {},
    CodexRefresh {
        connection_generation: u64,
        observed_revision: u64,
    },
    GithubGhAccess {},
    GithubHttpsCredentials {
        protocol: String,
        host: String,
    },
    // Preserved wire shape; canonical subscription adapters refuse headerless requests.
    ClaudeStream {
        route: ClaudeRoute,
        content_length: u64,
    },
    ClaudeStreamPinned {
        route: ClaudeRoute,
        content_length: u64,
        headers: ClaudeRequestHeaders,
    },
}
impl Command {
    pub fn validate(&self) -> Result<(), Error> {
        match self {
            Self::CodexRefresh {
                connection_generation,
                observed_revision,
            } if !positive(*connection_generation) || *observed_revision > COUNTER_MAX => {
                Err(Error::InvalidRequest)
            }
            Self::GithubHttpsCredentials { protocol, host }
                if protocol != "https" || host != "github.com" =>
            {
                Err(Error::InvalidRequest)
            }
            Self::ClaudeStream { content_length, .. }
            | Self::ClaudeStreamPinned { content_length, .. }
                if *content_length == 0 || *content_length > BODY_MAX =>
            {
                Err(Error::InvalidRequest)
            }
            Self::ClaudeStreamPinned { route, headers, .. } => headers.validate(*route),
            _ => Ok(()),
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u16,
    pub binding: Binding,
    pub request_id: String,
    pub capability: Capability,
    pub command: Command,
}
impl Request {
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let request: Self = decode(bytes)?;
        request.validate()?;
        Ok(request)
    }
    pub fn validate(&self) -> Result<(), Error> {
        self.binding.validate()?;
        if self.version != 1 || !uuid(&self.request_id) {
            return Err(Error::InvalidRequest);
        }
        self.command.validate()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodexAccess {
    pub access_token: AccessToken,
    pub connection_generation: u64,
    pub credential_revision: u64,
    pub expires_at: u64,
    pub chatgpt_user_id: String,
    pub chatgpt_account_id: String,
    #[serde(deserialize_with = "required")]
    pub chatgpt_plan_type: Option<String>,
}
impl CodexAccess {
    fn validate(&self) -> Result<(), Error> {
        if !positive(self.connection_generation)
            || !positive(self.credential_revision)
            || !positive(self.expires_at)
            || !identity(&self.chatgpt_user_id)
            || !identity(&self.chatgpt_account_id)
            || self
                .chatgpt_plan_type
                .as_ref()
                .is_some_and(|v| !identity(v))
        {
            return Err(Error::InvalidRequest);
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GithubAccess {
    pub access_token: AccessToken,
    pub username: String,
    pub connection_generation: u64,
    pub credential_revision: u64,
    #[serde(deserialize_with = "required")]
    pub expires_at: Option<u64>,
}
impl GithubAccess {
    fn validate(&self) -> Result<(), Error> {
        if self.username != "x-access-token"
            || !positive(self.connection_generation)
            || !positive(self.credential_revision)
            || self.expires_at.is_some_and(|v| !positive(v))
        {
            return Err(Error::InvalidRequest);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ContentType {
    #[serde(rename = "application/json")]
    Json,
    #[serde(rename = "text/event-stream")]
    EventStream,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Headers {
    pub content_type: ContentType,
    #[serde(deserialize_with = "required")]
    pub retry_after_seconds: Option<u16>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeHead {
    pub status: u16,
    pub headers: Headers,
}
/// Collapse unlisted statuses; there is no redirect or raw diagnostic forwarding.
pub fn upstream_status(status: u16) -> u16 {
    match status {
        200 | 400 | 401 | 403 | 429 | 500 | 502 | 503 | 504 => status,
        529 => 503,
        _ => 502,
    }
}
impl ClaudeHead {
    fn validate(&self, route: ClaudeRoute) -> Result<(), Error> {
        if upstream_status(self.status) != self.status
            || self
                .headers
                .retry_after_seconds
                .is_some_and(|v| v == 0 || v > 3600)
            || (self.status != 200 || route == ClaudeRoute::CountTokens)
                && self.headers.content_type != ContentType::Json
        {
            return Err(Error::InvalidRequest);
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Ready { ready: bool },
    CodexAccess { access: CodexAccess },
    GithubAccess { access: GithubAccess },
    ClaudeHead { head: ClaudeHead },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub version: u16,
    pub binding: Binding,
    pub request_id: String,
    pub result: Reply,
}
impl Response {
    pub fn decode(bytes: &[u8], request: &Request) -> Result<Self, Error> {
        let response: Self = decode(bytes)?;
        response.validate(request)?;
        Ok(response)
    }
    pub fn validate(&self, request: &Request) -> Result<(), Error> {
        request.validate()?;
        if self.version != 1
            || self.binding != request.binding
            || self.request_id != request.request_id
        {
            return Err(Error::StateChanged);
        }
        match (&self.result, &request.command) {
            (Reply::Ready { ready: true }, Command::Ready {}) => Ok(()),
            (Reply::CodexAccess { access }, Command::CodexAccess {}) => access.validate(),
            (
                Reply::CodexAccess { access },
                Command::CodexRefresh {
                    connection_generation,
                    observed_revision,
                },
            ) => {
                access.validate()?;
                if access.connection_generation != *connection_generation
                    || access.credential_revision <= *observed_revision
                {
                    Err(Error::StateChanged)
                } else {
                    Ok(())
                }
            }
            (
                Reply::GithubAccess { access },
                Command::GithubGhAccess {} | Command::GithubHttpsCredentials { .. },
            ) => access.validate(),
            (
                Reply::ClaudeHead { head },
                Command::ClaudeStream { route, .. } | Command::ClaudeStreamPinned { route, .. },
            ) => head.validate(*route),
            _ => Err(Error::StateChanged),
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamEnd {
    pub version: u16,
    pub binding: Binding,
    pub request_id: String,
    pub bytes: u64,
}
impl StreamEnd {
    pub fn validate(&self, request: &Request, actual: u64) -> Result<(), Error> {
        if self.version != 1
            || self.binding != request.binding
            || self.request_id != request.request_id
            || self.bytes != actual
            || self.bytes > COUNTER_MAX
        {
            return Err(Error::StateChanged);
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Refusal {
    pub version: u16,
    pub binding: Binding,
    pub request_id: String,
    pub error: Error,
}
impl Refusal {
    pub fn validate(&self, request: &Request) -> Result<(), Error> {
        if self.version != 1
            || self.binding != request.binding
            || self.request_id != request.request_id
        {
            Err(Error::StateChanged)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind {
    RequestBegin,
    RequestData,
    RequestEnd,
    ResponseBegin,
    ResponseData,
    ResponseEnd,
    Error,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameHeader {
    pub kind: FrameKind,
    pub length: usize,
}
impl FrameHeader {
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != 5 {
            return Err(Error::InvalidRequest);
        }
        let kind = match bytes[0] {
            0 => FrameKind::RequestBegin,
            1 => FrameKind::RequestData,
            2 => FrameKind::RequestEnd,
            3 => FrameKind::ResponseBegin,
            4 => FrameKind::ResponseData,
            5 => FrameKind::ResponseEnd,
            6 => FrameKind::Error,
            _ => return Err(Error::InvalidRequest),
        };
        let length =
            u32::from_be_bytes(bytes[1..].try_into().map_err(|_| Error::InvalidRequest)?) as usize;
        let data = matches!(kind, FrameKind::RequestData | FrameKind::ResponseData);
        if length == 0 || length > if data { DATA_MAX } else { CONTROL_MAX } {
            return Err(Error::InvalidRequest);
        }
        Ok(Self { kind, length })
    }
    pub fn check_payload(&self, payload: &[u8]) -> Result<(), Error> {
        if payload.len() == self.length {
            Ok(())
        } else {
            Err(Error::InvalidRequest)
        }
    }
}
/// Arithmetic only. The transport remains responsible for deadlines and ownership.
pub struct BodyCount {
    declared: u64,
    received: u64,
}
impl BodyCount {
    pub fn new(declared: u64) -> Result<Self, Error> {
        if declared == 0 || declared > BODY_MAX {
            return Err(Error::InvalidRequest);
        }
        Ok(Self {
            declared,
            received: 0,
        })
    }
    pub fn add(&mut self, bytes: usize) -> Result<(), Error> {
        if bytes == 0 || bytes > DATA_MAX {
            return Err(Error::InvalidRequest);
        }
        let next = self
            .received
            .checked_add(bytes as u64)
            .ok_or(Error::InvalidRequest)?;
        if next > self.declared {
            return Err(Error::InvalidRequest);
        }
        self.received = next;
        Ok(())
    }
    pub fn complete(&self) -> Result<u64, Error> {
        if self.received == self.declared {
            Ok(self.received)
        } else {
            Err(Error::InvalidRequest)
        }
    }
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    if bytes.is_empty() || bytes.len() > CONTROL_MAX {
        return Err(Error::InvalidRequest);
    }
    serde_json::from_slice(bytes).map_err(|_| Error::InvalidRequest)
}

/// Preallocate the whole bound so secret-bearing growth cannot free old buffers.
pub fn encode_control<T: Serialize>(value: &T) -> Result<Zeroizing<Vec<u8>>, Error> {
    encode_bounded(value, CONTROL_MAX)
}
fn encode_bounded<T: Serialize>(value: &T, limit: usize) -> Result<Zeroizing<Vec<u8>>, Error> {
    struct Writer {
        bytes: Zeroizing<Vec<u8>>,
        limit: usize,
    }
    impl std::io::Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit - self.bytes.len() {
                return Err(std::io::Error::other("control frame bound"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Writer {
        bytes: Zeroizing::new(Vec::with_capacity(limit)),
        limit,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| Error::InvalidRequest)?;
    Ok(writer.bytes)
}

/// Fixed locally generated Claude errors, never upstream messages or bodies.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeErrorBody {
    #[serde(rename = "type")]
    pub kind: String,
    pub error: ClaudeErrorDetail,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeErrorDetail {
    #[serde(rename = "type")]
    pub kind: String,
    pub message: String,
}
impl ClaudeErrorBody {
    pub fn for_status(status: u16) -> Result<Self, Error> {
        let (kind, message) = match status {
            400 => ("invalid_request_error", "The provider request was refused."),
            401 => (
                "authentication_error",
                "The provider connection needs sign-in.",
            ),
            403 => (
                "permission_error",
                "The provider connection refused this request.",
            ),
            429 => ("rate_limit_error", "The provider request is rate limited."),
            503 => (
                "overloaded_error",
                "The provider is temporarily unavailable.",
            ),
            500 | 502 | 504 => ("api_error", "The provider request is unavailable."),
            _ => return Err(Error::InvalidRequest),
        };
        Ok(Self {
            kind: "error".into(),
            error: ClaudeErrorDetail {
                kind: kind.into(),
                message: message.into(),
            },
        })
    }
    pub fn validate(&self, status: u16) -> Result<(), Error> {
        let expected = Self::for_status(status)?;
        if self.kind == expected.kind
            && self.error.kind == expected.error.kind
            && self.error.message == expected.error.message
        {
            Ok(())
        } else {
            Err(Error::InvalidRequest)
        }
    }
}

#[cfg(test)]
mod tests;
