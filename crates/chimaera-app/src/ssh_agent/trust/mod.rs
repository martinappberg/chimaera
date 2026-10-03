//! First-use native trust is an original-Connect operation. The transient
//! candidate becomes permanent only after a positive native cryptographic
//! receipt and account/config/Attempt revalidation. No session command runs.
mod bridge;
mod mux;
mod process;
mod storage;
use super::{
    lifecycle::NativePromptGuard,
    route::{self, RouteSelection},
    selection::{self, NativeIdentity, SelectionFailure},
};
use chimaera_link::{
    SshRoute, SshRouteAuthLeg, SshRouteGrantRequest, SshRouteMethod, SshRouteMode,
};
use std::{
    future::Future,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use tokio::{process::Command, sync::Mutex, time::Instant};

const SCOPE_FRAME: &str = "chimaera-askpass-scope-v1";
type Result<T> = std::result::Result<T, SelectionFailure>;
type Prompt = dyn Fn(String, NativePromptGuard) -> Pin<Box<dyn Future<Output = Option<String>> + Send>>
    + Send
    + Sync;
/// Constructed only by the native explicit-Connect caller; it is not a wire or
/// remote callback. The retained account mutex covers the real trust append.
#[derive(Clone)]
pub(crate) struct Owner {
    pub(crate) alias: String,
    pub(crate) guard: NativePromptGuard,
    pub(crate) account: Arc<Mutex<()>>,
    pub(crate) current: Arc<dyn Fn() -> bool + Send + Sync>,
    pub(crate) prompt: Arc<Prompt>,
}
struct Directory(PathBuf);
impl Directory {
    fn create() -> Result<Self> {
        let base = std::fs::canonicalize("/tmp").map_err(|_| SelectionFailure::Unavailable)?;
        let path = base.join(format!(
            "cx-native-trust-{}",
            &chimaera_core::generate_token()[..24]
        ));
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| SelectionFailure::Unavailable)?;
        Ok(Self(path))
    }
    fn file(&self, name: &str, bytes: &[u8], mode: u32) -> Result<PathBuf> {
        let path = self.0.join(name);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&path)
            .map_err(|_| SelectionFailure::Unavailable)?;
        file.write_all(bytes)
            .map_err(|_| SelectionFailure::Unavailable)?;
        Ok(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Master {
    process: process::Process,
    _bridge: bridge::Bridge,
    directory: Directory,
    destination: chimaera_link::SshAuthDestination,
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn method(value: SshRouteMethod) -> &'static str {
    match value {
        SshRouteMethod::Publickey => "publickey",
        SshRouteMethod::KeyboardInteractive => "keyboard-interactive",
        SshRouteMethod::Password => "password",
    }
}
fn public_record(lookup: &str, key: &chimaera_link::SshAuthHostKey) -> Result<String> {
    let bytes = chimaera_link::decode_packet(&key.key, chimaera_link::SSH_AUTH_KEY_MAX)
        .map_err(|_| SelectionFailure::Unavailable)?;
    let public = ssh_key::PublicKey::from_bytes(&bytes)
        .map_err(|_| SelectionFailure::Unavailable)?
        .to_openssh()
        .map_err(|_| SelectionFailure::Unavailable)?;
    Ok(format!(
        "{}{lookup} {public}\n",
        if key.is_ca { "@cert-authority " } else { "" }
    ))
}
fn command(
    directory: &Directory,
    known: &Path,
    details: &selection::ProbeDetails,
    leg: &SshRouteAuthLeg,
    empty: bool,
    predecessor: Option<&Master>,
) -> Result<Command> {
    let exe = std::env::current_exe().map_err(|_| SelectionFailure::Unavailable)?;
    let exe = exe
        .to_str()
        .filter(|exe| !exe.chars().any(|c| c.is_control() || c == '%'))
        .ok_or(SelectionFailure::Unavailable)?;
    let shim = directory.file(
        "askpass.sh",
        format!("#!/bin/sh\nexec {} --askpass \"$@\"\n", quote(exe)).as_bytes(),
        0o700,
    )?;
    let mut command = Command::new("/usr/bin/ssh");
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .env("SSH_ASKPASS", shim)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("DISPLAY", "native:0")
        .env("CHIMAERA_ASKPASS_SOCK", directory.0.join("askpass"))
        .env(chimaera_remote::ASKPASS_ALIAS_ENV, "");
    command.args(["-F", "/dev/null", "-N", "-T"]);
    if !empty {
        command.arg("-M").arg("-S").arg(directory.0.join("master"));
    }
    let policy = &details.policy;
    for (name, value) in [
        ("ControlPersist", "no".into()),
        ("ProxyJump", "none".into()),
        ("ForwardAgent", "no".into()),
        ("IdentityFile", "none".into()),
        ("CertificateFile", "none".into()),
        ("IdentitiesOnly", "no".into()),
        (
            "StrictHostKeyChecking",
            if leg.host_keys.is_empty() {
                "ask".into()
            } else {
                "yes".into()
            },
        ),
        ("HashKnownHosts", "no".into()),
        ("GlobalKnownHostsFile", "/dev/null".into()),
        ("UserKnownHostsFile", known.to_string_lossy().into_owned()),
        ("KnownHostsCommand", "none".into()),
        ("VerifyHostKeyDNS", "no".into()),
        ("UpdateHostKeys", "no".into()),
        ("HostKeyAlias", details.lookup.clone()),
        ("GSSAPIAuthentication", "no".into()),
        ("HostbasedAuthentication", "no".into()),
        (
            "PubkeyAuthentication",
            if leg.mode == SshRouteMode::Key {
                "host-bound".into()
            } else {
                "no".into()
            },
        ),
        (
            "PasswordAuthentication",
            if !empty && policy.methods.contains(&SshRouteMethod::Password) {
                "yes".into()
            } else {
                "no".into()
            },
        ),
        (
            "KbdInteractiveAuthentication",
            if !empty
                && policy
                    .methods
                    .contains(&SshRouteMethod::KeyboardInteractive)
            {
                "yes".into()
            } else {
                "no".into()
            },
        ),
        (
            "PreferredAuthentications",
            if empty {
                "publickey".into()
            } else {
                policy
                    .methods
                    .iter()
                    .map(|m| method(*m))
                    .collect::<Vec<_>>()
                    .join(",")
            },
        ),
        ("HostKeyAlgorithms", policy.host_key_algorithms.join(",")),
        (
            "CASignatureAlgorithms",
            policy.ca_signature_algorithms.join(","),
        ),
        (
            "PubkeyAcceptedAlgorithms",
            policy.pubkey_accepted_algorithms.join(","),
        ),
        ("KexAlgorithms", policy.kex_algorithms.join(",")),
        ("Ciphers", policy.ciphers.join(",")),
        ("MACs", policy.macs.join(",")),
        ("ConnectTimeout", "10".into()),
        ("ServerAliveInterval", "5".into()),
        ("ServerAliveCountMax", "2".into()),
        (
            "IdentityAgent",
            if leg.mode == SshRouteMode::Key {
                directory.0.join("agent").to_string_lossy().into_owned()
            } else {
                "none".into()
            },
        ),
    ] {
        command.arg("-o").arg(format!("{name}={value}"));
    }
    if empty {
        command.args(["-o", "ControlMaster=no", "-o", "ControlPath=none"]);
    }
    if let Some(master) = predecessor {
        command.arg("-o").arg(format!(
            "ProxyCommand={}",
            mux::forward_proxy(
                &master.directory.0.join("master"),
                &master.destination,
                &leg.destination
            )?
        ));
    } else {
        command.args(["-o", "ProxyCommand=none"]);
    }
    command.args([
        "-l",
        &leg.destination.user,
        "-p",
        &leg.destination.port.to_string(),
        "--",
        &leg.destination.hostname,
    ]);
    Ok(command)
}
struct Probe<'a> {
    effective: &'a route::Effective,
    identity: &'a NativeIdentity,
    leg: &'a SshRouteAuthLeg,
}
async fn probe(
    owner: &Owner,
    input: Probe<'_>,
    home: &Path,
    empty: bool,
    predecessor: Option<&Master>,
    deadline: Instant,
) -> Result<(Master, Vec<u8>)> {
    let Probe {
        effective,
        identity,
        leg,
    } = input;
    let directory = Directory::create()?;
    let details = selection::probe_details(&effective.text, home, identity)?;
    let mut records = String::new();
    for host in &leg.host_keys {
        records.push_str(&public_record(&details.lookup, host)?);
    }
    let candidate = directory.file("known", records.as_bytes(), 0o600)?;
    let bridge = bridge::Bridge::start(bridge::Admission {
        owner: owner.clone(),
        directory: &directory.0,
        identity: identity.clone(),
        text: effective.text.clone(),
        home: home.into(),
        candidate: candidate.clone(),
        leg,
        empty,
        deadline,
    })?;
    let mut command = command(&directory, &candidate, &details, leg, empty, predecessor)?;
    command.env(chimaera_remote::ASKPASS_ALIAS_ENV, &owner.alias);
    let process = process::Process::spawn(command)?;
    loop {
        if !owner.guard.active() || !(owner.current)() || Instant::now() >= deadline {
            return Err(SelectionFailure::Unavailable);
        }
        let positive = if empty {
            bridge.bound.load(Ordering::Acquire)
        } else {
            mux::alive(&directory.0.join("master"), process.pid()?).await?
        };
        if positive {
            let bytes = storage::candidate_bytes(&candidate)?;
            if empty
                && bridge
                    .receipt
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .as_ref()
                    != Some(&bytes)
            {
                return Err(SelectionFailure::Unavailable);
            }
            return Ok((
                Master {
                    process,
                    _bridge: bridge,
                    directory,
                    destination: leg.destination.clone(),
                },
                bytes,
            ));
        }
        if process.exited()? {
            return Err(SelectionFailure::Unavailable);
        }
        tokio::select! { biased; _=owner.guard.stopped()=>return Err(SelectionFailure::Unavailable), _=tokio::time::sleep(Duration::from_millis(50))=>{} }
    }
}

/// Trust commits retain both guards in the actual filesystem worker even if
/// the IPC observer disappears. There is no rollback after the append syscall.
struct Evidence<'a> {
    effective: &'a route::Effective,
    identity: &'a NativeIdentity,
    home: &'a Path,
    candidate: &'a Path,
    hosts: &'a [chimaera_link::SshAuthHostKey],
}
async fn commit(
    owner: &Owner,
    alias: &str,
    expected: &[route::Effective],
    target: storage::Destination,
    entry: Vec<u8>,
    evidence: Evidence<'_>,
) -> Result<()> {
    let admission = tokio::select! {
        biased;
        _ = owner.guard.stopped() => return Err(SelectionFailure::Unavailable),
        guard = owner.account.clone().lock_owned() => guard,
    };
    if !owner.guard.active() || !(owner.current)() || route::effective(alias).await? != expected {
        return Err(SelectionFailure::Unavailable);
    }
    let (_, hosts) = selection::candidate_trust(
        &evidence.effective.text,
        evidence.home,
        evidence.identity,
        evidence.candidate,
    )
    .await?;
    if hosts != evidence.hosts || storage::candidate_bytes(evidence.candidate)? != entry {
        return Err(SelectionFailure::Unavailable);
    }
    // A newly written user revocation is also authoritative. Target's exact
    // file snapshot refuses concurrent user edits at the real append syscall.
    match selection::native_trust(&evidence.effective.text, evidence.home, evidence.identity).await
    {
        Err(SelectionFailure::HostTrustRequired) => {}
        Err(error) => return Err(error),
        Ok(_) => return Err(SelectionFailure::Unavailable),
    }
    let owner = owner.clone();
    tokio::task::spawn_blocking(move || {
        let _admission = admission;
        if !(owner.current)() {
            return Err(SelectionFailure::Unavailable);
        }
        owner
            .guard
            .commit(|| target.append(&entry))
            .map_err(|_| SelectionFailure::Unavailable)?
    })
    .await
    .map_err(|_| SelectionFailure::Unavailable)?
}

pub(crate) async fn resolve(alias: &str, boot: String, owner: Owner) -> Result<RouteSelection> {
    let deadline = Instant::now() + Duration::from_secs(chimaera_link::SSH_AUTH_LIFETIME.into());
    let operation = async {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or(SelectionFailure::Unavailable)?;
        let agent = std::env::var_os("SSH_AUTH_SOCK").map(PathBuf::from);
        let effective = route::effective(alias).await?;
        let mut identities = Vec::new();
        let mut requests = Vec::new();
        let mut unknown = Vec::new();
        // Fix every mode/key/allow-list before the first host prompt or network
        // attempt. A refused key operation never becomes an interactive retry.
        for leg in &effective {
            let identity = selection::native_identity(&leg.text, &home, agent.clone()).await?;
            let details = selection::probe_details(&leg.text, &home, &identity)?;
            let (hosts, missing) = match selection::native_trust(&leg.text, &home, &identity).await
            {
                Ok((destination, hosts)) if destination == leg.destination => (hosts, false),
                Err(SelectionFailure::HostTrustRequired) => (vec![], true),
                Err(error) => return Err(error),
                _ => return Err(SelectionFailure::Unavailable),
            };
            requests.push(SshRouteAuthLeg {
                destination: leg.destination.clone(),
                mode: identity.mode,
                host_keys: hosts,
                user_keys: identity.user_keys.clone(),
                policy: Some(details.policy),
            });
            identities.push(identity);
            unknown.push(missing);
        }
        if let Some(last) = unknown.iter().rposition(|missing| *missing) {
            let support = chimaera_remote::SshAlgorithmSupport::capture()
                .await
                .map_err(|_| SelectionFailure::Unavailable)?;
            let mut masters = Vec::<Master>::new();
            for index in 0..=last {
                if !support.current() || !owner.guard.active() || !(owner.current)() {
                    return Err(SelectionFailure::Unavailable);
                }
                let leg = &effective[index];
                let identity = &identities[index];
                if unknown[index] {
                    let details = selection::probe_details(&leg.text, &home, identity)?;
                    let target = storage::Destination::capture(
                        details
                            .target
                            .clone()
                            .ok_or(SelectionFailure::HostTrustRequired)?,
                        &home,
                    )?;
                    let empty = identity.mode == SshRouteMode::Key;
                    let (master, bytes) = probe(
                        &owner,
                        Probe {
                            effective: leg,
                            identity,
                            leg: &requests[index],
                        },
                        &home,
                        empty,
                        masters.last(),
                        deadline,
                    )
                    .await?;
                    if bytes.is_empty()
                        || !bytes.ends_with(b"\n")
                        || bytes[..bytes.len() - 1].contains(&b'\n')
                    {
                        return Err(SelectionFailure::Unavailable);
                    }
                    // Recheck original global revocations after the receipt and
                    // before durable append; the candidate never replaces them.
                    let (_, hosts) = selection::candidate_trust(
                        &leg.text,
                        &home,
                        identity,
                        &master.directory.0.join("known"),
                    )
                    .await?;
                    commit(
                        &owner,
                        alias,
                        &effective,
                        target,
                        bytes,
                        Evidence {
                            effective: leg,
                            identity,
                            home: &home,
                            candidate: &master.directory.0.join("known"),
                            hosts: &hosts,
                        },
                    )
                    .await?;
                    let (destination, readback) =
                        selection::native_trust(&leg.text, &home, identity).await?;
                    if destination != leg.destination
                        || readback != hosts
                        || !owner.guard.active()
                        || !(owner.current)()
                    {
                        return Err(SelectionFailure::Unavailable);
                    }
                    requests[index].host_keys = readback;
                    if !empty && index < last {
                        masters.push(master);
                    } else {
                        master.process.stop(deadline).await?;
                    }
                    if empty && index < last {
                        let (master, _) = probe(
                            &owner,
                            Probe {
                                effective: leg,
                                identity,
                                leg: &requests[index],
                            },
                            &home,
                            false,
                            masters.last(),
                            deadline,
                        )
                        .await?;
                        masters.push(master);
                    }
                } else {
                    let (master, _) = probe(
                        &owner,
                        Probe {
                            effective: leg,
                            identity,
                            leg: &requests[index],
                        },
                        &home,
                        false,
                        masters.last(),
                        deadline,
                    )
                    .await?;
                    masters.push(master);
                }
            }
            while let Some(master) = masters.pop() {
                master.process.stop(deadline).await?;
            }
        }
        if !owner.guard.active()
            || !(owner.current)()
            || route::effective(alias).await? != effective
        {
            return Err(SelectionFailure::Unavailable);
        }
        let destination = effective
            .last()
            .ok_or(SelectionFailure::Unavailable)?
            .destination
            .clone();
        let route = SshRoute {
            version: 1,
            jumps: effective
                .iter()
                .take(effective.len() - 1)
                .map(|leg| leg.destination.clone())
                .collect(),
        };
        let legs = identities
            .into_iter()
            .zip(&requests)
            .map(|(identity, leg)| selection::selected_identity(identity, leg, boot.clone()))
            .collect();
        let request = SshRouteGrantRequest {
            version: 1,
            keeper_boot: boot,
            destination,
            route,
            legs: requests,
        };
        request
            .validate()
            .map_err(|_| SelectionFailure::UnsupportedConfiguration)?;
        Ok(RouteSelection { request, legs })
    };
    tokio::select! {
        biased;
        _ = owner.guard.stopped() => Err(SelectionFailure::Unavailable),
        _ = tokio::time::sleep_until(deadline) => Err(SelectionFailure::Unavailable),
        value = operation => value,
    }
}
