//! Fixed login-only startup and command consumer. This module is not attached to
//! ordinary daemon routes; a runtime/project bearer cannot enroll it.
pub use super::control_login::{
    FinishedLogin, LoginAction, LoginAttempt, LoginPhase, LoginStatus, PendingLogin,
};
pub use super::login_home::{Credential, LoginHome};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fmt, os::fd::OwnedFd, time::Duration};
use zeroize::Zeroizing;

const STARTUP_BYTES: usize = 4096;
const COMMAND_BYTES: usize = 8192;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidStartup,
    InvalidCommand,
    Unauthorized,
    Changed,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidStartup => "Provider control startup refused",
            Self::InvalidCommand => "Provider control command refused",
            Self::Unauthorized => "Provider control authorization refused",
            Self::Changed => "Provider control registration changed",
        })
    }
}
impl std::error::Error for Error {}

fn id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
fn uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
}
fn credential_digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
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
    fn valid(&self) -> bool {
        self.version == 1
            && id(&self.account_id)
            && id(&self.holder_id)
            && uuid(&self.process_boot)
            && self.registration_generation > 0
            && credential_digest(&self.worker_credential_digest)
    }
}

/// No Debug or serialization: the startup capability never enters status.
pub struct ControlBinding {
    registration: Registration,
    capability: Zeroizing<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Startup {
    version: u32,
    account_id: String,
    holder_id: String,
    process_boot: String,
    registration_generation: u64,
    worker_credential_digest: String,
    capability: Zeroizing<String>,
}
impl ControlBinding {
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > STARTUP_BYTES {
            return Err(Error::InvalidStartup);
        }
        let s: Startup = serde_json::from_slice(bytes).map_err(|_| Error::InvalidStartup)?;
        let registration = Registration {
            version: s.version,
            account_id: s.account_id,
            holder_id: s.holder_id,
            process_boot: s.process_boot,
            registration_generation: s.registration_generation,
            worker_credential_digest: s.worker_credential_digest,
        };
        if !registration.valid()
            || !(43..=256).contains(&s.capability.len())
            || !s
                .capability
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err(Error::InvalidStartup);
        }
        Ok(Self {
            registration,
            capability: s.capability,
        })
    }
    pub(super) fn agrees(&self, command: &ControlCommand) -> bool {
        self.registration == command.registration
    }
    pub(super) fn capability_for_service(&self) -> &str {
        &self.capability
    }
    pub(super) fn authenticates(&self, capability: &str) -> bool {
        let mut unequal = self.capability.len() ^ capability.len();
        for (i, expected) in self.capability.bytes().enumerate() {
            unequal |= usize::from(expected ^ capability.as_bytes().get(i).copied().unwrap_or(0));
        }
        unequal == 0
    }
    pub fn acknowledgment(&self) -> Registration {
        self.registration.clone()
    }

    /// The caller is the fixed supervisor/keeper adapter, which derives device
    /// identity from fresh account authentication. This method never accepts a
    /// runtime capability or enrolls a target from the supplied registration.
    pub fn consume(
        &self,
        capability: &str,
        registration: &Registration,
        authenticated_device: &str,
        bytes: Vec<u8>,
    ) -> Result<ControlCommand, Error> {
        let bytes = Zeroizing::new(bytes);
        if !self.authenticates(capability) {
            return Err(Error::Unauthorized);
        }
        if &self.registration != registration {
            return Err(Error::Changed);
        }
        if !id(authenticated_device) || bytes.len() > COMMAND_BYTES {
            return Err(Error::InvalidCommand);
        }
        let raw: RawCommand = serde_json::from_slice(&bytes).map_err(|_| Error::InvalidCommand)?;
        if raw.version != 1 || !uuid(&raw.operation_id) {
            return Err(Error::InvalidCommand);
        }
        let command = match raw.command {
            RawAction::Connect {} => Action::Connect,
            RawAction::Disconnect {
                acknowledge_cloud_work: true,
            } => Action::Disconnect,
            RawAction::Disconnect { .. } => return Err(Error::InvalidCommand),
            RawAction::Cancel { attempt_id } if uuid(&attempt_id) => Action::Cancel { attempt_id },
            RawAction::Submit {
                attempt_id,
                submission_nonce,
                code,
            } if uuid(&attempt_id)
                && uuid(&submission_nonce)
                && !code.is_empty()
                && code.len() <= 4096
                && !code.chars().any(|c| c.is_control() || c.is_whitespace()) =>
            {
                if raw.provider == Provider::Claude && !super::connect::claude::complete_code(&code)
                {
                    return Err(Error::InvalidCommand);
                }
                Action::Submit {
                    attempt_id,
                    submission_nonce,
                    code,
                }
            }
            _ => return Err(Error::InvalidCommand),
        };
        Ok(ControlCommand {
            operation_id: raw.operation_id,
            provider: raw.provider,
            expected_connection_generation: raw.expected_connection_generation,
            authenticated_device: authenticated_device.into(),
            registration: self.registration.clone(),
            command,
        })
    }
}

/// A private startup pipe, not a project-controlled file or ordinary bearer
/// request. Owned descriptor consumption closes the channel on every failure.
pub async fn read_control_startup(input: OwnedFd) -> Result<ControlBinding, Error> {
    let stat = rustix::fs::fstat(&input).map_err(|_| Error::InvalidStartup)?;
    if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::Fifo {
        return Err(Error::InvalidStartup);
    }
    let flags = rustix::fs::fcntl_getfl(&input).map_err(|_| Error::InvalidStartup)?;
    rustix::fs::fcntl_setfl(&input, flags | rustix::fs::OFlags::NONBLOCK)
        .map_err(|_| Error::InvalidStartup)?;
    let input = tokio::io::unix::AsyncFd::new(input).map_err(|_| Error::InvalidStartup)?;
    let mut bytes = Zeroizing::new(Vec::with_capacity(STARTUP_BYTES));
    tokio::time::timeout(STARTUP_TIMEOUT, async {
        let mut buf = Zeroizing::new([0u8; 1024]);
        loop {
            let mut ready = input.readable().await.map_err(|_| Error::InvalidStartup)?;
            let read = ready.try_io(|fd| {
                rustix::io::read(fd.get_ref(), &mut buf[..]).map_err(std::io::Error::from)
            });
            match read {
                Ok(Ok(0)) => return Ok(()),
                Ok(Ok(n)) if bytes.len() + n <= STARTUP_BYTES => bytes.extend_from_slice(&buf[..n]),
                Ok(Ok(_)) | Ok(Err(_)) => return Err(Error::InvalidStartup),
                Err(_) => continue,
            }
        }
    })
    .await
    .map_err(|_| Error::InvalidStartup)??;
    ControlBinding::parse(&bytes)
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Claude,
    Codex,
    Github,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCommand {
    version: u32,
    operation_id: String,
    provider: Provider,
    expected_connection_generation: u64,
    command: RawAction,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum RawAction {
    Connect {},
    Cancel {
        attempt_id: String,
    },
    Submit {
        attempt_id: String,
        submission_nonce: String,
        code: Zeroizing<String>,
    },
    Disconnect {
        acknowledge_cloud_work: bool,
    },
}

/// Secret-bearing commands have neither Debug nor Serialize. Consumers may only
/// dispatch these fixed variants; no shell/path/helper payload exists.
pub enum Action {
    Connect,
    Cancel {
        attempt_id: String,
    },
    Submit {
        attempt_id: String,
        submission_nonce: String,
        code: Zeroizing<String>,
    },
    Disconnect,
}
pub struct ControlCommand {
    operation_id: String,
    provider: Provider,
    expected_connection_generation: u64,
    authenticated_device: String,
    registration: Registration,
    command: Action,
}
impl ControlCommand {
    pub(super) fn into_action(self) -> Action {
        self.command
    }
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }
    pub fn provider(&self) -> Provider {
        self.provider
    }
    pub fn expected_connection_generation(&self) -> u64 {
        self.expected_connection_generation
    }
    pub fn authenticated_device(&self) -> &str {
        &self.authenticated_device
    }
    pub fn registration(&self) -> &Registration {
        &self.registration
    }
    pub fn action(&self) -> &Action {
        &self.command
    }
    /// Secret values are deliberately excluded. A consumed submission nonce is
    /// idempotent even if a retry carries another value; neither may be resent.
    pub fn nonsensitive_digest(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        for part in [
            self.operation_id.as_str(),
            self.authenticated_device.as_str(),
            self.registration.account_id.as_str(),
            self.registration.holder_id.as_str(),
            self.registration.process_boot.as_str(),
        ] {
            hash.update((part.len() as u64).to_be_bytes());
            hash.update(part.as_bytes());
        }
        hash.update(self.registration.registration_generation.to_be_bytes());
        hash.update(self.registration.worker_credential_digest.as_bytes());
        hash.update(self.expected_connection_generation.to_be_bytes());
        hash.update([match self.provider {
            Provider::Claude => 0,
            Provider::Codex => 1,
            Provider::Github => 2,
        }]);
        let (kind, attempt, nonce) = match &self.command {
            Action::Connect => (0, "", ""),
            Action::Cancel { attempt_id } => (1, attempt_id.as_str(), ""),
            Action::Submit {
                attempt_id,
                submission_nonce,
                ..
            } => (2, attempt_id.as_str(), submission_nonce.as_str()),
            Action::Disconnect => (3, "", ""),
        };
        hash.update([kind]);
        hash.update(attempt.as_bytes());
        hash.update(nonce.as_bytes());
        hash.finalize().into()
    }
}

#[cfg(test)]
#[path = "authority_tests.rs"]
mod tests;
