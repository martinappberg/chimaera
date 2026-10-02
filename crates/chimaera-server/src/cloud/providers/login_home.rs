//! Fixed trusted login homes. No project path, settings or credential store is
//! accepted. Filesystem methods are synchronous and belong in an owned blocking
//! worker; the login coordinator retains its admission through final import.
use super::authority::{Action, ControlCommand, Error, Provider};
use base64::Engine;
use rustix::fs::{openat, Mode, OFlags};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};
use zeroize::Zeroizing;

const LOGIN_ROOT: &str = "/state/provider-login";
const MAX_LEAF: u64 = 128 * 1024;
const MAX_SECRET: usize = 32 * 1024;
fn uid() -> u32 {
    rustix::process::geteuid().as_raw()
}
fn flags() -> OFlags {
    OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK
}
fn directory(file: &File, private: bool) -> Result<(), Error> {
    let m = file.metadata().map_err(|_| Error::InvalidStartup)?;
    if !m.is_dir()
        || !(m.uid() == 0 || m.uid() == uid())
        || private && (m.uid() != uid() || m.mode() & 0o077 != 0)
    {
        return Err(Error::InvalidStartup);
    }
    if m.mode() & 0o022 != 0 {
        #[cfg(test)]
        if m.mode() & 0o1000 != 0 {
            return Ok(());
        }
        return Err(Error::InvalidStartup);
    }
    Ok(())
}
fn pin(path: &Path) -> Result<File, Error> {
    if !path.is_absolute() {
        return Err(Error::InvalidStartup);
    }
    let mut current = File::from(
        rustix::fs::open("/", flags() | OFlags::DIRECTORY, Mode::empty())
            .map_err(|_| Error::InvalidStartup)?,
    );
    for component in path.components() {
        match component {
            Component::RootDir => continue,
            Component::Normal(name) => {
                directory(&current, false)?;
                current = File::from(
                    openat(&current, name, flags() | OFlags::DIRECTORY, Mode::empty())
                        .map_err(|_| Error::InvalidStartup)?,
                );
            }
            _ => return Err(Error::InvalidStartup),
        }
    }
    directory(&current, true)?;
    Ok(current)
}
fn regular(file: &File) -> Result<(), Error> {
    let m = file.metadata().map_err(|_| Error::InvalidCommand)?;
    if !m.is_file()
        || m.uid() != uid()
        || m.nlink() != 1
        || m.mode() & 0o077 != 0
        || m.len() > MAX_LEAF
    {
        return Err(Error::InvalidCommand);
    }
    Ok(())
}

pub struct LoginHome {
    root: File,
    path: PathBuf,
    provider: Provider,
}
impl LoginHome {
    /// The caller must retain its registered control admission/cleanup owner.
    /// This path is fixed in production; clients never supply it.
    pub fn prepare(command: &ControlCommand) -> Result<Self, Error> {
        Self::prepare_at(Path::new(LOGIN_ROOT), command)
    }
    fn prepare_at(prefix: &Path, command: &ControlCommand) -> Result<Self, Error> {
        if !matches!(command.action(), Action::Connect) {
            return Err(Error::InvalidCommand);
        }
        let parent = pin(prefix)?;
        let name = match command.provider() {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
            Provider::Github => "github",
        };
        match rustix::fs::mkdirat(&parent, name, Mode::from_raw_mode(0o700)) {
            Ok(()) => parent.sync_all().map_err(|_| Error::InvalidStartup)?,
            Err(rustix::io::Errno::EXIST) => {}
            Err(_) => return Err(Error::InvalidStartup),
        }
        let provider = File::from(
            openat(&parent, name, flags() | OFlags::DIRECTORY, Mode::empty())
                .map_err(|_| Error::InvalidStartup)?,
        );
        directory(&provider, true)?;
        // A previous attempt cannot be silently reused, even after caller abort.
        rustix::fs::mkdirat(
            &provider,
            command.operation_id(),
            Mode::from_raw_mode(0o700),
        )
        .map_err(|_| Error::InvalidStartup)?;
        provider.sync_all().map_err(|_| Error::InvalidStartup)?;
        let root = File::from(
            openat(
                &provider,
                command.operation_id(),
                flags() | OFlags::DIRECTORY,
                Mode::empty(),
            )
            .map_err(|_| Error::InvalidStartup)?,
        );
        directory(&root, true)?;
        Ok(Self {
            root,
            path: prefix.join(name).join(command.operation_id()),
            provider: command.provider(),
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Direct pinned image executable, never a login shell or client helper.
    /// Admission and owned process-group lifetime are the coordinator's job.
    pub fn official_login_command(&self) -> Result<tokio::process::Command, Error> {
        self.check()?;
        let (program, args): (&str, &[&str]) = match self.provider {
            Provider::Claude => ("/usr/local/bin/claude", &["auth", "login", "--claudeai"]),
            Provider::Codex => ("/usr/local/bin/codex", &["app-server"]),
            // Worker image installs the pinned official Debian package; its
            // goreleaser bindir=/usr and binary=bin/gh yields /usr/bin/gh.
            Provider::Github => (
                "/usr/bin/gh",
                &[
                    "auth",
                    "login",
                    "--hostname",
                    "github.com",
                    "--git-protocol",
                    "https",
                    "--web",
                    "--skip-ssh-key",
                ],
            ),
        };
        let mut command = tokio::process::Command::new(program);
        command.args(args);
        self.configure(&mut command);
        Ok(command)
    }
    fn configure(&self, command: &mut tokio::process::Command) {
        command
            .env_clear()
            .current_dir(&self.path)
            .env("HOME", &self.path)
            .env("CODEX_HOME", self.path.join(".codex"))
            .env("GH_CONFIG_DIR", self.path.join(".config/gh"))
            .env("XDG_CONFIG_HOME", self.path.join(".config"))
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .env("NO_COLOR", "1")
            .env("BROWSER", "true")
            .env("GH_BROWSER", "true")
            .env("GH_PROMPT_DISABLED", "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        // Only an async-signal-safe call runs between fork and exec. The outer
        // private home also blocks access to CLI-created nested directories.
        unsafe {
            command.pre_exec(|| {
                nix::libc::umask(0o077);
                Ok(())
            });
        }
    }
    pub fn check(&self) -> Result<(), Error> {
        let actual = pin(&self.path)?
            .metadata()
            .map_err(|_| Error::InvalidStartup)?;
        let retained = self.root.metadata().map_err(|_| Error::InvalidStartup)?;
        if actual.dev() != retained.dev() || actual.ino() != retained.ino() {
            return Err(Error::Changed);
        }
        Ok(())
    }
    fn read(&self, components: &[&str]) -> Result<Zeroizing<Vec<u8>>, Error> {
        self.check()?;
        let mut parent = self.root.try_clone().map_err(|_| Error::InvalidCommand)?;
        for name in &components[..components.len() - 1] {
            parent = File::from(
                openat(&parent, *name, flags() | OFlags::DIRECTORY, Mode::empty())
                    .map_err(|_| Error::InvalidCommand)?,
            );
            directory(&parent, false)?;
        }
        let mut file = File::from(
            openat(
                &parent,
                components[components.len() - 1],
                flags(),
                Mode::empty(),
            )
            .map_err(|_| Error::InvalidCommand)?,
        );
        regular(&file)?;
        let mut bytes = Zeroizing::new(Vec::with_capacity((MAX_LEAF + 1) as usize));
        (&mut file)
            .take(MAX_LEAF + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::InvalidCommand)?;
        if bytes.len() as u64 > MAX_LEAF {
            return Err(Error::InvalidCommand);
        }
        self.check()?;
        Ok(bytes)
    }
    /// Credential-only bounded extraction; no generic config/history copy.
    /// The broker separately validates provider identity and publication CAS.
    pub fn claude_leaf(&self) -> Result<Credential, Error> {
        if self.provider != Provider::Claude {
            return Err(Error::InvalidCommand);
        }
        let credentials: ClaudeLeaf =
            serde_json::from_slice(&self.read(&[".claude", ".credentials.json"])?)
                .map_err(|_| Error::InvalidCommand)?;
        let config: ClaudeIdentity = serde_json::from_slice(&self.read(&[".claude.json"])?)
            .map_err(|_| Error::InvalidCommand)?;
        let oauth = credentials.claude_ai_oauth;
        let identity = config.oauth_account;
        if oauth
            .client_id
            .as_deref()
            .is_some_and(|id| id != "9d1c250a-e61b-44d9-88ed-5944d1962f5e")
        {
            return Err(Error::InvalidCommand);
        }
        let credential = Credential {
            access: oauth.access_token,
            refresh: Some(oauth.refresh_token),
            id_token: None,
            expires_at: Some(
                oauth
                    .expires_at
                    .checked_div(1000)
                    .ok_or(Error::InvalidCommand)?,
            ),
            identity: Identity {
                user: identity.account_uuid,
                workspace: Some(identity.organization_uuid),
                plan: oauth.subscription_type.clone(),
            },
            scopes: oauth.scopes,
            claude: Some(ClaudeMetadata {
                subscription_type: oauth.subscription_type,
                rate_limit_tier: oauth.rate_limit_tier,
                refresh_expires_at: oauth.refresh_token_expires_at.map(|n| n / 1000),
            }),
        };
        credential.validate()?;
        Ok(credential)
    }
    pub fn codex_leaf(&self) -> Result<Credential, Error> {
        if self.provider != Provider::Codex {
            return Err(Error::InvalidCommand);
        }
        let leaf: CodexLeaf = serde_json::from_slice(&self.read(&[".codex", "auth.json"])?)
            .map_err(|_| Error::InvalidCommand)?;
        if leaf.auth_mode.as_deref().is_some_and(|m| m != "chatgpt")
            || leaf.openai_api_key.is_some()
            || leaf.personal_access_token.is_some()
            || leaf.agent_identity.is_some()
            || leaf.bedrock_api_key.is_some()
            || leaf.bedrock_access_keys.is_some()
        {
            return Err(Error::InvalidCommand);
        }
        let tokens = leaf.tokens.ok_or(Error::InvalidCommand)?;
        let claims = codex_claims(&tokens.access_token)?;
        let auth = claims.auth.ok_or(Error::InvalidCommand)?;
        let user = auth
            .chatgpt_user_id
            .or(auth.user_id)
            .ok_or(Error::InvalidCommand)?;
        let workspace = auth.chatgpt_account_id.ok_or(Error::InvalidCommand)?;
        if tokens
            .account_id
            .as_deref()
            .is_some_and(|id| id != workspace)
        {
            return Err(Error::InvalidCommand);
        }
        if let Some(id_token) = &tokens.id_token {
            let id = codex_claims(id_token)?.auth.ok_or(Error::InvalidCommand)?;
            if id.chatgpt_user_id.or(id.user_id).as_deref() != Some(&user)
                || id.chatgpt_account_id.as_deref() != Some(&workspace)
            {
                return Err(Error::InvalidCommand);
            }
        }
        let credential = Credential {
            access: tokens.access_token,
            refresh: Some(tokens.refresh_token),
            id_token: tokens.id_token,
            expires_at: claims.exp,
            identity: Identity {
                user,
                workspace: Some(workspace),
                plan: auth.chatgpt_plan_type,
            },
            scopes: vec![],
            claude: None,
        };
        credential.validate()?;
        Ok(credential)
    }
}

/// Only a supervisor-private credential channel serializes this object.
/// It deliberately has no Debug implementation or browser/status conversion.
#[derive(Serialize)]
pub struct Credential {
    access: Zeroizing<String>,
    refresh: Option<Zeroizing<String>>,
    id_token: Option<Zeroizing<String>>,
    expires_at: Option<i64>,
    identity: Identity,
    scopes: Vec<String>,
    claude: Option<ClaudeMetadata>,
}
#[derive(Serialize)]
struct Identity {
    user: String,
    workspace: Option<String>,
    plan: Option<String>,
}
#[derive(Serialize)]
struct ClaudeMetadata {
    subscription_type: Option<String>,
    rate_limit_tier: Option<String>,
    refresh_expires_at: Option<i64>,
}
fn bounded(s: &str, maximum: usize) -> bool {
    !s.is_empty() && s.len() <= maximum && !s.chars().any(char::is_control)
}
impl Credential {
    fn validate(&self) -> Result<(), Error> {
        if !bounded(&self.access, MAX_SECRET)
            || self
                .refresh
                .as_ref()
                .is_some_and(|s| !bounded(s, MAX_SECRET))
            || self
                .id_token
                .as_ref()
                .is_some_and(|s| !bounded(s, MAX_SECRET))
            || !bounded(&self.identity.user, 256)
            || !self
                .identity
                .workspace
                .as_deref()
                .is_some_and(|s| bounded(s, 256))
            || self
                .identity
                .plan
                .as_deref()
                .is_some_and(|s| !bounded(s, 256))
            || self.expires_at.is_none_or(|n| n <= 0)
            || self.scopes.len() > 32
            || self
                .scopes
                .iter()
                .any(|s| !bounded(s, 256) || s.chars().any(char::is_whitespace))
            || self.claude.as_ref().is_some_and(|m| {
                m.subscription_type
                    .as_deref()
                    .is_some_and(|s| !bounded(s, 256))
                    || m.rate_limit_tier
                        .as_deref()
                        .is_some_and(|s| !bounded(s, 256))
                    || m.refresh_expires_at.is_some_and(|n| n <= 0)
            })
        {
            return Err(Error::InvalidCommand);
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeLeaf {
    claude_ai_oauth: ClaudeOauth,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeOauth {
    access_token: Zeroizing<String>,
    refresh_token: Zeroizing<String>,
    expires_at: i64,
    scopes: Vec<String>,
    subscription_type: Option<String>,
    rate_limit_tier: Option<String>,
    refresh_token_expires_at: Option<i64>,
    client_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeIdentity {
    oauth_account: ClaudeAccount,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeAccount {
    account_uuid: String,
    organization_uuid: String,
}
#[derive(Deserialize)]
struct CodexLeaf {
    auth_mode: Option<String>,
    #[serde(rename = "OPENAI_API_KEY")]
    openai_api_key: Option<Zeroizing<String>>,
    tokens: Option<CodexTokens>,
    personal_access_token: Option<serde::de::IgnoredAny>,
    agent_identity: Option<serde::de::IgnoredAny>,
    bedrock_api_key: Option<serde::de::IgnoredAny>,
    bedrock_access_keys: Option<serde::de::IgnoredAny>,
}
#[derive(Deserialize)]
struct CodexTokens {
    access_token: Zeroizing<String>,
    refresh_token: Zeroizing<String>,
    id_token: Option<Zeroizing<String>>,
    account_id: Option<String>,
}
#[derive(Deserialize)]
struct CodexClaims {
    exp: Option<i64>,
    #[serde(rename = "https://api.openai.com/auth")]
    auth: Option<CodexIdentity>,
}
#[derive(Deserialize)]
struct CodexIdentity {
    chatgpt_user_id: Option<String>,
    user_id: Option<String>,
    chatgpt_account_id: Option<String>,
    chatgpt_plan_type: Option<String>,
}
fn codex_claims(token: &str) -> Result<CodexClaims, Error> {
    if !bounded(token, MAX_SECRET) {
        return Err(Error::InvalidCommand);
    }
    let mut parts = token.split('.');
    let _header = parts.next().ok_or(Error::InvalidCommand)?;
    let payload = parts.next().ok_or(Error::InvalidCommand)?;
    let signature = parts.next().ok_or(Error::InvalidCommand)?;
    if signature.is_empty() || parts.next().is_some() {
        return Err(Error::InvalidCommand);
    }
    let bytes = Zeroizing::new(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| Error::InvalidCommand)?,
    );
    serde_json::from_slice(&bytes).map_err(|_| Error::InvalidCommand)
}

#[cfg(test)]
#[path = "login_home_tests.rs"]
mod tests;
