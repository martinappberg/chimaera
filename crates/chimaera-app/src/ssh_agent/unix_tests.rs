//! Synthetic local sockets only; no user agent, config or credentials.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::{net::UnixListener, sync::Semaphore, task::JoinSet};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("cx-pin-{}", &chimaera_core::generate_token()[..16]));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn identities() -> Vec<u8> {
    let key = ssh_key::PrivateKey::new(
        ssh_key::private::KeypairData::Ed25519(ssh_key::private::Ed25519Keypair::from_seed(
            &[7; 32],
        )),
        "synthetic",
    )
    .unwrap()
    .public_key()
    .to_bytes()
    .unwrap();
    let mut result = vec![12];
    result.extend_from_slice(&1u32.to_be_bytes());
    result.extend_from_slice(&(key.len() as u32).to_be_bytes());
    result.extend_from_slice(&key);
    result.extend_from_slice(&0u32.to_be_bytes());
    result
}
struct AgentServer {
    task: tokio::task::JoinHandle<()>,
    probes: Arc<AtomicUsize>,
    signs: Arc<AtomicUsize>,
}
impl AgentServer {
    fn new(path: &std::path::Path, pause: Option<(Arc<Semaphore>, Arc<Semaphore>)>) -> Self {
        let listener = UnixListener::bind(path).unwrap();
        let probes = Arc::new(AtomicUsize::new(0));
        let signs = Arc::new(AtomicUsize::new(0));
        let counted = probes.clone();
        let signed = signs.clone();
        let task = tokio::spawn(async move {
            let mut handlers = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((mut stream, _)) = accepted else { return; };
                        let probes = counted.clone();
                        let signs = signed.clone();
                        let pause = pause.clone();
                        handlers.spawn(async move {
                            loop {
                                let Ok(length) = stream.read_u32().await else { return; };
                                if length == 0 || length as usize > SSH_AUTH_PACKET_MAX { return; }
                                let mut packet = vec![0; length as usize];
                                if stream.read_exact(&mut packet).await.is_err() { return; }
                                let response = if packet == [11] {
                                    probes.fetch_add(1, Ordering::SeqCst);
                                    if let Some((entered, release)) = &pause {
                                        entered.add_permits(1);
                                        let Ok(permit) = release.acquire().await else { return; };
                                        permit.forget();
                                    }
                                    identities()
                                } else {
                                    signs.fetch_add(1, Ordering::SeqCst);
                                    vec![6]
                                };
                                if stream.write_u32(response.len() as u32).await.is_err()
                                    || stream.write_all(&response).await.is_err() { return; }
                            }
                        });
                    },
                    _ = handlers.join_next(), if !handlers.is_empty() => {},
                }
            }
        });
        Self {
            task,
            probes,
            signs,
        }
    }
}
impl Drop for AgentServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn same_key_replacement_refuses_new_binding_but_preserves_original_stream() {
    let directory = Directory::new();
    let path = directory.0.join("agent");
    let original = AgentServer::new(&path, None);
    let (agent, _) = UnixAgent::capture(path.clone()).await.unwrap();
    let mut bound = agent.connect().await.unwrap();
    assert_eq!(original.probes.load(Ordering::SeqCst), 2);
    std::fs::remove_file(&path).unwrap();
    let replacement = AgentServer::new(&path, None);
    assert!(matches!(
        agent.connect().await,
        Err(Failure::KeyUnavailable)
    ));
    assert_eq!(replacement.probes.load(Ordering::SeqCst), 0);
    assert_eq!(bound.exchange(&[27]).await.unwrap(), [6]);
    assert_eq!(original.signs.load(Ordering::SeqCst), 1);
    assert_eq!(replacement.signs.load(Ordering::SeqCst), 0);
    // A later explicit selection can capture the new instance; the old one
    // never silently does so, even though both advertise the same public key.
    assert!(UnixAgent::capture(path).await.is_ok());
}

#[tokio::test]
async fn legal_symlink_is_preserved_but_retargeting_it_cannot_select_another_agent() {
    let directory = Directory::new();
    let first = directory.0.join("first");
    let second = directory.0.join("second");
    let alias = directory.0.join("selected");
    let _original = AgentServer::new(&first, None);
    let replacement = AgentServer::new(&second, None);
    std::os::unix::fs::symlink(&first, &alias).unwrap();
    let (agent, _) = UnixAgent::capture(alias.clone()).await.unwrap();
    assert_eq!(agent.path(), alias);
    assert!(agent.connect().await.is_ok());
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&second, &alias).unwrap();
    assert!(matches!(
        agent.connect().await,
        Err(Failure::KeyUnavailable)
    ));
    assert_eq!(replacement.probes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn replacement_during_original_enumeration_never_publishes_a_pin() {
    let directory = Directory::new();
    let path = directory.0.join("agent");
    let entered = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    let _original = AgentServer::new(&path, Some((entered.clone(), release.clone())));
    let capture = UnixAgent::capture(path.clone());
    let replace = async {
        entered.acquire().await.unwrap().forget();
        std::fs::remove_file(&path).unwrap();
        let replacement = AgentServer::new(&path, None);
        release.add_permits(1);
        replacement
    };
    let (result, replacement) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(capture, replace)
    })
    .await
    .unwrap();
    assert!(matches!(result, Err(Failure::KeyUnavailable)));
    assert_eq!(replacement.probes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn original_empty_identity_reply_retains_initial_no_keys_semantics() {
    let directory = Directory::new();
    let path = directory.0.join("agent");
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        assert_eq!(stream.read_u32().await.unwrap(), 1);
        assert_eq!(stream.read_u8().await.unwrap(), 11);
        stream.write_u32(5).await.unwrap();
        stream.write_all(&[12, 0, 0, 0, 0]).await.unwrap();
        // Retain the serving peer until capture completes its PID check.
        let _ = stream.read(&mut [0]).await;
    });
    let (_, response) = UnixAgent::capture(path).await.unwrap();
    assert!(matches!(
        super::super::selection::identity_reply(&response),
        Err(super::super::selection::SelectionFailure::NoKeys)
    ));
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap();
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn inherited_listener_pins_the_actual_darwin_responder_after_identity_reply() {
    use std::{
        os::{fd::AsRawFd, unix::process::CommandExt},
        process::Stdio,
    };
    let directory = Directory::new();
    let path = directory.0.join("agent");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let descriptor = listener.as_raw_fd();
    // No launchd registration or user agent. The synthetic child inherits a
    // listener created by a different PID, like socket-activated native SSH.
    let spawn = |connections: u8| {
        let mut command = std::process::Command::new("/usr/bin/python3");
        command
            .arg("-c")
            .arg(
                r#"
import socket,struct,sys
l=socket.socket(fileno=int(sys.argv[1]))
def read(c,n):
    b=b''
    while len(b)<n:
        p=c.recv(n-len(b))
        if not p: raise RuntimeError('closed')
        b+=p
    return b
for _ in range(int(sys.argv[3])):
    c,_=l.accept()
    assert read(c,4)==struct.pack('>I',1) and read(c,1)==b'\x0b'
    reply=bytes.fromhex(sys.argv[2])
    c.sendall(struct.pack('>I',len(reply))+reply)
    while c.recv(1): pass
    c.close()
"#,
            )
            .arg(descriptor.to_string())
            .arg(
                identities()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>(),
            )
            .arg(connections.to_string())
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(move || {
                if nix::libc::fcntl(descriptor, nix::libc::F_SETFD, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut command = tokio::process::Command::from(command);
        command.kill_on_drop(true);
        command.spawn().unwrap()
    };
    let mut child = spawn(2);
    let pid = child.id().unwrap();
    let result = tokio::time::timeout(Duration::from_secs(7), async {
        let (agent, _) = UnixAgent::capture(path).await?;
        let connection = agent.connect().await?;
        let matches = agent.pin.peer.pid == pid && Peer::read(&connection)? == agent.pin.peer;
        drop(connection);
        Ok::<_, Failure>((agent, matches))
    })
    .await;
    let status = match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
        Ok(status) => status,
        Err(_) => {
            let _ = child.start_kill();
            tokio::time::timeout(Duration::from_secs(2), child.wait())
                .await
                .expect("synthetic inherited agent must be reaped")
        }
    };
    assert!(status.unwrap().success());
    let (agent, matches) = match result {
        Ok(Ok(result)) => result,
        _ => panic!("synthetic inherited agent must be pinned"),
    };
    assert!(matches);
    // Preserve the exact listener inode while a different child responds with
    // the same public key. The captured source must refuse that new peer.
    let mut replacement = spawn(1);
    drop(listener);
    let result = tokio::time::timeout(Duration::from_secs(7), agent.connect()).await;
    let status = match tokio::time::timeout(Duration::from_secs(2), replacement.wait()).await {
        Ok(status) => status,
        Err(_) => {
            let _ = replacement.start_kill();
            tokio::time::timeout(Duration::from_secs(2), replacement.wait())
                .await
                .expect("synthetic replacement agent must be reaped")
        }
    };
    assert!(matches!(result, Ok(Err(Failure::KeyUnavailable))));
    assert!(status.unwrap().success());
}
