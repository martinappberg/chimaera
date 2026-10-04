//! Remote daemon orchestration over the system `ssh`: discovery, binary
//! install, daemon start, and port-forward tunnels. Shared by the CLI
//! (`chimaera connect`) and the native shell, so both speak the exact same
//! protocol to a host — including inheriting the user's `~/.ssh/config`
//! (ProxyJump, 2FA) by never reimplementing the ssh client.
//!
//! Every ssh/scp invocation here rides one chimaera-owned ControlMaster (see
//! [`ssh_opts`]): the user authenticates once — password or 2FA — and every
//! subsequent command, tunnel, and new window multiplexes that single
//! connection with no further prompts, kept warm by `ControlPersist` so
//! opening new things on the host stays instant. The same options set a
//! trust-on-first-use host-key policy, so a freshly installed app can connect
//! to a host it has never seen without a tty to confirm the key.
//! An explicit [`SshAuthentication`] scope instead freezes strict caller-selected
//! public trust and a destination-bound agent for one effect. Unsupported local
//! identity/proxy configuration cannot silently widen that authority; background
//! captured-master scopes remain unable to authenticate.
//! `ssh_algorithms` captures one bounded fixed-binary supported snapshot for all
//! legs. Policy-bearing effects emit its ordered intersection with native lists,
//! retain locally disabled methods, and refuse if that binary identity changes.

pub mod cluster;
pub mod hosts;
mod ssh_algorithms;
pub use ssh_algorithms::SshAlgorithmSupport;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context};
use chimaera_core::slurm::Scheduler;
use chimaera_core::{same_node, Manifest};
use tokio::process::{Child, Command};

/// Per-child context inherited by the native app's SSH_ASKPASS helper. This
/// must be set on every ssh/scp process individually: multiple hosts may
/// connect concurrently, so a process-global "current host" would race and
/// show one host's authentication prompt in another host's windows.
pub const ASKPASS_ALIAS_ENV: &str = "CHIMAERA_ASKPASS_ALIAS";
/// Local-only explicit authentication identity for a caller-gated MFA relay.
/// This is not a remote environment variable or a bearer credential.
pub const ASKPASS_CONTEXT_ENV: &str = "CHIMAERA_ASKPASS_CONTEXT";

tokio::task_local! {
    static EXISTING_MASTER_ONLY: MasterHandle;
    static SSH_AUTHENTICATION: SshAuthentication;
}

/// One explicit destination-bound authentication effect. Paths belong to the
/// caller, which must retain its agent/trust-file lease until the effect ends.
/// No Debug implementation: socket paths and selected identity stay private.
#[derive(Clone)]
pub struct SshAuthentication {
    alias: String,
    hostname: String,
    user: String,
    port: u16,
    socket: String,
    known_hosts: String,
    masters: Vec<MasterHandle>,
    fresh: bool,
    keyboard_interactive: Option<String>,
    route_helper: Option<String>,
    interactive_only: bool,
    policy: Option<SshAuthenticationPolicy>,
    support: Option<SshAlgorithmSupport>,
}
/// Closed resolved options for one original route leg; never native directives.
#[derive(Clone, serde::Serialize)]
pub struct SshAuthenticationPolicy {
    pub methods: Vec<String>,
    pub host_key_algorithms: Vec<String>,
    pub ca_signature_algorithms: Vec<String>,
    pub pubkey_accepted_algorithms: Vec<String>,
    pub kex_algorithms: Vec<String>,
    pub ciphers: Vec<String>,
    pub macs: Vec<String>,
}
impl SshAuthenticationPolicy {
    fn validate(&self, interactive: bool) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.methods.is_empty() && self.methods.len() <= 3,
            "invalid SSH authentication methods"
        );
        for (index, method) in self.methods.iter().enumerate() {
            anyhow::ensure!(
                matches!(
                    method.as_str(),
                    "publickey" | "keyboard-interactive" | "password"
                ) && !self.methods[..index].contains(method),
                "invalid SSH authentication method"
            );
        }
        anyhow::ensure!(
            if interactive {
                !self.methods.iter().any(|method| method == "publickey")
            } else {
                self.methods
                    .first()
                    .is_some_and(|method| method == "publickey")
            },
            "SSH authentication mode mismatch"
        );
        for list in [
            &self.host_key_algorithms,
            &self.ca_signature_algorithms,
            &self.pubkey_accepted_algorithms,
            &self.kex_algorithms,
            &self.ciphers,
            &self.macs,
        ] {
            anyhow::ensure!(
                !list.is_empty() && list.len() <= 64,
                "invalid SSH authentication algorithms"
            );
            let mut names = std::collections::HashSet::new();
            for name in list {
                anyhow::ensure!(
                    !name.is_empty()
                        && name.len() <= 128
                        && !name.starts_with(['+', '-'])
                        && name
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"-@._+".contains(&byte))
                        && names.insert(name),
                    "invalid SSH authentication algorithm"
                );
            }
        }
        anyhow::ensure!(
            serde_json::to_vec(self)?.len() <= 8 * 1024,
            "SSH authentication policy too large"
        );
        Ok(())
    }
}
impl SshAuthentication {
    /// The caller retains original-grant prompt/signature guards. This only
    /// restricts the fixed SSH effect; it cannot grant credential UI authority.
    pub fn with_policy(
        mut self,
        policy: SshAuthenticationPolicy,
        support: &SshAlgorithmSupport,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            self.keyboard_interactive.is_some(),
            "SSH route context missing"
        );
        policy.validate(self.interactive_only)?;
        self.policy = Some(support.restrict(&policy)?);
        self.support = Some(support.clone());
        Ok(self)
    }
    /// Resolve and freeze one canonical destination locally, without dialing.
    /// Configured private keys/certificates are unsupported. Configured jump or
    /// proxy routing is usable only through an already established captured mux.
    pub async fn new(
        alias: &str,
        hostname: &str,
        user: &str,
        port: u16,
        agent_socket: &Path,
        known_hosts: &Path,
    ) -> anyhow::Result<Self> {
        Self::new_inner(alias, hostname, user, port, agent_socket, known_hosts, None).await
    }
    /// A keeper's own immutable generated route, never a native ProxyCommand.
    /// The same executable implements the closed --ssh-route-leg helper; its
    /// context resolves only the original live owner, not a caller command.
    pub async fn new_route(
        alias: &str,
        destination: (&str, &str, u16),
        files: (&Path, &Path),
        context: &str,
        jump_aliases: &[String],
        interactive_only: bool,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            (32..=128).contains(&context.len())
                && context
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
                && jump_aliases.len() <= 3
                && jump_aliases
                    .iter()
                    .all(|alias| hosts::normalize_alias(alias).ok().as_ref() == Some(alias)),
            "invalid SSH route authentication context"
        );
        Self::new_inner(
            alias,
            destination.0,
            destination.1,
            destination.2,
            files.0,
            files.1,
            Some((context, jump_aliases, interactive_only)),
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    async fn new_inner(
        alias: &str,
        hostname: &str,
        user: &str,
        port: u16,
        agent_socket: &Path,
        known_hosts: &Path,
        route_context: Option<(&str, &[String], bool)>,
    ) -> anyhow::Result<Self> {
        let alias = hosts::normalize_alias(alias)?;
        anyhow::ensure!(
            auth_word(hostname) && auth_word(user) && port != 0,
            "invalid SSH authentication destination"
        );
        let socket = authentication_path(agent_socket)?;
        let known_hosts = authentication_path(known_hosts)?;
        validate_authentication_files(agent_socket, Path::new(&known_hosts)).await?;
        let mut command = transport_command("ssh");
        command.args(ssh_opts());
        command.args([
            "-o",
            "IdentityFile=none",
            "-o",
            "CertificateFile=none",
            "-G",
        ]);
        command.arg(&alias);
        let output = output_bounded(&mut command, 5, "SSH authentication configuration")
            .await
            .map_err(|_| anyhow::anyhow!("SSH authentication configuration unavailable"))?;
        anyhow::ensure!(
            output.status.success(),
            "SSH authentication configuration unavailable"
        );
        let config = std::str::from_utf8(&output.stdout)
            .map_err(|_| anyhow::anyhow!("SSH authentication configuration invalid"))?;
        let (mut fresh, master) =
            authentication_snapshot(&alias, Route::Alias, config, hostname, user, port)?;
        let route_helper = if let Some((context, jumps, _)) = route_context {
            let helper = authentication_route_helper(config, context, jumps)?;
            fresh = true;
            helper
        } else {
            None
        };
        // Destination and mux identity must come from the same resolution:
        // a second config read could bind an old grant to a different master.
        let mut masters = vec![master];
        let route = route_of(&alias);
        if route != Route::Alias && route.node() == Some(hostname) {
            let mut command = transport_command("ssh");
            command.args(route_opts(&alias, &route));
            command.args(["-o", &format!("User={user}"), "-o", &format!("Port={port}")]);
            command.args(ssh_opts());
            command.args([
                "-o",
                "IdentityFile=none",
                "-o",
                "CertificateFile=none",
                "-G",
            ]);
            command.arg(&alias);
            let output = output_bounded(&mut command, 5, "SSH authentication route")
                .await
                .map_err(|_| anyhow::anyhow!("SSH authentication route unavailable"))?;
            anyhow::ensure!(
                output.status.success(),
                "SSH authentication route unavailable"
            );
            let config = std::str::from_utf8(&output.stdout)
                .map_err(|_| anyhow::anyhow!("SSH authentication route invalid"))?;
            masters.push(authentication_snapshot(&alias, route, config, hostname, user, port)?.1);
        }
        if !fresh {
            let mut established = false;
            for master in &masters {
                established |= master.present().await?;
            }
            anyhow::ensure!(
                established,
                "SSH authentication routing requires an existing master"
            );
        }
        Ok(Self {
            alias,
            hostname: hostname.into(),
            user: user.into(),
            port,
            socket,
            known_hosts,
            masters,
            fresh,
            keyboard_interactive: route_context.map(|(context, _, _)| context.into()),
            route_helper,
            interactive_only: route_context.is_some_and(|(_, _, interactive)| interactive),
            policy: None,
            support: None,
        })
    }
    /// Permit a caller's existing askpass MFA relay for this exact explicit
    /// effect. The relay MUST check this opaque context, selected destination,
    /// original device and a verified key-signature receipt before prompting,
    /// then freshly authorize the answer. Configured continuation methods never
    /// authorize a prompt after key refusal; the original owner still decides.
    pub fn with_keyboard_interactive(mut self, context: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(
            (32..=128).contains(&context.len())
                && context
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte)),
            "invalid SSH authentication context"
        );
        self.keyboard_interactive = Some(context.to_owned());
        Ok(self)
    }
    /// Task-local and cancellation-safe; spawned owned work must enter its own
    /// scope. An existing-master-only scope always has stronger authority.
    pub async fn with_authentication<F: std::future::Future>(&self, future: F) -> F::Output {
        SSH_AUTHENTICATION.scope(self.clone(), future).await
    }
    fn options(&self, host: &str, route: &Route, node: bool) -> Vec<String> {
        let captured = self
            .masters
            .iter()
            .find(|master| master.host == host && master.route == *route);
        let allowed = host == self.alias
            && *route == Route::Alias
            && self.fresh
            && !node
            && EXISTING_MASTER_ONLY.try_with(|_| ()).is_err();
        // Freeze the validated config instead of re-reading identity/proxy
        // directives after validation. Exact captured sockets preserve routing.
        let mut options = vec!["-F".into(), "/dev/null".into()];
        let mfa = allowed && self.keyboard_interactive.is_some();
        let methods = self.policy.as_ref().map(|policy| policy.methods.join(","));
        let permits = |method: &str| {
            mfa && self.policy.as_ref().map_or_else(
                || method == "keyboard-interactive" || self.interactive_only,
                |policy| policy.methods.iter().any(|name| name == method),
            )
        };
        let settings = [
            (
                "IdentityAgent",
                if allowed {
                    self.socket.as_str()
                } else {
                    "none"
                },
            ),
            ("IdentityFile", "none"),
            ("CertificateFile", "none"),
            ("IdentitiesOnly", "no"),
            ("ForwardAgent", "no"),
            ("AddKeysToAgent", "no"),
            ("GlobalKnownHostsFile", "/dev/null"),
            ("UserKnownHostsFile", self.known_hosts.as_str()),
            ("StrictHostKeyChecking", "yes"),
            ("KnownHostsCommand", "none"),
            ("VerifyHostKeyDNS", "no"),
            ("UpdateHostKeys", "no"),
            ("BatchMode", if mfa { "no" } else { "yes" }),
            (
                "PasswordAuthentication",
                if permits("password") { "yes" } else { "no" },
            ),
            (
                "KbdInteractiveAuthentication",
                if permits("keyboard-interactive") {
                    "yes"
                } else {
                    "no"
                },
            ),
            ("HostbasedAuthentication", "no"),
            ("GSSAPIAuthentication", "no"),
            (
                "PreferredAuthentications",
                if let Some(methods) = &methods {
                    methods.as_str()
                } else if mfa && self.interactive_only {
                    "keyboard-interactive,password"
                } else if mfa {
                    "publickey,keyboard-interactive"
                } else {
                    "publickey"
                },
            ),
            (
                "PubkeyAuthentication",
                if self.interactive_only {
                    "no"
                } else {
                    "host-bound"
                },
            ),
            (
                "ProxyCommand",
                if allowed {
                    self.route_helper.as_deref().unwrap_or("none")
                } else {
                    "false"
                },
            ),
            ("ProxyJump", "none"),
        ];
        for (key, value) in settings {
            options.extend(["-o".into(), format!("{key}={value}")]);
        }
        if let Some(policy) = &self.policy {
            for (key, list) in [
                ("HostKeyAlgorithms", &policy.host_key_algorithms),
                ("CASignatureAlgorithms", &policy.ca_signature_algorithms),
                (
                    "PubkeyAcceptedAlgorithms",
                    &policy.pubkey_accepted_algorithms,
                ),
                ("KexAlgorithms", &policy.kex_algorithms),
                ("Ciphers", &policy.ciphers),
                ("MACs", &policy.macs),
            ] {
                options.extend(["-o".into(), format!("{key}={}", list.join(","))]);
            }
        }
        options.extend([
            "-o".into(),
            format!(
                "ControlPath={}",
                captured
                    .filter(|_| !node)
                    .map_or("none", |master| master.path.as_str())
            ),
        ]);
        if allowed {
            for (key, value) in [
                ("HostName", self.hostname.clone()),
                ("User", self.user.clone()),
                ("Port", self.port.to_string()),
            ] {
                options.extend(["-o".into(), format!("{key}={value}")]);
            }
        } else {
            options.extend([
                "-o".into(),
                "ControlMaster=no".into(),
                "-o".into(),
                "ControlPersist=no".into(),
            ]);
        }
        options
    }
}
fn authentication_route_helper(
    config: &str,
    context: &str,
    jumps: &[String],
) -> anyhow::Result<Option<String>> {
    anyhow::ensure!(
        (32..=128).contains(&context.len())
            && context
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
            && jumps.len() <= 3
            && jumps
                .iter()
                .all(|alias| hosts::normalize_alias(alias).ok().as_ref() == Some(alias)),
        "invalid SSH route authentication context"
    );
    let expected = if jumps.is_empty() {
        "none".into()
    } else {
        jumps.join(",")
    };
    let mut resolved = config
        .lines()
        .filter_map(|line| line.strip_prefix("proxyjump "));
    let actual = resolved.next().unwrap_or("none");
    anyhow::ensure!(
        actual == expected
            && resolved.next().is_none()
            && config
                .lines()
                .filter_map(|line| line.strip_prefix("proxycommand "))
                .all(|value| value == "none"),
        "SSH route configuration changed"
    );
    if jumps.is_empty() {
        return Ok(None);
    }
    let executable = authentication_path(&std::env::current_exe()?)?;
    Ok(Some(fixed_route_helper(
        &executable,
        context,
        jumps.len() - 1,
    )?))
}
fn fixed_route_helper(executable: &str, context: &str, leg: usize) -> anyhow::Result<String> {
    // ProxyCommand is parsed by OpenSSH's shell. These paths were validated for
    // argv use; quote the complete executable so allowed shell metacharacters
    // retain their literal filesystem meaning. Single quotes are forbidden by
    // authentication_path, and the two remaining arguments are closed atoms.
    let executable = authentication_path(Path::new(executable))?;
    anyhow::ensure!(
        (32..=128).contains(&context.len())
            && context
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
            && leg < 3,
        "invalid SSH route helper arguments"
    );
    Ok(format!("'{executable}' --ssh-route-leg {context} {leg}"))
}
fn authentication_opts(host: &str, route: &Route, node: bool) -> Vec<String> {
    SSH_AUTHENTICATION
        .try_with(|scope| scope.options(host, route, node))
        .unwrap_or_default()
}
fn auth_word(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-@:[]".contains(&byte))
}
fn authentication_path(path: &Path) -> anyhow::Result<String> {
    let text = path
        .to_str()
        .filter(|text| {
            path.is_absolute()
                && text.len() <= 1024
                && text
                    .bytes()
                    .all(|byte| byte.is_ascii_graphic() && !b"%$\"'\\".contains(&byte))
        })
        .context("invalid SSH authentication path")?;
    Ok(text.into())
}
async fn validate_authentication_files(socket: &Path, known_hosts: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        let socket = tokio::fs::symlink_metadata(socket)
            .await
            .map_err(|_| anyhow::anyhow!("SSH authentication socket unavailable"))?;
        let trust = tokio::fs::symlink_metadata(known_hosts)
            .await
            .map_err(|_| anyhow::anyhow!("SSH authentication trust unavailable"))?;
        anyhow::ensure!(
            socket.file_type().is_socket() && trust.is_file() && trust.len() <= 128 * 1024,
            "invalid SSH authentication files"
        );
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (socket, known_hosts);
        bail!("SSH authentication scope unsupported")
    }
}
fn authentication_config(
    config: &str,
    hostname: &str,
    user: &str,
    port: u16,
) -> anyhow::Result<bool> {
    let destination = parse_ssh_destination(config)?;
    anyhow::ensure!(
        destination == (hostname.into(), Some(user.into()), port),
        "SSH authentication destination changed"
    );
    let values = |key: &'static str| {
        config
            .lines()
            .filter_map(move |line| line.strip_prefix(key))
    };
    for key in ["identityfile ", "certificatefile "] {
        let mut found = false;
        for value in values(key) {
            found = true;
            anyhow::ensure!(
                value == "none",
                "SSH authentication configuration uses local identities"
            );
        }
        anyhow::ensure!(
            found,
            "SSH authentication identity configuration unavailable"
        );
    }
    Ok(!values("proxyjump ")
        .chain(values("proxycommand "))
        .any(|value| value != "none"))
}
fn authentication_snapshot(
    alias: &str,
    route: Route,
    config: &str,
    hostname: &str,
    user: &str,
    port: u16,
) -> anyhow::Result<(bool, MasterHandle)> {
    let fresh = authentication_config(config, hostname, user, port)?;
    let master = capture_master_config(alias, route, config)?;
    authentication_path(Path::new(&master.path))?;
    Ok((fresh, master))
}

/// Opaque identity of the actual expanded OpenSSH control socket. Alias names
/// and configured hostname tuples cannot identify a routed/shared master.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MasterIdentity([u8; 32]);

/// An exact captured mux leg, with no credential or authority to authenticate.
/// Deliberately no Debug: control paths and routing metadata stay out of logs.
#[derive(Clone)]
pub struct MasterHandle {
    host: String,
    route: Route,
    path: String,
    identity: MasterIdentity,
}
impl MasterHandle {
    pub fn identity(&self) -> MasterIdentity {
        self.identity
    }
    pub async fn present(&self) -> anyhow::Result<bool> {
        let mut command = self.command();
        command.args(["-O", "check"]).arg(&self.host);
        master_presence(command, 5).await
    }
    /// Owned teardown is pinned before spawning; a later route/config change
    /// cannot redirect it, and caller cancellation does not abandon the leg.
    pub async fn close(&self) -> anyhow::Result<()> {
        let mut command = self.command();
        command.args(["-O", "exit"]).arg(&self.host);
        captured_master_close(command).await
    }
    fn command(&self) -> Command {
        let mut command = mux_prologue(&self.host, &self.route);
        // -S sets the exact socket after the ordinary %C option, without
        // recomputing it from a changed hostname/jump/configuration.
        command.arg("-S").arg(&self.path);
        command
    }
    pub async fn with_existing<F: std::future::Future>(&self, future: F) -> F::Output {
        EXISTING_MASTER_ONLY.scope(self.clone(), future).await
    }
}
async fn captured_master_close(command: Command) -> anyhow::Result<()> {
    tokio::spawn(master_exit(command, Duration::from_secs(5)))
        .await
        .map_err(|_| anyhow::anyhow!("SSH captured login cleanup unavailable"))?
}

/// Run SSH helpers using an already authenticated ControlMaster only. If its
/// socket disappears, SSH must fail rather than dial, run a configured proxy,
/// or ask for credentials. Ordinary explicit connections retain their defaults.
///
/// This scope follows the current async task, including nested helpers, and is
/// restored on cancellation. Spawned tasks do not inherit it: an owned worker
/// must establish the scope inside that task before constructing SSH commands.
pub async fn with_existing_master<F: std::future::Future>(
    host: &str,
    future: F,
) -> anyhow::Result<F::Output> {
    let host = hosts::normalize_alias(host)?;
    let handle = capture_master(&host, route_of(&host)).await?;
    Ok(handle.with_existing(future).await)
}

fn scoped_master(host: &str, route: &Route) -> anyhow::Result<Option<MasterHandle>> {
    match EXISTING_MASTER_ONLY.try_with(Clone::clone) {
        Ok(handle) => {
            anyhow::ensure!(
                handle.host == host && handle.route == *route,
                "SSH captured login route changed"
            );
            Ok(Some(handle))
        }
        Err(_) => Ok(None),
    }
}

fn existing_master_opts(host: &str, route: &Route) -> Vec<String> {
    if let Ok(handle) = EXISTING_MASTER_ONLY.try_with(Clone::clone) {
        // First values win in OpenSSH. BatchMode alone still allows a fresh
        // public-key login and ProxyJump; the failed proxy prevents all fallback
        // transport after the mux lookup, including its disappearance race.
        let mut options: Vec<String> = [
            "-o",
            "ControlMaster=no",
            "-o",
            "ControlPersist=no",
            "-o",
            "BatchMode=yes",
            "-o",
            "KbdInteractiveAuthentication=no",
            "-o",
            "ProxyCommand=false",
            "-o",
            "ProxyJump=none",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        options.extend([
            "-o".into(),
            format!(
                "ControlPath={}",
                if handle.host == host && handle.route == *route {
                    &handle.path
                } else {
                    "none"
                }
            ),
        ]);
        options
    } else {
        vec![]
    }
}

/// Capture the routed and alias legs while the caller owns its SSH effect gate.
/// Resolution is a bounded local ssh -G, never a network/authentication attempt.
pub async fn master_handles(host: &str) -> anyhow::Result<Vec<MasterHandle>> {
    let host = hosts::normalize_alias(host)?;
    let route = route_of(&host);
    let first = capture_master(&host, route.clone()).await?;
    let mut handles = vec![first];
    if route != Route::Alias {
        let alias = capture_master(&host, Route::Alias).await?;
        if alias.identity != handles[0].identity {
            handles.push(alias);
        }
    }
    Ok(handles)
}
async fn capture_master(host: &str, route: Route) -> anyhow::Result<MasterHandle> {
    // Do not inherit a surrounding existing-only scope: ProxyJump contributes
    // to %C. Resolve the original socket before replacing fallback transport.
    let mut command = transport_command("ssh");
    command
        .args(route_opts(host, &route))
        .args(ssh_opts())
        .args(["-T", "-G"])
        .arg(host);
    capture_master_command(host, route, command).await
}
async fn capture_master_command(
    host: &str,
    route: Route,
    mut command: Command,
) -> anyhow::Result<MasterHandle> {
    let output = output_bounded(&mut command, 5, "SSH master identity")
        .await
        .map_err(|_| anyhow::anyhow!("SSH master identity unavailable"))?;
    anyhow::ensure!(output.status.success(), "SSH master identity unavailable");
    let text = std::str::from_utf8(&output.stdout)
        .map_err(|_| anyhow::anyhow!("SSH master identity invalid"))?;
    capture_master_config(host, route, text)
}
fn capture_master_config(host: &str, route: Route, text: &str) -> anyhow::Result<MasterHandle> {
    let paths: Vec<_> = text
        .lines()
        .filter_map(|line| line.strip_prefix("controlpath "))
        .take(2)
        .collect();
    anyhow::ensure!(
        paths.len() == 1
            && paths[0].starts_with('/')
            && paths[0].len() <= 1024
            && !paths[0].chars().any(char::is_control)
            && !paths[0].contains('%'),
        "SSH master identity invalid"
    );
    use sha2::Digest;
    Ok(MasterHandle {
        host: host.into(),
        route,
        path: paths[0].into(),
        identity: MasterIdentity(sha2::Sha256::digest(paths[0].as_bytes()).into()),
    })
}

/// The ssh ControlMaster socket path pattern for chimaera connections. `%C`
/// is ssh's own hash of (localhost, remotehost, port, user, ProxyJump): unique per
/// destination and short. The parent dir is created on demand (ssh will not
/// create it for the socket).
///
/// The WHOLE expanded socket path must stay under the ~104-byte unix-socket
/// (`sun_path`) limit. `data_dir()/cm/%C` clears it comfortably for the normal
/// `~/.chimaera` home, but an isolated `CHIMAERA_HOME` under a deep worktree
/// path (the dev app) overshoots even though `%C` keeps the leaf short — ssh
/// then fails every call with "ControlPath too long". So when the preferred
/// path wouldn't fit, anchor the socket in a short `/tmp` dir keyed by a hash of
/// the home: short enough for `sun_path`, yet still distinct per home so a dev
/// app's master never collides with the real app's. A normal home is
/// unaffected — it keeps `data_dir()/cm/%C`.
fn control_path() -> String {
    if let Some(t) = wsl_transport() {
        return wsl_control_path(&t.home);
    }
    let dir = control_dir(&chimaera_core::data_dir());
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("failed to create ssh control dir {}: {e}", dir.display());
    }
    dir.join("%C").to_string_lossy().into_owned()
}

/// On Windows every ssh/scp runs INSIDE the WSL2 distro that hosts the local
/// daemon: `connect`'s whole architecture rides on ControlMaster, which
/// Win32-OpenSSH does not implement — Linux OpenSSH in the distro does. The
/// shell wires this once the distro is known; `None` (always, on unix) means
/// spawn the system ssh directly.
#[derive(Clone, Debug)]
pub struct WslTransport {
    /// The distro whose ssh (and daemon) we ride.
    pub distro: String,
    /// The distro user every spawn pins with `-u` (the default user can
    /// change under us — Ubuntu's OOBE flips it on first interactive run).
    pub user: String,
    /// The distro-side `$HOME`, anchoring the ControlMaster sockets — a
    /// socket ssh creates inside the distro cannot live on a Windows path.
    pub home: String,
}

static WSL_TRANSPORT: std::sync::RwLock<Option<WslTransport>> = std::sync::RwLock::new(None);
static SSH_CONFIG: std::sync::RwLock<Option<PathBuf>> = std::sync::RwLock::new(None);

/// Select a process-owned SSH config for a sandbox. Ordinary clients leave this
/// unset and inherit their own config; a single-account service can supply only
/// explicitly approved host metadata without reading an operator's SSH config.
pub fn set_ssh_config(path: Option<PathBuf>) {
    *SSH_CONFIG.write().unwrap_or_else(|p| p.into_inner()) = path;
}

pub fn set_wsl_transport(t: Option<WslTransport>) {
    *WSL_TRANSPORT.write().unwrap_or_else(|p| p.into_inner()) = t;
}

fn wsl_transport() -> Option<WslTransport> {
    WSL_TRANSPORT
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

/// Whether the WSL transport is wired and remote connects can work at all
/// on this host — the shell refuses a connect when wiring failed rather
/// than silently spawning Win32-OpenSSH (which lacks ControlMaster).
pub fn wsl_transport_ready() -> bool {
    wsl_transport().is_some()
}

/// The ssh/scp process builder, transport-aware. `--exec` bypasses the
/// distro's login shell; env the ssh children need (`SSH_ASKPASS` et al)
/// crosses the boundary via `WSLENV`, which the shell exports. The user is
/// pinned with `-u` so a later change of the distro's default user (Ubuntu
/// OOBE) can never silently re-home ssh's config/keys/sockets mid-flight.
fn transport_command(program: &str) -> Command {
    // A policy-bearing effect must use the same trusted binary whose closed
    // supported-algorithm snapshot narrowed its arguments. Replacement refuses
    // rather than applying that snapshot to a different SSH implementation.
    if matches!(program, "ssh" | "scp") {
        if let Ok(Some(support)) = SSH_AUTHENTICATION.try_with(|scope| scope.support.clone()) {
            let mut command = if support.current() {
                Command::new(if program == "ssh" {
                    "/usr/bin/ssh"
                } else {
                    "/usr/bin/scp"
                })
            } else {
                Command::new("/usr/bin/false")
            };
            if program == "scp" {
                command.args(["-S", "/usr/bin/ssh"]);
            }
            command.env_remove(ASKPASS_CONTEXT_ENV);
            if EXISTING_MASTER_ONLY.try_with(|_| ()).is_err() {
                if let Ok(Some(context)) =
                    SSH_AUTHENTICATION.try_with(|scope| scope.keyboard_interactive.clone())
                {
                    command.env(ASKPASS_CONTEXT_ENV, context);
                }
            }
            return command;
        }
    }
    let mut command = match wsl_transport() {
        Some(t) => {
            let mut c = Command::new("wsl.exe");
            c.args(["-d", &t.distro, "-u", &t.user, "--exec", program]);
            // wsl.exe's own diagnostics are UTF-16LE without this; every
            // caller decodes stderr as UTF-8 (legacy inbox WSL ignores the
            // var — its errors stay mojibake, acceptably rare).
            c.env("WSL_UTF8", "1");
            #[cfg(windows)]
            c.creation_flags(chimaera_core::CREATE_NO_WINDOW);
            c
        }
        None => Command::new(program),
    };
    if matches!(program, "ssh" | "scp") {
        // Inherited context must never license an unrelated/background command.
        command.env_remove(ASKPASS_CONTEXT_ENV);
        if EXISTING_MASTER_ONLY.try_with(|_| ()).is_err() {
            if let Ok(Some(context)) =
                SSH_AUTHENTICATION.try_with(|scope| scope.keyboard_interactive.clone())
            {
                command.env(ASKPASS_CONTEXT_ENV, context);
            }
        }
        if let Some(path) = SSH_CONFIG
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            command.arg("-F").arg(path);
        }
    }
    command
}

/// A `curl` invocation with the same no-console discipline as every other
/// child this GUI-adjacent crate spawns on Windows.
fn curl_command() -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new("curl");
    #[cfg(windows)]
    c.creation_flags(chimaera_core::CREATE_NO_WINDOW);
    c
}

/// Per-child output ceilings. Every caller expects small control-plane JSON,
/// version text, or diagnostics; downloads write to a file. Keeping the caps
/// here prevents a noisy/malicious endpoint from exhausting the GUI process
/// before the wall-clock timeout fires.
const CHILD_STDOUT_MAX_BYTES: usize = 8 * 1024 * 1024;
const CHILD_STDERR_MAX_BYTES: usize = 1024 * 1024;

async fn read_bounded<R>(reader: R, cap: usize, stream: &str) -> anyhow::Result<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::with_capacity(cap.min(64 * 1024));
    reader
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .with_context(|| format!("failed to read child {stream}"))?;
    if bytes.len() > cap {
        bail!("child {stream} exceeded the {cap}-byte limit");
    }
    Ok(bytes)
}

/// Collect an already-spawned child's piped output with byte and time bounds.
/// On overflow/timeout the child is killed and reaped; dropping a read future
/// closes its pipe, so a producer cannot remain wedged behind backpressure.
async fn collect_child_bounded(
    child: Child,
    secs: u64,
    what: &str,
) -> anyhow::Result<std::process::Output> {
    collect_child_bounded_with_caps(
        child,
        secs,
        what,
        CHILD_STDOUT_MAX_BYTES,
        CHILD_STDERR_MAX_BYTES,
    )
    .await
}

async fn collect_child_bounded_with_caps(
    mut child: Child,
    secs: u64,
    what: &str,
    stdout_cap: usize,
    stderr_cap: usize,
) -> anyhow::Result<std::process::Output> {
    let stdout = child.stdout.take().context("child stdout was not piped")?;
    let stderr = child.stderr.take().context("child stderr was not piped")?;
    let collect = async {
        let (stdout, stderr) = tokio::try_join!(
            read_bounded(stdout, stdout_cap, "stdout"),
            read_bounded(stderr, stderr_cap, "stderr"),
        )?;
        let status = child.wait().await.context("failed to wait for child")?;
        Ok::<_, anyhow::Error>(std::process::Output {
            status,
            stdout,
            stderr,
        })
    };

    match tokio::time::timeout(Duration::from_secs(secs), collect).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(err)) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            Err(err).with_context(|| format!("failed to collect {what}"))
        }
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            bail!("{what} timed out after {secs}s")
        }
    }
}

/// Collect a command's output with hard byte and time bounds. Under the WSL
/// transport a wedged wsl.exe would otherwise hang forever — ssh's own
/// `ConnectTimeout` only bounds the TCP connect *inside* the distro.
async fn output_bounded(
    cmd: &mut Command,
    secs: u64,
    what: &str,
) -> anyhow::Result<std::process::Output> {
    cmd.kill_on_drop(true);
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = cmd
        .spawn()
        .with_context(|| format!("failed to run {what}"))?;
    collect_child_bounded(child, secs, what).await
}

/// ssh runs inside the distro, so its ControlMaster socket must live on a
/// distro-side path (the shell `mkdir -p`s the dir when wiring the
/// transport). Deliberately the same `~/.chimaera/cm` an in-distro `connect`
/// would use: the app and the distro share one authenticated master per
/// host. Pure so the shape is testable without touching the global.
fn wsl_control_path(home: &str) -> String {
    format!("{home}/.chimaera/cm/%C")
}

/// How a local file path is spelled for a process inside WSL (`C:\x\y` →
/// `/mnt/c/x/y`) — scp's "local" side runs in the distro under the transport.
pub fn local_path_for_scp(path: &std::path::Path) -> String {
    if wsl_transport().is_none() {
        return path.to_string_lossy().into_owned();
    }
    windows_path_as_wsl(&path.to_string_lossy())
}

/// How a Windows path is spelled for a process inside WSL: drive-letter
/// absolutes (including `\\?\`-verbatim ones, which `current_exe`/
/// canonicalize can produce) become `/mnt/<drive>/…`; UNC and other shapes
/// pass through untouched — callers must treat an un-translated result as
/// "not reachable from the distro", not silently use it. Shared by scp's
/// local side AND the shell's askpass-wrapper exe path, so the two can't
/// drift.
pub fn windows_path_as_wsl(s: &str) -> String {
    let s = s.strip_prefix(r"\\?\").unwrap_or(s).replace('\\', "/");
    if s.len() >= 3 && s.as_bytes()[1] == b':' && s.as_bytes()[2] == b'/' {
        format!("/mnt/{}{}", s[..1].to_ascii_lowercase(), &s[2..])
    } else {
        s
    }
}

/// The ControlMaster socket DIRECTORY for a given state dir: `<data_dir>/cm`
/// normally, or a short `/tmp/chimaera-<home-hash>/cm` when that would push the
/// expanded socket path past the `sun_path` limit (a deep isolated
/// `CHIMAERA_HOME`). Pure so the length invariant can be tested without env.
fn control_dir(data_dir: &std::path::Path) -> std::path::PathBuf {
    /// `%C` expands to a 40-hex-char hash. OpenSSH appends a temporary
    /// `.XXXXXXXXXXXX...` suffix while creating the mux listener, and macOS
    /// applies the same `sun_path` limit to that transient path.
    const C_LEAF_WITH_OPENSSH_TMP_SUFFIX: usize = 40 + 1 + 16;
    /// Headroom under the ~104-byte `sun_path` cap.
    const SUN_PATH_SAFE: usize = 100;

    let preferred = data_dir.join("cm");
    if preferred.as_os_str().len() + 1 + C_LEAF_WITH_OPENSSH_TMP_SUFFIX <= SUN_PATH_SAFE {
        preferred
    } else {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        data_dir.hash(&mut h);
        // Keep the `/cm` tail so the socket shape (`…/cm/%C`) is stable per home.
        std::path::PathBuf::from(format!("/tmp/chimaera-{:08x}", h.finish() as u32)).join("cm")
    }
}

/// The `-o` options shared by every chimaera ssh/scp call to a host.
///
/// *ControlMaster* — `ControlMaster=auto` makes the first connection the
/// master and later ones reuse it; `ControlPersist` keeps it warm after
/// clients disconnect so reconnects and new windows skip re-authentication.
///
/// *Host key* — `StrictHostKeyChecking=accept-new` is the trust-on-first-use
/// policy a windowed app needs: ssh has no tty to answer the "authenticity of
/// host … (yes/no)?" prompt, so a freshly installed app connecting to a host
/// it has never seen would otherwise fail outright. `accept-new` records an
/// unknown key automatically but still *refuses* a changed one — keeping the
/// MITM protection that a blanket `=no` would throw away.
///
/// *Liveness* — `ConnectTimeout` bounds the TCP connect so an unreachable
/// host fails in seconds instead of the OS default (minutes) — a hung
/// connect pins the UI in "connecting". `ServerAliveInterval`/`CountMax`
/// make the ControlMaster and `-N` tunnel children *notice* a dead link
/// (laptop sleep, network change) within ~45s and exit; without them a dead
/// tunnel keeps its local listener open for hours, so every liveness probe
/// lies "up" and reconnect becomes a no-op.
fn ssh_opts() -> [String; 16] {
    [
        "-o".into(),
        "ControlMaster=auto".into(),
        "-o".into(),
        format!("ControlPath={}", control_path()),
        "-o".into(),
        "ControlPersist=10m".into(),
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
        "-o".into(),
        "ConnectTimeout=15".into(),
        "-o".into(),
        "ServerAliveInterval=15".into(),
        "-o".into(),
        "ServerAliveCountMax=3".into(),
        // Compression: negotiated once at master creation and inherited by
        // every mux channel. Terminal streams and escape-sequence snapshots
        // compress enormously; on the WAN links this product targets (HPC
        // login nodes) that is bandwidth the tunnel doesn't spend, and the
        // CPU cost measured negligible (~2% of one core encrypting a
        // 75 Mbit/s flood, before compression shrinks it). Applies only to
        // chimaera's own masters — the user's ssh config is untouched.
        "-o".into(),
        "Compression=yes".into(),
    ]
}

/// An `ssh` command pre-loaded with the shared options, no host yet. For
/// flag-heavy invocations where the destination must come last
/// (`-O cancel -L …`, `-N -L …`); otherwise prefer [`ssh_cmd`]. Follows
/// `host`'s current [`Route`].
fn ssh_base(host: &str) -> Command {
    ssh_base_via(host, &route_of(host))
}

/// [`ssh_base`] along an explicit route (a tunnel tearing down the forward it
/// registered, a wedge check of one particular master).
fn ssh_base_via(host: &str, route: &Route) -> Command {
    let mut c = transport_command("ssh");
    c.env(ASKPASS_ALIAS_ENV, host);
    c.args(existing_master_opts(host, route));
    c.args(authentication_opts(host, route, false));
    c.args(route_opts(host, route));
    c.args(ssh_opts());
    c
}

/// An `ssh` command pre-loaded with the shared ControlMaster options and
/// pointed at `host`, ready for the remote command to be appended. The common
/// shape (`ssh <opts> host <cmd>`); every plain remote command goes through
/// here so they all share one authenticated connection.
fn ssh_cmd(host: &str) -> Command {
    let mut c = ssh_base(host);
    c.arg(host);
    c
}

/// The argv of an interactive `ssh` to `host` over chimaera's own
/// ControlMaster (the user's terminal on a cluster's login node): the shared
/// options along the host's current route, a forced tty, the host. No
/// password prompt when the master is up; ssh's own prompt in the terminal
/// when it isn't.
pub fn interactive_ssh_argv(host: &str) -> Vec<String> {
    let mut argv = vec!["ssh".to_string()];
    argv.extend(existing_master_opts(host, &route_of(host)));
    argv.extend(authentication_opts(host, &route_of(host), false));
    argv.extend(route_opts(host, &route_of(host)));
    argv.extend(ssh_opts());
    argv.push("-t".into());
    argv.push(host.to_string());
    argv
}

/// An `scp` command pre-loaded with the shared options, so a binary copy
/// reuses the connection the probe already authenticated instead of prompting
/// again.
fn scp_cmd(host: &str) -> Command {
    let mut c = transport_command("scp");
    c.env(ASKPASS_ALIAS_ENV, host);
    c.args(existing_master_opts(host, &route_of(host)));
    c.args(authentication_opts(host, &route_of(host), false));
    c.args(route_opts(host, &route_of(host)));
    c.args(ssh_opts());
    c
}

// --- Round-robin login nodes --------------------------------------------------
//
// An HPC alias often names a POOL of login nodes (one DNS name, rotated per
// lookup), and every login node mounts the same `$HOME`. The daemon runs on
// ONE of them: its manifest on the shared home is visible from every node, but
// its pid and loopback port mean something only on the node that wrote it. A
// ControlMaster keeps every command on the node it landed on — until a new
// master dials (after sleep, a `ControlPersist` expiry, an app relaunch) and
// lands on another node, where `kill -0 <pid>` answers for an unrelated process
// table. So the probe reports which node it ran on, a manifest written
// elsewhere is never judged from the wrong node, and every later ssh call for
// the alias is routed to the daemon's node.

/// Which node an ssh/scp call for a host alias lands on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Route {
    /// Wherever the alias's own ssh config sends a new connection.
    #[default]
    Alias,
    /// One named node, dialed with the alias's own config and only its
    /// `HostName` replaced: user, keys, ProxyJump and 2FA carry over, and the
    /// node gets its own ControlMaster (`%C` hashes the host name). One
    /// authentication, no dependency on another connection — but the name is
    /// resolved and dialed from THIS machine.
    Node(String),
    /// One named node reached through the alias's own ControlMaster (a `-W`
    /// first leg): the name is resolved and dialed from inside the cluster,
    /// for names this machine can't resolve or login nodes it can't reach.
    NodeViaAlias(String),
}

impl Route {
    /// The node this route pins, if any.
    pub fn node(&self) -> Option<&str> {
        match self {
            Route::Alias => None,
            Route::Node(node) | Route::NodeViaAlias(node) => Some(node),
        }
    }
}

/// The learned route per alias, process-wide: a connect records where the
/// daemon lives and every later call for that alias — the tunnel, stops,
/// session counts, compute tunnels, wedge checks — follows it, the way they
/// all share one ControlMaster. Absent = [`Route::Alias`].
static ROUTES: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, Route>>> =
    std::sync::LazyLock::new(Default::default);

/// The route every ssh/scp call for `host` currently takes.
pub fn route_of(host: &str) -> Route {
    ROUTES
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(host)
        .cloned()
        .unwrap_or_default()
}

fn set_route(host: &str, route: Route) {
    let mut routes = ROUTES.lock().unwrap_or_else(|p| p.into_inner());
    match route {
        Route::Alias => routes.remove(host),
        route => routes.insert(host.to_string(), route),
    };
}

/// The `-o` options that send a call for `host` along `route`. Placed before
/// [`ssh_opts`] and the destination: OpenSSH keeps the first value it sees,
/// so these override the alias's own `HostName` (and, for the via-alias
/// route, its `ProxyJump`/`ProxyCommand`) from `~/.ssh/config`.
fn route_opts(host: &str, route: &Route) -> Vec<String> {
    match route {
        Route::Alias => Vec::new(),
        Route::Node(node) => vec!["-o".into(), format!("HostName={node}")],
        Route::NodeViaAlias(node) => vec![
            "-o".into(),
            format!("HostName={node}"),
            "-o".into(),
            format!("ProxyCommand={}", master_proxy_command(host, None)),
        ],
    }
}

/// Whether a node name from a manifest is safe to put in ssh's argv and in a
/// `ProxyCommand` (whose `%h` a local shell expands): letters, digits, `-`,
/// `_`, `.`, no leading `-`/`.`. The manifest is data on a remote disk, never
/// trusted syntax.
fn valid_node_name(node: &str) -> bool {
    !node.is_empty()
        && node.len() <= 253
        && !node.starts_with(['-', '.'])
        && node
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The routes to try, in order, for reaching `node`. The direct dial goes
/// first — one prompt, no second connection to keep up — but only when THIS
/// machine resolves the name to an address the cluster resolves it to
/// (`cluster` from the landed node, `local` here). Otherwise the name may
/// reach an unrelated host through this machine's search domains, which the
/// trust-on-first-use host-key policy would accept and the password prompt
/// would then be answered to; such a name only ever travels inside the
/// cluster.
fn routes_to(node: &str, cluster: &[std::net::IpAddr], local: &[std::net::IpAddr]) -> Vec<Route> {
    let via = Route::NodeViaAlias(node.to_string());
    if cluster.iter().any(|addr| local.contains(addr)) {
        vec![Route::Node(node.to_string()), via]
    } else {
        vec![via]
    }
}

/// Where THIS machine resolves `node` (its sshd port), bounded so a slow
/// resolver can't stall a connect. Empty on failure — the direct rung is then
/// skipped, never guessed.
async fn local_addrs(node: &str) -> Vec<std::net::IpAddr> {
    let lookup = tokio::net::lookup_host((node, 22));
    match tokio::time::timeout(Duration::from_secs(5), lookup).await {
        Ok(Ok(addrs)) => addrs.map(|addr| addr.ip()).collect(),
        _ => Vec::new(),
    }
}

/// Where the connect flow currently is; consumers surface these however
/// fits (tracing lines in the CLI, progress events in the shell).
#[derive(Clone, Debug)]
pub enum Phase {
    /// Probing the host for a running daemon.
    Probing,
    /// The daemon runs on another node of the alias's login-node pool than
    /// the one this connection landed on; reaching `node` (which may ask the
    /// user to authenticate to it).
    Routing { node: String },
    /// Replacing an outdated remote daemon (graceful stop, then redeploy).
    Updating,
    /// Fetching the matching daemon binary from the GitHub release this build
    /// came from (the end-user path: no repo, no `just dist` stash).
    Downloading { target: String },
    /// Copying a chimaera binary to the host.
    Installing { binary: PathBuf },
    /// Starting the daemon on the host.
    Starting,
    /// Waiting for the local port-forward to come up.
    Tunneling { local_port: u16 },
}

/// Which trusted source may supply a daemon when deployment is needed.
/// Healthy reconnects never resolve a binary. Selected assemblies require the
/// original explicit artifact instead of consulting the public release feed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DeploymentSource {
    #[default]
    PublicRelease,
    ExplicitBinary,
}

/// Options for [`connect`].
#[derive(Default)]
pub struct ConnectOpts {
    /// Local port for the tunnel (defaults to the remote port if free).
    pub local_port: Option<u16>,
    /// Explicit binary to install on the host if chimaera is missing;
    /// otherwise `~/.chimaera/dist/` is searched for a matching build.
    pub binary: Option<PathBuf>,
    /// Fixed assembly deployment policy; the free default remains unchanged.
    pub deployment_source: DeploymentSource,
    /// Reinstall the executable, even at the same build. A direct-host daemon
    /// restarts gracefully (SIGTERM); its live sessions end. Cluster job mode
    /// only stages the executable for future jobs, without starting a daemon.
    pub update_daemon: bool,
    /// The user allowed a daemon on this cluster's login node (the warned
    /// per-host override). Without it, a host whose login shell reaches a
    /// batch scheduler never gets a daemon: connect answers [`ClusterHost`].
    pub login_serve: bool,
    /// The user said this host is not a cluster (Slurm's tools on a
    /// workstation's PATH): it connects like any host, and a daemon started
    /// here doesn't tell its agents they are on a shared login node.
    pub not_cluster: bool,
}

impl ConnectOpts {
    fn deployment_binary(&self) -> anyhow::Result<Option<&Path>> {
        if self.deployment_source == DeploymentSource::ExplicitBinary && self.binary.is_none() {
            bail!("The selected daemon requires an explicit compatible --binary artifact; automatic public release deployment is unavailable");
        }
        Ok(self.binary.as_deref())
    }
}

/// `connect` found a cluster: a host whose login shell reaches a batch
/// scheduler. Nothing was started, updated or tunnelled — chimaera runs on
/// such a host only inside jobs (see [`cluster`]), unless the user allowed
/// the login node ([`ConnectOpts::login_serve`]). Callers downcast for it
/// like [`TunnelPhaseError`].
#[derive(Clone, Debug)]
pub struct ClusterHost {
    pub host: String,
    pub scheduler: Scheduler,
    /// A daemon a previous connect left on the login node, if the probe saw
    /// its manifest — so the UI can offer to shut it down.
    pub login_daemon: Option<LoginDaemon>,
}

/// A chimaera daemon registered on a cluster's login node.
#[derive(Clone, Debug)]
pub struct LoginDaemon {
    /// The login node it registered on.
    pub node: String,
    pub pid: u32,
    /// `Some` when judged on its own node; `None` when it registered on
    /// another node of the alias's pool (its pid means nothing from here).
    pub alive: Option<bool>,
}

impl std::fmt::Display for ClusterHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} is a {} cluster: chimaera doesn't run on its login nodes — start a workspace as a job instead",
            self.host,
            self.scheduler.tag()
        )
    }
}

impl std::error::Error for ClusterHost {}

/// Which per-user state root on the HOST a connect targets. Every remote
/// side effect — the manifest probed, the binary installed, the daemon
/// started (and that daemon's own state, via `CHIMAERA_HOME`), the
/// reuse/update decision — derives from this one value, so the two roots are
/// fully disjoint: a dev daemon runs NEXT TO the real one and a dev connect
/// can never stop, replace, or even read the real daemon.
///
/// `Real` is the shared `~/.chimaera`, where the home IS the data dir
/// (manifest at `~/.chimaera/manifest.json`). `Dev` runs the daemon under
/// `CHIMAERA_HOME=~/.chimaera-dev`, which relocates its data dir to
/// `<home>/data` — hence the asymmetric manifest/log paths. The dev home
/// stays short and `$HOME`-anchored deliberately: the remote daemon's own
/// runtime dir (`<home>/run`) is bound by the same ~104-byte `sun_path`
/// limit as our local sockets (see [`control_dir`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RemoteHome {
    /// The end-user daemon at `~/.chimaera` (release binaries).
    #[default]
    Real,
    /// The isolated dev daemon at `~/.chimaera-dev` (locally built binaries).
    Dev,
}

impl RemoteHome {
    /// Which home THIS build targets — the build's property, never a
    /// per-host or per-connect choice: a dev build (the unstamped `0.0.1`
    /// sentinel) always talks to `~/.chimaera-dev` on both ends, a release
    /// always to `~/.chimaera`. No toggle exists, so a dev tunnel can never
    /// heal into the real daemon (or vice versa) across reconnects.
    pub fn current() -> Self {
        if chimaera_core::is_dev_build() {
            RemoteHome::Dev
        } else {
            RemoteHome::Real
        }
    }

    /// The state root as a `$HOME`-anchored fragment for remote shell
    /// commands (expanded by the remote shell, never locally).
    fn dir(self) -> &'static str {
        match self {
            RemoteHome::Real => "$HOME/.chimaera",
            RemoteHome::Dev => "$HOME/.chimaera-dev",
        }
    }

    /// The daemon's manifest path. Real: the home is the data dir. Dev:
    /// `CHIMAERA_HOME` relocates data one level down, to `<home>/data`.
    fn manifest_path(self) -> String {
        match self {
            RemoteHome::Real => format!("{}/manifest.json", self.dir()),
            RemoteHome::Dev => format!("{}/data/manifest.json", self.dir()),
        }
    }

    /// The directory the started daemon's stdout/stderr log lives in (same
    /// data-dir split as [`Self::manifest_path`]).
    fn log_dir(self) -> String {
        match self {
            RemoteHome::Real => format!("{}/logs", self.dir()),
            RemoteHome::Dev => format!("{}/data/logs", self.dir()),
        }
    }

    fn log_path(self) -> String {
        format!("{}/serve.log", self.log_dir())
    }

    fn bin_dir(self) -> String {
        format!("{}/bin", self.dir())
    }

    pub(crate) fn bin_path(self) -> String {
        format!("{}/chimaera", self.bin_dir())
    }

    /// The staged-upload scp destination, relative to the remote `$HOME`
    /// (scp has no remote shell to expand `$HOME` in).
    fn scp_staged_bin(self) -> &'static str {
        match self {
            RemoteHome::Real => ".chimaera/bin/chimaera.new",
            RemoteHome::Dev => ".chimaera-dev/bin/chimaera.new",
        }
    }

    /// The env assignment that scopes the started daemon (and all the state
    /// it writes) to this home — empty for the real home. An ENV PREFIX, not
    /// a flag: `chimaera serve` is a load-bearing CLI string and must stay
    /// exactly that.
    fn serve_env(self) -> &'static str {
        match self {
            RemoteHome::Real => "",
            RemoteHome::Dev => "CHIMAERA_HOME=$HOME/.chimaera-dev ",
        }
    }
}

/// What [`connect`] should do about a daemon already running on the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Same build (and no force): attach to it as-is.
    Reuse,
    /// Replace it: graceful stop, redeploy, restart. Chosen when the builds
    /// differ and it is provably safe (zero live sessions), or when forced.
    Update,
    /// Builds differ but sessions could die (live count > 0 or unknown):
    /// attach to the old daemon and surface the mismatch to the caller.
    ConnectOutdated,
}

/// Pure policy for daemon reuse vs replacement, shared by the remote
/// connect flow and the app's local-daemon startup. `sessions` `None`
/// means the count could not be determined — treated as busy, never as
/// empty. `force` replaces the daemon regardless of build or session count
/// (the explicit `--update-daemon` / host-row affordance).
pub fn update_decision(
    local_build: &str,
    remote_build: Option<&str>,
    sessions: Option<usize>,
    force: bool,
) -> Decision {
    if chimaera_core::builds_match(local_build, remote_build) && !force {
        return Decision::Reuse;
    }
    if force || sessions == Some(0) {
        return Decision::Update;
    }
    Decision::ConnectOutdated
}

/// A failure in the local-forward phase of [`connect`] (port bind / forward
/// setup), as opposed to auth, probe, or install failures. Distinguished so
/// callers retry ONLY these on a fresh local port — blindly re-running the
/// whole connect on an auth failure re-prompts the user's 2FA.
#[derive(Debug)]
pub struct TunnelPhaseError(pub String);

impl std::fmt::Display for TunnelPhaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TunnelPhaseError {}

/// A live port-forward to a remote daemon. Dropping it kills the ssh child
/// (but a ControlMaster-held forward survives — call [`Tunnel::close`] to
/// cancel it explicitly).
pub struct Tunnel {
    pub host: String,
    pub local_port: u16,
    pub manifest: Manifest,
    /// The forward was registered with an ssh ControlMaster and our child
    /// exited 0; the master holds the port, not `child`.
    pub mux_delegated: bool,
    /// The daemon at the far end is an older build than ours, left running
    /// because live sessions (or an unknown count) made replacing it unsafe.
    /// Callers surface this with their explicit update affordance.
    pub outdated: bool,
    /// The connected daemon's build id (`None` = predates build ids).
    pub remote_build: Option<String>,
    /// Live sessions counted on the remote daemon when the update decision
    /// was made; `None` when unneeded (builds matched) or undeterminable.
    pub live_sessions: Option<usize>,
    /// How the forward reaches the daemon's node: [`Route::Alias`] unless the
    /// alias is a login-node pool and the daemon runs on a node other than
    /// the one a new connection lands on.
    pub route: Route,
    child: Child,
}

impl Tunnel {
    /// The UI url for this tunnel. The host alias rides along so the UI can
    /// label the window with the name the user actually calls this machine.
    pub fn url(&self) -> String {
        format!(
            "http://127.0.0.1:{}/#token={}&host={}",
            self.local_port, self.manifest.token, self.host
        )
    }

    /// Wait for the tunnel child to exit (never returns for a healthy
    /// direct forward; returns quickly when delegated to a ControlMaster).
    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }

    /// Kill the tunnel child and cancel any master-held forward so local
    /// ports don't leak past the session that opened them. Only the forward
    /// is cancelled — the ControlMaster stays (ControlPersist), so reconnects
    /// and other windows on this host keep their authenticated connection.
    pub async fn close(mut self) {
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await;
        cancel_master_forward(
            &self.host,
            &self.route,
            &format!("{}:127.0.0.1:{}", self.local_port, self.manifest.port),
        )
        .await;
    }
}

/// Best-effort `ssh -O cancel` of a `-L` forward the host's ControlMaster
/// holds. A forward registered by a mux client belongs to the MASTER, not
/// the client — killing (or outliving) the client leaves the local listener
/// bound until the master expires, so every path that abandons such a
/// forward must cancel it by the exact spec it was opened with, on the
/// master (the `route`) it was registered with.
async fn cancel_master_forward(host: &str, route: &Route, spec: &str) {
    let command = scoped_master(host, route)
        .and_then(|captured| forward_cancel_command(host, route, spec, captured.as_ref()));
    if let Ok(command) = command {
        if forward_cancel(command, Duration::from_secs(10))
            .await
            .is_ok()
        {
            return;
        }
    }
    tracing::warn!("SSH forward cleanup unavailable");
}

fn forward_cancel_command(
    host: &str,
    route: &Route,
    spec: &str,
    captured: Option<&MasterHandle>,
) -> anyhow::Result<Command> {
    let mut command = if let Some(handle) = captured {
        anyhow::ensure!(
            handle.host == host && handle.route == *route,
            "SSH captured forward route changed"
        );
        handle.command()
    } else {
        mux_prologue(host, route)
    };
    command.args(["-O", "cancel", "-L", spec]).arg(host);
    Ok(command)
}

/// Detect and `-O exit` a ControlMaster to `host` whose TCP link is dead:
/// after laptop sleep the master PROCESS usually survives its connection,
/// and until `ServerAliveInterval × CountMax` (~45 s) expires every mux
/// client — the reconnect's probe included — queues on it. Returns whether
/// a master was terminated.
///
/// `-O check` / `-O exit` are answered by the master's own local event loop
/// (no packet crosses the network), so they are safe on a dead link; only
/// the `host true` session open traverses it, which is what makes it the
/// test — and the only step that may declare a wedge: an unanswered check
/// is "could not tell", not a verdict. The 15 s bound is one
/// `ServerAliveInterval` and deliberately not tighter: a merely LOADED
/// login node (an NFS-backed module init under sshd; load 20 on 64 cores
/// seen live) takes seconds to run `true`, and that same load is what
/// confirms a tunnel down — a false positive here kills a healthy master,
/// forcing the Duo re-auth the master exists to avoid and dropping that
/// alias's compute forwards.
/// `session_bound_secs` is how long the session-open test may take before
/// the master is declared wedged: 15 s when the health monitor already
/// confirmed the tunnel down (that same node load produced the misses), but
/// longer when there is no verdict at all — a launch-time restore or a click
/// on a host whose warm master merely sits on a loaded node must not lose
/// its master (and its Duo session) to a slow `true`.
///
/// A host routed to its daemon's node ([`Route`]) has two masters — the
/// node's, and the alias's own (the via-alias route's first leg, or the one
/// the connect first landed through) — and both are checked, the node's first
/// so its `-W` leg is gone before the master it rides.
pub async fn clear_wedged_master(host: &str, session_bound_secs: u64) -> bool {
    let route = route_of(host);
    let mut cleared = clear_wedged_master_via(host, &route, session_bound_secs).await;
    if route != Route::Alias {
        cleared |= clear_wedged_master_via(host, &Route::Alias, session_bound_secs).await;
    }
    cleared
}

/// Close the authenticated SSH logins owned by this state directory for a host.
/// Closing a forward alone intentionally preserves ControlPersist; account-wide
/// revocation must also terminate both legs of a routed login.
pub async fn close_master(host: &str) -> anyhow::Result<()> {
    let host = hosts::normalize_alias(host)?;
    let route = route_of(&host);
    // Dropping a caller must not abandon the alias leg after closing its node.
    close_masters_owned(route, move |route| {
        let host = host.clone();
        async move {
            let mut command = mux_prologue(&host, &route);
            command.args(["-O", "exit"]).arg(&host);
            master_exit(command, Duration::from_secs(5)).await
        }
    })
    .await
    .context("SSH login cleanup task did not finish")?
}

/// Bounded local mux evidence only; no network session, authentication or
/// destructive wedge check. `false` requires the exact absent-socket receipt;
/// an unknown/refused/stalled check is an error, never disappearance proof.
pub async fn master_present(host: &str) -> anyhow::Result<bool> {
    let host = hosts::normalize_alias(host)?;
    capture_master(&host, route_of(&host))
        .await?
        .present()
        .await
}

async fn master_presence(mut command: Command, seconds: u64) -> anyhow::Result<bool> {
    command.env("LC_ALL", "C").env("LANG", "C");
    let output = output_bounded(&mut command, seconds, "SSH master check")
        .await
        .map_err(|_| anyhow::anyhow!("SSH master check unavailable"))?;
    if master_already_absent(&output) {
        return Ok(false);
    }
    let running = output.status.success()
        && output.stdout.is_empty()
        && output.stderr.len() <= 128
        && std::str::from_utf8(&output.stderr)
            .ok()
            .is_some_and(|stderr| {
                let line = stderr.strip_suffix('\n').unwrap_or(stderr);
                line.strip_suffix('\r')
                    .unwrap_or(line)
                    .strip_prefix("Master running (pid=")
                    .and_then(|rest| rest.strip_suffix(')'))
                    .is_some_and(|pid| {
                        !pid.is_empty()
                            && pid.bytes().all(|b| b.is_ascii_digit())
                            && pid.parse::<u32>().ok().is_some_and(|pid| pid > 0)
                    })
            });
    anyhow::ensure!(running, "SSH master check unverified");
    Ok(true)
}

fn close_masters_owned<F, Fut>(
    route: Route,
    mut close: F,
) -> tokio::task::JoinHandle<anyhow::Result<()>>
where
    F: FnMut(Route) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<()>> + Send,
{
    tokio::spawn(async move {
        let first = close(route.clone()).await;
        let alias = if route != Route::Alias {
            close(Route::Alias).await
        } else {
            Ok(())
        };
        first.and(alias)
    })
}

async fn master_exit(mut command: Command, deadline: Duration) -> anyhow::Result<()> {
    // OpenSSH's explicit ENOENT diagnostic distinguishes an absent master from
    // a refused request, invalid config or failed spawn. Other locales fail
    // closed rather than turning an arbitrary nonzero exit into success.
    command.env("LC_ALL", "C");
    let output = tokio::time::timeout(
        deadline,
        output_bounded(&mut command, 10, "SSH login closure"),
    )
    .await
    .context("SSH login did not acknowledge closure within the deadline")?
    .map_err(|_| anyhow::anyhow!("SSH login closure could not be completed"))?;
    if !output.status.success() && !master_already_absent(&output) {
        bail!("SSH login did not acknowledge closure");
    }
    Ok(())
}

fn master_already_absent(output: &std::process::Output) -> bool {
    output.status.code() == Some(255)
        && output.stdout.is_empty()
        && std::str::from_utf8(&output.stderr).is_ok_and(|stderr| {
            let line = stderr.trim();
            line.starts_with("Control socket connect(")
                && line.ends_with("): No such file or directory")
                && !line.contains(['\n', '\r'])
        })
}

async fn clear_wedged_master_via(host: &str, route: &Route, session_bound_secs: u64) -> bool {
    match bounded_mux_ssh(host, route, &["-O", "check"], &[], 10).await {
        // No master (or ssh cannot even run): nothing to clear.
        Some(false) => return false,
        Some(true) => {}
        None => {
            tracing::warn!(
                "could not tell whether the ControlMaster to {host} is wedged: `-O check` did \
                 not answer within 10s; leaving it alone"
            );
            return false;
        }
    }
    // A master that answers `-O check` but cannot open a session is broken
    // either way — a stall (dead link) or a fast failure ("read from master
    // failed" from one mid-teardown) — and the flight would otherwise dial
    // it under 240 s bounds.
    if bounded_mux_ssh(host, route, &[], &["true"], session_bound_secs).await == Some(true) {
        return false;
    }
    match bounded_mux_ssh(host, route, &["-O", "exit"], &[], 5).await {
        Some(_) => tracing::info!(
            "ControlMaster to {host} was wedged after a link loss; terminated so the reconnect dials fresh"
        ),
        None => tracing::warn!(
            "ControlMaster to {host} was wedged after a link loss and did not answer `-O exit` \
             within 5s; the reconnect may have to wait for ssh's own keepalive timeout"
        ),
    }
    true
}

/// A non-interactive ssh at `host`'s ControlMaster along `route`: `BatchMode`
/// (no prompt, ever) and a tight `ConnectTimeout` placed BEFORE [`ssh_opts`] —
/// OpenSSH takes the first value it sees for most options, so one appended
/// after the shared set would look effective and be silently ignored. Every
/// mux control request and teardown probe starts here.
fn mux_prologue(host: &str, route: &Route) -> Command {
    let mut command = transport_command("ssh");
    command
        .env(ASKPASS_ALIAS_ENV, host)
        .args(existing_master_opts(host, route))
        .args(authentication_opts(host, route, false))
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=5"])
        .args(route_opts(host, route))
        .args(ssh_opts());
    command
}

/// `ssh <prologue> <opts> host <remote>` under a hard wall-clock bound:
/// `Some(exit-success)` when it finished, `None` when it was still running
/// at the deadline (then killed). A spawn failure counts as `Some(false)`.
async fn bounded_mux_ssh(
    host: &str,
    route: &Route,
    opts: &[&str],
    remote: &[&str],
    secs: u64,
) -> Option<bool> {
    let label = format!("ssh {} {host} {}", opts.join(" "), remote.join(" "));
    let what = label.trim();
    let mut command = mux_prologue(host, route);
    command
        .args(opts)
        .arg(host)
        .args(remote)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            tracing::debug!("could not spawn {what}: {err}");
            return Some(false);
        }
    };
    // The outer deadline decides "stalled" and drops the child (killed via
    // kill_on_drop); the inner bound only guards the pipe reads.
    match tokio::time::timeout(
        Duration::from_secs(secs),
        collect_child_bounded(child, secs + 5, what),
    )
    .await
    {
        Ok(Ok(output)) => Some(output.status.success()),
        Ok(Err(_)) => Some(false),
        Err(_) => None,
    }
}

/// Whether an HTTP server answers on `127.0.0.1:port` within 2s. A bare TCP
/// connect is NOT a liveness probe here: after laptop sleep an ssh forward's
/// local listener keeps accepting while the connection behind it is dead, so
/// only a served response proves the daemon end-to-end. Any HTTP status
/// counts — even a 401 had to come from the daemon.
pub async fn http_alive(port: u16) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let attempt = async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .ok()?;
        stream
            .write_all(
                format!(
                    "GET /api/v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .ok()?;
        let mut buf = Vec::with_capacity(16);
        while buf.len() < 5 {
            let mut chunk = [0u8; 16];
            let n = stream.read(&mut chunk).await.ok()?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        buf.starts_with(b"HTTP/").then_some(())
    };
    tokio::time::timeout(Duration::from_secs(2), attempt)
        .await
        .ok()
        .flatten()
        .is_some()
}

/// [`http_alive`] with identity: a bearer-authed request must come back 200.
/// Liveness alone is not enough on a multi-hop tunnel — a relay port on a
/// shared login node can be squatted by a stale relay or a foreign process,
/// and "something answered HTTP" would bless the wrong endpoint (found live:
/// a health probe passed through a previous connect's leaked relay while the
/// new tunnel's own forward was already dying). Only the intended daemon
/// holding this endpoint's token can answer 200.
pub async fn http_alive_authed(port: u16, token: &str) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let attempt = async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .ok()?;
        stream
            .write_all(
                format!(
                    "GET /api/v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
                     Authorization: Bearer {token}\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .ok()?;
        let mut buf = Vec::with_capacity(16);
        while buf.len() < 12 {
            let mut chunk = [0u8; 16];
            let n = stream.read(&mut chunk).await.ok()?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        // "HTTP/1.1 200" — status position is fixed by the HTTP/1.x grammar.
        (buf.starts_with(b"HTTP/") && buf.get(9..12) == Some(b"200")).then_some(())
    };
    tokio::time::timeout(Duration::from_secs(2), attempt)
        .await
        .ok()
        .flatten()
        .is_some()
}

/// The side-effecting host operations the [`connect`] decision phase drives,
/// behind a trait so [`resolve_daemon`]'s policy is unit-testable with a fake
/// (this crate can't be live-verified — no remote host in CI). The production
/// impl ([`SshOps`]) delegates each method VERBATIM to the free function of the
/// same name, so the seam can never drift from real behavior. The three
/// binary/deploy methods take `progress` as `&impl Fn(Phase)` (NOT `&dyn`): a
/// bare `dyn Fn` erases the closure's auto-traits, which would make the whole
/// `connect` future `!Send` and break the Tauri app's `spawn` of it — keeping
/// the concrete closure type lets `Send` flow through exactly as it did before
/// this seam existed.
///
/// Every op runs along `host`'s current [`Route`]; [`locate`] is the only
/// place that changes it.
trait RemoteOps {
    fn route(&self, host: &str) -> Route;
    fn set_route(&self, host: &str, route: Route);
    /// Where THIS machine resolves `node` (see [`routes_to`]).
    async fn local_addrs(&self, node: &str) -> Vec<std::net::IpAddr>;
    /// One probe exec. `detect` adds the scheduler check (the login shell's
    /// PATH walk) — the decision phase's first probe only.
    async fn remote_probe(&self, host: &str, detect: bool) -> anyhow::Result<ProbeRun>;
    /// The scheduler the last detecting probe found on `host`.
    fn scheduler(&self, host: &str) -> Scheduler;
    async fn remote_sessions_count(
        &self,
        host: &str,
        manifest: &Manifest,
    ) -> anyhow::Result<Option<usize>>;
    async fn remote_daemon_extension(
        &self,
        host: &str,
        manifest: &Manifest,
    ) -> anyhow::Result<Option<bool>>;
    async fn resolve_local_binary(
        &self,
        host: &str,
        binary: Option<&Path>,
        progress: &impl Fn(Phase),
    ) -> anyhow::Result<PathBuf>;
    async fn stop_remote(&self, host: &str, pid: u32) -> anyhow::Result<()>;
    async fn deploy_binary(
        &self,
        host: &str,
        path: &Path,
        progress: &impl Fn(Phase),
    ) -> anyhow::Result<()>;
    async fn start_remote(&self, host: &str) -> anyhow::Result<Manifest>;
    async fn ensure_remote_binary(
        &self,
        host: &str,
        binary: Option<&Path>,
        progress: &impl Fn(Phase),
    ) -> anyhow::Result<()>;
}

/// The production [`RemoteOps`]: every method is a one-line delegation to the
/// existing free function, so behavior is preserved by construction. Carries
/// the [`RemoteHome`] so the whole decision phase is scoped to one root — the
/// policy in [`resolve_daemon`] never needs to know which.
struct SshOps {
    home: RemoteHome,
    /// The host isn't a cluster, by the user's word (`ConnectOpts::not_cluster`).
    not_cluster: bool,
}

impl RemoteOps for SshOps {
    fn route(&self, host: &str) -> Route {
        route_of(host)
    }
    fn set_route(&self, host: &str, route: Route) {
        set_route(host, route)
    }
    async fn local_addrs(&self, node: &str) -> Vec<std::net::IpAddr> {
        local_addrs(node).await
    }
    async fn remote_probe(&self, host: &str, detect: bool) -> anyhow::Result<ProbeRun> {
        probe_run(host, self.home, detect).await
    }
    fn scheduler(&self, host: &str) -> Scheduler {
        scheduler_of(host).map(|s| s.kind).unwrap_or_default()
    }
    async fn remote_sessions_count(
        &self,
        host: &str,
        manifest: &Manifest,
    ) -> anyhow::Result<Option<usize>> {
        remote_sessions_count(host, manifest).await
    }
    async fn remote_daemon_extension(
        &self,
        host: &str,
        manifest: &Manifest,
    ) -> anyhow::Result<Option<bool>> {
        remote_daemon_extension(host, manifest).await
    }
    async fn resolve_local_binary(
        &self,
        host: &str,
        binary: Option<&Path>,
        progress: &impl Fn(Phase),
    ) -> anyhow::Result<PathBuf> {
        resolve_local_binary(host, binary, self.home, &progress).await
    }
    async fn stop_remote(&self, host: &str, pid: u32) -> anyhow::Result<()> {
        stop_remote(host, pid).await
    }
    async fn deploy_binary(
        &self,
        host: &str,
        path: &Path,
        progress: &impl Fn(Phase),
    ) -> anyhow::Result<()> {
        deploy_binary(host, path, self.home, &progress).await
    }
    async fn start_remote(&self, host: &str) -> anyhow::Result<Manifest> {
        start_remote(host, self.home, self.not_cluster).await
    }
    async fn ensure_remote_binary(
        &self,
        host: &str,
        binary: Option<&Path>,
        progress: &impl Fn(Phase),
    ) -> anyhow::Result<()> {
        ensure_remote_binary(host, binary, self.home, &progress).await
    }
}

/// Find `host`'s daemon and leave `host`'s route pointing at the node it runs
/// on — or at the node a fresh start belongs on. `Ok(None)` = nothing to
/// attach to (no manifest, or its node provably gone); `Ok(Some((manifest,
/// alive)))` carries a verdict taken ON the manifest's node, so `alive ==
/// false` means provably dead there. A manifest another node wrote whose node
/// can't be reached is an error — never "dead": starting a second daemon over
/// the same shared state would resume every session next to the running one.
async fn locate(
    ops: &impl RemoteOps,
    host: &str,
    progress: &impl Fn(Phase),
    first: Option<ProbeRun>,
) -> anyhow::Result<Option<(Manifest, bool)>> {
    // `first`: a probe the caller already ran along the current route — it
    // stands in for whichever probe below comes first, so the decision phase
    // pays one exec, not two.
    let mut first = first;
    // A route an earlier connect learned goes first: it lands straight on the
    // daemon's node (one dial, one prompt) instead of wherever the pool sends
    // a new master. Anything but a verdict from that node starts over.
    let learned = ops.route(host);
    if learned != Route::Alias {
        let run = match first.take() {
            Some(run) => Ok(run),
            None => ops.remote_probe(host, false).await,
        };
        let why = match run {
            // Stopped since (a graceful stop removes the manifest): a fresh
            // start there keeps the alias on the node it was routed to.
            Ok(ProbeRun::Ran(None)) => return Ok(None),
            Ok(ProbeRun::Ran(Some(p))) if p.here() => return Ok(Some((p.manifest, p.alive))),
            Ok(ProbeRun::Ran(Some(p))) => format!("registered on {} now", p.manifest.hostname),
            Ok(ProbeRun::Failed(f)) => f.to_string(),
            Err(e) => format!("{e:#}"),
        };
        tracing::info!(
            "{host}: no verdict from {} ({why}); probing afresh",
            learned.node().unwrap_or_default()
        );
        ops.set_route(host, Route::Alias);
    }

    let run = match first.take() {
        Some(run) => run,
        None => ops.remote_probe(host, false).await?,
    };
    let landed = match run {
        // An unreachable host reads as nothing running, as it always has:
        // the start path then surfaces ssh's own error.
        ProbeRun::Failed(_) | ProbeRun::Ran(None) => return Ok(None),
        ProbeRun::Ran(Some(p)) if p.here() => return Ok(Some((p.manifest, p.alive))),
        ProbeRun::Ran(Some(p)) => p,
    };
    let node = landed.manifest.hostname.clone();
    if landed.manifest_node_resolves == Some(false) {
        tracing::warn!(
            "{host}: the daemon registered on {node} (pid {}) is gone with its node — the name no \
             longer resolves on {}; starting a fresh daemon there",
            landed.manifest.pid,
            landed.node
        );
        return Ok(None);
    }
    if !valid_node_name(&node) {
        bail!(unreachable_node_error(
            host,
            &landed,
            "its name is not a plain host name"
        ));
    }
    tracing::info!(
        "{host}: this connection landed on {} but the daemon is registered on {node}; routing there",
        landed.node
    );
    progress(Phase::Routing { node: node.clone() });
    let local = ops.local_addrs(&node).await;
    let mut why = String::new();
    for route in routes_to(&node, &landed.manifest_node_addrs, &local) {
        ops.set_route(host, route.clone());
        match ops.remote_probe(host, false).await {
            // The verdict now comes from the node that wrote the manifest.
            Ok(ProbeRun::Ran(Some(p))) if p.here() && same_node(&p.node, &node) => {
                tracing::info!("{host}: reached {node} ({route:?})");
                return Ok(Some((p.manifest, p.alive)));
            }
            // Inside the cluster the name leads back to the node we landed
            // on: a renamed host, whose own verdict is the local one. Only
            // the `-W` route proves that — the user's ssh config can send
            // the direct dial anywhere (a ProxyCommand with a fixed target
            // ignores `HostName`), so a direct dial landing back here says
            // nothing about the name.
            Ok(ProbeRun::Ran(Some(p)))
                if matches!(route, Route::NodeViaAlias(_)) && same_node(&p.node, &landed.node) =>
            {
                ops.set_route(host, Route::Alias);
                return Ok(Some((p.manifest, p.alive)));
            }
            // No manifest over this route: either that node's daemon just
            // stopped (a graceful stop removes it), or the dial reached a
            // machine that doesn't share this home. Only the node we landed
            // on — which just read the manifest — can tell them apart.
            Ok(ProbeRun::Ran(None)) => {
                ops.set_route(host, Route::Alias);
                match ops.remote_probe(host, false).await? {
                    ProbeRun::Ran(None) => return Ok(None),
                    _ => {
                        why = format!(
                            "dialing {node} reached a machine that doesn't see {host}'s manifest"
                        );
                        break;
                    }
                }
            }
            Ok(ProbeRun::Ran(Some(p))) => {
                why = if same_node(&p.node, &node) {
                    format!(
                        "the manifest changed while connecting (now {})",
                        p.manifest.hostname
                    )
                } else {
                    format!("dialing {node} reached a machine calling itself {}", p.node)
                };
            }
            Ok(ProbeRun::Failed(f)) => {
                let network = f.network_level();
                why = f.to_string();
                // An auth failure or a cancelled prompt must not raise a
                // second prompt along another route.
                if !network {
                    break;
                }
            }
            Err(e) => {
                why = format!("{e:#}");
                break;
            }
        }
    }
    ops.set_route(host, Route::Alias);
    bail!(unreachable_node_error(host, &landed, &why))
}

/// The honest failure for a daemon registered on a node this connect could
/// not reach: what is where, why nothing was started, and the way out.
fn unreachable_node_error(host: &str, landed: &Probe, why: &str) -> String {
    let m = &landed.manifest;
    // ssh ends its own complaint with a period.
    let why = why.trim_end_matches('.');
    format!(
        "{host}'s daemon runs on login node {} (pid {}), but this connection landed on {} and \
         could not reach {}: {why}. Nothing was started: a second daemon would resume the same \
         sessions next to the running one. Reconnect once {} is reachable — or, if that node is \
         gone for good, remove the daemon's manifest.json on {host} and reconnect.",
        m.hostname, m.pid, landed.node, m.hostname, m.hostname
    )
}

/// The DECISION phase of [`connect`]: probe the host's daemon and decide
/// whether to reuse, replace, attach-outdated, or fresh-start it — returning
/// `(manifest, outdated, live_sessions)` for the tunnel-attach phase to forward
/// against. Split out behind [`RemoteOps`] so the policy is exercisable without
/// ssh, including the resolve-binary-BEFORE-stop ordering in the Update arm
/// (the past bug: a failed download once stranded a stopped daemon).
async fn resolve_daemon(
    ops: &impl RemoteOps,
    host: &str,
    opts: &ConnectOpts,
    progress: &impl Fn(Phase),
) -> anyhow::Result<(Manifest, bool, Option<usize>)> {
    progress(Phase::Probing);
    let local_build = chimaera_core::BUILD_ID;
    let mut outdated = false;
    let mut live_sessions = None;
    // One remote exec answers "is there a manifest", "is its pid alive",
    // "was it written on this node" and "does the login shell reach a batch
    // scheduler" (`probe_run`): every ssh exec through the ControlMaster
    // costs a channel-open RTT plus a fork on a loaded login node. `locate`
    // adds execs only for a manifest another node wrote, and leaves every op
    // below routed to the daemon's node.
    let first = ops.remote_probe(host, true).await?;
    let scheduler = ops.scheduler(host);
    if scheduler.is_cluster() && !opts.login_serve && !opts.not_cluster {
        // A cluster: nothing of ours may keep running on its login node, so
        // nothing is started, updated, or attached to — and no other login
        // node is dialed to judge an old daemon (that could prompt for a
        // second login). The probe's own view is reported as-is.
        let login_daemon = match &first {
            ProbeRun::Ran(Some(p)) => Some(LoginDaemon {
                node: p.manifest.hostname.clone(),
                pid: p.manifest.pid,
                alive: p.here().then_some(p.alive),
            }),
            _ => None,
        };
        tracing::info!(
            "{host} is a {} cluster; no daemon on its login node",
            scheduler.tag()
        );
        if opts.update_daemon {
            // Repair the executable for future jobs without starting anything
            // on the login node or disrupting jobs already using their inode.
            let binary = ops
                .resolve_local_binary(host, opts.deployment_binary()?, progress)
                .await?;
            ops.deploy_binary(host, &binary, progress).await?;
        }
        return Err(ClusterHost {
            host: host.to_string(),
            scheduler,
            login_daemon,
        }
        .into());
    }
    let manifest = match locate(ops, host, progress, Some(first)).await? {
        Some((m, true)) => {
            // Only pay for the session-count round trip when it can change
            // the decision (build mismatch, or an explicit update request).
            let sessions = if opts.update_daemon
                || !chimaera_core::builds_match(local_build, m.build.as_deref())
            {
                ops.remote_sessions_count(host, &m).await?
            } else {
                None
            };
            match update_decision(
                local_build,
                m.build.as_deref(),
                sessions,
                opts.update_daemon,
            ) {
                Decision::Reuse => {
                    tracing::info!("daemon already running on {host} (pid {})", m.pid);
                    m
                }
                Decision::Update => {
                    tracing::info!(
                        "replacing daemon on {host} (build {}, ours {local_build}, {} live sessions)",
                        m.build.as_deref().unwrap_or("pre-build-id"),
                        sessions.map_or("unknown".to_string(), |n| n.to_string()),
                    );
                    progress(Phase::Updating);
                    if opts.deployment_source == DeploymentSource::PublicRelease
                        && opts.binary.is_none()
                        && ops.remote_daemon_extension(host, &m).await? == Some(true)
                    {
                        bail!("The remote daemon has a selected runtime; automatic public release replacement is unavailable. Supply an explicit compatible --binary artifact");
                    }
                    // Secure the replacement binary BEFORE stopping the
                    // running daemon: a failed download/build must never leave
                    // the host with nothing running (the bug that stranded a
                    // stopped daemon when a dev build 404'd on download).
                    let bin = ops
                        .resolve_local_binary(host, opts.deployment_binary()?, progress)
                        .await?;
                    ops.stop_remote(host, m.pid).await?;
                    ops.deploy_binary(host, &bin, progress).await?;
                    progress(Phase::Starting);
                    let started = ops.start_remote(host).await?;
                    // A stop whose exit went unconfirmed (a link blip during
                    // the wait) leaves the old daemon serving: the new one
                    // refuses to start beside it and the wait hands back the
                    // OLD manifest. That is a working daemon, so connect to
                    // it — but as outdated, never as a completed update.
                    if started.pid == m.pid
                        || !chimaera_core::builds_match(local_build, started.build.as_deref())
                    {
                        tracing::warn!(
                            "daemon on {host} was not replaced — the previous build (pid {}) is still serving; connecting to it as outdated",
                            started.pid
                        );
                        outdated = true;
                        live_sessions = sessions;
                    }
                    started
                }
                Decision::ConnectOutdated => {
                    tracing::info!(
                        "daemon on {host} is an older build ({} vs ours {local_build}) but {} — connecting to it as-is",
                        m.build.as_deref().unwrap_or("pre-build-id"),
                        sessions.map_or("its session count is unknown".to_string(), |n| {
                            format!("has {n} live session{}", if n == 1 { "" } else { "s" })
                        }),
                    );
                    outdated = true;
                    live_sessions = sessions;
                    m
                }
            }
        }
        _ => {
            if opts.update_daemon || opts.deployment_source == DeploymentSource::ExplicitBinary {
                // A selected assembly must deploy its explicit artifact rather
                // than start an unclassified executable already on disk.
                // A broken/missing daemon is precisely when repair is useful;
                // do not trust an existing on-disk binary just because it exists.
                let binary = ops
                    .resolve_local_binary(host, opts.deployment_binary()?, progress)
                    .await?;
                ops.deploy_binary(host, &binary, progress).await?;
            } else {
                ops.ensure_remote_binary(host, opts.binary.as_deref(), progress)
                    .await?;
            }
            progress(Phase::Starting);
            ops.start_remote(host).await?
        }
    };
    Ok((manifest, outdated, live_sessions))
}

/// Connect to the daemon on `host`, installing and starting it if needed,
/// and bring up a local port-forward. `progress` fires as phases begin.
pub async fn connect(
    host: &str,
    opts: ConnectOpts,
    progress: impl Fn(Phase),
) -> anyhow::Result<Tunnel> {
    // Normalize whatever the caller has (saved entries predate validation;
    // "ssh cluster" typed verbatim reached ssh as one hostname in the field)
    // so every ssh invocation below sees a real destination.
    let host = &hosts::normalize_alias(host)?;
    // Dev-ness is the build's property (see `RemoteHome::current`): a dev
    // build ALWAYS targets `~/.chimaera-dev`, a release always `~/.chimaera`.
    // There is no per-connect override, so a release client can never touch
    // a dev home and a dev client can never stop or replace the real daemon.
    let ops = SshOps {
        home: RemoteHome::current(),
        not_cluster: opts.not_cluster,
    };
    let (manifest, outdated, live_sessions) = resolve_daemon(&ops, host, &opts, &progress).await?;
    // Where `locate` left the alias: the forward must end on the daemon's
    // node, the only one whose loopback it listens on.
    let route = route_of(host);

    let mut local_port = pick_local_port(opts.local_port, manifest.port)?;
    progress(Phase::Tunneling { local_port });
    let tunnel = open_proven_tunnel(host, local_port, manifest.port, &manifest.token).await;
    let (child, mux_delegated) = match tunnel {
        Ok(tunnel) => tunnel,
        // The availability probe binds on OUR side, but under the WSL
        // transport ssh's -L binds inside the distro — a different port
        // namespace — so the first pick can collide with an in-distro
        // listener the probe cannot see. A stale mux forward can also accept
        // locally without reaching this daemon. One retry on a fresh
        // OS-assigned port covers both; an EXPLICITLY requested port stays a
        // hard error.
        Err(e) if opts.local_port.is_none() => {
            tracing::warn!("tunnel on 127.0.0.1:{local_port} failed ({e:#}); retrying fresh");
            local_port = ephemeral_port()?;
            progress(Phase::Tunneling { local_port });
            open_proven_tunnel(host, local_port, manifest.port, &manifest.token).await?
        }
        Err(e) => return Err(e),
    };
    match route.node() {
        Some(node) => tracing::info!(
            "tunnel up: 127.0.0.1:{local_port} -> {host} (login node {node}):{}",
            manifest.port
        ),
        None => tracing::info!(
            "tunnel up: 127.0.0.1:{local_port} -> {host}:{}",
            manifest.port
        ),
    }

    Ok(Tunnel {
        host: host.to_string(),
        local_port,
        remote_build: manifest.build.clone(),
        manifest,
        mux_delegated,
        outdated,
        live_sessions,
        route,
        child,
    })
}

/// Find `host`'s daemon under `home` and route every later ssh call for
/// `host` to the node it runs on — what a caller that talks to the daemon
/// over ssh (curl against its loopback port) must do first. `Ok(None)` =
/// nothing registered; otherwise the manifest and a liveness verdict taken on
/// its own node. May ask to authenticate to that node. The route is keyed by
/// `host` exactly as given, so later calls must pass the same string.
pub async fn locate_daemon(
    host: &str,
    home: RemoteHome,
) -> anyhow::Result<Option<(Manifest, bool)>> {
    locate(
        &SshOps {
            home,
            not_cluster: false,
        },
        host,
        &|_| {},
        None,
    )
    .await
}

/// Frame the manifest in [`remote_probe`] / [`start_remote`] output on BOTH
/// sides: an echoing `~/.bashrc` under non-interactive ssh (common on HPC)
/// puts noise before AND after a script's output, so the client parses
/// strictly between these. Never substrings of serde JSON.
const MANIFEST_BEGIN: &str = "---chimaera-manifest-begin---";
const MANIFEST_END: &str = "---chimaera-manifest-end---";

/// POSIX-sh fragment printing the pid recorded in the manifest at `$f` (a
/// serde-pretty JSON file; `sed` is on every remote, `jq` is not). It takes
/// the FIRST `"pid"` line of pretty output and the LAST on a compact line,
/// so `chimaera_core::Manifest` must stay FLAT — a nested pid would win.
const SH_MANIFEST_PID: &str =
    r#"sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$f" | head -n 1"#;

/// POSIX-sh predicate: read the manifest's pid into `$p` and `kill -0` it.
/// Shared by the probe and the start-wait so the two can never disagree on
/// what "alive" means (a fn, not a const: it interpolates the sed above).
fn sh_manifest_alive() -> String {
    format!("p=$({SH_MANIFEST_PID}); [ -n \"$p\" ] && kill -0 \"$p\" 2>/dev/null")
}

/// Exit code of a remote wait loop whose host has no usable `sleep`
/// (neither `sleep 0.5` nor `sleep 1`). Without it the loop would spin
/// through its tick budget in milliseconds — measured at 24 ms — and
/// fabricate a "did not start" / "still running" verdict.
const NO_SLEEP_EXIT: i32 = 5;

/// Probe ONCE whether `sleep` accepts fractions (GNU and BSD do; a minimal
/// busybox may not): `$frac` = 0 means half-second ticks, otherwise whole
/// seconds counted double so a tick-bounded loop keeps its deadline. The
/// probe's own half second is the loop's first wait, hence `i=1`.
const SH_SLEEP_PROBE: &str = "sleep 0.5 2>/dev/null; frac=$?; i=0; [ \"$frac\" -eq 0 ] && i=1;";

/// One tick of a remote wait loop after [`SH_SLEEP_PROBE`]: half a second,
/// or a whole second counted twice; a `sleep` that fails outright exits
/// [`NO_SLEEP_EXIT`] instead of spinning.
fn sh_tick() -> String {
    format!(
        "if [ \"$frac\" -eq 0 ]; then sleep 0.5 || exit {NO_SLEEP_EXIT}; i=$((i+1)); \
         else sleep 1 2>/dev/null || exit {NO_SLEEP_EXIT}; i=$((i+2)); fi;"
    )
}

/// Wrap a POSIX-sh script for `ssh host <cmd>`: sshd hands the command to
/// the user's LOGIN shell, which on HPC accounts may be tcsh or fish, so the
/// script rides inside `sh -c '…'`. Two escapes make the single-quoted body
/// read identically under sh, bash, zsh, dash, fish, and csh: an inner `'`
/// becomes `'\''`, and — csh expands `!` even inside single quotes — an
/// inner `!` becomes `'\!'` (the backslash sits outside the quotes, where
/// every shell honors it and the POSIX ones drop it). Quotes first: the
/// bang escape introduces quotes of its own.
fn sh_wrap(script: &str) -> String {
    let body = script.replace('\'', r"'\''").replace('!', r"'\!'");
    format!("sh -c '{body}'")
}

/// POSIX-sh fragment printing the node name recorded in the manifest at `$f`
/// — the same flat-JSON `sed` discipline as [`SH_MANIFEST_PID`].
const SH_MANIFEST_HOSTNAME: &str =
    r#"sed -n 's/.*"hostname"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$f" | head -n 1"#;

/// POSIX-sh fragment setting `$d` to whether node name `$h` exists in the
/// name service of the node the script runs on (`$n`): `found <addr>…` (its
/// addresses, for the direct-route check), `gone` (the resolver answered "no
/// such name", `EAI_NONAME`), or `unknown` (no perl, no clear answer, or a
/// resolver that can't even resolve `$n` itself — one that knows no node
/// names must never declare a node gone). Not `getent`: it exits the same for
/// "no such name" and "the DNS server is down", and an outage must never read
/// as "that node is gone". The verdict rides stdout, not an exit code — perl
/// dying at compile time exits with whatever `errno` held.
const SH_NODE_RESOLVES: &str = r#"d=unknown; if command -v perl >/dev/null 2>&1; then r=$(perl -MSocket=:addrinfo,SOCK_STREAM -e 'use strict; my ($h, $n) = @ARGV; my ($own) = getaddrinfo($n, "22"); if ($own) { print STDOUT "unknown"; exit 0 } my ($e, @r) = getaddrinfo($h, "22", {socktype => SOCK_STREAM()}); if ($e) { print STDOUT ($e == EAI_NONAME() ? "gone" : "unknown"); exit 0 } my %s; print STDOUT join(" ", "found", grep { defined $_ and not $s{$_}++ } map { (getnameinfo($_->{addr}, NI_NUMERICHOST()))[1] } @r)' "$h" "$n" 2>/dev/null); case "$r" in found*|gone) d=$r;; esac; fi;"#;

/// What one probe exec found, seen from the node it ran on.
#[derive(Clone, Debug)]
pub struct Probe {
    pub manifest: Manifest,
    /// The node the probe ran on (`uname -n`); empty if the host didn't say.
    pub node: String,
    /// `kill -0` of the manifest's pid on `node` — the daemon's liveness only
    /// when [`Probe::here`]; anywhere else it tested an unrelated process
    /// table.
    pub alive: bool,
    /// For a manifest another node wrote: whether that node's name still
    /// resolves on `node`. `Some(false)` is the cluster's name service saying
    /// the node no longer exists; `None` = not asked, or no clear answer.
    pub manifest_node_resolves: Option<bool>,
    /// The addresses `node` resolves that name to (when it does).
    pub manifest_node_addrs: Vec<std::net::IpAddr>,
}

impl Probe {
    /// Whether the manifest was written on the node the probe ran on — the
    /// only node where its pid and loopback port mean anything. A host that
    /// doesn't report its node name is taken at its word, as before nodes
    /// were compared.
    pub fn here(&self) -> bool {
        self.node.is_empty() || same_node(&self.node, &self.manifest.hostname)
    }
}

/// One probe exec over a host's current [`Route`].
#[derive(Debug)]
enum ProbeRun {
    /// The script ran; `None` = no readable manifest.
    Ran(Option<Probe>),
    /// ssh (or the remote shell) failed before the script could answer.
    Failed(ProbeFailure),
}

#[derive(Debug)]
struct ProbeFailure {
    /// The exit status, as ssh's caller would print it.
    status: String,
    stderr: String,
}

impl ProbeFailure {
    /// Whether the dial never reached an sshd that could have authenticated
    /// us — name resolution, TCP connect, or the banner exchange failed — the
    /// one case where another route to the same node is worth a try. An auth
    /// failure or a cancelled prompt is not: retrying would prompt again.
    /// OpenSSH's own client messages, stable across releases.
    fn network_level(&self) -> bool {
        [
            "Could not resolve hostname",
            "connect to host",
            "kex_exchange_identification",
            "banner exchange",
        ]
        .iter()
        .any(|marker| self.stderr.contains(marker))
    }
}

impl std::fmt::Display for ProbeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // ssh's own complaint is its last line; a chatty login banner
        // printed to stderr sits before it.
        match self.stderr.lines().rev().find(|l| !l.trim().is_empty()) {
            Some(line) => write!(f, "{}", line.trim()),
            None => write!(f, "ssh exited {}", self.status),
        }
    }
}

/// Fetch the manifest under `home`, whether its recorded pid answers
/// `kill -0`, and which node answered, in ONE remote exec — the decision
/// phase's whole probe. A manifest + a `kill -0` exec cost two serial round
/// trips, and every exec through the ControlMaster is a channel-open RTT plus
/// a remote fork (~300-500 ms on a loaded login node at WAN latency). `None`
/// = no readable manifest (or ssh itself failed — "nothing running", as the
/// connect flow has always read an unreachable host).
/// Which batch scheduler `host`'s login shell reaches — one probe exec (which
/// also raises the ControlMaster, so it may ask the user to authenticate).
/// Every later cluster command finds the scheduler on the PATH this learned.
pub async fn detect_scheduler(host: &str, home: RemoteHome) -> anyhow::Result<SchedulerInfo> {
    let host = &hosts::normalize_alias(host)?;
    if let ProbeRun::Failed(f) = probe_run(host, home, true).await? {
        bail!("could not reach {host}: {f}");
    }
    Ok(scheduler_of(host).unwrap_or_default())
}

pub async fn remote_probe(host: &str, home: RemoteHome) -> anyhow::Result<Option<Probe>> {
    match probe_run(host, home, true).await? {
        ProbeRun::Ran(probe) => Ok(probe),
        ProbeRun::Failed(_) => Ok(None),
    }
}

async fn probe_run(host: &str, home: RemoteHome, detect: bool) -> anyhow::Result<ProbeRun> {
    let script = if detect {
        format!("{} {}", sh_scheduler(), probe_script(&home.manifest_path()))
    } else {
        probe_script(&home.manifest_path())
    };
    let cmd = sh_wrap(&script);
    // SSH_ONESHOT_SECS, not shorter: the first call to a host raises the
    // ControlMaster and may sit in an askpass password/Duo prompt.
    let output = output_bounded(ssh_cmd(host).arg(cmd), SSH_ONESHOT_SECS, "ssh").await?;
    if !output.status.success() {
        return Ok(ProbeRun::Failed(ProbeFailure {
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if detect {
        set_scheduler(host, parse_scheduler_line(&stdout));
    }
    let probe =
        parse_probe_output(&stdout).with_context(|| format!("probing the daemon on {host}"))?;
    Ok(ProbeRun::Ran(probe))
}

/// Marks the scheduler verdict line in a probe's stdout (an echoing rc file
/// can't fake it, and it is never a substring of the manifest JSON).
const SCHED_MARK: &str = "---chimaera-sched---";

/// What the login shell can reach on a host: the scheduler, and the
/// directory its submit command lives in (prefixed to `PATH` by every later
/// cluster command, which run in a plain `sh -c`, not a login shell).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SchedulerInfo {
    pub kind: Scheduler,
    pub bindir: String,
}

static SCHEDULERS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, SchedulerInfo>>,
> = std::sync::LazyLock::new(Default::default);

/// The scheduler the last detecting probe of `host` found in this process.
pub fn scheduler_of(host: &str) -> Option<SchedulerInfo> {
    SCHEDULERS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(host)
        .cloned()
}

fn set_scheduler(host: &str, info: Option<SchedulerInfo>) {
    let mut map = SCHEDULERS.lock().unwrap_or_else(|p| p.into_inner());
    match info {
        Some(info) => map.insert(host.to_string(), info),
        None => map.remove(host),
    };
}

/// POSIX-sh fragment printing which batch scheduler the user's LOGIN shell
/// can reach: `SCHED_MARK <tag> <dir of the submit command>`. A PATH walk,
/// never `command -v`: clusters wrap these tools in profile shell functions
/// (`command -v squeue` then names the function, not a file), and the walk
/// reads the same under every shell. The PATH comes from the user's login
/// shell when it answers `-lc` (tcsh refuses it; a profile that `exec`s
/// another shell swallows it), else from `sh -l` (the system profile, where
/// clusters put their scheduler), else this shell's.
fn sh_scheduler() -> String {
    format!(
        r#"P=$("${{SHELL:-/bin/sh}}" -lc 'printf "\n%s%s\n" __chimaera_path__ "$PATH"' </dev/null 2>/dev/null | sed -n 's/^__chimaera_path__//p' | tail -n 1); [ -n "$P" ] || P=$(sh -lc 'printf "\n%s%s\n" __chimaera_path__ "$PATH"' </dev/null 2>/dev/null | sed -n 's/^__chimaera_path__//p' | tail -n 1); [ -n "$P" ] || P=$PATH; has() {{ _o=$IFS; IFS=:; set -f; for _d in $P; do if [ -n "$_d" ] && [ -f "$_d/$1" ] && [ -x "$_d/$1" ]; then IFS=$_o; set +f; printf %s "$_d"; return 0; fi; done; IFS=$_o; set +f; return 1; }}; s=none; b=; if b=$(has sbatch) && has squeue >/dev/null && has scancel >/dev/null && has sinfo >/dev/null; then s=slurm; elif b=$(has qsub) && has qstat >/dev/null; then s=pbs; elif b=$(has bsub) && has bjobs >/dev/null; then s=lsf; else b=; fi; printf '\n%s %s %s\n' '{SCHED_MARK}' "$s" "$b";"#
    )
}

/// The scheduler verdict in a detecting probe's stdout; `None` when the
/// line is missing (the probe never got that far).
fn parse_scheduler_line(stdout: &str) -> Option<SchedulerInfo> {
    let rest = stdout
        .lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix(SCHED_MARK))?;
    let mut words = rest.split_whitespace();
    let kind = Scheduler::from_tag(words.next().unwrap_or("none"));
    let bindir = words.next().unwrap_or("").to_string();
    // The dir rides into later command lines as a PATH prefix: only a plain
    // absolute path is kept.
    let bindir = if bindir.starts_with('/')
        && bindir
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c))
    {
        bindir
    } else {
        String::new()
    };
    Some(SchedulerInfo { kind, bindir })
}

/// The POSIX-sh script [`remote_probe`] runs on the host: the manifest at
/// `manifest_path` (if readable) framed between [`MANIFEST_BEGIN`] and
/// [`MANIFEST_END`], then a trailer: the pid it tested, the node it ran on,
/// the node the manifest names and — only when those two differ — whether
/// that name still resolves here, and an `alive`/`dead` verdict. `$HOME` in
/// the path is expanded by the remote shell (as an assignment RHS it is never
/// word-split). A missing or unreadable manifest prints nothing and exits 0.
/// Pure so the tests can run it under real shells.
fn probe_script(manifest_path: &str) -> String {
    format!(
        "f={manifest_path}; [ -r \"$f\" ] || exit 0; if {alive}; then a=alive; else a=dead; fi; \
         n=$(uname -n 2>/dev/null); h=$({hostname}); d=same; \
         if [ \"$h\" != \"$n\" ]; then {resolves} fi; \
         printf '\\n{begin}\\n'; cat \"$f\"; \
         printf '\\n{end}\\npid=%s\\nnode=%s\\nhost=%s\\ndns=%s\\n%s\\n' \"$p\" \"$n\" \"$h\" \"$d\" \"$a\"",
        alive = sh_manifest_alive(),
        hostname = SH_MANIFEST_HOSTNAME,
        resolves = SH_NODE_RESOLVES,
        begin = MANIFEST_BEGIN,
        end = MANIFEST_END,
    )
}

/// The manifest JSON framed between the markers in a remote script's
/// stdout, plus the trailer the script printed after the end marker.
/// `Ok(None)` = no begin marker (the script printed nothing: no manifest);
/// a begin without an end is a cut-off transcript → `Err`.
fn framed_manifest(stdout: &str) -> anyhow::Result<Option<(&str, &str)>> {
    let Some((_, rest)) = stdout.split_once(MANIFEST_BEGIN) else {
        return Ok(None);
    };
    let (json, trailer) = rest
        .split_once(MANIFEST_END)
        .context("remote output was cut off before the manifest end marker")?;
    Ok(Some((json.trim(), trailer)))
}

/// The `pid=<n>` trailer line: the pid the remote `sed` extracted and
/// tested. Cross-checked against the parsed manifest so a silent sed/serde
/// disagreement is an error, never "dead" — which would start a second
/// daemon next to a live one.
fn trailer_pid(trailer: &str) -> Option<u32> {
    trailer
        .lines()
        .find_map(|l| l.trim().strip_prefix("pid="))
        .and_then(|p| p.trim().parse().ok())
}

/// A `key=value` trailer line's value.
fn trailer_field<'a>(trailer: &'a str, key: &str) -> Option<&'a str> {
    trailer
        .lines()
        .find_map(|l| l.trim().strip_prefix(key)?.strip_prefix('='))
        .map(str::trim)
}

fn trailer_verdict(trailer: &str) -> Option<bool> {
    trailer.lines().find_map(|l| match l.trim() {
        "alive" => Some(true),
        "dead" => Some(false),
        _ => None,
    })
}

fn check_trailer_pid(manifest: &Manifest, trailer: &str, what: &str) -> anyhow::Result<()> {
    match trailer_pid(trailer) {
        Some(p) if p == manifest.pid => Ok(()),
        p => bail!(
            "{what}: the remote tested pid {} but the manifest says {}",
            p.map_or("<none>".to_string(), |p| p.to_string()),
            manifest.pid
        ),
    }
}

/// [`remote_probe`]'s stdout → a [`Probe`]. No framed manifest = `None`; an
/// unparsable one counts as none (a corrupt file: a fresh start) — but never
/// when the remote found a LIVE pid in it, or when the remote read it as
/// another node's record (whose daemon can't be judged from here); a pid
/// mismatch or a missing verdict is an `Err`. The trailer's node fields are
/// optional so a host that can't name itself degrades to the pre-node probe.
fn parse_probe_output(stdout: &str) -> anyhow::Result<Option<Probe>> {
    let Some((json, trailer)) = framed_manifest(stdout)? else {
        return Ok(None);
    };
    let node = trailer_field(trailer, "node")
        .unwrap_or_default()
        .to_string();
    let manifest = match serde_json::from_str::<Manifest>(json) {
        Ok(manifest) => manifest,
        Err(err) => {
            if trailer_pid(trailer).is_some() && trailer_verdict(trailer) == Some(true) {
                bail!(
                    "the manifest is unparsable ({err}) yet its pid is alive; refusing to start a second daemon"
                );
            }
            let written_on = trailer_field(trailer, "host").unwrap_or_default();
            if !written_on.is_empty() && !node.is_empty() && !same_node(written_on, &node) {
                bail!(
                    "the manifest is unparsable ({err}) and names node {written_on}, not {node}; refusing to start a second daemon"
                );
            }
            return Ok(None);
        }
    };
    check_trailer_pid(&manifest, trailer, "probe")?;
    let alive = trailer_verdict(trailer).context("probe output carried no alive/dead verdict")?;
    let mut dns = trailer_field(trailer, "dns")
        .unwrap_or_default()
        .split_whitespace();
    let manifest_node_resolves = match dns.next() {
        Some("found") => Some(true),
        Some("gone") => Some(false),
        _ => None,
    };
    let manifest_node_addrs = match manifest_node_resolves {
        Some(true) => dns.filter_map(|addr| addr.parse().ok()).collect(),
        _ => Vec::new(),
    };
    // The resolver was asked about the name the remote `sed` read: the same
    // cross-check as the pid, so a sed/serde disagreement can never turn into
    // "gone" for the wrong name — which would start a second daemon.
    if manifest_node_resolves.is_some() {
        let resolved = trailer_field(trailer, "host").unwrap_or_default();
        anyhow::ensure!(
            resolved == manifest.hostname,
            "probe: the remote resolved node {resolved:?} but the manifest names {:?}",
            manifest.hostname
        );
    }
    Ok(Some(Probe {
        manifest,
        node,
        alive,
        manifest_node_resolves,
        manifest_node_addrs,
    }))
}

/// [`start_remote`]'s wait output → the manifest of the daemon that came up.
fn parse_start_wait_output(stdout: &str) -> anyhow::Result<Manifest> {
    let (json, trailer) =
        framed_manifest(stdout)?.context("the wait exited 0 without printing a manifest")?;
    let manifest: Manifest = serde_json::from_str(json).context("unparsable manifest")?;
    check_trailer_pid(&manifest, trailer, "start wait")?;
    Ok(manifest)
}

/// Count live sessions on the daemon `manifest` describes by asking the
/// daemon itself: `curl` over ssh against its loopback port, authenticated
/// with the manifest's own token. `Ok(None)` = could not determine (no
/// curl, daemon unreachable, bad payload) — callers must treat unknown as
/// busy, never as zero. `Err` only when ssh itself cannot run.
pub async fn remote_sessions_count(
    host: &str,
    manifest: &Manifest,
) -> anyhow::Result<Option<usize>> {
    // The token rides stdin as a curl config line (`--config -`), never in
    // argv: `ps` on a shared login node must not show other users the
    // daemon token. (`-H @-` would be neater but needs curl >= 7.55;
    // `--config -` works on cluster-vintage curls too.)
    let cmd = format!(
        "curl -fsS -m 5 --config - http://127.0.0.1:{}/api/v1/sessions",
        manifest.port
    );
    let mut command = ssh_cmd(host);
    command
        .arg(cmd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().context("failed to run ssh")?;
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let line = format!("header = \"Authorization: Bearer {}\"\n", manifest.token);
        stdin.write_all(line.as_bytes()).await.ok();
        // Dropping stdin sends EOF, which ends the config for curl.
    }
    let output = collect_child_bounded(child, SSH_ONESHOT_SECS, "ssh session count").await?;
    if !output.status.success() {
        tracing::debug!(
            "session count on {host} unavailable: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return Ok(None);
    }
    Ok(count_alive_sessions(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

/// Observe composition only immediately before an implicit public replacement.
/// Reuse the original route/address/token; no tunnel or route fallback is added.
async fn remote_daemon_extension(host: &str, manifest: &Manifest) -> anyhow::Result<Option<bool>> {
    anyhow::ensure!(
        !manifest.token.is_empty()
            && manifest.token.len() <= 512
            && manifest
                .token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte)),
        "Remote daemon composition credential shape refused"
    );
    let cmd = format!(
        "curl -fsS -m 5 --config - http://127.0.0.1:{}/api/v1/health",
        manifest.port
    );
    let mut command = ssh_cmd(host);
    command
        .arg(cmd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .context("failed to run ssh composition probe")?;
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let line = format!("header = \"Authorization: Bearer {}\"\n", manifest.token);
        if stdin.write_all(line.as_bytes()).await.is_err() {
            let _ = child.start_kill();
            let _ = child.wait().await;
            bail!("Remote daemon composition could not be confirmed");
        }
    }
    let output = collect_child_bounded_with_caps(
        child,
        SSH_ONESHOT_SECS,
        "ssh composition probe",
        16 * 1024,
        16 * 1024,
    )
    .await?;
    anyhow::ensure!(
        output.status.success(),
        "Remote daemon composition could not be confirmed"
    );
    parse_daemon_extension(&output.stdout, manifest)
}

fn parse_daemon_extension(bytes: &[u8], manifest: &Manifest) -> anyhow::Result<Option<bool>> {
    anyhow::ensure!(
        bytes.len() <= 16 * 1024,
        "Remote daemon composition exceeded bound"
    );
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("Remote daemon composition could not be confirmed"))?;
    anyhow::ensure!(
        value["name"] == "chimaera"
            && value["pid"].as_u64() == Some(u64::from(manifest.pid))
            && value["hostname"].as_str() == Some(manifest.hostname.as_str())
            && value["version"].as_str() == Some(manifest.version.as_str())
            && manifest
                .build
                .as_deref()
                .is_none_or(|build| value["build"].as_str() == Some(build)),
        "Remote daemon composition identity did not match the original manifest"
    );
    match value.get("daemon_extension") {
        None => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("Remote daemon composition could not be confirmed")),
    }
}

/// Parse a `GET /api/v1/sessions` payload and count `alive: true` entries
/// (the list also carries finished sessions for recents/last-words).
/// `None` for anything that is not the expected JSON array.
pub fn count_alive_sessions(payload: &str) -> Option<usize> {
    let value: serde_json::Value = serde_json::from_str(payload.trim()).ok()?;
    Some(
        value
            .as_array()?
            .iter()
            .filter(|s| {
                s.get("alive")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
            })
            .count(),
    )
}

/// Gracefully stop the daemon on `host`: SIGTERM, then ONE remote exec waits
/// up to ~10 s for it to go. Never escalates to SIGKILL — a daemon that will
/// not die may be holding sessions that must not be torn out from under
/// their owner, so that errors honestly. Three outcomes: gone (`Ok`), still
/// running after the deadline (`Err`), or the wait's ssh ended early (a
/// dropped link) — then one bounded `kill -0` re-check decides, and if even
/// that cannot run, `Ok` with a warning: this sits between "stop" and
/// "deploy" in the update path, where an `Err` would strand the host with a
/// stopped daemon and the old binary (deploy stages `.new` + `mv -f`; a
/// daemon that was in fact still running makes the restart's wait return
/// its manifest).
pub async fn stop_remote(host: &str, pid: u32) -> anyhow::Result<()> {
    tracing::info!("stopping daemon on {host} (pid {pid})");
    // The exit code is the wire: 0 = gone, STOP_STILL_ALIVE_EXIT,
    // STOP_SIGNAL_FAILED_EXIT (stderr says why), NO_SLEEP_EXIT; anything
    // else is ssh's own (255 on a dropped link) = unconfirmed.
    let cmd = sh_wrap(&stop_script(pid, STOP_WAIT_TICKS));
    let output = output_bounded(ssh_cmd(host).arg(cmd), SSH_ONESHOT_SECS, "ssh").await?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    match output.status.code() {
        Some(STOP_STILL_ALIVE_EXIT) => bail!(still_running_error(host, pid)),
        Some(STOP_SIGNAL_FAILED_EXIT) => {
            bail!("ssh {host} \"kill -TERM {pid}\" failed: {}", stderr.trim())
        }
        Some(NO_SLEEP_EXIT) => bail!(
            "{host} has no usable `sleep`; cannot wait for the daemon to stop: {}",
            stderr.trim()
        ),
        _ => {}
    }
    // SIGTERM was dispatched, then ssh itself ended (255: the link dropped
    // mid-wait). One non-interactive re-check: `kill -0` exits 1 once the
    // pid is gone.
    let mut check = mux_prologue(host, &route_of(host));
    check
        .arg(host)
        .arg(sh_wrap(&format!("kill -0 {pid} 2>/dev/null")));
    match output_bounded(&mut check, 30, "ssh kill -0").await {
        Ok(out) if out.status.success() => bail!(still_running_error(host, pid)),
        Ok(out) if out.status.code() == Some(1) => Ok(()),
        other => {
            let recheck = match other {
                Ok(out) => format!(
                    "{}: {}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
                Err(err) => format!("{err:#}"),
            };
            tracing::warn!(
                "stop sent to the daemon on {host} (pid {pid}) but its exit could not be \
                 confirmed (wait ended {}: {}; re-check: {recheck}) — proceeding as stopped",
                output.status,
                stderr.trim()
            );
            Ok(())
        }
    }
}

fn still_running_error(host: &str, pid: u32) -> String {
    format!(
        "daemon on {host} (pid {pid}) is still running 10s after SIGTERM — \
         refusing to kill -9 it; something is keeping it busy (open UI tabs \
         hold its sockets). Close them and retry, or stop it by hand."
    )
}

/// Remote exit codes of [`stop_remote`]'s one-shot script: small, and
/// distinct from what the shell (1, 2, 126, 127) or ssh itself (255) emits.
const STOP_SIGNAL_FAILED_EXIT: i32 = 3;
const STOP_STILL_ALIVE_EXIT: i32 = 4;
/// How many half-second ticks [`stop_remote`] waits after SIGTERM (10 s).
const STOP_WAIT_TICKS: u32 = 20;

/// The POSIX-sh script [`stop_remote`] runs: SIGTERM `pid`, then up to
/// `ticks` half-second waits for it to go. Pure so tests can run it under
/// real shells with a short deadline.
fn stop_script(pid: u32, ticks: u32) -> String {
    format!(
        "kill -TERM {pid} || exit {failed}; {probe} while :; do \
         kill -0 {pid} 2>/dev/null || exit 0; [ $i -lt {ticks} ] || exit {alive}; {tick} done",
        failed = STOP_SIGNAL_FAILED_EXIT,
        probe = SH_SLEEP_PROBE,
        alive = STOP_STILL_ALIVE_EXIT,
        tick = sh_tick(),
    )
}

/// `uname -sm` on the host, lowercased: e.g. `("linux", "x86_64")`.
pub async fn remote_target(host: &str) -> anyhow::Result<(String, String)> {
    let output = output_bounded(ssh_cmd(host).arg("uname -sm"), SSH_ONESHOT_SECS, "ssh").await?;
    if !output.status.success() {
        bail!(
            "could not detect the OS/arch of {host}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_lowercase();
    let mut parts = text.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some(os), Some(arch)) => Ok((os.to_string(), arch.to_string())),
        _ => bail!("unexpected `uname -sm` output from {host}: {text:?}"),
    }
}

/// The local stash of deployable builds: `~/.chimaera/dist/`. Populated by
/// `just dist` (or by hand); searched when no explicit binary is given.
pub fn dist_dir() -> PathBuf {
    chimaera_core::data_dir().join("dist")
}

/// The expected dist file name for a remote target: static musl for linux
/// (no glibc roulette on clusters), plain names elsewhere.
pub fn dist_name(os: &str, arch: &str) -> String {
    match os {
        "linux" => format!("chimaera-{arch}-linux-musl"),
        other => format!("chimaera-{arch}-{other}"),
    }
}

/// The Rust target triple for a detected `uname -sm` pair, matching the
/// daemon asset names the release workflow publishes (`chimaera-<triple>`).
/// `None` for a target we don't build (no silent guessing — the caller
/// falls back to the explicit-binary / `just dist` message).
pub fn release_triple(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("linux", "aarch64" | "arm64") => Some("aarch64-unknown-linux-musl"),
        ("darwin", "arm64" | "aarch64") => Some("aarch64-apple-darwin"),
        _ => None,
    }
}

/// A daemon asset resolved from the GitHub releases API: the release version
/// it belongs to, its download URL, and its published sha256 (hex).
struct ReleaseAsset {
    version: String,
    url: String,
    sha256: String,
}

/// Resolve the daemon asset to download for `triple`. Prefers the release this
/// build came from (`v{VERSION}` — so a real release's daemon shares our build
/// id and connect never loops "updating"); falls back to GitHub's `latest`
/// release when there is no matching one, so a dev build (version `0.0.1`) — or
/// any version without a published release — still gets a working daemon
/// instead of a hard 404.
async fn resolve_release_asset(triple: &str) -> anyhow::Result<ReleaseAsset> {
    let asset_name = format!("chimaera-{triple}");
    let version = chimaera_core::VERSION;
    if let Some(a) = release_asset(&format!("tags/v{version}"), &asset_name).await? {
        return Ok(a);
    }
    tracing::info!("no v{version} release with {asset_name}; falling back to the latest release");
    release_asset("latest", &asset_name)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no published release provides {asset_name}"))
}

/// Look up `asset_name` in the release identified by `release_ref`
/// (`tags/vX.Y.Z` or `latest`) via the GitHub API. `Ok(None)` when that release
/// doesn't exist or lacks the asset — so the caller can fall back — and `Err`
/// only on a transport/parse failure.
async fn release_asset(
    release_ref: &str,
    asset_name: &str,
) -> anyhow::Result<Option<ReleaseAsset>> {
    let repo = repo_slug().context("could not derive the repo from the repository URL")?;
    let api = format!("https://api.github.com/repos/{repo}/releases/{release_ref}");
    // No `-f`: a missing release answers 404 with a JSON body we detect below,
    // which we want to treat as "fall back", not as a curl error.
    let mut cmd = curl_command();
    cmd.args(["-sSL", "--max-time", "30"]).args([
        "-H",
        "Accept: application/vnd.github+json",
        &api,
    ]);
    // Unauthenticated api.github.com is rate-limited PER IP — CI runners
    // share pool IPs and hit it constantly. Ride a token when the
    // environment has one (GITHUB_TOKEN in workflows); end-user machines do
    // fine on the anonymous quota.
    if let Ok(token) = std::env::var("GITHUB_TOKEN").or_else(|_| std::env::var("GH_TOKEN")) {
        if !token.is_empty() {
            cmd.args(["-H", &format!("Authorization: Bearer {token}")]);
        }
    }
    let out = output_bounded(&mut cmd, 45, "curl (release metadata)")
        .await
        .context("is curl installed?")?;
    if !out.status.success() {
        bail!(
            "release metadata request failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let meta: serde_json::Value =
        serde_json::from_slice(&out.stdout).context("bad release metadata payload")?;
    // A missing release is `{"message": "Not Found", ...}` — no `tag_name`.
    // Anything ELSE without a tag (rate limit, auth failure) is a transient
    // API error and must surface as one: falling back would misreport it as
    // "no release provides <asset>" and strand provisioning with a lie.
    let Some(tag) = meta.get("tag_name").and_then(serde_json::Value::as_str) else {
        let message = meta
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unrecognized payload");
        if message == "Not Found" {
            return Ok(None);
        }
        bail!("GitHub API refused the release lookup: {message}");
    };
    let Some(asset) = meta
        .get("assets")
        .and_then(serde_json::Value::as_array)
        .and_then(|assets| {
            assets
                .iter()
                .find(|a| a.get("name").and_then(serde_json::Value::as_str) == Some(asset_name))
        })
    else {
        return Ok(None);
    };
    let url = asset
        .get("browser_download_url")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("{asset_name} in {tag} has no download url"))?;
    let sha256 = asset
        .get("digest")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("release {tag} has no checksum for {asset_name}"))?
        .trim_start_matches("sha256:")
        .to_string();
    Ok(Some(ReleaseAsset {
        version: tag.trim_start_matches('v').to_string(),
        url: url.to_string(),
        sha256,
    }))
}

/// Where auto-fetched daemon binaries are cached: `~/.chimaera/dist/cache/`,
/// keyed by target triple *and* version so an app upgrade fetches a fresh
/// daemon instead of redeploying a stale cached one. Kept separate from the
/// `just dist` stash in [`dist_dir`], which a developer owns and overrides
/// with.
fn download_cache_path(triple: &str, version: &str) -> PathBuf {
    dist_dir()
        .join("cache")
        .join(format!("chimaera-{triple}-{version}"))
}

/// Fetch the daemon binary matching `(os, arch)` from GitHub releases (see
/// [`resolve_release_asset`] for version-vs-latest selection), caching it under
/// [`download_cache_path`] keyed by the resolved version. This is the end-user
/// auto-install path — the app ships no repo and no `just dist` stash. Public
/// for the Windows shell, which provisions a WSL distro exactly the way
/// `connect` provisions a remote host: same assets, same cache, same verify.
///
/// Downloads with the system `curl` (kept dependency-free, like every other
/// ssh/scp/curl shell-out here) and verifies the bytes against the release's
/// published sha256 before trusting them — we're about to run this on the
/// user's login-node account.
pub async fn fetch_release_binary(
    os: &str,
    arch: &str,
    progress: &impl Fn(Phase),
) -> anyhow::Result<PathBuf> {
    let triple = release_triple(os, arch)
        .ok_or_else(|| anyhow::anyhow!("no prebuilt daemon is published for {os}/{arch}"))?;
    let asset = resolve_release_asset(triple).await?;
    let cached = download_cache_path(triple, &asset.version);
    if cached.is_file() {
        tracing::info!("using cached daemon {}", cached.display());
        return Ok(cached);
    }
    progress(Phase::Downloading {
        target: triple.to_string(),
    });

    let tmp = cached.with_extension("part");
    if let Some(parent) = cached.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    tracing::info!("downloading daemon {} from {}", asset.version, asset.url);
    // --max-time bounds the download so a stalled connection can't pin a
    // caller (the Windows shell provisions BEFORE its first window opens).
    let out = output_bounded(
        curl_command()
            .args(["-fSL", "--retry", "2", "--max-time", "300", "-o"])
            .arg(&tmp)
            .arg(&asset.url),
        330,
        "curl (daemon download)",
    )
    .await
    .context("is curl installed?")?;
    if !out.status.success() {
        std::fs::remove_file(&tmp).ok();
        bail!(
            "could not download {}: {}",
            asset.url,
            String::from_utf8_lossy(&out.stderr).trim(),
        );
    }

    let got = sha256_file(&tmp).await?;
    if !got.eq_ignore_ascii_case(&asset.sha256) {
        std::fs::remove_file(&tmp).ok();
        bail!(
            "checksum mismatch on the downloaded daemon (expected {}, got {got})",
            asset.sha256,
        );
    }

    // The exec bit only matters where the cached binary runs in place (unix);
    // on Windows the cache is transfer-only — the mode is set at install time
    // inside the distro/host.
    #[cfg(unix)]
    {
        let mut perms = std::fs::metadata(&tmp)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&tmp, perms)?;
    }
    std::fs::rename(&tmp, &cached)
        .with_context(|| format!("failed to finalize {}", cached.display()))?;
    tracing::info!("cached daemon at {}", cached.display());
    Ok(cached)
}

/// `owner/repo` from [`chimaera_core::REPOSITORY`] (`https://github.com/owner/repo`).
fn repo_slug() -> Option<String> {
    chimaera_core::REPOSITORY
        .trim_end_matches('/')
        .strip_prefix("https://github.com/")
        .map(str::to_string)
}

/// Hex sha256 of `file`, computed in-process — a Windows host has neither
/// `sha256sum` nor `shasum`, and the verify step must never depend on
/// platform hash tools. Streamed in fixed chunks: this crate ships inside
/// the daemon binary run on RSS-policed login nodes, so the hash must never
/// buffer a whole (growing) release asset.
async fn sha256_file(file: &Path) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncReadExt;
    let mut f = tokio::fs::File::open(file)
        .await
        .with_context(|| format!("failed to open {} for checksum", file.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Resolve the LOCAL binary to deploy to a host of the target inferred from
/// `host`: an explicit `binary`, else a developer's `just dist` stash, else
/// auto-fetched from our release. Touches only the local machine (bar a
/// read-only `uname` over the shared connection) — callers resolve this
/// *before* stopping a running daemon, so a failed fetch never strands a host
/// with no daemon.
///
/// A DEV connect deploys YOUR build, so its source policy differs twice:
/// no release fallback (a downloaded release masquerading as the dev daemon
/// would silently test the wrong code — fail loudly instead), and the real
/// `~/.chimaera/dist` stash is searched as well, because `just dist` writes
/// there while an isolated app's `dist_dir()` (under `CHIMAERA_HOME`) no
/// longer points at it.
async fn resolve_local_binary(
    host: &str,
    binary: Option<&Path>,
    home: RemoteHome,
    progress: &impl Fn(Phase),
) -> anyhow::Result<PathBuf> {
    if let Some(p) = binary {
        if !p.is_file() {
            bail!("binary {} does not exist", p.display());
        }
        return Ok(p.to_path_buf());
    }
    let (os, arch) = remote_target(host).await?;
    let name = dist_name(&os, &arch);
    if home == RemoteHome::Dev {
        // A dev connect deploys YOUR build: the (possibly isolated) dist dir
        // first, then the real `~/.chimaera/dist` stash `just dist` writes to.
        let candidate = dist_dir().join(&name);
        if candidate.is_file() {
            return Ok(candidate);
        }
        if let Some(stash) = dirs::home_dir().map(|h| h.join(".chimaera").join("dist").join(&name))
        {
            if stash.is_file() {
                return Ok(stash);
            }
        }
        bail!(
            "no locally built daemon for {host} ({os}/{arch}) — a dev connect \
             deploys YOUR build, never a release download.\n\
             Build one with either:\n\
             \x20 just dist                 (in the chimaera repo: builds musl \
             binaries into ~/.chimaera/dist)\n\
             \x20 chimaera connect {host} --binary /path/to/chimaera-built-for-{host}"
        );
    }
    // The REAL home runs release binaries ONLY — the `just dist` stash is
    // never a source here. A stash build carries the unstamped 0.0.1 sentinel,
    // and a dev binary started in the real home relocates its state to
    // `~/.chimaera-dev` (dev is dev, on both ends): the connect polling
    // `~/.chimaera/manifest.json` never sees it come up, and every retry
    // piles another daemon onto the host. Testing your own build against a
    // host is what the dev connect is for; `--binary` stays as the explicit
    // override.
    fetch_release_binary(&os, &arch, progress)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "chimaera is not installed on {host} ({os}/{arch}) and could not be \
                 fetched automatically: {e}.\n\
                 Provide one with either:\n\
                 \x20 just dist                 (in the chimaera repo: builds musl \
                 binaries into ~/.chimaera/dist)\n\
                 \x20 chimaera connect {host} --binary /path/to/chimaera-built-for-{host}"
            )
        })
}

/// Copy `path` to `<home>/bin/chimaera` on the host, staged + renamed so an
/// interrupted copy never leaves a half-written executable and the old inode
/// stays intact for anything still running it.
async fn deploy_binary(
    host: &str,
    path: &Path,
    home: RemoteHome,
    progress: &impl Fn(Phase),
) -> anyhow::Result<()> {
    progress(Phase::Installing {
        binary: path.to_path_buf(),
    });
    tracing::info!("installing {} on {host} ({})", path.display(), home.dir());
    ssh_run(host, &format!("mkdir -p {}", home.bin_dir())).await?;
    let output = output_bounded(
        scp_cmd(host)
            // Under the WSL transport scp itself runs in the distro, so the
            // "local" side must be spelled as a /mnt path.
            .arg(local_path_for_scp(path))
            .arg(format!("{host}:{}", home.scp_staged_bin())),
        600,
        "scp",
    )
    .await?;
    if !output.status.success() {
        bail!(
            "scp to {host} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    ssh_run(
        host,
        &format!(
            "chmod +x {bin}.new && mv -f {bin}.new {bin}",
            bin = home.bin_path()
        ),
    )
    .await?;
    Ok(())
}

/// Ensure the host has a chimaera binary, installing one only if absent (the
/// fresh-host path — an existing binary is left as-is). Replacing an outdated
/// one is the caller's job: resolve + deploy around a graceful stop, in that
/// order, so a failed fetch never kills a working daemon.
///
/// The DEV home always deploys, even over an existing binary: its whole point
/// is running THIS build, and with no dev daemon alive there is nothing a
/// redeploy could disturb — while a stale binary from last week would
/// otherwise silently start in place of the code under test.
async fn ensure_remote_binary(
    host: &str,
    binary: Option<&Path>,
    home: RemoteHome,
    progress: &impl Fn(Phase),
) -> anyhow::Result<()> {
    if home == RemoteHome::Real {
        // Reuse the existing binary only if it can actually SERVE this home:
        // executability is not enough. A dev (0.0.1-sentinel) binary stranded
        // in the real home — deployed there by a pre-fix release that trusted
        // the dist stash — starts fine but relocates its state to
        // `~/.chimaera-dev`, so the manifest this connect polls never
        // appears. Probe the version and replace anything that is not a
        // stamped release.
        match remote_binary_version(host, home).await? {
            Some(v) if !chimaera_core::version_is_dev(&v) => return Ok(()),
            Some(v) => tracing::info!(
                "replacing the dev build ({v}) stranded at {} — it cannot serve the real home",
                home.bin_path()
            ),
            None => {}
        }
    }
    let path = resolve_local_binary(host, binary, home, progress).await?;
    deploy_binary(host, &path, home, progress).await
}

/// The version the binary installed in `home` reports (`chimaera X.Y.Z` →
/// `X.Y.Z`), or `None` when it is missing, not executable, or prints
/// something unrecognizable — all "not usable, redeploy" to the caller.
async fn remote_binary_version(host: &str, home: RemoteHome) -> anyhow::Result<Option<String>> {
    let output = output_bounded(
        ssh_cmd(host).arg(sh_wrap(&format!(
            "{} --version 2>/dev/null",
            home.bin_path()
        ))),
        SSH_ONESHOT_SECS,
        "ssh",
    )
    .await?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(parse_cli_version(&String::from_utf8_lossy(&output.stdout)))
}

/// Parse clap's `--version` line (`chimaera X.Y.Z`) to `X.Y.Z`. The name
/// check guards against stray shell noise on stdout being read as a version.
fn parse_cli_version(text: &str) -> Option<String> {
    let mut words = text
        .lines()
        .find(|l| !l.trim().is_empty())?
        .split_whitespace();
    if words.next()? != "chimaera" {
        return None;
    }
    words.next().map(str::to_string)
}

/// Start the daemon on the host, then wait — in ONE remote exec — until its
/// manifest reports a live pid.
async fn start_remote(host: &str, home: RemoteHome, not_cluster: bool) -> anyhow::Result<Manifest> {
    tracing::info!("starting chimaera daemon on {host} ({})", home.dir());
    ssh_run(
        host,
        // Detach the daemon so it outlives this one-shot ssh command. Primary
        // path: `serve --daemonize` forks + `setsid(2)`s IN-PROCESS, so it needs
        // no util-linux `setsid`/`nohup` (absent on macOS/BSD and minimal Linux
        // containers) — that is what lets `connect` start a daemon on ANY POSIX
        // remote. Its parent exits 0 the instant the child owns its new session,
        // so `&& exit` ends the ssh command promptly and `connect` moves on to
        // the wait exec below.
        //
        // Fallback, reached only when `--daemonize` is rejected (a pre-flag
        // remote binary — an older release, which by definition sits on a Linux
        // host already provisioned with one): the proven `setsid nohup … &
        // disown`. `;` not `&&` after `mkdir`: with `&&` the trailing `&`
        // backgrounds the whole list and the daemon becomes the foreground child
        // of a subshell still holding the ssh channel's stdio, so sshd never
        // closes the session and `connect` hangs. Found the hard way on a real
        // cluster.
        //
        // The `>> {log}` target MUST stay a regular file: the daemonize child
        // (chimaera/src/daemonize.rs) keeps only regular-file stdio and
        // re-points everything else at /dev/null, so redirecting to a
        // fifo/pipe — or dropping the redirect — silently sends the remote
        // daemon's logs to /dev/null.
        //
        // Sent BARE (not through `sh_wrap`): pre-existing, and it still assumes
        // a POSIX login shell — folding it into the wait exec is the follow-up.
        &format!(
            "mkdir -p {log_dir}; \
             {env}{bin} serve --daemonize >> {log} 2>&1 < /dev/null && exit; \
             {env}setsid nohup {bin} serve >> {log} 2>&1 < /dev/null & disown",
            log_dir = home.log_dir(),
            env = format!(
                "{}{}",
                home.serve_env(),
                if not_cluster {
                    format!("{}=1 ", chimaera_core::cluster::ENV_NOT_A_CLUSTER)
                } else {
                    String::new()
                }
            ),
            bin = home.bin_path(),
            log = home.log_path(),
        ),
    )
    .await?;
    // Wait for the manifest AND a live pid in ONE remote loop instead of up
    // to 15 × 2 client-side execs (each a channel-open RTT + remote fork):
    // 30 half-second ticks = the same 15 s deadline. The manifest is written
    // atomically (tmp + rename), so a readable file is a complete one. The
    // loop prints the manifest (framed) and exits 0 the moment the daemon is
    // up, 1 at the deadline. The old per-exec poll shrugged off one transient
    // ssh failure; a dropped channel (255) retries the wait once.
    let wait = sh_wrap(&start_wait_script(&home.manifest_path(), START_WAIT_TICKS));
    let mut output = output_bounded(ssh_cmd(host).arg(&wait), SSH_ONESHOT_SECS, "ssh").await?;
    if output.status.code() == Some(255) {
        tracing::warn!(
            "the start wait on {host} lost its ssh channel ({}); retrying once",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        output = output_bounded(ssh_cmd(host).arg(&wait), SSH_ONESHOT_SECS, "ssh").await?;
    }
    if output.status.success() {
        return parse_start_wait_output(&String::from_utf8_lossy(&output.stdout)).with_context(
            || format!("daemon on {host} started but its manifest could not be read"),
        );
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    match output.status.code() {
        Some(1) => bail!(
            "daemon on {host} did not start within 15s (check {} there)",
            home.log_path()
        ),
        Some(NO_SLEEP_EXIT) => bail!(
            "{host} has no usable `sleep`; cannot wait for the daemon to start: {}",
            stderr.trim()
        ),
        _ => bail!(
            "waiting for the daemon on {host} failed before its deadline ({}): {}",
            output.status,
            stderr.trim()
        ),
    }
}

/// How many half-second ticks [`start_remote`] waits for the manifest (15 s).
const START_WAIT_TICKS: u32 = 30;

/// The POSIX-sh script [`start_remote`] runs after the start line: up to
/// `ticks` half-second waits for a readable manifest at `manifest_path`
/// written on THIS node whose pid answers `kill -0`, then print it framed
/// (with the tested pid) and exit 0; exit 1 at the deadline. The node check
/// matters on a home shared across nodes: until the new daemon writes its own
/// record the file may still hold another node's, whose pid can be alive
/// here as an unrelated process. Pure so tests can run it under real shells.
fn start_wait_script(manifest_path: &str, ticks: u32) -> String {
    format!(
        "f={manifest_path}; n=$(uname -n 2>/dev/null); {probe} while :; do \
         if [ -r \"$f\" ]; then h=$({hostname}); \
         if {{ [ -z \"$n\" ] || [ \"$h\" = \"$n\" ]; }} && {alive}; then printf '\\n{begin}\\n'; cat \"$f\"; \
         printf '\\n{end}\\npid=%s\\n' \"$p\"; exit 0; fi; fi; \
         [ $i -lt {ticks} ] || exit 1; {tick} done",
        probe = SH_SLEEP_PROBE,
        hostname = SH_MANIFEST_HOSTNAME,
        alive = sh_manifest_alive(),
        begin = MANIFEST_BEGIN,
        end = MANIFEST_END,
        tick = sh_tick(),
    )
}

/// Choose the local tunnel port: explicit flag, else the remote port if
/// locally free, else an OS-assigned free port.
fn pick_local_port(requested: Option<u16>, remote_port: u16) -> anyhow::Result<u16> {
    if let Some(port) = requested {
        return Ok(port);
    }
    // Under the WSL transport the -L bind happens inside the distro, where
    // the remote port number is often ALREADY taken (the WSL-hosted local
    // daemon) — and this Windows-side probe can't see that. Skip straight to
    // an ephemeral pick there; matching the remote port is only a nicety.
    if wsl_transport().is_none() && std::net::TcpListener::bind(("127.0.0.1", remote_port)).is_ok()
    {
        return Ok(remote_port);
    }
    ephemeral_port()
}

fn ephemeral_port() -> anyhow::Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .context("failed to find a free local port")?;
    Ok(listener.local_addr()?.port())
}

fn spawn_tunnel(host: &str, local: u16, remote: u16) -> anyhow::Result<Child> {
    ssh_base(host)
        // Exit non-zero the instant the local bind fails instead of sitting
        // idle: a reconnect that reuses a not-quite-released port then fails
        // in <1s (caught by wait_for_port's early-exit branch) rather than
        // eating the full 15s timeout before the fresh-port retry.
        .args(["-o", "ExitOnForwardFailure=yes"])
        .arg("-N")
        .arg("-L")
        .arg(format!("{local}:127.0.0.1:{remote}"))
        .arg(host)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| TunnelPhaseError(format!("failed to spawn ssh tunnel: {e}")).into())
}

/// Bring up one login-host forward and prove the intended daemon answers
/// through it before publishing "connected". A local `-L` listener appears
/// before the path behind it is usable, and a stale ControlMaster forward can
/// keep accepting after its transport died; TCP readiness alone therefore
/// creates a connected-but-everything-fails window.
async fn open_proven_tunnel(
    host: &str,
    local: u16,
    remote: u16,
    token: &str,
) -> anyhow::Result<(Child, bool)> {
    let mut child = spawn_tunnel(host, local, remote)?;
    let mux_observed = match wait_for_port(local, &mut child).await {
        Ok(mux) => mux,
        Err(error) => {
            // `wait_for_port` may have observed a successful mux exit before
            // timing out on its listener. That proves this attempt registered
            // the forward, so it also owns cancelling it.
            let cancel_master = forward_delegated(false, &mut child);
            abandon_tunnel_attempt(host, local, remote, child, cancel_master).await;
            return Err(error);
        }
    };
    let Some(mux_delegated) = tunnel_proven(local, token, 15, mux_observed, &mut child).await
    else {
        let cancel_master = forward_delegated(mux_observed, &mut child);
        abandon_tunnel_attempt(host, local, remote, child, cancel_master).await;
        return Err(TunnelPhaseError(format!(
            "ssh tunnel on 127.0.0.1:{local} opened, but the authenticated remote daemon did not answer"
        ))
        .into());
    };
    Ok((child, mux_delegated))
}

/// Tear down the ownership shape of an abandoned attempt. Only issue
/// `-O cancel` when this child proved it delegated: cancelling speculatively
/// after a bind collision could tear down a different healthy caller's
/// identical forward.
async fn abandon_tunnel_attempt(
    host: &str,
    local: u16,
    remote: u16,
    mut child: Child,
    cancel_master: bool,
) {
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    if cancel_master {
        cancel_master_forward(
            host,
            &route_of(host),
            &format!("{local}:127.0.0.1:{remote}"),
        )
        .await;
    }
}

fn forward_delegated(observed: bool, child: &mut Child) -> bool {
    observed
        || child
            .try_wait()
            .ok()
            .flatten()
            .is_some_and(|status| status.success())
}

/// Poll the local tunnel port until it accepts connections (15s timeout).
/// Returns true if the forward was delegated to an ssh ControlMaster: the mux
/// client registers the forward with the master and exits 0 immediately, so a
/// zero exit here is success, not failure.
async fn wait_for_port(port: u16, tunnel: &mut Child) -> anyhow::Result<bool> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut mux_delegated = false;
    loop {
        if !mux_delegated {
            if let Some(status) = tunnel.try_wait()? {
                if status.success() {
                    mux_delegated = true;
                } else {
                    return Err(
                        TunnelPhaseError(format!("ssh tunnel exited early: {status}")).into(),
                    );
                }
            }
        }
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return Ok(mux_delegated);
        }
        if tokio::time::Instant::now() > deadline {
            return Err(TunnelPhaseError(format!(
                "tunnel did not come up on 127.0.0.1:{port} within 15s"
            ))
            .into());
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// One-shot ssh/scp deadline. Generous on purpose: the FIRST call to a host
/// raises the ControlMaster, and that may sit in an in-app askpass prompt
/// (password / Duo) for up to its 180s window — a tighter bound here would
/// cut users off mid-typing.
const SSH_ONESHOT_SECS: u64 = 240;

/// The line worth showing from a failed ssh exec: ssh's own complaint is its
/// last stderr line (a chatty login banner sits before it), else the exit
/// status.
fn ssh_failure_line(stderr: &[u8], status: &std::process::ExitStatus) -> String {
    let text = String::from_utf8_lossy(stderr);
    match text.lines().rev().find(|l| !l.trim().is_empty()) {
        Some(line) => line.trim().to_string(),
        None => format!("ssh exited {status}"),
    }
}

/// Run a remote command, failing loudly if it does not exit 0.
async fn ssh_run(host: &str, cmd: &str) -> anyhow::Result<()> {
    let output = output_bounded(ssh_cmd(host).arg(cmd), SSH_ONESHOT_SECS, "ssh").await?;
    if !output.status.success() {
        bail!(
            "ssh {host} {cmd:?} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

// --- Workspace jobs: reaching a chimaera inside a Slurm job ------------------
//
// A cluster workspace runs as a Slurm job whose daemon listens on its compute
// node's address (`serve --bind-routable`, token-gated). Reaching it is a
// short ladder, probed per connect and honest about defeat:
//
//   Direct (preferred) — the login ControlMaster forwards `local -> node:port`
//     (`ssh -L`), exactly the forward HPC centers tell users to keep open to a
//     service in their own job. The login node's sshd does the forwarding;
//     nothing of ours runs there.
//   SshAdopt (fallback) — ssh to the NODE itself, first leg relayed through
//     the login master (`-W`), for clusters whose compute nodes the login node
//     can't reach on a high port but that adopt ssh into the user's job.
//   neither — "can't reach compute nodes from here"; the job keeps running.

/// Which rung of the node-tunnel ladder carried the connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComputeRung {
    /// The login master forwards straight to the job's port on its node.
    Direct,
    /// Laptop ssh end-to-end to the node (clusters that adopt ssh into the
    /// user's job and accept the user's own credentials on nodes).
    SshAdopt,
}

/// A live tunnel to a compute-node daemon.
pub struct ComputeTunnel {
    /// The LOGIN host alias (what the user calls the cluster).
    pub host: String,
    pub node: String,
    pub job_id: String,
    pub local_port: u16,
    /// The job daemon's port on the node.
    pub port: u16,
    pub token: String,
    pub rung: ComputeRung,
    /// The `-L` spec of a forward the login ControlMaster may hold instead of
    /// `child`: always the direct rung's (cancelled on close whether or not
    /// its mux client delegated — that can happen after the probe). `None` for
    /// the ssh-adopt rung — `node_ssh_base` pins `ControlPath=none`, so that
    /// child owns its forward end-to-end and dies with it.
    master_forward: Option<String>,
    /// Captured before an existing-only scope ends. Cleanup's owned task must
    /// never recompute a %C socket from later SSH config or task-local state.
    master_handle: Option<MasterHandle>,
    /// The login alias's [`Route`] when the tunnel opened — which master
    /// holds `master_forward`.
    route: Route,
    child: std::sync::Arc<tokio::sync::Mutex<Child>>,
    closing: std::sync::Arc<tokio::sync::Semaphore>,
}

impl ComputeTunnel {
    /// The UI url: alias + job + node ride the hash so the window can label
    /// itself "alias › node" and scope itself to the allocation.
    pub fn url(&self) -> String {
        format!(
            "http://127.0.0.1:{}/#token={}&host={}&job={}&node={}",
            self.local_port, self.token, self.host, self.job_id, self.node
        )
    }

    /// Wait for the tunnel child (never returns for a healthy ssh-adopt
    /// forward; quickly when the direct rung delegated to the master).
    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.lock().await.wait().await
    }

    /// Kill the tunnel; a master-held forward is also cancelled so local
    /// ports don't leak past the window that opened them.
    pub async fn close(mut self) {
        let _ = self.try_close().await;
    }

    /// Reap the tunnel child and positively acknowledge cancellation of its
    /// exact captured forward. Failure retains the cleanup identity for retry;
    /// an aborted caller cannot cancel the bounded owned cleanup task.
    pub async fn try_close(&mut self) -> anyhow::Result<()> {
        let permit = self
            .closing
            .clone()
            .try_acquire_owned()
            .map_err(|_| anyhow::anyhow!("compute tunnel cleanup already in progress"))?;
        let child = self.child.clone();
        let host = self.host.clone();
        let route = self.route.clone();
        let spec = self.master_forward.clone();
        let captured = self.master_handle.clone();
        compute_cleanup_owned(child, permit, route, spec, move |route, spec| {
            let host = host.clone();
            async move {
                let command = forward_cancel_command(&host, &route, &spec, captured.as_ref())?;
                forward_cancel(command, Duration::from_secs(10)).await
            }
        })
        .await
        .context("compute tunnel cleanup task did not finish")?
    }
}

fn compute_cleanup_owned<F, Fut>(
    child: std::sync::Arc<tokio::sync::Mutex<Child>>,
    permit: tokio::sync::OwnedSemaphorePermit,
    route: Route,
    spec: Option<String>,
    cancel: F,
) -> tokio::task::JoinHandle<anyhow::Result<()>>
where
    F: FnOnce(Route, String) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<()>> + Send,
{
    tokio::spawn(async move {
        let _permit = permit;
        let reaped = tokio::time::timeout(Duration::from_secs(2), async {
            let mut child = child.lock().await;
            if child
                .try_wait()
                .map_err(|_| anyhow::anyhow!("compute child status unavailable"))?
                .is_none()
            {
                let _ = child.start_kill();
                child
                    .wait()
                    .await
                    .map_err(|_| anyhow::anyhow!("compute child could not be reaped"))?;
            }
            Ok(())
        })
        .await
        .map_err(|_| anyhow::anyhow!("compute child did not finish cleanup"))
        .and_then(|result| result);
        // A failed child reap must not prevent attempting the captured mux
        // forward. Both proofs are necessary before a held allocation is freed.
        let canceled = if let Some(spec) = spec {
            cancel(route, spec).await
        } else {
            Ok(())
        };
        reaped.and(canceled)
    })
}

async fn forward_cancel(mut command: Command, deadline: Duration) -> anyhow::Result<()> {
    command.env("LC_ALL", "C").env("LANG", "C");
    let output = tokio::time::timeout(
        deadline,
        output_bounded(&mut command, 10, "compute forward cleanup"),
    )
    .await
    .context("compute forward cleanup did not finish")?
    .map_err(|_| anyhow::anyhow!("compute forward cleanup could not be completed"))?;
    // OpenSSH distinguishes an absent requested forward from a refused cancel.
    // Unknown/localized/mixed diagnostics remain uncertain, never success.
    let diagnostics = std::str::from_utf8(&output.stderr).ok().map(str::trim);
    // OpenSSH's cancel branch can exit zero even after MUX_S_FAILURE. Its
    // fixed error pair distinguishes an absent exact request from refusal.
    let absent = matches!(output.status.code(), Some(0 | 255))
        && output.stdout.is_empty()
        && diagnostics.is_some_and(|diagnostics| {
            let mut lines = diagnostics.lines();
            lines.next()
                == Some("mux_client_forward: forwarding request failed: port not forwarded")
                && matches!(
                    lines.next(),
                    None | Some("muxclient: master cancel forward request failed")
                )
                && lines.next().is_none()
        });
    anyhow::ensure!(
        (output.status.success() && output.stdout.is_empty() && diagnostics == Some(""))
            || absent
            || master_already_absent(&output),
        "compute forward cleanup was not acknowledged"
    );
    Ok(())
}

/// The `-W`-relay ProxyCommand that carries a node-bound ssh's first leg
/// over the login host's existing ControlMaster — no re-auth, no ProxyJump
/// entry required in the user's ssh config. `pin` rides the master of one
/// login node ([`Route::Node`]); `None` the alias's own. The full shared
/// option set, so a first leg that has to dial the master gets the same
/// keepalives and compression as every other. Quoted so a spacey ControlPath
/// survives the shell that runs ProxyCommand; every `%` is doubled because
/// the OUTER ssh percent-expands the ProxyCommand string (a bare `%C` dies
/// with "unknown key %C" — found live on the first rung-B attempt) and the
/// INNER ssh must receive it intact.
fn master_proxy_command(host: &str, pin: Option<&str>) -> String {
    let mut words = vec!["ssh".to_string()];
    if let Some(node) = pin {
        words.push(format!("-o HostName={node}"));
    }
    words.extend(
        ssh_opts()
            .iter()
            .map(|opt| format!("\"{}\"", opt.replace('%', "%%"))),
    );
    // The alias is user input that reaches a shell: single-quoted, with `%`
    // doubled for the outer ssh's own token expansion.
    words.push(format!(
        "-W %h:%p '{}'",
        host.replace('%', "%%").replace('\'', r"'\''")
    ));
    words.join(" ")
}

/// [`master_proxy_command`] over whichever master the login alias's `route`
/// rides: a login node's own for [`Route::Node`]; the alias's for
/// [`Route::NodeViaAlias`] too, since that node's master is itself carried
/// over it.
fn node_proxy_command(host: &str, route: &Route) -> String {
    match route {
        Route::Node(node) => master_proxy_command(host, Some(node)),
        Route::Alias | Route::NodeViaAlias(_) => master_proxy_command(host, None),
    }
}

/// The ssh options for the node leg itself: NO ControlMaster (the child
/// owns its connection; a per-node master would leak sockets per job), fail
/// fast instead of prompting (a rung probe must never hang on interactive
/// auth — a cluster that needs it reads as "rung unavailable" for now).
fn node_ssh_base(host: &str, route: &Route) -> Command {
    let mut c = transport_command("ssh");
    c.env(ASKPASS_ALIAS_ENV, host);
    // The node rung is a different endpoint, not another mux session on the
    // login master. Never substitute that master for the intended node leg.
    c.args(["-o", "ControlPath=none"]);
    // This rung owns a fresh node connection rather than a mux. It must not
    // turn a background existing-only scope into node authentication.
    c.args(existing_master_opts(host, route));
    c.args(authentication_opts(host, route, true));
    c.args([
        "-o",
        "BatchMode=yes",
        "-o",
        "StrictHostKeyChecking=accept-new",
        "-o",
        "ConnectTimeout=15",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
        "-o",
    ]);
    c.arg(format!("ProxyCommand={}", node_proxy_command(host, route)));
    c
}

/// `user@node` for the direct node leg: node hostnames don't match the
/// user's `Host <alias>` config stanza, so the alias's resolved username
/// must be carried explicitly (found live: the node leg went out under the
/// LOCAL username). `ssh -G` resolves config locally — no connection.
async fn node_target(host: &str, node: &str) -> String {
    let mut cmd = transport_command("ssh");
    cmd.env(ASKPASS_ALIAS_ENV, host).arg("-G").arg(host);
    let user = match output_bounded(&mut cmd, 15, "ssh -G").await {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| l.strip_prefix("user ").map(|u| u.trim().to_string())),
        _ => None,
    };
    match user {
        Some(user) if !user.is_empty() => format!("{user}@{node}"),
        _ => node.to_string(),
    }
}

/// Resolve only the portable destination from the user's local SSH config.
/// Private keys, proxy commands and every other executable option stay local.
/// The app uses this when asking its optional account service to keep an alias
/// connected; on Windows resolution runs inside the configured WSL transport.
pub async fn ssh_destination(host: &str) -> anyhow::Result<(String, Option<String>, u16)> {
    let alias = hosts::normalize_alias(host)?;
    let mut command = transport_command("ssh");
    command.arg("-G").arg(&alias);
    let output = output_bounded(&mut command, 15, "ssh configuration resolution").await?;
    if !output.status.success() {
        bail!("could not resolve SSH destination for {alias}");
    }
    parse_ssh_destination(&String::from_utf8(output.stdout)?)
}

fn parse_ssh_destination(config: &str) -> anyhow::Result<(String, Option<String>, u16)> {
    let value = |key: &str| {
        config
            .lines()
            .find_map(|line| line.strip_prefix(key).map(str::trim))
    };
    let hostname = value("hostname ")
        .filter(|value| !value.is_empty())
        .context("SSH destination has no hostname")?;
    let user = value("user ")
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let port = value("port ").unwrap_or("22").parse::<u16>()?;
    if port == 0 || hostname.starts_with('-') || hostname.chars().any(char::is_whitespace) {
        bail!("invalid SSH destination");
    }
    Ok((hostname.to_string(), user, port))
}

fn spawn_node_tunnel(
    host: &str,
    route: &Route,
    node_target: &str,
    local: u16,
    remote: u16,
) -> anyhow::Result<Child> {
    node_ssh_base(host, route)
        .args(["-o", "ExitOnForwardFailure=yes"])
        .arg("-N")
        .arg("-L")
        .arg(format!("{local}:127.0.0.1:{remote}"))
        .arg(node_target)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| TunnelPhaseError(format!("failed to spawn node ssh tunnel: {e}")).into())
}

fn spawn_direct_node_tunnel(
    host: &str,
    route: &Route,
    node: &str,
    local: u16,
    remote: u16,
) -> anyhow::Result<Child> {
    ssh_base_via(host, route)
        .args(["-o", "ExitOnForwardFailure=yes"])
        .arg("-N")
        .arg("-L")
        .arg(format!("{local}:{node}:{remote}"))
        .arg(host)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| TunnelPhaseError(format!("failed to spawn direct node tunnel: {e}")).into())
}

/// Build the tunnel to a workspace job's daemon: the direct forward, then
/// ssh into the node, else an honest error. The arbiter on every rung is
/// `tunnel_proven`: an authed 200 through OUR forward, from a child that is
/// still running afterwards (or that delegated the forward to the
/// ControlMaster and exited 0 — the direct rung whenever a master is up) — a
/// forward that binds but can't reach the daemon, answers with the wrong
/// daemon, or dies right after answering is a failure, not a success.
pub async fn connect_compute_node(
    host: &str,
    node: &str,
    job_id: &str,
    port: u16,
    token: &str,
) -> anyhow::Result<ComputeTunnel> {
    anyhow::ensure!(!node.is_empty(), "job {job_id} has no node yet (queued?)");
    anyhow::ensure!(
        valid_node_name(node),
        "job {job_id}'s node name {node:?} is not a plain host name"
    );
    // One read of the login alias's route for the whole ladder: every rung's
    // forward registers on that master, and every cancel (and the tunnel's
    // own close) must reach the same one even if a login reconnect re-routes
    // the alias meanwhile.
    let route = route_of(host);
    let master_handle = scoped_master(host, &route)?;
    let mk = |local_port, rung, master_forward, child| ComputeTunnel {
        host: host.to_string(),
        node: node.to_string(),
        job_id: job_id.to_string(),
        local_port,
        port,
        token: token.to_string(),
        rung,
        master_forward,
        master_handle: master_handle.clone(),
        route: route.clone(),
        child: std::sync::Arc::new(tokio::sync::Mutex::new(child)),
        closing: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
    };

    // Direct — the login master forwards to the node's port.
    let local = pick_local_port(None, port)?;
    let spec = format!("{local}:{node}:{port}");
    match spawn_direct_node_tunnel(host, &route, node, local, port) {
        Ok(mut child) => match wait_for_port(local, &mut child).await {
            Ok(mux) => {
                if tunnel_proven(local, token, 15, mux, &mut child)
                    .await
                    .is_some()
                {
                    tracing::info!(%node, %job_id, "workspace job tunnel up (direct)");
                    // Always cancelled on close, delegated or not: the mux
                    // client may hand the forward to the master just after
                    // the probe answers, and a cancel the master doesn't
                    // hold is a no-op it answers locally.
                    return Ok(mk(local, ComputeRung::Direct, Some(spec.clone()), child));
                }
                // A delegated forward outlives the exited mux client — the
                // probe failing does not tear it down, so cancel or the
                // master keeps proxying the local port until it expires.
                let cancel_master = forward_delegated(mux, &mut child);
                child.kill().await.ok();
                if cancel_master {
                    cancel_master_forward(host, &route, &spec).await;
                }
                tracing::info!(%node, "direct forward opened but the job's daemon did not answer");
            }
            Err(err) => tracing::info!(%node, %err, "direct forward unavailable"),
        },
        Err(err) => tracing::info!(%node, %err, "direct forward spawn failed"),
    }

    // SshAdopt — laptop ssh end-to-end to the node.
    let target = node_target(host, node).await;
    let local = pick_local_port(None, port)?;
    match spawn_node_tunnel(host, &route, &target, local, port) {
        Ok(mut child) => match wait_for_port(local, &mut child).await {
            Ok(mux) => {
                if tunnel_proven(local, token, 10, mux, &mut child)
                    .await
                    .is_some()
                {
                    tracing::info!(%node, %job_id, "workspace job tunnel up (ssh into the node)");
                    return Ok(mk(local, ComputeRung::SshAdopt, None, child));
                }
                child.kill().await.ok();
                tracing::info!(%node, "ssh into the node forwarded but the job's daemon did not answer");
            }
            Err(err) => tracing::info!(%node, %err, "ssh into the node unavailable"),
        },
        Err(err) => tracing::info!(%node, %err, "ssh into the node spawn failed"),
    }

    bail!(
        "can't reach compute nodes on this cluster from here: neither a forward through the \
         login node to {node}:{port} nor ssh into {node} reached the workspace's chimaera — \
         the job keeps running"
    )
}

/// The compute-rung arbiter: poll [`http_alive_authed`] until
/// `deadline_secs` (the LOCAL listener accepts immediately, but the path
/// behind it — the chained rung's login-resident relay especially — takes
/// seconds to establish; a single-shot probe reads "still handshaking" as
/// "not supported", found live), then confirm the ssh child ITSELF is still
/// running. The second check closes a live-found race: a chained relay
/// whose login-side bind clashed can die (ExitOnForwardFailure) moments
/// AFTER a stale relay on the same port answered the probe for it.
///
/// Returns the effective mux ownership on success. `wait_for_port` can observe
/// the listener in the tiny interval before the mux client exits 0; a
/// successful exit after the authenticated answer therefore upgrades the
/// result to delegated instead of falsely rejecting a healthy forward.
async fn tunnel_proven(
    port: u16,
    token: &str,
    deadline_secs: u64,
    mux: bool,
    child: &mut Child,
) -> Option<bool> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(deadline_secs);
    loop {
        if http_alive_authed(port, token).await {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    match child.try_wait() {
        Ok(None) => Some(mux),
        // A direct `ssh -N` never exits successfully while it owns a live
        // forward. Success here means the ControlMaster accepted ownership,
        // even if `wait_for_port` saw the listener a scheduling tick first.
        Ok(Some(status)) if status.success() => Some(true),
        Ok(Some(_)) | Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture_authentication() -> SshAuthentication {
        SshAuthentication {
            alias: "fixture".into(),
            hostname: "login.example.invalid".into(),
            user: "person".into(),
            port: 2222,
            socket: "/tmp/fixture-agent".into(),
            known_hosts: "/tmp/fixture-trust".into(),
            masters: vec![fixture_master("/tmp/fixture-master")],
            fresh: true,
            keyboard_interactive: None,
            route_helper: None,
            interactive_only: false,
            policy: None,
            support: None,
        }
    }
    #[test]
    fn routed_authentication_freezes_exact_jump_order_and_a_closed_same_binary_helper() {
        let context = "a".repeat(96);
        let jumps = vec!["cxjump-first".into(), "cxjump-second".into()];
        let helper =
            authentication_route_helper("proxyjump cxjump-first,cxjump-second\n", &context, &jumps)
                .unwrap()
                .unwrap();
        assert_eq!(
            helper,
            format!(
                "'{}' --ssh-route-leg {context} 1",
                std::env::current_exe().unwrap().display()
            )
        );
        assert!(
            authentication_route_helper("", &context, &[])
                .unwrap()
                .is_none(),
            "OpenSSH omits the default none field"
        );
        for config in [
            "proxyjump cxjump-second,cxjump-first\n",
            "proxyjump cxjump-first,cxjump-second\nproxyjump other\n",
            "proxyjump cxjump-first,cxjump-second\nproxycommand /bin/false\n",
        ] {
            assert!(authentication_route_helper(config, &context, &jumps).is_err());
        }
        for context in ["short", "a;command", "a b", "a%h"] {
            assert!(authentication_route_helper("", context, &[]).is_err());
        }
        let mut scope = fixture_authentication();
        scope.fresh = true;
        scope.keyboard_interactive = Some(context);
        scope.route_helper = Some(helper.clone());
        scope.interactive_only = true;
        let options = scope.options("fixture", &Route::Alias, false);
        assert!(options.contains(&format!("ProxyCommand={helper}")));
        assert!(options.contains(&"PasswordAuthentication=yes".into()));
        assert!(options.contains(&"PubkeyAuthentication=no".into()));
        assert!(options.contains(&"PreferredAuthentications=keyboard-interactive,password".into()));
        assert!(
            scope
                .options("other", &Route::Alias, false)
                .contains(&"ProxyCommand=false".into()),
            "an unrelated effect receives no route authentication"
        );
        scope.interactive_only = false;
        let options = scope.options("fixture", &Route::Alias, false);
        assert!(options.contains(&"PasswordAuthentication=no".into()));
        assert!(options.contains(&"PubkeyAuthentication=host-bound".into()));
    }
    #[cfg(unix)]
    #[test]
    fn same_binary_helper_path_is_a_literal_shell_word_even_with_metacharacters() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "cx-helper-word-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir(&root).unwrap();
        let context = "a".repeat(96);
        for name in [
            "keeper;false",
            "keeper`false`",
            "keeper&false",
            "keeper(false)",
        ] {
            let executable = root.join(name);
            std::fs::write(&executable, b"#!/bin/sh\nprintf '%s\n' \"$@\"\n").unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
            let command = fixed_route_helper(executable.to_str().unwrap(), &context, 2).unwrap();
            let output = std::process::Command::new("/bin/sh")
                .args(["-c", &command])
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!("--ssh-route-leg\n{context}\n2\n")
            );
        }
        assert!(fixed_route_helper("/tmp/keeper'unsafe", &context, 0).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn authentication_files_are_exact_caller_owned_leases_not_symlinks_or_fifos() {
        use std::os::unix::{fs::symlink, net::UnixListener};
        let root = std::env::temp_dir().join(format!(
            "cx-auth-files-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let socket = root.join("agent");
        let trust = root.join("trust");
        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::write(&trust, b"public trust fixture").unwrap();
        validate_authentication_files(&socket, &trust)
            .await
            .unwrap();
        let link = root.join("link");
        symlink(&trust, &link).unwrap();
        assert!(validate_authentication_files(&socket, &link).await.is_err());
        assert!(validate_authentication_files(&trust, &trust).await.is_err());
        let fifo = root.join("fifo");
        let mut command = Command::new("mkfifo");
        command.arg(&fifo);
        assert!(output_bounded(&mut command, 5, "fixture FIFO")
            .await
            .unwrap()
            .status
            .success());
        assert!(tokio::time::timeout(
            Duration::from_secs(1),
            validate_authentication_files(&socket, &fifo)
        )
        .await
        .unwrap()
        .is_err());
        let oversized = std::fs::File::create(&trust).unwrap();
        oversized.set_len(128 * 1024 + 1).unwrap();
        assert!(validate_authentication_files(&socket, &trust)
            .await
            .is_err());
        assert!(
            socket.exists() && trust.exists(),
            "validation never removes caller leases"
        );
        drop(listener);
        drop(oversized);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn authentication_config_refuses_accumulated_keys_and_destination_changes() {
        let config = "hostname login.example.invalid\nuser person\nport 2222\nidentityfile none\ncertificatefile none\n";
        assert!(authentication_config(config, "login.example.invalid", "person", 2222).unwrap());
        for extra in [
            "identityfile /tmp/private-key\n",
            "certificatefile /tmp/certificate\n",
        ] {
            assert!(authentication_config(
                &format!("{config}{extra}"),
                "login.example.invalid",
                "person",
                2222
            )
            .is_err());
        }
        assert!(!authentication_config(
            &format!("{config}proxyjump jump.example.invalid\n"),
            "login.example.invalid",
            "person",
            2222
        )
        .unwrap());
        assert!(
            authentication_config(config, "different.example.invalid", "person", 2222).is_err()
        );
        for path in [
            "relative",
            "/tmp/%h",
            "/tmp/${SOCKET}",
            "/tmp/a b",
            "/tmp/a\nb",
            "/tmp/'quoted'",
        ] {
            assert!(authentication_path(Path::new(path)).is_err());
        }
    }
    #[test]
    fn authentication_snapshot_correlates_destination_and_master_before_config_changes() {
        let original = "hostname login.example.invalid\nuser person\nport 2222\nidentityfile none\ncertificatefile none\ncontrolpath /tmp/original-master\n";
        let (_, master) = authentication_snapshot(
            "fixture",
            Route::Alias,
            original,
            "login.example.invalid",
            "person",
            2222,
        )
        .unwrap();
        let changed = original
            .replace("login.example.invalid", "other.example.invalid")
            .replace("original-master", "other-master");
        let expanded = original.replace("original-master", "${OTHER_MASTER}");
        assert!(authentication_snapshot(
            "fixture",
            Route::Alias,
            &expanded,
            "login.example.invalid",
            "person",
            2222
        )
        .is_err());
        assert!(authentication_snapshot(
            "fixture",
            Route::Alias,
            &changed,
            "login.example.invalid",
            "person",
            2222
        )
        .is_err());
        assert_eq!(master.path, "/tmp/original-master");
        let mut scope = fixture_authentication();
        scope.masters = vec![master];
        assert!(scope
            .options("fixture", &Route::Alias, false)
            .contains(&"ControlPath=/tmp/original-master".into()));
        assert!(scope
            .options(
                "fixture",
                &Route::Node("other.example.invalid".into()),
                false
            )
            .contains(&"ControlPath=none".into()));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn resolved_policy_arguments_preserve_native_order_and_disable_unowned_credentials() {
        let support = SshAlgorithmSupport::capture().await.unwrap();
        let policy = SshAuthenticationPolicy {
            methods: vec![
                "publickey".into(),
                "password".into(),
                "keyboard-interactive".into(),
            ],
            host_key_algorithms: vec!["ssh-ed25519".into()],
            ca_signature_algorithms: vec!["ssh-ed25519".into()],
            pubkey_accepted_algorithms: vec![
                "webauthn-sk-ecdsa-sha2-nistp256@openssh.com".into(),
                "ssh-ed25519".into(),
            ],
            kex_algorithms: vec!["future-native-kex".into(), "curve25519-sha256".into()],
            ciphers: vec!["chacha20-poly1305@openssh.com".into()],
            macs: vec!["hmac-sha2-256-etm@openssh.com".into()],
        };
        let scope = fixture_authentication()
            .with_keyboard_interactive("fixture_original_owner_context_000000000000")
            .unwrap()
            .with_policy(policy, &support)
            .unwrap();
        scope
            .with_authentication(async {
                let mut command = ssh_base_via("fixture", &Route::Alias);
                command.args(["-G", "fixture"]);
                let output = output_bounded(&mut command, 5, "fixture resolved policy")
                    .await
                    .unwrap();
                assert!(output.status.success());
                let text = std::str::from_utf8(&output.stdout).unwrap();
                for exact in [
                    "preferredauthentications publickey,password,keyboard-interactive",
                    "passwordauthentication yes",
                    "kbdinteractiveauthentication yes",
                    "hostkeyalgorithms ssh-ed25519",
                    "casignaturealgorithms ssh-ed25519",
                    "kexalgorithms curve25519-sha256",
                    "ciphers chacha20-poly1305@openssh.com",
                    "macs hmac-sha2-256-etm@openssh.com",
                ] {
                    assert!(text.lines().any(|line| line == exact), "{exact}");
                }
                for options in [
                    scope.options("other", &Route::Alias, false),
                    scope.options("fixture", &Route::Alias, true),
                ] {
                    assert!(options.contains(&"IdentityAgent=none".into()));
                    assert!(options.contains(&"PasswordAuthentication=no".into()));
                    assert!(options.contains(&"KbdInteractiveAuthentication=no".into()));
                }
                scope.masters[0]
                    .with_existing(async {
                        let options = scope.options("fixture", &Route::Alias, false);
                        assert!(options.contains(&"PasswordAuthentication=no".into()));
                        assert!(options.contains(&"KbdInteractiveAuthentication=no".into()));
                    })
                    .await;
            })
            .await;
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn explicit_key_mfa_context_preserves_strict_identity_and_existing_master_overrides() {
        let context = "fixture_explicit_context_00000000000000000000";
        let scope = fixture_authentication()
            .with_keyboard_interactive(context)
            .unwrap();
        for invalid in [
            "short",
            "fixture_context_with_whitespace_0000000000 x",
            "fixture_context_with_newline_000000000000\n",
        ] {
            assert!(fixture_authentication()
                .with_keyboard_interactive(invalid)
                .is_err());
        }
        fn context_value(command: &Command) -> Option<String> {
            command
                .as_std()
                .get_envs()
                .find(|(key, _)| *key == ASKPASS_CONTEXT_ENV)
                .and_then(|(_, value)| value)
                .map(|value| value.to_string_lossy().into_owned())
        }
        scope
            .with_authentication(async {
                let mut command = ssh_base_via("fixture", &Route::Alias);
                assert_eq!(context_value(&command).as_deref(), Some(context));
                command.args(["-G", "fixture"]);
                let output = output_bounded(&mut command, 5, "fixture explicit MFA configuration")
                    .await
                    .unwrap();
                assert!(output.status.success());
                let text = std::str::from_utf8(&output.stdout).unwrap();
                for exact in [
                    "batchmode no",
                    "kbdinteractiveauthentication yes",
                    "passwordauthentication no",
                    "preferredauthentications publickey,keyboard-interactive",
                    "pubkeyauthentication host-bound",
                    "forwardagent no",
                    "identityfile none",
                    "certificatefile none",
                    "stricthostkeychecking true",
                ] {
                    assert!(text.lines().any(|line| line == exact), "{exact}");
                }
                assert_eq!(context_value(&scp_cmd("fixture")).as_deref(), Some(context));
                let unrelated = scope.options("other", &Route::Alias, false);
                assert!(unrelated.contains(&"KbdInteractiveAuthentication=no".into()));
                assert!(unrelated.contains(&"IdentityAgent=none".into()));
                scope.masters[0]
                    .with_existing(async {
                        let mut command = ssh_base_via("fixture", &Route::Alias);
                        assert!(context_value(&command).is_none());
                        command.args(["-G", "fixture"]);
                        let output =
                            output_bounded(&mut command, 5, "fixture existing MFA refusal")
                                .await
                                .unwrap();
                        assert!(output.status.success());
                        let text = std::str::from_utf8(&output.stdout).unwrap();
                        for exact in [
                            "batchmode yes",
                            "kbdinteractiveauthentication no",
                            "passwordauthentication no",
                            "identityagent none",
                        ] {
                            assert!(text.lines().any(|line| line == exact), "{exact}");
                        }
                    })
                    .await;
            })
            .await;
        assert!(context_value(&ssh_cmd("fixture")).is_none());
    }

    #[tokio::test]
    async fn authentication_scope_is_nested_isolated_and_existing_master_always_wins() {
        let scope = fixture_authentication();
        let mut nested = scope.clone();
        nested.socket = "/tmp/nested-agent".into();
        assert!(authentication_opts("fixture", &Route::Alias, false).is_empty());
        scope
            .with_authentication(async {
                assert!(authentication_opts("fixture", &Route::Alias, false)
                    .contains(&"IdentityAgent=/tmp/fixture-agent".into()));
                nested
                    .with_authentication(async {
                        assert!(authentication_opts("fixture", &Route::Alias, false)
                            .contains(&"IdentityAgent=/tmp/nested-agent".into()));
                    })
                    .await;
                assert!(authentication_opts("fixture", &Route::Alias, false)
                    .contains(&"IdentityAgent=/tmp/fixture-agent".into()));
                assert!(tokio::spawn(async {
                    authentication_opts("fixture", &Route::Alias, false).is_empty()
                })
                .await
                .unwrap());
                let protected = fixture_master("/tmp/protected-master");
                protected
                    .with_existing(async {
                        let command = ssh_base_via("fixture", &Route::Alias);
                        let args: Vec<_> = command
                            .as_std()
                            .get_args()
                            .map(|a| a.to_string_lossy().into_owned())
                            .collect();
                        for expected in [
                            "ProxyCommand=false",
                            "ControlMaster=no",
                            "ControlPath=/tmp/protected-master",
                        ] {
                            let prefix = expected.split('=').next().unwrap().to_string() + "=";
                            assert_eq!(
                                args.iter()
                                    .find(|value| value.starts_with(&prefix))
                                    .unwrap(),
                                expected
                            );
                        }
                    })
                    .await;
                for (host, route, node) in [
                    ("different", Route::Alias, false),
                    ("fixture", Route::Node("changed".into()), false),
                    ("fixture", Route::Alias, true),
                ] {
                    let args = authentication_opts(host, &route, node);
                    for expected in [
                        "IdentityAgent=none",
                        "ProxyCommand=false",
                        "ControlPath=none",
                        "ForwardAgent=no",
                    ] {
                        assert!(args.contains(&expected.into()));
                    }
                }
            })
            .await;
        assert!(authentication_opts("fixture", &Route::Alias, false).is_empty());
        assert!(tokio::time::timeout(
            Duration::from_millis(1),
            scope.with_authentication(std::future::pending::<()>())
        )
        .await
        .is_err());
        assert!(authentication_opts("fixture", &Route::Alias, false).is_empty());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn authentication_effective_ssh_config_never_loads_extra_keys_or_dials_on_scope_loss() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-auth-scope-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let config = root.join("config");
        std::fs::write(&config, "Host fixture\n HostName login.example.invalid\n User person\n Port 2222\n IdentityFile /tmp/forbidden-key\n CertificateFile /tmp/forbidden-cert\n ProxyJump jump.example.invalid\n").unwrap();
        let mut command = Command::new("/usr/bin/ssh");
        command.arg("-F").arg(&config).args([
            "-o",
            "IdentityFile=none",
            "-o",
            "CertificateFile=none",
            "-G",
            "fixture",
        ]);
        let output = output_bounded(&mut command, 5, "fixture configuration")
            .await
            .unwrap();
        assert!(output.status.success());
        assert!(authentication_config(
            std::str::from_utf8(&output.stdout).unwrap(),
            "login.example.invalid",
            "person",
            2222
        )
        .is_err());
        let mut scope = fixture_authentication();
        scope.masters[0].path = root.join("missing-master").to_str().unwrap().into();
        scope
            .with_authentication(async {
                let mut command = ssh_base_via("fixture", &Route::Alias);
                command.args(["-G", "fixture"]);
                let output = output_bounded(&mut command, 5, "fixture frozen configuration")
                    .await
                    .unwrap();
                assert!(output.status.success());
                let text = std::str::from_utf8(&output.stdout).unwrap();
                assert!(
                    authentication_config(text, "login.example.invalid", "person", 2222).unwrap()
                );
                assert!(text
                    .lines()
                    .any(|line| line == "stricthostkeychecking true"));
                assert!(text.lines().any(|line| line == "forwardagent no"));
                let mut command = ssh_base_via("fixture", &Route::Node("127.0.0.1".into()));
                command.args(["-o", "ConnectTimeout=1", "fixture", "true"]);
                let output = output_bounded(&mut command, 5, "fixture changed route")
                    .await
                    .unwrap();
                assert!(!output.status.success());
            })
            .await;
        std::fs::remove_dir_all(root).unwrap();
    }
    fn fixture_master(path: &str) -> MasterHandle {
        use sha2::Digest;
        MasterHandle {
            host: "fixture".into(),
            route: Route::Alias,
            path: path.into(),
            identity: MasterIdentity(sha2::Sha256::digest(path.as_bytes()).into()),
        }
    }

    #[tokio::test]
    async fn existing_master_scope_is_nested_cancel_safe_and_task_local() {
        let handle = fixture_master("/tmp/fixture-master");
        assert!(existing_master_opts("fixture", &Route::Alias).is_empty());
        let (protected, ordinary) = tokio::join!(
            handle.with_existing(async {
                assert!(!existing_master_opts("fixture", &Route::Alias).is_empty());
                handle
                    .with_existing(async {
                        tokio::task::yield_now().await;
                        assert!(!existing_master_opts("fixture", &Route::Alias).is_empty());
                    })
                    .await;
                assert!(!existing_master_opts("fixture", &Route::Alias).is_empty());
                // An owned task must opt in itself; parent scopes do not leak.
                tokio::spawn(async { existing_master_opts("fixture", &Route::Alias).is_empty() })
                    .await
                    .unwrap()
            }),
            async {
                tokio::task::yield_now().await;
                existing_master_opts("fixture", &Route::Alias).is_empty()
            }
        );
        assert!(protected && ordinary);
        let timed = tokio::time::timeout(
            Duration::from_millis(1),
            handle.with_existing(async {
                assert!(!existing_master_opts("fixture", &Route::Alias).is_empty());
                std::future::pending::<()>().await;
            }),
        )
        .await;
        assert!(timed.is_err());
        assert!(existing_master_opts("fixture", &Route::Alias).is_empty());
    }

    #[tokio::test]
    async fn existing_master_restrictions_precede_route_and_shared_options() {
        let handle = fixture_master("/tmp/fixture-master");
        handle
            .with_existing(async {
                for command in [
                    ssh_base_via("fixture", &Route::NodeViaAlias("node-fixture".into())),
                    scp_cmd("fixture"),
                    node_ssh_base("fixture", &Route::Alias),
                ] {
                    let args: Vec<_> = command
                        .as_std()
                        .get_args()
                        .map(|arg| arg.to_string_lossy().into_owned())
                        .collect();
                    let first = |prefix: &str| {
                        args.iter()
                            .find(|arg| arg.starts_with(prefix))
                            .cloned()
                            .unwrap()
                    };
                    assert_eq!(first("ControlMaster="), "ControlMaster=no");
                    assert_eq!(first("BatchMode="), "BatchMode=yes");
                    assert_eq!(first("ProxyCommand="), "ProxyCommand=false");
                    assert_eq!(first("ProxyJump="), "ProxyJump=none");
                }
            })
            .await;
        assert!(ssh_base("fixture")
            .as_std()
            .get_args()
            .any(|arg| arg == "ControlMaster=auto"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn master_presence_requires_bounded_exact_receipts() {
        for (script, expected) in [
            ("printf 'Master running (pid=123)\\n' >&2", Some(true)),
            ("printf 'Master running (pid=123)\\r\\n' >&2", Some(true)),
            ("printf 'Control socket connect(/tmp/fixture): No such file or directory\\n' >&2; exit 255", Some(false)),
            ("printf 'Master running (pid=0)\\n' >&2", None),
            ("printf 'Master running (pid=4294967296)\\n' >&2", None),
            ("printf 'Master running (pid=123)\\nnoise\\n' >&2", None),
            ("printf 'fixture-secret'; printf 'Master running (pid=123)\\n' >&2", None),
            ("printf 'fixture-secret refusal\\n' >&2; exit 255", None),
            ("exit 0", None),
        ] {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script]);
            let result = master_presence(command, 2).await;
            assert_eq!(result.as_ref().ok().copied(), expected);
            if let Err(error) = result { assert!(!error.to_string().contains("fixture-secret")); }
        }
        assert!(master_presence(Command::new("/fixture-missing-ssh"), 1)
            .await
            .is_err());
        let mut stalled = Command::new("/bin/sh");
        stalled.args(["-c", "exec sleep 10"]);
        assert!(
            tokio::time::timeout(Duration::from_secs(3), master_presence(stalled, 1))
                .await
                .unwrap()
                .is_err()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn real_openssh_mux_disappearance_never_dials_proxies_or_askpass() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let fixture = Fixture(PathBuf::from(format!(
            "/tmp/chimaera-mux-{}",
            &chimaera_core::generate_token()[..12]
        )));
        std::fs::create_dir(&fixture.0).unwrap();
        let socket = fixture.0.join("control");
        let config = fixture.0.join("config");
        let proxy = fixture.0.join("proxy-ran");
        let askpass = fixture.0.join("askpass");
        let asked = fixture.0.join("askpass-ran");
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        std::fs::write(&config, format!("Host fixture\n HostName 127.0.0.1\n Port {}\n User fixture\n ControlPath {}\n ControlMaster auto\n BatchMode no\n ProxyCommand sh -c 'printf attempted > {}'\n",
            tcp.local_addr().unwrap().port(), socket.display(), proxy.display())).unwrap();
        std::fs::write(
            &askpass,
            format!("#!/bin/sh\nprintf attempted > {}\n", asked.display()),
        )
        .unwrap();
        std::fs::set_permissions(&askpass, std::fs::Permissions::from_mode(0o700)).unwrap();
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        // Minimal OpenSSH mux v4 hello/alive exchange with the actual client;
        // no SSH server, real user key, remote process or network account.
        let mux = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            async fn read_packet(stream: &mut tokio::net::UnixStream) -> Vec<u8> {
                let length = stream.read_u32().await.unwrap();
                assert!(length <= 1024);
                let mut bytes = vec![0; length as usize];
                stream.read_exact(&mut bytes).await.unwrap();
                bytes
            }
            let hello = read_packet(&mut stream).await;
            assert_eq!(&hello[..8], &[0, 0, 0, 1, 0, 0, 0, 4]);
            stream
                .write_all(&[0, 0, 0, 8, 0, 0, 0, 1, 0, 0, 0, 4])
                .await
                .unwrap();
            let alive = read_packet(&mut stream).await;
            assert_eq!(&alive[..4], &[0x10, 0, 0, 4]);
            let mut reply = vec![0, 0, 0, 12, 0x80, 0, 0, 5];
            reply.extend_from_slice(&alive[4..8]);
            reply.extend_from_slice(&123u32.to_be_bytes());
            stream.write_all(&reply).await.unwrap();
        });
        let handle = fixture_master(socket.to_str().unwrap());
        let mut check = Command::new("ssh");
        check
            .arg("-F")
            .arg(&config)
            .args(["-O", "check"])
            .arg("fixture");
        assert!(master_presence(check, 5).await.unwrap());
        mux.await.unwrap();
        std::fs::remove_file(&socket).unwrap();
        handle
            .with_existing(async {
                for node in [false, true] {
                    let mut command = if node {
                        node_ssh_base("fixture", &Route::Alias)
                    } else {
                        ssh_base_via("fixture", &Route::Alias)
                    };
                    command
                        .arg("-F")
                        .arg(&config)
                        .arg("-S")
                        .arg(&socket)
                        .arg("fixture")
                        .arg("true")
                        .env("SSH_ASKPASS", &askpass)
                        .env("SSH_ASKPASS_REQUIRE", "force")
                        .env("DISPLAY", "fixture");
                    assert!(!output_bounded(&mut command, 5, "fixture SSH")
                        .await
                        .unwrap()
                        .status
                        .success());
                }
            })
            .await;
        assert!(!proxy.exists());
        assert!(!asked.exists());
        assert!(
            tokio::time::timeout(Duration::from_millis(50), tcp.accept())
                .await
                .is_err()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn captured_master_keeps_original_proxyjump_socket_and_close_leg() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let fixture = Fixture(PathBuf::from(format!(
            "/tmp/chimaera-captured-{}",
            &chimaera_core::generate_token()[..8]
        )));
        std::fs::create_dir(&fixture.0).unwrap();
        let config = fixture.0.join("config");
        let pattern = fixture.0.join("%C");
        std::fs::write(&config, format!("Host fixture\n HostName 127.0.0.1\n User fixture\n ProxyJump jump-one.invalid\n ControlPath {}\n", pattern.display())).unwrap();
        let resolve = |guarded: bool| {
            let mut command = Command::new("ssh");
            if guarded {
                command.args(["-o", "ProxyCommand=false", "-o", "ProxyJump=none"]);
            }
            command
                .arg("-F")
                .arg(&config)
                .args(["-T", "-G"])
                .arg("fixture");
            command
        };
        let captured = capture_master_command("fixture", Route::Alias, resolve(false))
            .await
            .unwrap();
        let altered = capture_master_command("fixture", Route::Alias, resolve(true))
            .await
            .unwrap();
        assert!(captured.identity != altered.identity); // %C includes ProxyJump.
        captured
            .with_existing(async {
                let command = ssh_base_via("fixture", &Route::Alias);
                let control = command
                    .as_std()
                    .get_args()
                    .find(|arg| arg.to_string_lossy().starts_with("ControlPath="))
                    .unwrap();
                assert_eq!(
                    control.to_string_lossy(),
                    format!("ControlPath={}", captured.path)
                );
                let wrong_route = ssh_base_via("fixture", &Route::Node("new-node.invalid".into()));
                assert!(wrong_route
                    .as_std()
                    .get_args()
                    .any(|arg| arg == "ControlPath=none"));
            })
            .await;
        let listener = tokio::net::UnixListener::bind(&captured.path).unwrap();
        let entered = std::sync::Arc::new(tokio::sync::Notify::new());
        let release = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        let (e, r) = (entered.clone(), release.clone());
        let mux = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let length = stream.read_u32().await.unwrap();
            assert!(length <= 1024);
            let mut hello = vec![0; length as usize];
            stream.read_exact(&mut hello).await.unwrap();
            stream
                .write_all(&[0, 0, 0, 8, 0, 0, 0, 1, 0, 0, 0, 4])
                .await
                .unwrap();
            let length = stream.read_u32().await.unwrap();
            assert!(length <= 1024);
            let mut request = vec![0; length as usize];
            stream.read_exact(&mut request).await.unwrap();
            assert_eq!(&request[..4], &[0x10, 0, 0, 5]); // exact terminate, not check/dial
            e.notify_one();
            r.acquire().await.unwrap().forget();
            let mut reply = vec![0, 0, 0, 8, 0x80, 0, 0, 1];
            reply.extend_from_slice(&request[4..8]);
            stream.write_all(&reply).await.unwrap();
        });
        // Retargeting config no longer selects the socket used by this handle.
        std::fs::write(&config, format!("Host fixture\n HostName changed.invalid\n User changed\n ProxyJump jump-two.invalid\n ControlPath {}\n", fixture.0.join("different").display())).unwrap();
        let mut command = captured.command();
        command
            .arg("-F")
            .arg(&config)
            .args(["-O", "exit"])
            .arg("fixture");
        let caller = tokio::spawn(captured_master_close(command));
        tokio::time::timeout(Duration::from_secs(3), entered.notified())
            .await
            .unwrap();
        caller.abort();
        let _ = caller.await;
        release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(3), mux)
            .await
            .unwrap()
            .unwrap();
        assert!(!fixture.0.join("different").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn compute_captured_forward_ignores_config_drift_and_survives_caller_abort() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let fixture = Fixture(PathBuf::from(format!(
            "/tmp/chimaera-forward-{}",
            &chimaera_core::generate_token()[..8]
        )));
        std::fs::create_dir(&fixture.0).unwrap();
        let config = fixture.0.join("config");
        let old = fixture.0.join("old.sock");
        let new = fixture.0.join("new.sock");
        let write_config = |path: &Path| {
            std::fs::write(
                &config,
                format!(
                    "Host fixture\n HostName old-login.invalid\n User fixture\n ControlPath {}\n",
                    path.display()
                ),
            )
            .unwrap();
        };
        write_config(&old);
        let mut resolve = Command::new("ssh");
        resolve.arg("-F").arg(&config).args(["-T", "-G", "fixture"]);
        let captured = capture_master_command("fixture", Route::Alias, resolve)
            .await
            .unwrap();
        let pinned = captured
            .with_existing(async { scoped_master("fixture", &Route::Alias).unwrap().unwrap() })
            .await;
        assert!(scoped_master("fixture", &Route::Alias).unwrap().is_none());
        assert!(captured
            .with_existing(async { scoped_master("fixture", &Route::Node("changed-login".into())) })
            .await
            .is_err());
        write_config(&new);
        let listener = tokio::net::UnixListener::bind(&old).unwrap();
        let entered = std::sync::Arc::new(tokio::sync::Notify::new());
        let release = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        let (e, r) = (entered.clone(), release.clone());
        let mux = tokio::spawn(async move {
            for index in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let length = stream.read_u32().await.unwrap();
                assert!(length <= 1024);
                let mut hello = vec![0; length as usize];
                stream.read_exact(&mut hello).await.unwrap();
                stream
                    .write_all(&[0, 0, 0, 8, 0, 0, 0, 1, 0, 0, 0, 4])
                    .await
                    .unwrap();
                let length = stream.read_u32().await.unwrap();
                assert!(length <= 1024);
                let mut request = vec![0; length as usize];
                stream.read_exact(&mut request).await.unwrap();
                assert_eq!(&request[..4], &0x10000007u32.to_be_bytes());
                let mut expected = Vec::new();
                expected.extend(1u32.to_be_bytes()); // local forward
                expected.extend(0u32.to_be_bytes()); // default listening host
                expected.extend(50001u32.to_be_bytes());
                expected.extend(15u32.to_be_bytes());
                expected.extend(b"compute-fixture");
                expected.extend(9000u32.to_be_bytes());
                assert_eq!(&request[8..], expected);
                if index == 1 {
                    e.notify_one();
                    r.acquire().await.unwrap().forget();
                }
                let mut response = Vec::from(8u32.to_be_bytes());
                response.extend(0x80000001u32.to_be_bytes());
                response.extend(&request[4..8]);
                stream.write_all(&response).await.unwrap();
            }
        });
        // Failed-rung cleanup happens while the existing-only scope is active.
        captured
            .with_existing(async {
                cancel_master_forward("fixture", &Route::Alias, "50001:compute-fixture:9000").await
            })
            .await;
        let child = Command::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let child = std::sync::Arc::new(tokio::sync::Mutex::new(child));
        let budget = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let mut tunnel = ComputeTunnel {
            host: "fixture".into(),
            node: "compute-fixture".into(),
            job_id: "123".into(),
            local_port: 50001,
            port: 9000,
            token: "fixture".into(),
            rung: ComputeRung::Direct,
            master_forward: Some("50001:compute-fixture:9000".into()),
            master_handle: Some(pinned),
            route: Route::Alias,
            child: child.clone(),
            closing: budget.clone(),
        };
        // Successful opens retain their captured leg after the scope has ended.
        let caller = tokio::spawn(async move { tunnel.try_close().await });
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        caller.abort();
        let _ = caller.await;
        assert_eq!(budget.available_permits(), 0);
        release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(5), mux)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while budget.available_permits() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(child.lock().await.try_wait().unwrap().is_some());
        assert!(!new.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn compute_forward_cancel_requires_positive_proof_even_with_zero_exit() {
        for (script, succeeds) in [
            ("exit 0", true),
            ("exit 1", false),
            ("printf 'mux_client_forward: forwarding request failed: port not forwarded\\r\\nmuxclient: master cancel forward request failed\\r\\n' >&2; exit 0", true),
            ("printf 'Master refused forwarding request: Permission denied\n' >&2; exit 0", false),
            ("printf 'mux_client_forward: forwarding request failed: port not forwarded\nmuxclient: master cancel forward request failed\n' >&2; exit 0", true),
            ("printf 'Control socket connect(/tmp/fixture): No such file or directory\n' >&2; exit 255", true),
            ("printf 'mux_client_forward: forwarding request failed: port not in permitted opens\nmuxclient: master cancel forward request failed\n' >&2; exit 0", false),
            ("printf 'other failure\nmux_client_forward: forwarding request failed: port not forwarded\n' >&2; exit 255", false),
            ("printf 'untrusted output'; printf 'mux_client_forward: forwarding request failed: port not forwarded\n' >&2; exit 255", false),
        ] {
            let mut command=Command::new("/bin/sh");command.args(["-c",script]);
            assert_eq!(forward_cancel(command,Duration::from_secs(2)).await.is_ok(),succeeds);
        }
        assert!(
            forward_cancel(Command::new("/fixture-missing-ssh"), Duration::from_secs(1))
                .await
                .is_err()
        );
        let mut slow = Command::new("/bin/sleep");
        slow.arg("30");
        assert!(forward_cancel(slow, Duration::from_millis(20))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn compute_cleanup_retains_exact_route_and_forward_across_failure_and_retry() {
        let child = Command::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let child = std::sync::Arc::new(tokio::sync::Mutex::new(child));
        let budget = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let route = Route::NodeViaAlias("captured-login".into());
        let spec = "50001:compute-fixture:9000".to_owned();
        for succeeds in [false, true] {
            let output = seen.clone();
            let result = compute_cleanup_owned(
                child.clone(),
                budget.clone().try_acquire_owned().unwrap(),
                route.clone(),
                Some(spec.clone()),
                move |route, spec| async move {
                    output.lock().unwrap().push((route, spec));
                    anyhow::ensure!(succeeds, "fixture cancel unavailable");
                    Ok(())
                },
            )
            .await
            .unwrap();
            assert_eq!(result.is_ok(), succeeds);
            assert!(child.lock().await.try_wait().unwrap().is_some());
            assert_eq!(budget.available_permits(), 1);
        }
        assert_eq!(
            *seen.lock().unwrap(),
            vec![(route.clone(), spec.clone()), (route, spec)]
        );
    }

    #[tokio::test]
    async fn compute_cleanup_bounds_child_lock_and_still_cancels_forward() {
        let child = Command::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let child = std::sync::Arc::new(tokio::sync::Mutex::new(child));
        let locked = child.clone().lock_owned().await;
        let budget = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let proof = cancelled.clone();
        let result = tokio::time::timeout(
            Duration::from_secs(4),
            compute_cleanup_owned(
                child.clone(),
                budget.clone().try_acquire_owned().unwrap(),
                Route::Alias,
                Some("50001:compute-fixture:9000".into()),
                move |route, spec| async move {
                    assert_eq!(route, Route::Alias);
                    assert_eq!(spec, "50001:compute-fixture:9000");
                    proof.store(true, std::sync::atomic::Ordering::Release);
                    Ok(())
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(result.is_err());
        assert!(cancelled.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(budget.available_permits(), 1);
        drop(locked);
        let mut child = child.lock().await;
        child.kill().await.unwrap();
        child.wait().await.unwrap();
    }

    #[tokio::test]
    async fn compute_cleanup_finishes_captured_forward_after_caller_abort() {
        let child = Command::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let child = std::sync::Arc::new(tokio::sync::Mutex::new(child));
        let budget = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let entered = std::sync::Arc::new(tokio::sync::Notify::new());
        let release = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        let finished = std::sync::Arc::new(tokio::sync::Notify::new());
        let (e, r, f) = (entered.clone(), release.clone(), finished.clone());
        let cleanup = compute_cleanup_owned(
            child.clone(),
            budget.clone().try_acquire_owned().unwrap(),
            Route::Node("captured-login".into()),
            Some("50001:compute-fixture:9000".into()),
            move |route, spec| async move {
                assert_eq!(route, Route::Node("captured-login".into()));
                assert_eq!(spec, "50001:compute-fixture:9000");
                e.notify_one();
                r.acquire().await.unwrap().forget();
                f.notify_one();
                Ok(())
            },
        );
        let caller = tokio::spawn(cleanup);
        entered.notified().await;
        caller.abort();
        let _ = caller.await;
        assert_eq!(budget.available_permits(), 0);
        release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(2), finished.notified())
            .await
            .unwrap();
        assert!(child.lock().await.try_wait().unwrap().is_some());
        tokio::time::timeout(Duration::from_secs(2), async {
            while budget.available_permits() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn master_exit_requires_acknowledgment_or_an_explicit_absent_socket() {
        for (script, succeeds) in [
            ("exit 0", true),
            ("exit 1", false),
            (
                "printf 'Control socket connect(/tmp/fixture): No such file or directory\\n' >&2; exit 255",
                true,
            ),
            (
                "printf 'Control socket connect(/tmp/fixture): Permission denied\\n' >&2; exit 255",
                false,
            ),
            (
                "printf 'unrelated failure\\nControl socket connect(/tmp/fixture): No such file or directory\\n' >&2; exit 255",
                false,
            ),
            (
                "printf 'untrusted output'; printf 'Control socket connect(/tmp/fixture): No such file or directory\\n' >&2; exit 255",
                false,
            ),
        ] {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script]);
            assert_eq!(
                master_exit(command, Duration::from_secs(2)).await.is_ok(),
                succeeds
            );
        }
        let missing = Command::new("/chimaera-fixture-missing-ssh");
        assert!(master_exit(missing, Duration::from_secs(1)).await.is_err());
        let mut stalled = Command::new("/bin/sleep");
        stalled.arg("30");
        assert!(master_exit(stalled, Duration::from_millis(20))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn master_cleanup_attempts_both_routes_even_when_the_node_refuses() {
        for route in [
            Route::Node("n".into()),
            Route::NodeViaAlias("n".into()),
            Route::Alias,
        ] {
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let output = seen.clone();
            let result = close_masters_owned(route.clone(), move |leg| {
                output.lock().unwrap().push(leg.clone());
                async move {
                    if leg != Route::Alias {
                        bail!("fixture refused closure");
                    }
                    Ok(())
                }
            })
            .await
            .unwrap();
            let expected = if route == Route::Alias {
                vec![Route::Alias]
            } else {
                vec![route, Route::Alias]
            };
            assert_eq!(*seen.lock().unwrap(), expected);
            assert_eq!(result.is_ok(), expected.len() == 1);
        }
    }

    #[tokio::test]
    async fn master_cleanup_finishes_the_alias_after_its_caller_is_cancelled() {
        let started = std::sync::Arc::new(tokio::sync::Notify::new());
        let proceed = std::sync::Arc::new(tokio::sync::Notify::new());
        let finished = std::sync::Arc::new(tokio::sync::Notify::new());
        let caller = {
            let (started, proceed, finished) = (started.clone(), proceed.clone(), finished.clone());
            tokio::spawn(async move {
                close_masters_owned(Route::NodeViaAlias("n".into()), move |leg| {
                    let (started, proceed, finished) =
                        (started.clone(), proceed.clone(), finished.clone());
                    async move {
                        if leg != Route::Alias {
                            started.notify_one();
                            proceed.notified().await;
                        } else {
                            finished.notify_one();
                        }
                        Ok(())
                    }
                })
                .await
            })
        };
        tokio::time::timeout(Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        proceed.notify_one();
        tokio::time::timeout(Duration::from_secs(2), finished.notified())
            .await
            .unwrap();
    }

    #[test]
    fn keeper_ssh_destination_retains_only_address_user_and_port() {
        let config = "host work\nhostname login.example.test\nuser scientist\nport 2202\nidentityfile /secret/key\nproxycommand sensitive command\n";
        assert_eq!(
            parse_ssh_destination(config).unwrap(),
            ("login.example.test".into(), Some("scientist".into()), 2202)
        );
        assert_eq!(
            parse_ssh_destination("hostname localhost\n").unwrap(),
            ("localhost".into(), None, 22)
        );
        for config in [
            "user only",
            "hostname -option",
            "hostname bad host",
            "hostname okay\nport 0",
            "hostname okay\nport 65536",
        ] {
            assert!(parse_ssh_destination(config).is_err());
        }
    }

    /// The probe and start-wait frame the manifest on BOTH sides and print
    /// the pid they tested: noise around the frame (an echoing ~/.bashrc) is
    /// ignored, a cut-off frame, a pid that disagrees with the manifest, or a
    /// missing verdict is an error — never a silent "dead" that would start a
    /// second daemon — and a corrupt manifest is a fresh start only when
    /// nothing was found alive in it.
    #[test]
    fn probe_output_is_parsed_strictly_between_markers() {
        let manifest = serde_json::to_string_pretty(&fake_manifest(Some("b.1"), 42)).unwrap();
        let framed = |trailer: &str| {
            format!(
                "noise from ~/.bashrc\n{MANIFEST_BEGIN}\n{manifest}\n{MANIFEST_END}\n{trailer}more noise\n"
            )
        };
        let p = parse_probe_output(&framed("pid=42\nalive\n"))
            .unwrap()
            .expect("manifest");
        assert_eq!(p.manifest.pid, 42);
        assert!(p.alive);
        assert!(
            p.here(),
            "a host that doesn't name its node is taken at its word"
        );
        let p = parse_probe_output(&framed("pid=42\ndead\n"))
            .unwrap()
            .expect("manifest");
        assert!(!p.alive);
        // The node trailer: the fake manifest was written on "host".
        let p = parse_probe_output(&framed("pid=42\nnode=HOST\nhost=host\ndns=same\nalive\n"))
            .unwrap()
            .expect("manifest");
        assert!(p.here(), "node names are case-insensitive");
        assert_eq!(p.manifest_node_resolves, None);
        let p = parse_probe_output(&framed("pid=42\nnode=ln02\nhost=host\ndns=gone\ndead\n"))
            .unwrap()
            .expect("manifest");
        assert!(!p.here(), "written on another node");
        assert_eq!(p.node, "ln02");
        assert_eq!(p.manifest_node_resolves, Some(false));
        let p = parse_probe_output(&framed(
            "pid=42\nnode=ln02\nhost=host\ndns=found 10.0.0.1 fe80::1%eth0 ::1\ndead\n",
        ))
        .unwrap()
        .expect("manifest");
        assert_eq!(p.manifest_node_resolves, Some(true));
        assert_eq!(
            p.manifest_node_addrs,
            vec![
                "10.0.0.1".parse::<std::net::IpAddr>().unwrap(),
                "::1".parse().unwrap()
            ],
            "numeric addresses; a scoped link-local is skipped"
        );
        let p = parse_probe_output(&framed("pid=42\nnode=ln02\nhost=host\ndns=unknown\ndead\n"))
            .unwrap()
            .expect("manifest");
        assert_eq!(p.manifest_node_resolves, None);
        assert!(
            parse_probe_output(&framed("pid=42\nnode=ln02\nhost=other\ndns=gone\ndead\n")).is_err(),
            "the resolver was asked about a name serde doesn't see — never a \"gone\" for it"
        );
        assert!(
            parse_probe_output("only noise\n").unwrap().is_none(),
            "no begin marker = no manifest"
        );
        assert!(
            parse_probe_output(&format!("{MANIFEST_BEGIN}\n{manifest}\n")).is_err(),
            "cut off before the end marker"
        );
        assert!(
            parse_probe_output(&framed("pid=43\nalive\n")).is_err(),
            "sed and serde disagree on the pid"
        );
        assert!(
            parse_probe_output(&framed("pid=\ndead\n")).is_err(),
            "sed found no pid"
        );
        assert!(
            parse_probe_output(&framed("pid=42\n")).is_err(),
            "no verdict"
        );
        let corrupt =
            |trailer: &str| format!("{MANIFEST_BEGIN}\n{{not json\n{MANIFEST_END}\n{trailer}");
        assert!(
            parse_probe_output(&corrupt("pid=\ndead\n"))
                .unwrap()
                .is_none(),
            "a corrupt manifest with nothing alive is a fresh start"
        );
        assert!(
            parse_probe_output(&corrupt("pid=42\nalive\n")).is_err(),
            "a corrupt manifest with a live pid is never a fresh start"
        );
        assert!(
            parse_probe_output(&corrupt("pid=42\nnode=ln02\nhost=ln01\ndns=found\ndead\n"))
                .is_err(),
            "nor one another node wrote — its pid was never tested there"
        );
        assert!(
            parse_probe_output(&corrupt("pid=42\nnode=ln01\nhost=ln01\ndns=same\ndead\n"))
                .unwrap()
                .is_none(),
            "a corrupt manifest this node wrote, nothing alive: a fresh start"
        );
        assert_eq!(
            parse_start_wait_output(&framed("pid=42\n")).unwrap().pid,
            42
        );
        assert!(parse_start_wait_output("nothing\n").is_err());
        assert!(parse_start_wait_output(&framed("pid=7\n")).is_err());
    }

    /// The remote pid and node-name extraction is a `sed` over the manifest;
    /// pin both patterns against serde's real pretty AND compact renderings
    /// with the host's own sed (BSD on macOS, GNU on Linux — both POSIX), so
    /// a renderer change can never silently make every daemon look dead, or
    /// every manifest look like another node's.
    #[cfg(unix)]
    #[test]
    fn manifest_pid_sed_matches_serde_pretty_and_compact() {
        let dir =
            std::env::temp_dir().join(format!("chimaera-remote-pid-sed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = fake_manifest(Some("b.1"), 4242);
        // The pattern takes the FIRST "pid" line: the real Manifest must stay
        // flat, or a nested pid would be picked instead of the daemon's.
        let pretty = serde_json::to_string_pretty(&manifest).unwrap();
        assert_eq!(
            pretty.lines().filter(|l| l.contains("\"pid\"")).count(),
            1,
            "Manifest must stay flat — a nested pid would be picked first:\n{pretty}"
        );
        for (name, text) in [
            (
                "pretty.json",
                serde_json::to_string_pretty(&manifest).unwrap(),
            ),
            ("compact.json", serde_json::to_string(&manifest).unwrap()),
        ] {
            let path = dir.join(name);
            std::fs::write(&path, text).unwrap();
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("f={}; {SH_MANIFEST_PID}", path.display()))
                .output()
                .expect("run sh");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim(),
                "4242",
                "{name}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("f={}; {SH_MANIFEST_HOSTNAME}", path.display()))
                .output()
                .expect("run sh");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim(),
                manifest.hostname,
                "{name}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The LOGIN shells the wrapped remote commands get run through here —
    /// sshd hands `ssh host <cmd>` to `$SHELL -c`, so the tests do the same.
    /// The POSIX ones (`sh`: bash-in-POSIX-mode on macOS, dash on Debian CI;
    /// `dash` itself, the strictest), `bash`/`zsh`, and the hostile ones —
    /// `tcsh` (ships with macOS) and `fish` when installed — which read none
    /// of the script's syntax and reach it only through `sh_wrap`. Absent
    /// shells skip.
    #[cfg(unix)]
    fn login_shells() -> Vec<&'static str> {
        ["sh", "dash", "bash", "zsh", "tcsh", "fish"]
            .into_iter()
            .filter(|shell| {
                std::process::Command::new(shell)
                    .args(["-c", "true"])
                    .output()
                    .is_ok_and(|out| out.status.success())
            })
            .collect()
    }

    /// Run `script` exactly as a remote would: wrapped by `sh_wrap`, handed
    /// to `shell -c` the way sshd hands a command to the login shell.
    #[cfg(unix)]
    fn run_script(shell: &str, script: &str) -> std::process::Output {
        std::process::Command::new(shell)
            .args(["-c", &sh_wrap(script)])
            .output()
            .expect("spawn shell")
    }

    /// `sh_wrap`'s quoting is the one form every login shell reads the same:
    /// a script full of single quotes, double quotes, `$(…)`, `$((…))`,
    /// backslashes, and a `!` (csh history expansion — `tcsh -c "sh -c 'echo
    /// hello!world'"` dies with "Event not found") must reach `sh`
    /// byte-for-byte through each of them, tcsh and fish included.
    /// The scheduler check finds Slurm by walking the LOGIN shell's PATH —
    /// tools that exist only on a profile-managed PATH count, a partial
    /// toolset doesn't — and reports the submit command's directory. It runs
    /// under every login shell a cluster account may have (tcsh refuses
    /// `-lc`, so its PATH comes from `sh -l` / the shell itself).
    #[cfg(unix)]
    #[test]
    fn scheduler_check_walks_the_login_path_under_every_shell() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("chimaera-sched-{}", std::process::id()));
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let tool = |name: &str| {
            let p = bin.join(name);
            std::fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        for t in ["sbatch", "squeue", "scancel"] {
            tool(t);
        }
        let run = |shell: &str| {
            let path = format!("{}:/usr/bin:/bin", bin.display());
            let out = std::process::Command::new(shell)
                .args(["-c", &sh_wrap(&sh_scheduler())])
                .env("PATH", &path)
                .env("SHELL", "/nonexistent-shell")
                .env("HOME", &dir)
                .output()
                .expect("spawn shell");
            parse_scheduler_line(&String::from_utf8_lossy(&out.stdout))
        };
        for shell in login_shells() {
            let info = run(shell).expect("a verdict line");
            assert_eq!(info.kind, Scheduler::None, "{shell}: sinfo is missing");
        }
        tool("sinfo");
        for shell in login_shells() {
            let info = run(shell).expect("a verdict line");
            assert_eq!(info.kind, Scheduler::Slurm, "{shell}");
            assert_eq!(info.bindir, bin.display().to_string(), "{shell}");
        }
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(
            parse_scheduler_line("noise\n---chimaera-sched--- slurm /opt/x;rm\n"),
            Some(SchedulerInfo {
                kind: Scheduler::Slurm,
                bindir: String::new()
            }),
            "a dir that isn't a plain path is dropped, never spliced into a later PATH"
        );
        assert_eq!(parse_scheduler_line("no verdict here"), None);
    }

    #[cfg(unix)]
    #[test]
    fn sh_wrap_survives_every_login_shell() {
        let script =
            r#"printf '%s\n' "a'b" 'c"d' $((1+2)) "$(printf '%s' '\(x\)')" '\1' hello!world"#;
        let shells = login_shells();
        // macOS ships tcsh: the hostile-login-shell case must really run here,
        // not silently skip.
        #[cfg(target_os = "macos")]
        assert!(shells.contains(&"tcsh"), "{shells:?}");
        for shell in shells {
            let out = run_script(shell, script);
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                "a'b\nc\"d\n3\n\\(x\\)\n\\1\nhello!world\n",
                "{shell}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    /// A pid that is certainly not alive: a child already reaped.
    #[cfg(unix)]
    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    /// Spawn `cmd` detached from this process (backgrounded by a throwaway
    /// shell that exits at once), returning its pid. The stop script's
    /// `kill -0` must see the process VANISH after SIGTERM, which a direct
    /// child of the test process never does — it lingers as a zombie until
    /// reaped. Orphaning it to init mirrors the daemon's setsid'd lifetime.
    #[cfg(unix)]
    fn spawn_orphan(cmd: &str) -> u32 {
        // The orphan must not inherit the pipe `output()` waits on, or the
        // throwaway shell's exit is invisible until the orphan itself ends.
        let out = std::process::Command::new("sh")
            .args(["-c", &format!("{cmd} >/dev/null 2>&1 </dev/null & echo $!")])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .expect("pid")
    }

    /// A manifest written on THIS machine, as the daemon here would.
    #[cfg(unix)]
    fn write_manifest(path: &Path, pid: u32) {
        write_manifest_on(path, pid, &uname_n());
    }

    /// A manifest as the daemon on `node` would have written it.
    #[cfg(unix)]
    fn write_manifest_on(path: &Path, pid: u32, node: &str) {
        let manifest = Manifest {
            hostname: node.to_string(),
            ..fake_manifest(Some("b.1"), pid)
        };
        std::fs::write(path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    }

    /// `uname -n` — what the remote scripts compare a manifest against. The
    /// daemon records `gethostname(2)`; the two must agree on one machine.
    #[cfg(unix)]
    fn uname_n() -> String {
        let out = std::process::Command::new("uname")
            .arg("-n")
            .output()
            .unwrap();
        let node = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(
            Some(node.clone()),
            chimaera_core::this_node(),
            "uname -n vs gethostname"
        );
        node
    }

    /// The resolver control in the probe: a node that can't resolve its own
    /// name never answers `found`/`gone` (CI sandboxes may lack a self entry).
    #[cfg(unix)]
    fn own_name_resolves() -> bool {
        use std::net::ToSocketAddrs;
        (uname_n().as_str(), 22).to_socket_addrs().is_ok()
    }

    #[cfg(unix)]
    fn perl_available() -> bool {
        std::process::Command::new("perl")
            .args(["-MSocket=:addrinfo", "-e", "exit 0"])
            .output()
            .is_ok_and(|out| out.status.success())
    }

    /// The one-exec probe command through every login shell, against real
    /// files and pids: alive (this test process), dead (a reaped child), and
    /// absent (no manifest → nothing printed, exit 0).
    #[cfg(unix)]
    #[test]
    fn probe_script_runs_under_every_login_shell() {
        let dir =
            std::env::temp_dir().join(format!("chimaera-probe-script-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("manifest.json");
        let script = probe_script(&path.display().to_string());
        for shell in login_shells() {
            write_manifest(&path, std::process::id());
            let out = run_script(shell, &script);
            assert!(
                out.status.success(),
                "{shell}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let p = parse_probe_output(&String::from_utf8_lossy(&out.stdout))
                .unwrap()
                .expect("manifest");
            assert_eq!(p.manifest.pid, std::process::id());
            assert!(p.alive, "{shell}: this process is alive");
            assert_eq!(p.node, uname_n(), "{shell}: the probe names its node");
            assert!(p.here(), "{shell}: written on this node");
            assert_eq!(
                p.manifest_node_resolves, None,
                "{shell}: no lookup when local"
            );

            write_manifest(&path, dead_pid());
            let out = run_script(shell, &script);
            let p = parse_probe_output(&String::from_utf8_lossy(&out.stdout))
                .unwrap()
                .expect("manifest");
            assert!(!p.alive, "{shell}: a reaped pid is dead");

            // Another node's record: its pid (alive HERE — this process) is
            // not the daemon's liveness, and its name is looked up.
            write_manifest_on(&path, std::process::id(), "localhost");
            let out = run_script(shell, &script);
            let p = parse_probe_output(&String::from_utf8_lossy(&out.stdout))
                .unwrap()
                .expect("manifest");
            assert!(!p.here(), "{shell}: written on another node");
            if perl_available() && own_name_resolves() {
                assert_eq!(
                    p.manifest_node_resolves,
                    Some(true),
                    "{shell}: localhost resolves"
                );
                assert!(
                    !p.manifest_node_addrs.is_empty(),
                    "{shell}: with its addresses"
                );
            }
            write_manifest_on(&path, dead_pid(), "chimaera-gone-node.invalid");
            let out = run_script(shell, &script);
            let p = parse_probe_output(&String::from_utf8_lossy(&out.stdout))
                .unwrap()
                .expect("manifest");
            assert!(!p.here());
            // `.invalid` never resolves (RFC 2606); only a resolver that
            // couldn't answer at all may leave it unknown — never "found".
            assert_ne!(p.manifest_node_resolves, Some(true), "{shell}");

            std::fs::remove_file(&path).unwrap();
            let out = run_script(shell, &script);
            assert!(out.status.success(), "{shell}: absent manifest exits 0");
            assert!(parse_probe_output(&String::from_utf8_lossy(&out.stdout))
                .unwrap()
                .is_none());
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The start-wait command through every login shell: prints the manifest
    /// as soon as one appears with a live pid (written mid-loop here, so the
    /// sleep path is exercised) and exits 1 at the deadline otherwise.
    #[cfg(unix)]
    #[test]
    fn start_wait_script_runs_under_every_login_shell() {
        let dir =
            std::env::temp_dir().join(format!("chimaera-start-script-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("manifest.json");
        for shell in login_shells() {
            std::fs::remove_file(&path).ok();
            let child = std::process::Command::new(shell)
                .args([
                    "-c",
                    &sh_wrap(&start_wait_script(&path.display().to_string(), 6)),
                ])
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            std::thread::sleep(Duration::from_millis(700));
            write_manifest(&path, std::process::id());
            let out = child.wait_with_output().unwrap();
            assert!(out.status.success(), "{shell}: manifest appeared in time");
            let m = parse_start_wait_output(&String::from_utf8_lossy(&out.stdout)).unwrap();
            assert_eq!(m.pid, std::process::id());

            // A manifest whose pid is dead never satisfies the loop.
            write_manifest(&path, dead_pid());
            let out = run_script(shell, &start_wait_script(&path.display().to_string(), 2));
            assert_eq!(
                out.status.code(),
                Some(1),
                "{shell}: a dead pid must time out"
            );

            // Nor does another node's record, even when its pid happens to
            // be alive here: that is the file a fresh start replaces, not
            // the daemon it started.
            write_manifest_on(&path, std::process::id(), "other-login-node");
            let out = run_script(shell, &start_wait_script(&path.display().to_string(), 2));
            assert_eq!(
                out.status.code(),
                Some(1),
                "{shell}: another node's manifest must time out"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The stop command through every login shell: SIGTERM ends a cooperative
    /// process (exit 0); one that ignores SIGTERM reaches the deadline and
    /// reports the distinct still-alive code — never SIGKILL; a dead pid
    /// reports the signal failure.
    #[cfg(unix)]
    #[test]
    fn stop_script_runs_under_every_login_shell() {
        for shell in login_shells() {
            let victim = spawn_orphan("sleep 30");
            let out = run_script(shell, &stop_script(victim, 6));
            assert!(
                out.status.success(),
                "{shell}: {}",
                String::from_utf8_lossy(&out.stderr)
            );

            // `exec` keeps the pid: the ignored disposition survives into sleep.
            let stubborn = spawn_orphan("sh -c 'trap \"\" TERM; exec sleep 30'");
            std::thread::sleep(Duration::from_millis(200));
            let out = run_script(shell, &stop_script(stubborn, 2));
            assert_eq!(
                out.status.code(),
                Some(STOP_STILL_ALIVE_EXIT),
                "{shell}: still alive after the deadline"
            );
            let _ = run_script("sh", &format!("kill -9 {stubborn}"));

            let out = run_script(shell, &stop_script(dead_pid(), 2));
            assert_eq!(
                out.status.code(),
                Some(STOP_SIGNAL_FAILED_EXIT),
                "{shell}: SIGTERM to a dead pid fails"
            );
        }
    }

    /// A remote with no `sleep` at all must not spin through its tick budget
    /// in milliseconds and fabricate "still running" / "did not start": both
    /// wait loops exit the distinct no-sleep code instead. Run with a PATH
    /// holding only `sh`, so `sleep` (external) is missing while `sh -c`
    /// still resolves.
    #[cfg(unix)]
    #[test]
    fn wait_loops_report_a_missing_sleep_instead_of_spinning() {
        let dir = std::env::temp_dir().join(format!("chimaera-no-sleep-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink("/bin/sh", dir.join("sh")).ok();
        let run = |script: &str| {
            std::process::Command::new("/bin/sh")
                .env("PATH", &dir)
                .args(["-c", &sh_wrap(script)])
                .output()
                .unwrap()
        };
        let stubborn = spawn_orphan("sh -c 'trap \"\" TERM; exec sleep 30'");
        std::thread::sleep(Duration::from_millis(200));
        let out = run(&stop_script(stubborn, 20));
        assert_eq!(
            out.status.code(),
            Some(NO_SLEEP_EXIT),
            "stop: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = run_script("sh", &format!("kill -9 {stubborn}"));
        let out = run(&start_wait_script(
            &dir.join("absent.json").display().to_string(),
            30,
        ));
        assert_eq!(
            out.status.code(),
            Some(NO_SLEEP_EXIT),
            "start wait: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn child_stream_reader_fails_at_byte_cap() {
        let err = read_bounded(tokio::io::repeat(b'x'), 32, "test")
            .await
            .expect_err("infinite output must hit the byte ceiling");
        assert!(err.to_string().contains("32-byte limit"), "{err:#}");
    }

    /// Every ssh/scp call must carry the ControlMaster trio (so one auth
    /// covers the whole session; socket path uses ssh's short `%C` token),
    /// the trust-on-first-use host-key policy that lets a fresh install reach
    /// a never-seen host with no tty, and the liveness bounds that keep a
    /// dead link (laptop sleep) from leaving zombie masters and forwards.
    /// A deep isolated CHIMAERA_HOME (the dev app) must not blow the ~104-byte
    /// unix-socket path limit: the socket falls back to a short /tmp dir keyed
    /// by the home, still ending in `/cm` (so `…/cm/%C` holds), and distinct
    /// homes never collide (a dev master never rides the real app's).
    #[test]
    fn control_dir_stays_under_sun_path_for_a_deep_home() {
        use std::path::Path;
        // Normal home: unchanged — data_dir/cm.
        let normal = control_dir(Path::new("/Users/x/.chimaera"));
        assert!(
            normal.to_string_lossy().ends_with("/.chimaera/cm"),
            "{}",
            normal.display()
        );

        // Deep isolated home (a worktree CHIMAERA_HOME) overshoots → /tmp fallback.
        let deep = control_dir(Path::new(
            "/Users/martinkjellberg/dev/chimaera/.claude/worktrees/magical-colden-00b63c/.chimaera-dev-app/data",
        ));
        assert!(deep.starts_with("/tmp/"), "{}", deep.display());
        assert!(deep.ends_with("cm"), "{}", deep.display());
        // dir + '/' + the 40-char %C expansion + OpenSSH's temporary suffix
        // must clear the ~104-byte cap.
        assert!(
            deep.as_os_str().len() + 1 + 40 + 1 + 16 <= 104,
            "{}",
            deep.display()
        );
        // A different deep home resolves to a different socket dir.
        let other = control_dir(Path::new(
            "/Users/martinkjellberg/dev/chimaera/.claude/worktrees/some-other-worktree-abcdef/.chimaera-dev-app/data",
        ));
        assert_ne!(other, deep);

        // The isolated native app's normal state dir is shorter than a deep
        // worktree but still too long once OpenSSH's temporary suffix is
        // included.
        let app_home = control_dir(Path::new(
            "/Users/martinkjellberg/.chimaera-dev-app/chimaera/data",
        ));
        assert!(app_home.starts_with("/tmp/"), "{}", app_home.display());
        assert!(
            app_home.as_os_str().len() + 1 + 40 + 1 + 16 <= 104,
            "{}",
            app_home.display()
        );
    }

    /// The WSL transport's path vocabulary, pure halves only (the global
    /// transport switch is never flipped in tests — other tests exercise the
    /// direct-ssh path concurrently).
    #[test]
    fn wsl_transport_path_spelling() {
        // scp's "local" side runs in the distro: drive-letter absolutes
        // become /mnt paths, spaces survive, non-drive paths pass through.
        assert_eq!(
            windows_path_as_wsl(r"C:\Users\First Last\bin\chimaera.exe"),
            "/mnt/c/Users/First Last/bin/chimaera.exe"
        );
        assert_eq!(windows_path_as_wsl(r"D:\x"), "/mnt/d/x");
        // \\?\-verbatim paths (current_exe / canonicalize can produce them).
        assert_eq!(
            windows_path_as_wsl(r"\\?\C:\Users\x\chimaera.exe"),
            "/mnt/c/Users/x/chimaera.exe"
        );
        // UNC passes through — callers must treat that as unreachable.
        assert_eq!(windows_path_as_wsl(r"\\server\share\f"), "//server/share/f");
        assert_eq!(windows_path_as_wsl("/already/unixy"), "/already/unixy");
        // The in-distro master socket: same ~/.chimaera/cm shape an
        // in-distro connect uses, comfortably under the sun_path cap.
        let cp = wsl_control_path("/home/someuser");
        assert_eq!(cp, "/home/someuser/.chimaera/cm/%C");
        assert!(cp.len() <= 100);
    }

    #[test]
    fn ssh_opts_multiplex_and_accept_new_hosts() {
        let opts = ssh_opts();
        assert_eq!(opts[0], "-o");
        assert_eq!(opts[1], "ControlMaster=auto");
        assert_eq!(opts[2], "-o");
        assert!(opts[3].starts_with("ControlPath="), "{}", opts[3]);
        assert!(opts[3].ends_with("/cm/%C"), "{}", opts[3]);
        assert_eq!(opts[4], "-o");
        assert_eq!(opts[5], "ControlPersist=10m");
        assert_eq!(opts[6], "-o");
        assert_eq!(opts[7], "StrictHostKeyChecking=accept-new");
        assert_eq!(opts[8], "-o");
        assert_eq!(opts[9], "ConnectTimeout=15");
        assert_eq!(opts[10], "-o");
        assert_eq!(opts[11], "ServerAliveInterval=15");
        assert_eq!(opts[12], "-o");
        assert_eq!(opts[13], "ServerAliveCountMax=3");
        assert_eq!(opts[14], "-o");
        // The documented one-line revert if a fast-LAN flow ever regresses
        // (docs/design/perf-remote-plan.md R5) — pinned so dropping or reordering
        // it is a visible, deliberate change.
        assert_eq!(opts[15], "Compression=yes");
        assert_eq!(opts.len(), 16, "pin the whole option list");
    }

    #[test]
    fn ssh_and_scp_children_carry_their_askpass_alias() {
        fn alias(command: &Command) -> Option<String> {
            command
                .as_std()
                .get_envs()
                .find(|(key, _)| *key == ASKPASS_ALIAS_ENV)
                .and_then(|(_, value)| value)
                .map(|value| value.to_string_lossy().into_owned())
        }

        assert_eq!(alias(&ssh_cmd("cluster")), Some("cluster".into()));
        assert_eq!(alias(&scp_cmd("remote-2")), Some("remote-2".into()));
        assert_eq!(
            alias(&node_ssh_base("login.example.edu", &Route::Alias)),
            Some("login.example.edu".into())
        );
    }

    /// The fresh-port retry in the app keys off this downcast; if the tunnel
    /// errors stop carrying the marker, every failure would re-run the whole
    /// connect (and re-prompt 2FA).
    #[test]
    fn tunnel_phase_errors_downcast_through_anyhow() {
        let err: anyhow::Error = TunnelPhaseError("bind clash".into()).into();
        assert!(err.downcast_ref::<TunnelPhaseError>().is_some());
        assert_eq!(format!("{err}"), "bind clash");
        let other = anyhow::anyhow!("auth failed");
        assert!(other.downcast_ref::<TunnelPhaseError>().is_none());
    }

    /// The whole point of the HTTP probe: a listener that accepts but never
    /// answers (a dead ssh forward after laptop sleep looks exactly like
    /// this) is DOWN, while anything that answers HTTP is up. A bare TCP
    /// connect can't tell them apart — that regression made reconnect a
    /// silent no-op.
    #[tokio::test]
    async fn http_alive_requires_a_response_not_just_an_accept() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // Accepts and holds the socket open, never writing: down.
        let silent = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let silent_port = silent.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((s, _)) = silent.accept().await {
                held.push(s); // keep it open so this mimics a live-but-dead forward
            }
        });
        assert!(
            !http_alive(silent_port).await,
            "accept-only listener must read as down"
        );

        // Answers any bytes with an HTTP status line: up (even a 401 proves
        // the daemon end-to-end).
        let http = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let http_port = http.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = http.accept().await {
                let mut buf = [0u8; 512];
                let _ = s.read(&mut buf).await;
                let _ = s
                    .write_all(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n")
                    .await;
            }
        });
        assert!(
            http_alive(http_port).await,
            "an HTTP answer must read as up"
        );

        // Nothing listening at all: down.
        let free = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let free_port = free.local_addr().unwrap().port();
        drop(free);
        assert!(
            !http_alive(free_port).await,
            "closed port must read as down"
        );
    }

    #[tokio::test]
    async fn http_alive_authed_probes_health_with_endpoint_identity() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut chunk = [0u8; 512];
                    let n = stream.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..n]);
                    if request.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8(request).unwrap();
                assert!(
                    request.starts_with("GET /api/v1/health HTTP/1.1\r\n"),
                    "liveness must use the cheap health route: {request:?}"
                );
                let status = if request.contains("\r\nAuthorization: Bearer right-token\r\n") {
                    "200 OK"
                } else {
                    "401 Unauthorized"
                };
                stream
                    .write_all(format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\n\r\n").as_bytes())
                    .await
                    .unwrap();
            }
        });

        assert!(http_alive_authed(port, "right-token").await);
        assert!(
            !http_alive_authed(port, "wrong-token").await,
            "an HTTP answer from the wrong endpoint identity is not live"
        );
        server.await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tunnel_proof_detects_mux_exit_after_listener_readiness() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 512];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                .await
                .unwrap();
        });
        let mut exited = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
        exited.wait().await.unwrap();

        assert_eq!(
            tunnel_proven(port, "token", 1, false, &mut exited).await,
            Some(true),
            "a successful exit after the listener appeared is mux delegation"
        );
        server.await.unwrap();
    }

    /// The remote-home fragments are load-bearing shell strings: every remote
    /// side effect derives from them, so this pins both roots — especially the
    /// dev asymmetry (`CHIMAERA_HOME` relocates the daemon's data to
    /// `<home>/data`, so its manifest/log sit one level deeper than the real
    /// home's) and that scoping rides an ENV PREFIX, never a flag on the
    /// load-bearing `chimaera serve` string.
    #[test]
    fn remote_home_fragments_split_real_and_dev() {
        use RemoteHome::{Dev, Real};
        assert_eq!(Real.manifest_path(), "$HOME/.chimaera/manifest.json");
        assert_eq!(
            Dev.manifest_path(),
            "$HOME/.chimaera-dev/data/manifest.json"
        );
        assert_eq!(Real.log_path(), "$HOME/.chimaera/logs/serve.log");
        assert_eq!(Dev.log_path(), "$HOME/.chimaera-dev/data/logs/serve.log");
        assert_eq!(Real.bin_path(), "$HOME/.chimaera/bin/chimaera");
        assert_eq!(Dev.bin_path(), "$HOME/.chimaera-dev/bin/chimaera");
        assert_eq!(Real.serve_env(), "");
        assert_eq!(Dev.serve_env(), "CHIMAERA_HOME=$HOME/.chimaera-dev ");
        // scp destinations are $HOME-relative: scp expands no shell variables.
        assert!(!Real.scp_staged_bin().contains('$'));
        assert!(!Dev.scp_staged_bin().contains('$'));
        // The two roots must be disjoint — a dev path may never alias a real
        // one (".chimaera-dev" starts with ".chimaera", so check the boundary).
        assert!(!Dev.dir().starts_with(&format!("{}/", Real.dir())));
        assert_ne!(Dev.dir(), Real.dir());
        // The build is the only selector: tests run on the unstamped 0.0.1
        // sentinel, so `current()` must resolve Dev here — a release build
        // (stamped version) resolves Real by the same predicate.
        assert!(chimaera_core::is_dev_build());
        assert_eq!(RemoteHome::current(), RemoteHome::Dev);
    }

    #[test]
    fn dist_names_map_targets() {
        assert_eq!(dist_name("linux", "x86_64"), "chimaera-x86_64-linux-musl");
        assert_eq!(dist_name("linux", "aarch64"), "chimaera-aarch64-linux-musl");
        assert_eq!(dist_name("darwin", "arm64"), "chimaera-arm64-darwin");
    }

    /// The auto-fetch path must map detected targets to the exact asset names
    /// the release workflow publishes, and skip targets we don't build.
    #[test]
    fn release_triples_match_published_assets() {
        assert_eq!(
            release_triple("linux", "x86_64"),
            Some("x86_64-unknown-linux-musl")
        );
        assert_eq!(
            release_triple("linux", "aarch64"),
            Some("aarch64-unknown-linux-musl")
        );
        // Some clusters' uname reports arm64 for 64-bit ARM.
        assert_eq!(
            release_triple("linux", "arm64"),
            Some("aarch64-unknown-linux-musl")
        );
        assert_eq!(
            release_triple("darwin", "arm64"),
            Some("aarch64-apple-darwin")
        );
        // Not published → no guess.
        assert_eq!(release_triple("darwin", "x86_64"), None);
        assert_eq!(release_triple("windows", "x86_64"), None);
    }

    #[test]
    fn repo_slug_drives_the_release_api() {
        assert_eq!(
            repo_slug().as_deref(),
            Some("martinappberg/chimaera"),
            "owner/repo feeds the api.github.com releases url"
        );
    }

    /// The download cache is keyed by version so an app upgrade never
    /// redeploys a stale cached daemon (which would loop the update check).
    #[test]
    fn download_cache_is_versioned() {
        let a = download_cache_path("x86_64-unknown-linux-musl", "0.1.1");
        let b = download_cache_path("x86_64-unknown-linux-musl", "0.1.2");
        assert_ne!(a, b);
        assert!(a.ends_with("chimaera-x86_64-unknown-linux-musl-0.1.1"));
        assert!(a.starts_with(dist_dir()));
    }

    #[test]
    fn local_port_prefers_remote_port_when_free() {
        // Bind a port to force the fallback path.
        let holder = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let held = holder.local_addr().unwrap().port();
        let picked = pick_local_port(None, held).unwrap();
        assert_ne!(picked, held, "held port must not be picked");
        assert_eq!(pick_local_port(Some(4321), held).unwrap(), 4321);
    }

    /// The full reuse/update/attach-outdated policy, with session counts a
    /// caller would have fetched (or failed to fetch — `None` = busy).
    #[test]
    fn update_decision_matrix() {
        const OURS: &str = "ff52221.100";
        // Same source (timestamps differ across targets): reuse.
        assert_eq!(
            update_decision(OURS, Some("ff52221.999"), None, false),
            Decision::Reuse
        );
        assert_eq!(
            update_decision(OURS, Some(OURS), Some(3), false),
            Decision::Reuse
        );
        // Different build + provably idle: safe to replace.
        assert_eq!(
            update_decision(OURS, Some("d4e587f.50"), Some(0), false),
            Decision::Update
        );
        // Missing build id = ancient: same rules as a mismatch.
        assert_eq!(
            update_decision(OURS, None, Some(0), false),
            Decision::Update
        );
        // Live sessions, or a count we couldn't get: never silently kill.
        assert_eq!(
            update_decision(OURS, Some("d4e587f.50"), Some(2), false),
            Decision::ConnectOutdated
        );
        assert_eq!(
            update_decision(OURS, Some("d4e587f.50"), None, false),
            Decision::ConnectOutdated
        );
        assert_eq!(
            update_decision(OURS, None, None, false),
            Decision::ConnectOutdated
        );
        // Force (--update-daemon) replaces regardless of sessions or build.
        assert_eq!(
            update_decision(OURS, Some("d4e587f.50"), Some(7), true),
            Decision::Update
        );
        assert_eq!(update_decision(OURS, None, None, true), Decision::Update);
        assert_eq!(
            update_decision(OURS, Some(OURS), Some(0), true),
            Decision::Update
        );
    }

    /// Session counting only trusts the expected shape and only counts
    /// `alive: true` (the list also carries finished sessions).
    #[test]
    fn count_alive_sessions_parses_payloads() {
        assert_eq!(count_alive_sessions("[]"), Some(0));
        assert_eq!(
            count_alive_sessions(
                r#"[
                    {"id": "a", "alive": true},
                    {"id": "b", "alive": false},
                    {"id": "c", "alive": true},
                    {"id": "d"}
                ]"#
            ),
            Some(2)
        );
        // Not the sessions payload => unknown, never zero.
        assert_eq!(count_alive_sessions(""), None);
        assert_eq!(count_alive_sessions("unauthorized"), None);
        assert_eq!(count_alive_sessions(r#"{"error": "no"}"#), None);
    }

    /// The real-home reuse check keys on this parse: a dev (0.0.1) version
    /// must be recognized so a stranded dev binary gets replaced, and shell
    /// noise must never read as a version (that would wrongly skip a deploy).
    #[test]
    fn cli_version_parses_only_the_expected_shape() {
        assert_eq!(parse_cli_version("chimaera 0.1.7\n"), Some("0.1.7".into()));
        assert_eq!(parse_cli_version("\nchimaera 0.0.1"), Some("0.0.1".into()));
        assert!(chimaera_core::version_is_dev("0.0.1"));
        assert_eq!(parse_cli_version("bash: chimaera: command not found"), None);
        assert_eq!(parse_cli_version("chimaera"), None);
        assert_eq!(parse_cli_version(""), None);
    }

    // --- resolve_daemon characterization (fake side effects) ----------------
    //
    // The crate can't be live-verified (no remote host in CI), so these pin
    // the connect DECISION phase against a fake RemoteOps: the ordered call log
    // proves which side effects fire (and their order — the Update arm's
    // resolve-before-stop guard), and a phase-label capture proves the progress
    // emits. SshOps delegates verbatim, so what holds for the fake holds live.

    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    /// One recorded [`RemoteOps`] call, in order.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Call {
        RemoteProbe,
        RemoteSessionsCount,
        RemoteDaemonExtension,
        ResolveLocalBinary,
        StopRemote,
        DeployBinary,
        StartRemote,
        EnsureRemoteBinary,
    }

    /// What a scripted probe answers along one route.
    type ProbeScript = Box<dyn Fn(&Route) -> anyhow::Result<ProbeRun>>;

    /// A scripted [`RemoteOps`] that records its ordered call log — each call
    /// with the route it ran along — and returns canned outcomes: no ssh, no
    /// host, no real binary.
    struct FakeOps {
        log: RefCell<Vec<(Call, Route)>>,
        route: RefCell<Route>,
        probe_manifest: Option<Manifest>,
        alive: bool,
        sessions: Option<usize>,
        daemon_extension: Option<bool>,
        composition_unconfirmed: bool,
        resolved_bin: PathBuf,
        start_manifest: Manifest,
        /// Overrides the default probe (the manifest, written on the node
        /// the probe lands on) — for multi-node scenarios.
        probe: Option<ProbeScript>,
        /// Where "this machine" resolves any node name.
        local: Vec<std::net::IpAddr>,
        /// What the detecting probe reports.
        scheduler: Scheduler,
    }

    impl FakeOps {
        fn base() -> Self {
            FakeOps {
                log: RefCell::new(Vec::new()),
                route: RefCell::new(Route::Alias),
                probe_manifest: None,
                alive: false,
                sessions: None,
                daemon_extension: None,
                composition_unconfirmed: false,
                resolved_bin: PathBuf::from("/unused"),
                start_manifest: fake_manifest(Some(chimaera_core::BUILD_ID), 999),
                probe: None,
                local: vec![CLUSTER_ADDR],
                scheduler: Scheduler::None,
            }
        }
        fn record(&self, c: Call) {
            let route = self.route.borrow().clone();
            self.log.borrow_mut().push((c, route));
        }
        fn calls(&self) -> Vec<Call> {
            self.log.borrow().iter().map(|(c, _)| *c).collect()
        }
        fn routed(&self) -> Vec<(Call, Route)> {
            self.log.borrow().clone()
        }
    }

    impl RemoteOps for FakeOps {
        fn route(&self, _host: &str) -> Route {
            self.route.borrow().clone()
        }
        fn set_route(&self, _host: &str, route: Route) {
            *self.route.borrow_mut() = route;
        }
        async fn local_addrs(&self, _node: &str) -> Vec<std::net::IpAddr> {
            self.local.clone()
        }
        async fn remote_probe(&self, _host: &str, _detect: bool) -> anyhow::Result<ProbeRun> {
            self.record(Call::RemoteProbe);
            if let Some(probe) = &self.probe {
                return probe(&self.route.borrow());
            }
            Ok(ProbeRun::Ran(self.probe_manifest.clone().map(|m| Probe {
                node: m.hostname.clone(),
                manifest: m,
                alive: self.alive,
                manifest_node_resolves: None,
                manifest_node_addrs: Vec::new(),
            })))
        }
        fn scheduler(&self, _host: &str) -> Scheduler {
            self.scheduler
        }
        async fn remote_sessions_count(
            &self,
            _host: &str,
            _manifest: &Manifest,
        ) -> anyhow::Result<Option<usize>> {
            self.record(Call::RemoteSessionsCount);
            Ok(self.sessions)
        }
        async fn remote_daemon_extension(
            &self,
            _host: &str,
            _manifest: &Manifest,
        ) -> anyhow::Result<Option<bool>> {
            self.record(Call::RemoteDaemonExtension);
            anyhow::ensure!(!self.composition_unconfirmed, "composition unconfirmed");
            Ok(self.daemon_extension)
        }
        async fn resolve_local_binary(
            &self,
            _host: &str,
            _binary: Option<&Path>,
            _progress: &impl Fn(Phase),
        ) -> anyhow::Result<PathBuf> {
            self.record(Call::ResolveLocalBinary);
            anyhow::ensure!(
                self.resolved_bin != Path::new("FAIL"),
                "replacement unavailable"
            );
            Ok(self.resolved_bin.clone())
        }
        async fn stop_remote(&self, _host: &str, _pid: u32) -> anyhow::Result<()> {
            self.record(Call::StopRemote);
            Ok(())
        }
        async fn deploy_binary(
            &self,
            _host: &str,
            _path: &Path,
            _progress: &impl Fn(Phase),
        ) -> anyhow::Result<()> {
            self.record(Call::DeployBinary);
            Ok(())
        }
        async fn start_remote(&self, _host: &str) -> anyhow::Result<Manifest> {
            self.record(Call::StartRemote);
            Ok(self.start_manifest.clone())
        }
        async fn ensure_remote_binary(
            &self,
            _host: &str,
            _binary: Option<&Path>,
            _progress: &impl Fn(Phase),
        ) -> anyhow::Result<()> {
            self.record(Call::EnsureRemoteBinary);
            Ok(())
        }
    }

    fn fake_manifest(build: Option<&str>, pid: u32) -> Manifest {
        Manifest {
            hostname: "host".into(),
            port: 4600,
            token: "token".into(),
            pid,
            version: "0.0.1".into(),
            started_at: 0,
            build: build.map(str::to_string),
            slurm_job_id: None,
            runtime_leases: false,
        }
    }

    /// Drive `resolve_daemon` against `fake`, returning its result and the
    /// ordered list of phase labels emitted via the progress sink.
    async fn run_resolve(
        fake: &FakeOps,
        update_daemon: bool,
    ) -> ((Manifest, bool, Option<usize>), Vec<&'static str>) {
        let (out, phases) = try_resolve(fake, update_daemon).await;
        (out.expect("resolve_daemon"), phases)
    }

    async fn try_resolve(
        fake: &FakeOps,
        update_daemon: bool,
    ) -> (
        anyhow::Result<(Manifest, bool, Option<usize>)>,
        Vec<&'static str>,
    ) {
        let phases = RefCell::new(Vec::<&'static str>::new());
        let progress = |p: Phase| {
            phases.borrow_mut().push(match p {
                Phase::Probing => "probing",
                Phase::Routing { .. } => "routing",
                Phase::Updating => "updating",
                Phase::Downloading { .. } => "downloading",
                Phase::Installing { .. } => "installing",
                Phase::Starting => "starting",
                Phase::Tunneling { .. } => "tunneling",
            });
        };
        let opts = ConnectOpts {
            update_daemon,
            ..Default::default()
        };
        let out = resolve_daemon(fake, "host", &opts, &progress).await;
        (out, phases.into_inner())
    }

    /// Reuse: a matching build with no forced update attaches to the running
    /// daemon as-is — the session count is skipped (it can't change the
    /// decision), nothing is stopped/deployed/started, and the only phase is
    /// the initial probe.
    #[tokio::test]
    async fn resolve_daemon_never_starts_or_attaches_on_a_cluster() {
        // Nothing running: a plain host would get a fresh daemon; a cluster
        // gets nothing at all — one probe, then the typed answer.
        let fake = FakeOps {
            scheduler: Scheduler::Slurm,
            ..FakeOps::base()
        };
        let (out, phases) = try_resolve(&fake, false).await;
        let err = out.expect_err("a cluster is not connected to");
        let cluster = err.downcast_ref::<ClusterHost>().expect("ClusterHost");
        assert_eq!(cluster.scheduler, Scheduler::Slurm);
        assert!(cluster.login_daemon.is_none());
        assert_eq!(fake.calls(), vec![Call::RemoteProbe]);
        assert_eq!(phases, vec!["probing"]);

        // A daemon an earlier connect left on the login node is reported —
        // never attached to, updated, or stopped by a connect.
        let fake = FakeOps {
            scheduler: Scheduler::Slurm,
            probe_manifest: Some(fake_manifest(Some("old.1"), 42)),
            alive: true,
            sessions: Some(0),
            ..FakeOps::base()
        };
        let (out, _) = try_resolve(&fake, true).await;
        let err = out.expect_err("still a cluster");
        let found = err
            .downcast_ref::<ClusterHost>()
            .and_then(|c| c.login_daemon.clone())
            .expect("the old daemon is reported");
        assert_eq!((found.pid, found.alive), (42, Some(true)));
        assert_eq!(
            fake.calls(),
            vec![
                Call::RemoteProbe,
                Call::ResolveLocalBinary,
                Call::DeployBinary
            ],
            "repair replaces the executable without stopping or starting a login daemon"
        );
    }

    /// The warned override: a cluster the user allowed is a regular remote.
    #[tokio::test]
    async fn resolve_daemon_on_a_host_said_not_to_be_a_cluster_behaves_like_any_host() {
        let fake = FakeOps {
            scheduler: Scheduler::Slurm,
            ..FakeOps::base()
        };
        let opts = ConnectOpts {
            not_cluster: true,
            ..Default::default()
        };
        let (manifest, _, _) = resolve_daemon(&fake, "host", &opts, &|_| {})
            .await
            .expect("started");
        assert_eq!(manifest.pid, 999);
    }

    #[tokio::test]
    async fn resolve_daemon_on_an_allowed_cluster_behaves_like_any_host() {
        let fake = FakeOps {
            scheduler: Scheduler::Slurm,
            ..FakeOps::base()
        };
        let opts = ConnectOpts {
            login_serve: true,
            ..Default::default()
        };
        let (manifest, _, _) = resolve_daemon(&fake, "host", &opts, &|_| {})
            .await
            .expect("started");
        assert_eq!(manifest.pid, 999);
        assert_eq!(
            fake.calls(),
            vec![
                Call::RemoteProbe,
                Call::EnsureRemoteBinary,
                Call::StartRemote
            ]
        );
    }

    #[tokio::test]
    async fn repair_redeploys_when_no_daemon_is_running_and_resolves_before_mutation() {
        let fake = FakeOps::base();
        let (result, _) = try_resolve(&fake, true).await;
        assert!(result.is_ok());
        assert_eq!(
            fake.calls(),
            vec![
                Call::RemoteProbe,
                Call::ResolveLocalBinary,
                Call::DeployBinary,
                Call::StartRemote
            ]
        );
        let failed = FakeOps {
            resolved_bin: PathBuf::from("FAIL"),
            ..FakeOps::base()
        };
        let (result, _) = try_resolve(&failed, true).await;
        assert!(result.is_err());
        assert_eq!(
            failed.calls(),
            vec![Call::RemoteProbe, Call::ResolveLocalBinary]
        );
    }

    #[tokio::test]
    async fn resolve_daemon_reuses_matching_build() {
        let fake = FakeOps {
            probe_manifest: Some(fake_manifest(Some(chimaera_core::BUILD_ID), 42)),
            alive: true,
            sessions: Some(3),
            resolved_bin: PathBuf::from("/unused"),
            start_manifest: fake_manifest(Some(chimaera_core::BUILD_ID), 999),
            ..FakeOps::base()
        };
        let ((manifest, outdated, live), phases) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 42, "returns the probed daemon");
        assert!(!outdated);
        assert_eq!(live, None);
        assert_eq!(fake.calls(), vec![Call::RemoteProbe]);
        assert_eq!(phases, vec!["probing"]);
    }

    #[test]
    fn daemon_composition_requires_original_identity_and_strict_boolean() {
        let manifest = fake_manifest(Some("original.1"), 42);
        let mut body = serde_json::json!({"name":"chimaera","pid":42,"hostname":"host","version":"0.0.1","build":"original.1"});
        let parse = |value: &serde_json::Value| {
            parse_daemon_extension(&serde_json::to_vec(value).unwrap(), &manifest)
        };
        assert_eq!(parse(&body).unwrap(), None);
        body["daemon_extension"] = serde_json::json!(false);
        assert_eq!(parse(&body).unwrap(), Some(false));
        body["daemon_extension"] = serde_json::json!(true);
        assert_eq!(parse(&body).unwrap(), Some(true));
        for (key, value) in [
            ("pid", serde_json::json!(43)),
            ("hostname", serde_json::json!("other")),
            ("build", serde_json::json!("successor.2")),
            ("version", serde_json::json!("9.0.0")),
            ("name", serde_json::json!("other")),
            ("daemon_extension", serde_json::json!(null)),
            ("daemon_extension", serde_json::json!("true")),
        ] {
            let mut changed = body.clone();
            changed[key] = value;
            assert!(parse(&changed).is_err(), "refused {key}");
        }
        assert!(parse_daemon_extension(b"{", &manifest).is_err());
        assert!(parse_daemon_extension(&vec![b' '; 16 * 1024 + 1], &manifest).is_err());
    }

    #[tokio::test]
    async fn selected_cli_without_artifact_refuses_all_deployment_branches_without_effects() {
        for (manifest, alive, update, cluster) in [
            (Some(fake_manifest(None, 42)), true, false, false),
            (
                Some(fake_manifest(Some(chimaera_core::BUILD_ID), 42)),
                true,
                true,
                false,
            ),
            (None, false, false, false),
            (None, false, true, false),
            (None, false, true, true),
        ] {
            let fake = FakeOps {
                probe_manifest: manifest,
                alive,
                sessions: Some(0),
                scheduler: if cluster {
                    Scheduler::Slurm
                } else {
                    Scheduler::None
                },
                ..FakeOps::base()
            };
            let opts = ConnectOpts {
                deployment_source: DeploymentSource::ExplicitBinary,
                update_daemon: update,
                ..Default::default()
            };
            let error = resolve_daemon(&fake, "host", &opts, &|_| {})
                .await
                .unwrap_err();
            assert!(error.to_string().contains("explicit compatible --binary"));
            assert!(
                fake.calls()
                    .iter()
                    .all(|call| matches!(call, Call::RemoteProbe | Call::RemoteSessionsCount)),
                "{:?}",
                fake.calls()
            );
        }
    }

    #[tokio::test]
    async fn selected_cli_reconnect_and_explicit_artifact_keep_original_effect_order() {
        let matching = FakeOps {
            probe_manifest: Some(fake_manifest(Some(chimaera_core::BUILD_ID), 42)),
            alive: true,
            sessions: Some(3),
            ..FakeOps::base()
        };
        let opts = ConnectOpts {
            deployment_source: DeploymentSource::ExplicitBinary,
            ..Default::default()
        };
        let (manifest, outdated, _) = resolve_daemon(&matching, "host", &opts, &|_| {})
            .await
            .unwrap();
        assert_eq!(manifest.pid, 42);
        assert!(!outdated);
        assert_eq!(matching.calls(), vec![Call::RemoteProbe]);
        let replacement = FakeOps {
            probe_manifest: Some(fake_manifest(None, 42)),
            alive: true,
            sessions: Some(0),
            ..FakeOps::base()
        };
        let opts = ConnectOpts {
            deployment_source: DeploymentSource::ExplicitBinary,
            binary: Some(PathBuf::from("/explicit-selected-artifact")),
            ..Default::default()
        };
        resolve_daemon(&replacement, "host", &opts, &|_| {})
            .await
            .unwrap();
        assert_eq!(
            replacement.calls(),
            vec![
                Call::RemoteProbe,
                Call::RemoteSessionsCount,
                Call::ResolveLocalBinary,
                Call::StopRemote,
                Call::DeployBinary,
                Call::StartRemote
            ]
        );
        let initial = FakeOps::base();
        resolve_daemon(&initial, "host", &opts, &|_| {})
            .await
            .unwrap();
        assert_eq!(
            initial.calls(),
            vec![
                Call::RemoteProbe,
                Call::ResolveLocalBinary,
                Call::DeployBinary,
                Call::StartRemote
            ]
        );
    }

    #[tokio::test]
    async fn public_implicit_replacement_refuses_selected_or_unconfirmed_remote_composition() {
        for unconfirmed in [false, true] {
            let fake = FakeOps {
                probe_manifest: Some(fake_manifest(None, 42)),
                alive: true,
                sessions: Some(0),
                daemon_extension: Some(true),
                composition_unconfirmed: unconfirmed,
                ..FakeOps::base()
            };
            assert!(
                resolve_daemon(&fake, "host", &ConnectOpts::default(), &|_| {})
                    .await
                    .is_err()
            );
            assert_eq!(
                fake.calls(),
                vec![
                    Call::RemoteProbe,
                    Call::RemoteSessionsCount,
                    Call::RemoteDaemonExtension
                ]
            );
        }
        for composition in [None, Some(false)] {
            let compatible = FakeOps {
                probe_manifest: Some(fake_manifest(None, 42)),
                alive: true,
                sessions: Some(0),
                daemon_extension: composition,
                ..FakeOps::base()
            };
            resolve_daemon(&compatible, "host", &ConnectOpts::default(), &|_| {})
                .await
                .unwrap();
            assert_eq!(
                compatible.calls(),
                vec![
                    Call::RemoteProbe,
                    Call::RemoteSessionsCount,
                    Call::RemoteDaemonExtension,
                    Call::ResolveLocalBinary,
                    Call::StopRemote,
                    Call::DeployBinary,
                    Call::StartRemote
                ]
            );
        }
        let fake = FakeOps {
            probe_manifest: Some(fake_manifest(None, 42)),
            alive: true,
            sessions: Some(0),
            daemon_extension: Some(true),
            ..FakeOps::base()
        };
        let opts = ConnectOpts {
            binary: Some(PathBuf::from("/user-explicit-override")),
            ..Default::default()
        };
        resolve_daemon(&fake, "host", &opts, &|_| {}).await.unwrap();
        assert_eq!(
            fake.calls(),
            vec![
                Call::RemoteProbe,
                Call::RemoteSessionsCount,
                Call::ResolveLocalBinary,
                Call::StopRemote,
                Call::DeployBinary,
                Call::StartRemote
            ]
        );
    }

    /// Update: a build mismatch with a provably idle daemon (sessions == 0)
    /// replaces it — and CRITICALLY resolves the replacement binary BEFORE
    /// stopping the old daemon, so a failed fetch never strands the host.
    /// Returns the freshly started daemon's manifest, not outdated.
    #[tokio::test]
    async fn resolve_daemon_update_resolves_binary_before_stop() {
        let fake = FakeOps {
            // No build id = ancient = mismatch against any real BUILD_ID.
            probe_manifest: Some(fake_manifest(None, 42)),
            alive: true,
            sessions: Some(0),
            resolved_bin: PathBuf::from("/tmp/chimaera"),
            start_manifest: fake_manifest(Some(chimaera_core::BUILD_ID), 999),
            ..FakeOps::base()
        };
        let ((manifest, outdated, live), phases) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 999, "returns the freshly started daemon");
        assert!(!outdated);
        assert_eq!(live, None);
        assert_eq!(
            fake.calls(),
            vec![
                Call::RemoteProbe,
                Call::RemoteSessionsCount,
                Call::RemoteDaemonExtension,
                Call::ResolveLocalBinary,
                Call::StopRemote,
                Call::DeployBinary,
                Call::StartRemote,
            ],
            "resolve-local-binary must precede stop-remote"
        );
        assert_eq!(phases, vec!["probing", "updating", "starting"]);
    }

    /// Update via `--update-daemon`: force replaces even a matching build with
    /// live sessions, still fetching the count first (for the log line). Same
    /// resolve-before-stop ordering as the mismatch path.
    #[tokio::test]
    async fn resolve_daemon_force_update_ignores_live_sessions() {
        let fake = FakeOps {
            probe_manifest: Some(fake_manifest(Some(chimaera_core::BUILD_ID), 42)),
            alive: true,
            sessions: Some(5),
            resolved_bin: PathBuf::from("/tmp/chimaera"),
            start_manifest: fake_manifest(Some(chimaera_core::BUILD_ID), 999),
            ..FakeOps::base()
        };
        let ((manifest, outdated, _live), phases) = run_resolve(&fake, true).await;
        assert_eq!(manifest.pid, 999);
        assert!(!outdated);
        assert_eq!(
            fake.calls(),
            vec![
                Call::RemoteProbe,
                Call::RemoteSessionsCount,
                Call::RemoteDaemonExtension,
                Call::ResolveLocalBinary,
                Call::StopRemote,
                Call::DeployBinary,
                Call::StartRemote,
            ]
        );
        assert_eq!(phases, vec!["probing", "updating", "starting"]);
    }

    /// Update whose stop went unconfirmed: the start-wait hands back the OLD
    /// daemon's manifest (same pid, old build) because the new one refused to
    /// start beside it. That is a working daemon, so connect — but report it
    /// as outdated with its live count, never as a completed update.
    #[tokio::test]
    async fn resolve_daemon_update_that_did_not_replace_reports_outdated() {
        let fake = FakeOps {
            probe_manifest: Some(fake_manifest(Some("old.1"), 42)),
            alive: true,
            sessions: Some(0),
            resolved_bin: PathBuf::from("/tmp/chimaera"),
            start_manifest: fake_manifest(Some("old.1"), 42),
            ..FakeOps::base()
        };
        let ((manifest, outdated, live), phases) = run_resolve(&fake, false).await;
        assert_eq!(
            manifest.pid, 42,
            "connects to the daemon that is actually serving"
        );
        assert!(
            outdated,
            "an update that left the old build serving is not a success"
        );
        assert_eq!(live, Some(0));
        assert_eq!(phases, vec!["probing", "updating", "starting"]);
    }

    /// ConnectOutdated: a build mismatch with live sessions and no forced
    /// update attaches to the old daemon as-is — surfacing the mismatch and the
    /// live count, with no stop/deploy/start and only the probe phase.
    #[tokio::test]
    async fn resolve_daemon_connects_outdated_with_live_sessions() {
        let fake = FakeOps {
            probe_manifest: Some(fake_manifest(None, 42)),
            alive: true,
            sessions: Some(2),
            resolved_bin: PathBuf::from("/unused"),
            start_manifest: fake_manifest(Some(chimaera_core::BUILD_ID), 999),
            ..FakeOps::base()
        };
        let ((manifest, outdated, live), phases) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 42, "attaches to the old daemon");
        assert!(outdated);
        assert_eq!(live, Some(2));
        assert_eq!(
            fake.calls(),
            vec![Call::RemoteProbe, Call::RemoteSessionsCount]
        );
        assert_eq!(phases, vec!["probing"]);
    }

    /// Fresh start (no manifest): the one-exec probe reports nothing running,
    /// then ensure-binary and start a new daemon.
    #[tokio::test]
    async fn resolve_daemon_fresh_start_when_no_manifest() {
        let fake = FakeOps {
            probe_manifest: None,
            // Unused here: with no manifest the fake probe reports nothing.
            alive: false,
            sessions: None,
            resolved_bin: PathBuf::from("/unused"),
            start_manifest: fake_manifest(Some(chimaera_core::BUILD_ID), 999),
            ..FakeOps::base()
        };
        let ((manifest, outdated, live), phases) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 999);
        assert!(!outdated);
        assert_eq!(live, None);
        assert_eq!(
            fake.calls(),
            vec![
                Call::RemoteProbe,
                Call::EnsureRemoteBinary,
                Call::StartRemote
            ]
        );
        assert_eq!(phases, vec!["probing", "starting"]);
    }

    /// Fresh start (stale manifest, dead pid): the probe returns a manifest
    /// whose pid is dead, which falls through to the same fresh-start path.
    #[tokio::test]
    async fn resolve_daemon_fresh_start_when_manifest_pid_dead() {
        let fake = FakeOps {
            probe_manifest: Some(fake_manifest(Some(chimaera_core::BUILD_ID), 42)),
            alive: false,
            sessions: None,
            resolved_bin: PathBuf::from("/unused"),
            start_manifest: fake_manifest(Some(chimaera_core::BUILD_ID), 999),
            ..FakeOps::base()
        };
        let ((manifest, _outdated, _live), phases) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 999);
        assert_eq!(
            fake.calls(),
            vec![
                Call::RemoteProbe,
                Call::EnsureRemoteBinary,
                Call::StartRemote,
            ]
        );
        assert_eq!(phases, vec!["probing", "starting"]);
    }

    // --- round-robin login nodes ------------------------------------------
    //
    // A pool alias ("login.cluster.edu" → ln01..lnNN) over one shared $HOME:
    // the daemon runs on ln01, a fresh ControlMaster landed on ln02. Before
    // the node was compared, the ln02 probe's `kill -0` (an unrelated process
    // table) read "dead" and connect started a second daemon on ln02 — over
    // the same manifest and session ledger, orphaning ln01's daemon and
    // resuming its sessions a second time.

    const LN01: &str = "ln01.cluster.edu";
    const LN02: &str = "ln02.cluster.edu";
    /// Where the cluster resolves a routed node's name; the fake's "this
    /// machine" agrees unless a test says otherwise.
    const CLUSTER_ADDR: std::net::IpAddr =
        std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 1));

    /// The daemon's manifest, written on `node`.
    fn manifest_on(node: &str, build: Option<&str>, pid: u32) -> Manifest {
        Manifest {
            hostname: node.to_string(),
            ..fake_manifest(build, pid)
        }
    }

    /// `manifest` as seen by a probe that ran on `node`.
    fn seen_from(
        node: &str,
        manifest: &Manifest,
        alive: bool,
        resolves: Option<bool>,
    ) -> anyhow::Result<ProbeRun> {
        Ok(ProbeRun::Ran(Some(Probe {
            manifest: manifest.clone(),
            node: node.to_string(),
            alive,
            manifest_node_resolves: resolves,
            manifest_node_addrs: match resolves {
                Some(true) => vec![CLUSTER_ADDR],
                _ => Vec::new(),
            },
        })))
    }

    fn ssh_failed(stderr: &str) -> anyhow::Result<ProbeRun> {
        Ok(ProbeRun::Failed(ProbeFailure {
            status: "exit status: 255".to_string(),
            stderr: stderr.to_string(),
        }))
    }

    /// The pool alias lands on ln02; `node_route` answers for every routed
    /// probe.
    fn pool(
        daemon: Manifest,
        landed_kill0: bool,
        resolves: Option<bool>,
        node_route: impl Fn(&Route, &Manifest) -> anyhow::Result<ProbeRun> + 'static,
    ) -> FakeOps {
        FakeOps {
            probe: Some(Box::new(move |route| match route {
                Route::Alias => seen_from(LN02, &daemon, landed_kill0, resolves),
                route => node_route(route, &daemon),
            })),
            ..FakeOps::base()
        }
    }

    /// THE bug: a manifest from another login node is never judged dead by
    /// the node the connection landed on. The routed probe on ln01 finds the
    /// daemon alive, so connect attaches to it — every later op (here none;
    /// see the update case) runs along the ln01 route, and nothing starts.
    #[tokio::test]
    async fn a_manifest_from_another_login_node_is_not_judged_from_this_one() {
        let fake = pool(
            manifest_on(LN01, Some(chimaera_core::BUILD_ID), 42),
            // ln02 has no pid 42 (or an unrelated one): the old verdict.
            false,
            Some(true),
            |route, m| match route {
                Route::Node(n) if n == LN01 => seen_from(LN01, m, true, None),
                other => panic!("unexpected route {other:?}"),
            },
        );
        let ((manifest, outdated, _), phases) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 42, "attaches to ln01's daemon");
        assert!(!outdated);
        assert_eq!(
            fake.routed(),
            vec![
                (Call::RemoteProbe, Route::Alias),
                (Call::RemoteProbe, Route::Node(LN01.into())),
            ],
            "no fresh start"
        );
        assert_eq!(
            *fake.route.borrow(),
            Route::Node(LN01.into()),
            "left routed"
        );
        assert_eq!(phases, vec!["probing", "routing"]);
    }

    /// Every op after the routing decision runs on the daemon's node: an
    /// idle outdated daemon on ln01 is counted, stopped, and restarted THERE.
    #[tokio::test]
    async fn a_routed_update_counts_stops_and_restarts_on_the_daemons_node() {
        let fake = FakeOps {
            sessions: Some(0),
            resolved_bin: PathBuf::from("/tmp/chimaera"),
            start_manifest: manifest_on(LN01, Some(chimaera_core::BUILD_ID), 999),
            ..pool(
                manifest_on(LN01, Some("old.1"), 42),
                false,
                Some(true),
                |_, m| seen_from(LN01, m, true, None),
            )
        };
        let ((manifest, outdated, _), phases) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 999);
        assert!(!outdated);
        let ln01 = Route::Node(LN01.into());
        assert_eq!(
            fake.routed(),
            vec![
                (Call::RemoteProbe, Route::Alias),
                (Call::RemoteProbe, ln01.clone()),
                (Call::RemoteSessionsCount, ln01.clone()),
                (Call::RemoteDaemonExtension, ln01.clone()),
                (Call::ResolveLocalBinary, ln01.clone()),
                (Call::StopRemote, ln01.clone()),
                (Call::DeployBinary, ln01.clone()),
                (Call::StartRemote, ln01),
            ]
        );
        assert_eq!(phases, vec!["probing", "routing", "updating", "starting"]);
    }

    /// Provably dead ON its own node: the verdict now counts, and the fresh
    /// daemon starts on that node (the route stays there).
    #[tokio::test]
    async fn a_daemon_dead_on_its_own_node_is_replaced_there() {
        let fake = pool(
            manifest_on(LN01, Some(chimaera_core::BUILD_ID), 42),
            true, // ln02 has an unrelated pid 42 alive — irrelevant
            Some(true),
            |_, m| seen_from(LN01, m, false, None),
        );
        let ((manifest, ..), _) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 999);
        let ln01 = Route::Node(LN01.into());
        assert_eq!(
            fake.routed(),
            vec![
                (Call::RemoteProbe, Route::Alias),
                (Call::RemoteProbe, ln01.clone()),
                (Call::EnsureRemoteBinary, ln01.clone()),
                (Call::StartRemote, ln01),
            ]
        );
    }

    /// Unreachable is not dead: when ln01 can't be reached (here: the auth
    /// prompt was refused) nothing is started, the error says what is where
    /// and why, and no second route is tried — it would prompt again.
    #[tokio::test]
    async fn an_unreachable_daemon_node_is_an_error_never_a_fresh_start() {
        let fake = pool(
            manifest_on(LN01, Some(chimaera_core::BUILD_ID), 42),
            false,
            Some(true),
            |_, _| {
                ssh_failed(
                    "mkjellbe@ln01.cluster.edu: Permission denied (gssapi-with-mic,password).",
                )
            },
        );
        let (out, phases) = try_resolve(&fake, false).await;
        let err = format!("{:#}", out.expect_err("must not resolve"));
        assert!(
            err.contains("runs on login node ln01.cluster.edu (pid 42)"),
            "{err}"
        );
        assert!(err.contains("landed on ln02.cluster.edu"), "{err}");
        assert!(err.contains("Permission denied"), "{err}");
        assert!(err.contains("Nothing was started"), "{err}");
        assert!(
            !err.contains(".."),
            "ssh's own period is not doubled: {err}"
        );
        assert_eq!(
            fake.routed(),
            vec![
                (Call::RemoteProbe, Route::Alias),
                (Call::RemoteProbe, Route::Node(LN01.into())),
            ]
        );
        assert_eq!(*fake.route.borrow(), Route::Alias, "no half-learned route");
        assert_eq!(phases, vec!["probing", "routing"]);
    }

    /// A direct dial that never reached ln01's sshd (the name doesn't
    /// resolve here, a firewall) falls back to reaching it from inside the
    /// cluster, through the alias's own master.
    #[tokio::test]
    async fn a_node_this_machine_cannot_dial_is_reached_through_the_alias() {
        let fake = pool(
            manifest_on(LN01, Some(chimaera_core::BUILD_ID), 42),
            false,
            Some(true),
            |route, m| match route {
                Route::Node(_) => {
                    ssh_failed("ssh: connect to host ln01.cluster.edu port 22: Operation timed out")
                }
                Route::NodeViaAlias(_) => seen_from(LN01, m, true, None),
                Route::Alias => unreachable!(),
            },
        );
        let ((manifest, ..), _) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 42);
        assert_eq!(
            fake.routed(),
            vec![
                (Call::RemoteProbe, Route::Alias),
                (Call::RemoteProbe, Route::Node(LN01.into())),
                (Call::RemoteProbe, Route::NodeViaAlias(LN01.into())),
            ]
        );
        assert_eq!(*fake.route.borrow(), Route::NodeViaAlias(LN01.into()));
    }

    /// Both routes failing is the same honest error.
    #[tokio::test]
    async fn no_route_to_the_daemons_node_is_an_error() {
        let fake = pool(
            manifest_on(LN01, Some(chimaera_core::BUILD_ID), 42),
            false,
            Some(true),
            |_, _| {
                ssh_failed(
                    "ssh: Could not resolve hostname ln01.cluster.edu: nodename nor servname provided",
                )
            },
        );
        let (out, _) = try_resolve(&fake, false).await;
        assert!(format!("{:#}", out.unwrap_err()).contains("could not reach ln01.cluster.edu"));
        assert_eq!(
            fake.calls(),
            vec![Call::RemoteProbe; 3],
            "alias + both routes, no start"
        );
        assert_eq!(*fake.route.borrow(), Route::Alias);
    }

    /// A routed probe that finds no manifest is confirmed from the node we
    /// landed on: really gone → a fresh start there; still there → the dial
    /// reached a machine that doesn't share this home, and nothing starts.
    #[tokio::test]
    async fn a_manifest_missing_over_the_route_is_confirmed_where_we_landed() {
        let gone = FakeOps {
            probe: Some(Box::new({
                let seen = std::cell::Cell::new(0);
                move |route| {
                    seen.set(seen.get() + 1);
                    match (route, seen.get()) {
                        (Route::Alias, 1) => seen_from(
                            LN02,
                            &manifest_on(LN01, Some(chimaera_core::BUILD_ID), 42),
                            false,
                            Some(true),
                        ),
                        (Route::Node(_), _) | (Route::Alias, _) => Ok(ProbeRun::Ran(None)),
                        (other, _) => panic!("dialed {other:?}"),
                    }
                }
            })),
            ..FakeOps::base()
        };
        let ((manifest, ..), _) = run_resolve(&gone, false).await;
        assert_eq!(manifest.pid, 999, "stopped meanwhile: a fresh start");
        assert_eq!(
            gone.routed(),
            vec![
                (Call::RemoteProbe, Route::Alias),
                (Call::RemoteProbe, Route::Node(LN01.into())),
                (Call::RemoteProbe, Route::Alias),
                (Call::EnsureRemoteBinary, Route::Alias),
                (Call::StartRemote, Route::Alias),
            ]
        );

        let elsewhere = pool(
            manifest_on(LN01, Some(chimaera_core::BUILD_ID), 42),
            false,
            Some(true),
            |_, _| Ok(ProbeRun::Ran(None)),
        );
        let (out, _) = try_resolve(&elsewhere, false).await;
        assert!(
            format!("{:#}", out.unwrap_err()).contains("doesn't see host's manifest"),
            "a machine without the shared home is never where a daemon starts"
        );
        assert_eq!(elsewhere.calls(), vec![Call::RemoteProbe; 3]);
    }

    /// A name this machine resolves somewhere else than the cluster does is
    /// never dialed from here (a search domain can turn it into an unrelated
    /// host — and hand that host the password); it only travels inside the
    /// cluster.
    #[tokio::test]
    async fn a_node_this_machine_resolves_elsewhere_is_only_reached_inside_the_cluster() {
        let fake = FakeOps {
            local: vec!["203.0.113.9".parse().unwrap()],
            ..pool(
                manifest_on("login1", Some(chimaera_core::BUILD_ID), 42),
                false,
                Some(true),
                |route, m| match route {
                    Route::NodeViaAlias(n) if n == "login1" => seen_from("login1", m, true, None),
                    other => panic!("dialed {other:?}"),
                },
            )
        };
        let ((manifest, ..), _) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 42);
        assert_eq!(*fake.route.borrow(), Route::NodeViaAlias("login1".into()));
    }

    /// The user's ssh config can send the direct dial anywhere (a
    /// ProxyCommand with a fixed target ignores `HostName`). Landing back on
    /// the node we started from proves nothing about the name there — the
    /// in-cluster route decides, and here finds the daemon alive on ln01.
    #[tokio::test]
    async fn a_direct_dial_redirected_back_here_is_not_a_renamed_host() {
        let fake = pool(
            manifest_on(LN01, Some(chimaera_core::BUILD_ID), 42),
            false,
            Some(true),
            |route, m| match route {
                Route::Node(_) => seen_from(LN02, m, false, Some(true)),
                Route::NodeViaAlias(_) => seen_from(LN01, m, true, None),
                Route::Alias => unreachable!(),
            },
        );
        let ((manifest, ..), _) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 42, "attaches to ln01's live daemon");
        assert_eq!(
            fake.calls(),
            vec![Call::RemoteProbe; 3],
            "no fresh start from ln02's verdict"
        );
        assert_eq!(*fake.route.borrow(), Route::NodeViaAlias(LN01.into()));
    }

    /// The node's name no longer exists in the cluster (decommissioned or
    /// renamed): its daemon went with it, so a fresh start on the landed node
    /// is safe — and the only case a foreign manifest yields one unrouted.
    #[tokio::test]
    async fn a_daemon_whose_node_no_longer_exists_is_replaced_where_we_landed() {
        let fake = pool(
            manifest_on(LN01, Some(chimaera_core::BUILD_ID), 42),
            false,
            Some(false),
            |route, _| panic!("dialed {route:?} for a node that doesn't resolve"),
        );
        let ((manifest, ..), phases) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 999);
        assert_eq!(
            fake.routed(),
            vec![
                (Call::RemoteProbe, Route::Alias),
                (Call::EnsureRemoteBinary, Route::Alias),
                (Call::StartRemote, Route::Alias),
            ]
        );
        assert_eq!(phases, vec!["probing", "starting"]);
    }

    /// A renamed host: the manifest's old name still leads to the node we
    /// landed on, so the landed probe's verdict was local after all.
    #[tokio::test]
    async fn a_renamed_host_keeps_its_local_verdict() {
        let fake = pool(
            manifest_on("old-name.cluster.edu", Some(chimaera_core::BUILD_ID), 42),
            true,
            Some(true),
            |_, m| seen_from(LN02, m, true, None),
        );
        let ((manifest, ..), _) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 42);
        assert_eq!(*fake.route.borrow(), Route::Alias, "one node — no route");
        assert_eq!(
            fake.routed().last().map(|(_, route)| route.clone()),
            Some(Route::NodeViaAlias("old-name.cluster.edu".into())),
            "only the in-cluster route may prove a rename"
        );
    }

    /// A manifest's node name is data from a remote disk: anything but a
    /// plain host name is refused before it reaches ssh's argv or a
    /// ProxyCommand's shell.
    #[tokio::test]
    async fn a_hostile_node_name_is_never_dialed() {
        let fake = pool(
            manifest_on("ln01;touch${IFS}/tmp/x", Some(chimaera_core::BUILD_ID), 42),
            false,
            Some(true),
            |route, _| panic!("dialed {route:?}"),
        );
        let (out, _) = try_resolve(&fake, false).await;
        assert!(format!("{:#}", out.unwrap_err()).contains("not a plain host name"));
        assert_eq!(fake.calls(), vec![Call::RemoteProbe]);
    }

    /// A route learned by an earlier connect goes first — straight to the
    /// daemon's node, no detour through wherever the pool lands.
    #[tokio::test]
    async fn a_learned_route_is_probed_first() {
        let fake = pool(
            manifest_on(LN01, Some(chimaera_core::BUILD_ID), 42),
            false,
            Some(true),
            |_, m| seen_from(LN01, m, true, None),
        );
        *fake.route.borrow_mut() = Route::Node(LN01.into());
        let ((manifest, ..), phases) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 42);
        assert_eq!(
            fake.routed(),
            vec![(Call::RemoteProbe, Route::Node(LN01.into()))]
        );
        assert_eq!(phases, vec!["probing"]);
    }

    /// A learned route that no longer answers starts over from the alias —
    /// here the daemon has since been started on the node the alias lands on.
    #[tokio::test]
    async fn a_stale_learned_route_starts_over() {
        let fake = FakeOps {
            probe: Some(Box::new(|route| match route {
                Route::Node(_) => {
                    ssh_failed("ssh: connect to host ln01.cluster.edu port 22: No route to host")
                }
                _ => seen_from(
                    LN02,
                    &manifest_on(LN02, Some(chimaera_core::BUILD_ID), 77),
                    true,
                    None,
                ),
            })),
            ..FakeOps::base()
        };
        *fake.route.borrow_mut() = Route::Node(LN01.into());
        let ((manifest, ..), _) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 77);
        assert_eq!(
            fake.routed(),
            vec![
                (Call::RemoteProbe, Route::Node(LN01.into())),
                (Call::RemoteProbe, Route::Alias),
            ]
        );
        assert_eq!(*fake.route.borrow(), Route::Alias);
    }

    /// The route's ssh options: a node route overrides only `HostName`; the
    /// via-alias route adds a `-W` first leg over the alias's own master,
    /// with its `%` escaped for the outer ssh's expansion.
    #[test]
    fn route_options_pin_the_node() {
        assert!(route_opts("pool", &Route::Alias).is_empty());
        assert_eq!(
            route_opts("pool", &Route::Node(LN01.into())),
            vec!["-o".to_string(), format!("HostName={LN01}")]
        );
        let via = route_opts("pool", &Route::NodeViaAlias(LN01.into()));
        assert_eq!(via[..2], ["-o".to_string(), format!("HostName={LN01}")]);
        assert_eq!(via[2], "-o");
        let proxy = via[3]
            .strip_prefix("ProxyCommand=")
            .expect("a ProxyCommand");
        assert!(proxy.starts_with("ssh "), "{proxy}");
        assert!(proxy.ends_with(" -W %h:%p 'pool'"), "{proxy}");
        assert!(
            proxy.contains("%%C"),
            "ControlPath token survives the outer expansion: {proxy}"
        );
        assert!(
            !proxy.contains("HostName"),
            "the first leg is the alias's own master: {proxy}"
        );
        assert!(master_proxy_command("pool", Some(LN01)).contains(&format!("-o HostName={LN01}")));
        // The alias reaches a local shell and ssh's own `%` expansion.
        assert!(master_proxy_command("a;b%h'c", None).ends_with(r"-W %h:%p 'a;b%%h'\''c'"));
    }

    #[test]
    fn node_names_are_validated_before_ssh_sees_them() {
        for ok in [LN01, "login1", "login-a.cluster.example", "node_7"] {
            assert!(valid_node_name(ok), "{ok}");
        }
        for bad in [
            "",
            "-oProxyCommand=x",
            ".hidden",
            "a b",
            "a;b",
            "$(id)",
            "a%C",
            "a\"b",
        ] {
            assert!(!valid_node_name(bad), "{bad}");
        }
        let (a, b): (std::net::IpAddr, std::net::IpAddr) = (
            "10.0.0.1".parse().unwrap(),
            "171.67.99.169".parse().unwrap(),
        );
        assert_eq!(
            routes_to(LN01, &[a, b], &[b]),
            vec![Route::Node(LN01.into()), Route::NodeViaAlias(LN01.into())],
            "the direct dial reaches an address the cluster means"
        );
        assert_eq!(
            routes_to("sh04-ln03", &[a], &[b]),
            vec![Route::NodeViaAlias("sh04-ln03".into())],
            "resolved elsewhere here (a cluster's bare names: 10.x inside, public outside)"
        );
        assert_eq!(
            routes_to(LN01, &[a], &[]),
            vec![Route::NodeViaAlias(LN01.into())]
        );
        assert_eq!(
            routes_to(LN01, &[], &[a]),
            vec![Route::NodeViaAlias(LN01.into())]
        );
    }

    #[test]
    fn only_network_level_failures_try_another_route() {
        let failure = |stderr: &str| ProbeFailure {
            status: "exit status: 255".to_string(),
            stderr: stderr.to_string(),
        };
        for net in [
            "ssh: Could not resolve hostname ln01: Name or service not known",
            "ssh: connect to host ln01 port 22: Connection timed out",
            "ssh: connect to host ln01 port 22: Connection refused",
            "kex_exchange_identification: read: Connection reset by peer",
            "Connection timed out during banner exchange",
        ] {
            assert!(failure(net).network_level(), "{net}");
        }
        for auth in [
            "u@ln01: Permission denied (gssapi-with-mic,password).",
            "Host key verification failed.",
            "Received disconnect from 1.2.3.4 port 22:2: Too many authentication failures",
        ] {
            assert!(!failure(auth).network_level(), "{auth}");
        }
        assert_eq!(
            failure("Welcome to the cluster\nu@ln01: Permission denied (password).\n").to_string(),
            "u@ln01: Permission denied (password).",
            "ssh's own last line, not the banner"
        );
    }
}
