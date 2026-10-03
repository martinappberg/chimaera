//! One immutable, fixed-binary capability snapshot shared by every route leg.
use super::SshAuthenticationPolicy;
use anyhow::{bail, ensure};
use std::{
    collections::HashSet,
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    sync::{oneshot, Semaphore},
    time::Instant,
};

const QUERIES: [&str; 6] = [
    "HostKeyAlgorithms",
    "CASignatureAlgorithms",
    "PubkeyAcceptedAlgorithms",
    "KexAlgorithms",
    "Ciphers",
    "MACs",
];
const LIMIT: usize = 64 * 1024;
static SLOTS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(4)));

#[derive(Clone, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
    bytes: u64,
    modified: (i64, i64),
}
fn identity() -> anyhow::Result<Identity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata("/usr/bin/ssh")
            .map_err(|_| anyhow::anyhow!("SSH algorithm support unavailable"))?;
        ensure!(
            metadata.is_file() && metadata.uid() == 0 && metadata.mode() & 0o022 == 0,
            "SSH algorithm support unavailable"
        );
        Ok(Identity {
            device: metadata.dev(),
            inode: metadata.ino(),
            bytes: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
        })
    }
    #[cfg(not(unix))]
    {
        bail!("SSH algorithm support unavailable")
    }
}
struct Snapshot {
    identity: Identity,
    algorithms: [HashSet<String>; 6],
}
/// Contains no caller-selected executable, query, configuration or environment.
/// No Debug implementation: it is an opaque enforcement snapshot, not a grant.
#[derive(Clone)]
pub struct SshAlgorithmSupport(Arc<Snapshot>);
impl SshAlgorithmSupport {
    /// The owner retains capacity and child reaping even if its observer drops.
    /// SSH's closed -Q path exits without reading configuration or spawning a
    /// proxy/helper; no user-controlled subprocess tree participates.
    pub async fn capture() -> anyhow::Result<Self> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let permit = SLOTS
            .clone()
            .try_acquire_owned()
            .map_err(|_| anyhow::anyhow!("SSH algorithm support busy"))?;
        let before = identity()?;
        let (mut sender, receiver) = oneshot::channel();
        tokio::spawn(async move {
            let _permit = permit;
            let result = collect(before, deadline, &mut sender).await;
            let _ = sender.send(result);
        });
        tokio::time::timeout_at(deadline, receiver)
            .await
            .map_err(|_| anyhow::anyhow!("SSH algorithm support expired"))?
            .map_err(|_| anyhow::anyhow!("SSH algorithm support unavailable"))?
    }
    pub fn current(&self) -> bool {
        identity().is_ok_and(|identity| identity == self.0.identity)
    }
    /// Keep original allowed order; no keeper default or unsupported name is
    /// introduced. Callers retain the full original policy in the wire receipt.
    pub fn restrict(
        &self,
        policy: &SshAuthenticationPolicy,
    ) -> anyhow::Result<SshAuthenticationPolicy> {
        policy.validate(
            policy
                .methods
                .first()
                .is_none_or(|name| name != "publickey"),
        )?;
        ensure!(self.current(), "SSH algorithm binary changed");
        let lists = [
            &policy.host_key_algorithms,
            &policy.ca_signature_algorithms,
            &policy.pubkey_accepted_algorithms,
            &policy.kex_algorithms,
            &policy.ciphers,
            &policy.macs,
        ];
        let mut filtered = Vec::with_capacity(6);
        for (list, supported) in lists.into_iter().zip(&self.0.algorithms) {
            let retained = list
                .iter()
                .filter(|name| supported.contains(*name))
                .cloned()
                .collect::<Vec<_>>();
            ensure!(!retained.is_empty(), "SSH algorithm policy unsupported");
            filtered.push(retained);
        }
        let mut output = policy.clone();
        output.host_key_algorithms = filtered[0].clone();
        output.ca_signature_algorithms = filtered[1].clone();
        output.pubkey_accepted_algorithms = filtered[2].clone();
        output.kex_algorithms = filtered[3].clone();
        output.ciphers = filtered[4].clone();
        output.macs = filtered[5].clone();
        Ok(output)
    }
}
async fn collect(
    before: Identity,
    deadline: Instant,
    sender: &mut oneshot::Sender<anyhow::Result<SshAlgorithmSupport>>,
) -> anyhow::Result<SshAlgorithmSupport> {
    let mut algorithms: [HashSet<String>; 6] = std::array::from_fn(|_| HashSet::new());
    let mut bytes = 0usize;
    for (index, query) in QUERIES.into_iter().enumerate() {
        ensure!(
            identity()? == before && !sender.is_closed() && Instant::now() < deadline,
            "SSH algorithm support unavailable"
        );
        let mut command = tokio::process::Command::new("/usr/bin/ssh");
        command
            .env_clear()
            .args(["-Q", query])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|_| anyhow::anyhow!("SSH algorithm support unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("SSH algorithm support unavailable"))?;
        let mut output = Vec::new();
        let read = async {
            stdout
                .take((LIMIT - bytes) as u64 + 1)
                .read_to_end(&mut output)
                .await
                .map_err(|_| anyhow::anyhow!("SSH algorithm support unavailable"))?;
            ensure!(
                output.len() <= LIMIT - bytes,
                "SSH algorithm support oversized"
            );
            let status = child
                .wait()
                .await
                .map_err(|_| anyhow::anyhow!("SSH algorithm support unavailable"))?;
            ensure!(status.success(), "SSH algorithm support unavailable");
            Ok::<_, anyhow::Error>(())
        };
        let result = tokio::select! {
            biased;
            _ = sender.closed() => Err(anyhow::anyhow!("SSH algorithm support canceled")),
            _ = tokio::time::sleep_until(deadline) => Err(anyhow::anyhow!("SSH algorithm support expired")),
            result = read => result,
        };
        if let Err(error) = result {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(error);
        }
        bytes += output.len();
        let text = std::str::from_utf8(&output)
            .map_err(|_| anyhow::anyhow!("SSH algorithm support invalid"))?;
        algorithms[index] = parse(text)?;
    }
    ensure!(
        identity()? == before && !sender.is_closed() && Instant::now() < deadline,
        "SSH algorithm support unavailable"
    );
    Ok(SshAlgorithmSupport(Arc::new(Snapshot {
        identity: before,
        algorithms,
    })))
}
fn parse(text: &str) -> anyhow::Result<HashSet<String>> {
    let mut algorithms = HashSet::new();
    for name in text.lines() {
        ensure!(
            !name.is_empty()
                && name.len() <= 128
                && !name.starts_with(['+', '-'])
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-@._+".contains(&byte))
                && algorithms.insert(name.into())
                && algorithms.len() <= 256,
            "SSH algorithm support invalid"
        );
    }
    if algorithms.is_empty() {
        bail!("SSH algorithm support empty");
    }
    Ok(algorithms)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> SshAuthenticationPolicy {
        SshAuthenticationPolicy {
            methods: vec![
                "publickey".into(),
                "password".into(),
                "keyboard-interactive".into(),
            ],
            host_key_algorithms: vec!["ssh-ed25519".into(), "rsa-sha2-512".into()],
            ca_signature_algorithms: vec!["ssh-ed25519".into()],
            pubkey_accepted_algorithms: vec![
                "webauthn-sk-ecdsa-sha2-nistp256@openssh.com".into(),
                "ssh-ed25519".into(),
            ],
            kex_algorithms: vec![
                "mlkem768x25519-sha256".into(),
                "sntrup761x25519-sha512".into(),
                "curve25519-sha256".into(),
            ],
            ciphers: vec![
                "chacha20-poly1305@openssh.com".into(),
                "aes256-gcm@openssh.com".into(),
            ],
            macs: vec!["hmac-sha2-256-etm@openssh.com".into()],
        }
    }
    #[test]
    fn newer_native_preferences_only_intersect_older_support_and_never_change_wire_policy() {
        let support = SshAlgorithmSupport(Arc::new(Snapshot {
            identity: identity().unwrap(),
            algorithms: [
                "ssh-ed25519\nrsa-sha2-512",
                "ssh-ed25519",
                "ssh-ed25519",
                "curve25519-sha256",
                "aes256-gcm@openssh.com\nchacha20-poly1305@openssh.com",
                "hmac-sha2-256-etm@openssh.com",
            ]
            .map(|text| parse(text).unwrap()),
        }));
        let original = policy();
        let before = serde_json::to_vec(&original).unwrap();
        let emitted = support.restrict(&original).unwrap();
        assert_eq!(emitted.kex_algorithms, ["curve25519-sha256"]);
        assert_eq!(emitted.pubkey_accepted_algorithms, ["ssh-ed25519"]);
        assert_eq!(
            emitted.ciphers,
            ["chacha20-poly1305@openssh.com", "aes256-gcm@openssh.com"]
        );
        assert_eq!(emitted.methods, original.methods);
        assert_eq!(serde_json::to_vec(&original).unwrap(), before);
        for index in 0..6 {
            let mut snapshot = Snapshot {
                identity: identity().unwrap(),
                algorithms: std::array::from_fn(|_| HashSet::new()),
            };
            for (set, list) in snapshot.algorithms.iter_mut().zip([
                &original.host_key_algorithms,
                &original.ca_signature_algorithms,
                &original.pubkey_accepted_algorithms,
                &original.kex_algorithms,
                &original.ciphers,
                &original.macs,
            ]) {
                set.extend(list.iter().cloned());
            }
            snapshot.algorithms[index].clear();
            assert!(SshAlgorithmSupport(Arc::new(snapshot))
                .restrict(&original)
                .is_err());
        }
    }
    #[test]
    fn supported_snapshot_parser_refuses_directives_duplicates_and_unbounded_names() {
        for text in [
            "",
            "ssh-ed25519\nssh-ed25519\n",
            "+ssh-ed25519\n",
            "ssh-ed25519 extra\n",
        ] {
            assert!(parse(text).is_err());
        }
        assert!(parse(&"a".repeat(129)).is_err());
    }
}
