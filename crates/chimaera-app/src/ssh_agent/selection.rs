//! Native-only SSH configuration, public host trust and agent identity selection.
//! No private-key file or keeper-supplied path participates in this selection.
use super::{
    packet::Reader, unix::UnixAgent, AgentConnection, Algorithms, GrantVerifier, Key, LocalAgent,
    Policy,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use chimaera_link::{
    SshAuthDestination, SshAuthGrantRequest, SshAuthHostKey, SSH_AUTH_KEYS_MAX, SSH_AUTH_KEY_MAX,
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectionFailure {
    UnsupportedConfiguration,
    AgentUnavailable,
    NoKeys,
    TooManyKeys,
    HostTrustRequired,
    RevokedHost,
    Unavailable,
}
type Result<T> = std::result::Result<T, SelectionFailure>;

pub(crate) struct Selection {
    pub(crate) request: SshAuthGrantRequest,
    pub(crate) agent: UnixAgent,
    algorithms: Algorithms,
}
impl Selection {
    pub(crate) fn verifier(
        self,
        deadline: tokio::time::Instant,
    ) -> std::result::Result<(SshAuthGrantRequest, GrantVerifier<UnixAgent>), super::Failure> {
        let mut verifier = GrantVerifier::new(&self.request, deadline, self.agent)?;
        verifier.policy.algorithms = Some(self.algorithms);
        Ok((self.request, verifier))
    }
}

/// Only an explicit native Connect invokes this. HOME and the agent socket are
/// captured from native process state, never a webview/remote request body.
pub(crate) async fn resolve(alias: &str, keeper_boot: String) -> Result<Selection> {
    tokio::time::timeout(Duration::from_secs(30), resolve_native(alias, keeper_boot))
        .await
        .map_err(|_| SelectionFailure::Unavailable)?
}
async fn resolve_native(alias: &str, keeper_boot: String) -> Result<Selection> {
    let alias = chimaera_remote::hosts::normalize_alias(alias)
        .map_err(|_| SelectionFailure::UnsupportedConfiguration)?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or(SelectionFailure::Unavailable)?;
    let mut command = Command::new("/usr/bin/ssh");
    command.args(["-G", "--", &alias]);
    let text = bounded_output(command, 0, None).await?;
    from_native_config(
        &text,
        &home,
        std::env::var_os("SSH_AUTH_SOCK").map(PathBuf::from),
        keeper_boot,
    )
    .await
}

pub(super) async fn from_native_config(
    text: &str,
    home: &Path,
    environment_agent: Option<PathBuf>,
    keeper_boot: String,
) -> Result<Selection> {
    from_native_config_inner(text, home, environment_agent, keeper_boot, false).await
}
async fn from_native_config_inner(
    text: &str,
    home: &Path,
    environment_agent: Option<PathBuf>,
    keeper_boot: String,
    routed_policy: bool,
) -> Result<Selection> {
    let config = Config::parse(text)?;
    let algorithms = config.algorithms()?;
    let (destination, host_keys) = trusted_host(&config, home, &algorithms).await?;
    if !config.key_enabled()? {
        return Err(SelectionFailure::NoKeys);
    }
    if routed_policy {
        config.route_policy(chimaera_link::SshRouteMode::Key)?;
    } else {
        config.key_mfa_policy()?;
    }
    let socket = match config.one("identityagent")? {
        None | Some("SSH_AUTH_SOCK") => {
            environment_agent.ok_or(SelectionFailure::AgentUnavailable)?
        }
        Some("none") => return Err(SelectionFailure::AgentUnavailable),
        Some(path) => local_path(path, home)?,
    };
    let agent = UnixAgent::new(socket).map_err(|_| SelectionFailure::AgentUnavailable)?;
    let mut user_keys = identities(&agent).await?;
    user_keys.retain(|encoded| {
        chimaera_link::decode_packet(encoded, SSH_AUTH_KEY_MAX)
            .ok()
            .and_then(|blob| Key::parse(blob).ok())
            .is_some_and(|key| algorithms.offered(&key, false))
    });
    if user_keys.is_empty() {
        return Err(SelectionFailure::NoKeys);
    }
    match config.one("identitiesonly")? {
        Some("no") => {}
        Some("yes") => {
            let allowed = config.public_identities(home).await?;
            user_keys.retain(|key| allowed.contains(key));
            if user_keys.is_empty() {
                return Err(SelectionFailure::NoKeys);
            }
        }
        _ => return Err(SelectionFailure::UnsupportedConfiguration),
    }
    if user_keys.len() > SSH_AUTH_KEYS_MAX {
        return Err(SelectionFailure::TooManyKeys);
    }
    let request = SshAuthGrantRequest {
        version: 1,
        keeper_boot,
        destination,
        host_keys,
        user_keys,
    };
    Policy::new(&request).map_err(|_| SelectionFailure::UnsupportedConfiguration)?;
    Ok(Selection {
        request,
        agent,
        algorithms,
    })
}

async fn trusted_host(
    config: &Config<'_>,
    home: &Path,
    algorithms: &Algorithms,
) -> Result<(SshAuthDestination, Vec<SshAuthHostKey>)> {
    let destination = config.destination()?;
    let lookup = config.lookup(&destination)?;
    let mut host_keys = Vec::new();
    let mut seen = BTreeSet::new();
    for key in ["userknownhostsfile", "globalknownhostsfile"] {
        let paths = config
            .one(key)?
            .ok_or(SelectionFailure::UnsupportedConfiguration)?;
        let paths: Vec<_> = paths.split_whitespace().collect();
        if paths.len() > 8 {
            return Err(SelectionFailure::UnsupportedConfiguration);
        }
        for path in paths {
            if path == "none" {
                continue;
            }
            let path = local_path(path, home)?;
            let Some(snapshot) = public_file(&path, 1024 * 1024).await? else {
                continue;
            };
            let mut command = Command::new("/usr/bin/ssh-keygen");
            command.args(["-F", &lookup, "-f", "/dev/stdin"]);
            let matching = bounded_output(command, 1, Some(snapshot)).await?;
            for entry in matching_trust(&matching)? {
                let blob = chimaera_link::decode_packet(&entry.key, SSH_AUTH_KEY_MAX)
                    .map_err(|_| SelectionFailure::UnsupportedConfiguration)?;
                let key = Key::parse(blob.clone())
                    .map_err(|_| SelectionFailure::UnsupportedConfiguration)?;
                if !entry.is_ca && !algorithms.offered(&key, true) {
                    continue;
                }
                if seen.insert(blob) {
                    host_keys.push(entry);
                }
            }
        }
    }
    if host_keys.is_empty() {
        return Err(SelectionFailure::HostTrustRequired);
    }
    if host_keys.len() > SSH_AUTH_KEYS_MAX {
        return Err(SelectionFailure::TooManyKeys);
    }
    Ok((destination, host_keys))
}

/// Route resolution may accept only the separately parsed ProxyJump field.
/// Every other version-one trust/routing refusal is retained before key reads.
pub(super) fn route_settings(text: &str) -> Result<(SshAuthDestination, String)> {
    let config = Config::parse_mode(text, true)?;
    Ok((
        config.destination()?,
        config.one("proxyjump")?.unwrap_or("none").into(),
    ))
}
pub(super) async fn from_routed_config(
    text: &str,
    home: &Path,
    environment_agent: Option<PathBuf>,
    keeper_boot: String,
) -> Result<Selection> {
    let text = routed_text(text)?;
    from_native_config_inner(&text, home, environment_agent, keeper_boot, true).await
}
pub(super) fn resolved_policy(
    text: &str,
    mode: chimaera_link::SshRouteMode,
) -> Result<chimaera_link::SshRoutePolicy> {
    Config::parse_mode(text, true)?.route_policy(mode)
}
fn routed_text(text: &str) -> Result<String> {
    route_settings(text)?;
    let text = text
        .lines()
        .map(|line| {
            if line.starts_with("proxyjump ") {
                "proxyjump none"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(text)
}
pub(super) fn no_agent_selected(text: &str, environment_agent: &Option<PathBuf>) -> Result<bool> {
    let text = routed_text(text)?;
    let config = Config::parse(&text)?;
    Ok(match config.one("identityagent")? {
        None | Some("SSH_AUTH_SOCK") => environment_agent.is_none(),
        Some("none") => true,
        _ => false,
    })
}
pub(super) async fn routed_interactive(
    text: &str,
    home: &Path,
) -> Result<chimaera_link::SshRouteAuthLeg> {
    let text = routed_text(text)?;
    let config = Config::parse(&text)?;
    let algorithms = config.algorithms()?;
    let policy = config.route_policy(chimaera_link::SshRouteMode::Interactive)?;
    let (destination, host_keys) = trusted_host(&config, home, &algorithms).await?;
    Ok(chimaera_link::SshRouteAuthLeg {
        policy: Some(policy),
        destination,
        mode: chimaera_link::SshRouteMode::Interactive,
        host_keys,
        user_keys: vec![],
    })
}

struct Config<'a> {
    lines: Vec<(&'a str, &'a str)>,
}
impl<'a> Config<'a> {
    fn parse(text: &'a str) -> Result<Self> {
        Self::parse_mode(text, false)
    }
    fn parse_mode(text: &'a str, routed: bool) -> Result<Self> {
        let mut lines = Vec::new();
        for line in text.lines() {
            let (name, value) = line
                .split_once(' ')
                .ok_or(SelectionFailure::UnsupportedConfiguration)?;
            if lines.len() >= 512 {
                return Err(SelectionFailure::UnsupportedConfiguration);
            }
            lines.push((name, value));
        }
        let config = Self { lines };
        // These policies cannot be reproduced by the portable keeper tuple.
        // Refuse explicitly rather than bypassing a local routing/trust rule.
        for name in [
            "proxycommand",
            "proxyjump",
            "knownhostscommand",
            "revokedhostkeys",
        ] {
            if !(routed && name == "proxyjump")
                && config.one(name)?.is_some_and(|value| value != "none")
            {
                return Err(SelectionFailure::UnsupportedConfiguration);
            }
        }
        for name in ["checkhostip", "verifyhostkeydns"] {
            if config
                .one(name)?
                .is_some_and(|value| !matches!(value, "no" | "false"))
            {
                return Err(SelectionFailure::UnsupportedConfiguration);
            }
        }
        if !matches!(
            config.one("pubkeyauthentication")?,
            Some("true" | "yes" | "host-bound" | "false" | "no")
        ) {
            return Err(SelectionFailure::UnsupportedConfiguration);
        }
        Ok(config)
    }
    fn one(&self, name: &str) -> Result<Option<&'a str>> {
        let mut values = self
            .lines
            .iter()
            .filter(|(key, _)| *key == name)
            .map(|(_, value)| *value);
        let value = values.next();
        if values.next().is_some() {
            return Err(SelectionFailure::UnsupportedConfiguration);
        }
        Ok(value)
    }
    fn preferred(&self) -> Result<Vec<&'a str>> {
        let value = self
            .one("preferredauthentications")?
            .unwrap_or("publickey,keyboard-interactive,password");
        let methods: Vec<_> = value.split(',').take(17).collect();
        if methods.is_empty()
            || methods.len() > 16
            || methods.iter().any(|method| {
                !matches!(
                    *method,
                    "gssapi-with-mic"
                        | "hostbased"
                        | "publickey"
                        | "keyboard-interactive"
                        | "password"
                        | "none"
                )
            })
        {
            return Err(SelectionFailure::UnsupportedConfiguration);
        }
        Ok(methods)
    }
    fn enabled(&self, name: &str) -> Result<bool> {
        match self.one(name)?.unwrap_or("yes") {
            "true" | "yes" => Ok(true),
            "false" | "no" => Ok(false),
            _ => Err(SelectionFailure::UnsupportedConfiguration),
        }
    }
    fn key_enabled(&self) -> Result<bool> {
        Ok(
            !matches!(self.one("pubkeyauthentication")?, Some("no" | "false"))
                && self.preferred()?.contains(&"publickey"),
        )
    }
    fn key_mfa_policy(&self) -> Result<()> {
        let methods = self.preferred()?;
        if !self.enabled("kbdinteractiveauthentication")?
            || methods
                .iter()
                .position(|method| *method == "publickey")
                .zip(
                    methods
                        .iter()
                        .position(|method| *method == "keyboard-interactive"),
                )
                .is_none_or(|(key, mfa)| key > mfa)
        {
            return Err(SelectionFailure::UnsupportedConfiguration);
        }
        Ok(())
    }
    fn route_policy(
        &self,
        mode: chimaera_link::SshRouteMode,
    ) -> Result<chimaera_link::SshRoutePolicy> {
        use chimaera_link::{SshRouteMethod, SshRouteMode, SshRoutePolicy};
        let preferred = self.preferred()?;
        for option in ["gssapiauthentication", "hostbasedauthentication"] {
            if self
                .one(option)?
                .is_some_and(|value| !matches!(value, "no" | "false"))
            {
                return Err(SelectionFailure::UnsupportedConfiguration);
            }
        }
        let mut methods = Vec::new();
        for method in preferred {
            let selected = match method {
                "publickey" if mode == SshRouteMode::Key && self.key_enabled()? => {
                    Some(SshRouteMethod::Publickey)
                }
                "keyboard-interactive" if self.enabled("kbdinteractiveauthentication")? => {
                    Some(SshRouteMethod::KeyboardInteractive)
                }
                "password" if self.enabled("passwordauthentication")? => {
                    Some(SshRouteMethod::Password)
                }
                _ => None,
            };
            if let Some(method) = selected {
                methods.push(method);
            }
        }
        let list = |name| -> Result<Vec<String>> {
            let value = self
                .one(name)?
                .ok_or(SelectionFailure::UnsupportedConfiguration)?;
            let mut names = Vec::new();
            for name in value.split(',') {
                if !names.iter().any(|existing| existing == name) {
                    names.push(name.to_owned());
                }
                if names.len() > 64 {
                    return Err(SelectionFailure::UnsupportedConfiguration);
                }
            }
            Ok(names)
        };
        let policy = SshRoutePolicy {
            version: 1,
            methods,
            host_key_algorithms: list("hostkeyalgorithms")?,
            ca_signature_algorithms: list("casignaturealgorithms")?,
            pubkey_accepted_algorithms: list("pubkeyacceptedalgorithms")?,
            kex_algorithms: list("kexalgorithms")?,
            ciphers: list("ciphers")?,
            macs: list("macs")?,
        };
        policy
            .validate(mode)
            .map_err(|_| SelectionFailure::UnsupportedConfiguration)?;
        Ok(policy)
    }
    async fn public_identities(&self, home: &Path) -> Result<BTreeSet<String>> {
        let mut keys = BTreeSet::new();
        let mut count = 0;
        for (name, value) in &self.lines {
            if !matches!(*name, "identityfile" | "certificatefile") || *value == "none" {
                continue;
            }
            count += 1;
            if count > 32 {
                return Err(SelectionFailure::UnsupportedConfiguration);
            }
            let mut path = local_path(value, home)?;
            // For a private IdentityFile consult only its public sibling. A
            // missing public sibling never triggers private key loading.
            if *name == "identityfile" && path.extension().is_none_or(|value| value != "pub") {
                path.as_mut_os_string().push(".pub");
            }
            if let Some(key) = public_identity(&path).await? {
                keys.insert(key);
            }
        }
        Ok(keys)
    }
    fn algorithms(&self) -> Result<Algorithms> {
        let value = |key| {
            self.one(key)?
                .ok_or(SelectionFailure::UnsupportedConfiguration)
        };
        Algorithms::parse(
            value("hostkeyalgorithms")?,
            value("pubkeyacceptedalgorithms")?,
            value("casignaturealgorithms")?,
        )
        .map_err(|_| SelectionFailure::UnsupportedConfiguration)
    }
    fn lookup(&self, destination: &SshAuthDestination) -> Result<String> {
        let alias = self.one("hostkeyalias")?;
        let hostname = alias.unwrap_or(&destination.hostname);
        if hostname.is_empty()
            || hostname.starts_with('-')
            || hostname.chars().any(char::is_whitespace)
        {
            return Err(SelectionFailure::UnsupportedConfiguration);
        }
        // OpenSSH uses HostKeyAlias verbatim, even for a nonstandard port.
        Ok(if alias.is_some() || destination.port == 22 {
            hostname.to_owned()
        } else {
            format!("[{hostname}]:{}", destination.port)
        })
    }
    fn destination(&self) -> Result<SshAuthDestination> {
        let required = |key| {
            self.one(key)?
                .ok_or(SelectionFailure::UnsupportedConfiguration)
        };
        let destination = SshAuthDestination {
            hostname: required("hostname")?.into(),
            user: required("user")?.into(),
            port: required("port")?
                .parse()
                .map_err(|_| SelectionFailure::UnsupportedConfiguration)?,
        };
        destination
            .validate()
            .map_err(|_| SelectionFailure::UnsupportedConfiguration)?;
        Ok(destination)
    }
}
fn local_path(value: &str, home: &Path) -> Result<PathBuf> {
    if value.is_empty()
        || value.len() > 4096
        || value
            .chars()
            .any(|c| c.is_control() || matches!(c, '%' | '$' | '"' | '\''))
    {
        return Err(SelectionFailure::UnsupportedConfiguration);
    }
    let path = if let Some(suffix) = value.strip_prefix("~/") {
        home.join(suffix)
    } else {
        PathBuf::from(value)
    };
    if !path.is_absolute() {
        return Err(SelectionFailure::UnsupportedConfiguration);
    }
    Ok(path)
}

async fn public_file(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    let file = match tokio::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOFOLLOW)
        .open(path)
        .await
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(SelectionFailure::UnsupportedConfiguration),
    };
    let metadata = file
        .metadata()
        .await
        .map_err(|_| SelectionFailure::Unavailable)?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(SelectionFailure::UnsupportedConfiguration);
    }
    let mut bytes = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(2),
        file.take(limit + 1).read_to_end(&mut bytes),
    )
    .await
    .map_err(|_| SelectionFailure::Unavailable)?
    .map_err(|_| SelectionFailure::Unavailable)?;
    if bytes.len() as u64 > limit {
        return Err(SelectionFailure::UnsupportedConfiguration);
    }
    Ok(Some(bytes))
}

async fn public_identity(path: &Path) -> Result<Option<String>> {
    let Some(bytes) = public_file(path, 32 * 1024).await? else {
        return Ok(None);
    };
    let text =
        std::str::from_utf8(&bytes).map_err(|_| SelectionFailure::UnsupportedConfiguration)?;
    let blob = if let Ok(key) = ssh_key::PublicKey::from_openssh(text.trim()) {
        key.to_bytes()
    } else {
        ssh_key::Certificate::from_openssh(text.trim()).and_then(|key| key.to_bytes())
    }
    .map_err(|_| SelectionFailure::UnsupportedConfiguration)?;
    Key::parse(blob.clone()).map_err(|_| SelectionFailure::UnsupportedConfiguration)?;
    Ok(Some(STANDARD.encode(blob)))
}

async fn identities(agent: &impl LocalAgent) -> Result<Vec<String>> {
    let response = tokio::time::timeout(Duration::from_secs(5), async {
        let mut connection = agent.connect().await?;
        connection.exchange(&[11]).await
    })
    .await
    .map_err(|_| SelectionFailure::AgentUnavailable)?
    .map_err(|_| SelectionFailure::AgentUnavailable)?;
    identity_reply(&response)
}
fn identity_reply(bytes: &[u8]) -> Result<Vec<String>> {
    let parse = || {
        let mut reader = Reader::new(bytes)?;
        reader.byte_is(12)?;
        let count = reader.u32()?;
        if count > 256 {
            return Err(super::Failure::InvalidRequest);
        }
        let mut keys = Vec::new();
        let mut seen = BTreeSet::new();
        for _ in 0..count {
            let key = reader.string()?;
            let comment = reader.string()?;
            if key.len() > SSH_AUTH_KEY_MAX || comment.len() > 4096 {
                return Err(super::Failure::InvalidRequest);
            }
            if Key::parse(key.to_vec()).is_ok() && seen.insert(key.to_vec()) {
                keys.push(STANDARD.encode(key));
            }
        }
        reader.end()?;
        Ok(keys)
    };
    let keys = parse().map_err(|_| SelectionFailure::AgentUnavailable)?;
    if keys.is_empty() {
        return Err(SelectionFailure::NoKeys);
    }
    Ok(keys)
}

fn matching_trust(text: &str) -> Result<Vec<SshAuthHostKey>> {
    let mut keys = Vec::new();
    for line in text
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let mut fields = line.split_whitespace();
        let first = fields
            .next()
            .ok_or(SelectionFailure::UnsupportedConfiguration)?;
        if first == "@revoked" {
            return Err(SelectionFailure::RevokedHost);
        }
        let is_ca = first == "@cert-authority";
        if first.starts_with('@') && !is_ca {
            return Err(SelectionFailure::UnsupportedConfiguration);
        }
        if is_ca {
            fields
                .next()
                .ok_or(SelectionFailure::UnsupportedConfiguration)?;
        }
        let algorithm = fields
            .next()
            .ok_or(SelectionFailure::UnsupportedConfiguration)?;
        let encoded = fields
            .next()
            .ok_or(SelectionFailure::UnsupportedConfiguration)?;
        let blob = chimaera_link::decode_packet(encoded, SSH_AUTH_KEY_MAX)
            .map_err(|_| SelectionFailure::UnsupportedConfiguration)?;
        let key = Key::parse(blob).map_err(|_| SelectionFailure::UnsupportedConfiguration)?;
        let expected = key.certificate.as_ref().map_or_else(
            || key.data.algorithm().as_str().to_string(),
            |_| key.data.algorithm().to_certificate_type(),
        );
        if expected != algorithm || is_ca && key.certificate.is_some() {
            return Err(SelectionFailure::UnsupportedConfiguration);
        }
        keys.push(SshAuthHostKey {
            key: encoded.into(),
            is_ca,
        });
        if keys.len() > SSH_AUTH_KEYS_MAX {
            return Err(SelectionFailure::TooManyKeys);
        }
    }
    Ok(keys)
}

pub(super) async fn bounded_output(
    mut command: Command,
    empty_status: i32,
    input: Option<Vec<u8>>,
) -> Result<String> {
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command.as_std_mut().process_group(0);
    struct Owned(tokio::process::Child);
    impl Drop for Owned {
        fn drop(&mut self) {
            if let Some(id) = self.0.id() {
                unsafe {
                    nix::libc::kill(-(id as i32), nix::libc::SIGKILL);
                }
            }
            let _ = self.0.start_kill();
        }
    }
    let mut child = Owned(command.spawn().map_err(|_| SelectionFailure::Unavailable)?);
    let mut stdout = child
        .0
        .stdout
        .take()
        .ok_or(SelectionFailure::Unavailable)?
        .take(256 * 1024 + 1);
    let stdin = child.0.stdin.take();
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        let write = async {
            if let (Some(mut stdin), Some(input)) = (stdin, input) {
                stdin
                    .write_all(&input)
                    .await
                    .map_err(|_| SelectionFailure::Unavailable)?;
                stdin
                    .shutdown()
                    .await
                    .map_err(|_| SelectionFailure::Unavailable)?;
            }
            Ok::<_, SelectionFailure>(())
        };
        let read = async {
            stdout
                .read_to_end(&mut bytes)
                .await
                .map_err(|_| SelectionFailure::Unavailable)
        };
        tokio::try_join!(write, read)?;
        if bytes.len() > 256 * 1024 {
            return Err(SelectionFailure::UnsupportedConfiguration);
        }
        let status = child
            .0
            .wait()
            .await
            .map_err(|_| SelectionFailure::Unavailable)?;
        if !status.success()
            && (empty_status == 0 || status.code() != Some(empty_status) || !bytes.is_empty())
        {
            return Err(SelectionFailure::Unavailable);
        }
        String::from_utf8(bytes).map_err(|_| SelectionFailure::UnsupportedConfiguration)
    })
    .await
    .map_err(|_| SelectionFailure::Unavailable)?;
    result
}
use std::os::unix::process::CommandExt;

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;
