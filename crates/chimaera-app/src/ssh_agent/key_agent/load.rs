//! Only one fixed captured-file loader can speak to this private relay. It is
//! owned through observer loss; neither its private frame nor its passphrase
//! reaches the keeper, a log or a key file.
use super::{
    files::Captured,
    process::{self, Lease},
};
use crate::ssh_agent::{
    selection::SelectionFailure,
    trust::{NativePrompt, Owner},
};
use std::{
    os::unix::process::CommandExt,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    process::{Child, Command},
    sync::{oneshot, watch},
    time::Instant,
};
use zeroize::Zeroizing;
type Result<T> = std::result::Result<T, SelectionFailure>;
#[derive(Clone)]
pub(crate) struct SelectedFile(Arc<Mutex<Captured>>);
impl SelectedFile {
    pub(crate) fn capture(path: PathBuf) -> Result<Option<Self>> {
        Ok(Captured::capture(path)?.map(|file| Self(Arc::new(Mutex::new(file)))))
    }
    pub(super) fn label(&self) -> String {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .label()
            .to_string_lossy()
            .into_owned()
    }
    pub(super) async fn check(&self, lease: &Lease) -> Result<()> {
        let file = self.clone();
        let lease = lease.clone();
        tokio::task::spawn_blocking(move || {
            lease.check()?;
            let result = file.0.lock().unwrap_or_else(|e| e.into_inner()).check();
            lease.check()?;
            result
        })
        .await
        .map_err(|_| SelectionFailure::Unavailable)?
    }
    async fn snapshot(&self, lease: &Lease) -> Result<Zeroizing<Vec<u8>>> {
        let file = self.clone();
        let lease = lease.clone();
        tokio::task::spawn_blocking(move || {
            lease.check()?;
            let result = file.0.lock().unwrap_or_else(|e| e.into_inner()).snapshot();
            lease.check()?;
            result
        })
        .await
        .map_err(|_| SelectionFailure::Unavailable)?
    }
}
/// Public certificate metadata is retained with its exact descriptor; no
/// configured or implicit certificate can turn into a plain-key load.
#[derive(Clone)]
pub(crate) struct CertificateFile {
    file: SelectedFile,
    pub(crate) public: String,
}
impl CertificateFile {
    pub(crate) async fn capture(
        path: PathBuf,
        admission: &super::Admission,
    ) -> Result<Option<Self>> {
        let retained = admission.clone();
        tokio::task::spawn_blocking(move || {
            let _retained = retained;
            let Some(mut captured) = Captured::certificate(path)? else {
                return Ok(None);
            };
            let bytes = captured.snapshot()?;
            let text = std::str::from_utf8(&bytes)
                .map_err(|_| SelectionFailure::UnsupportedConfiguration)?;
            let certificate = ssh_key::Certificate::from_openssh(text.trim())
                .map_err(|_| SelectionFailure::UnsupportedConfiguration)?;
            let blob = certificate
                .to_bytes()
                .map_err(|_| SelectionFailure::UnsupportedConfiguration)?;
            super::super::Key::parse(blob.clone())
                .map_err(|_| SelectionFailure::UnsupportedConfiguration)?;
            use base64::Engine;
            Ok(Some(Self {
                file: SelectedFile(Arc::new(Mutex::new(captured))),
                public: base64::engine::general_purpose::STANDARD.encode(blob),
            }))
        })
        .await
        .map_err(|_| SelectionFailure::Unavailable)?
    }
    pub(crate) async fn check(&self, admission: &super::Admission) -> Result<()> {
        let retained = admission.clone();
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || {
            let _retained = retained;
            let result = file.0.lock().unwrap_or_else(|e| e.into_inner()).check();
            result
        })
        .await
        .map_err(|_| SelectionFailure::Unavailable)?
    }
}
struct Observer(watch::Sender<bool>);
impl Drop for Observer {
    fn drop(&mut self) {
        self.0.send_replace(true);
    }
}
struct Loader {
    child: Option<Child>,
    lease: Lease,
    completed: bool,
}
impl Loader {
    fn child(&mut self) -> &mut Child {
        self.child.as_mut().expect("owned loader child")
    }
}
impl Drop for Loader {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            if !self.completed {
                self.lease.revoke();
                let _ = child.start_kill();
                let lease = self.lease.clone();
                tokio::spawn(async move {
                    let _lease = lease;
                    let _ = child.wait().await;
                });
            }
        }
    }
}
fn current(owner: &Owner, lease: &Lease, deadline: Instant) -> Result<()> {
    if !owner.guard.active() || !(owner.current)() || Instant::now() >= deadline {
        return Err(SelectionFailure::Unavailable);
    }
    lease.check()
}
fn listener(lease: &Lease, name: &'static std::ffi::CStr) -> Result<UnixListener> {
    lease.check()?;
    let path = lease
        .root()
        .join(name.to_str().map_err(|_| SelectionFailure::Unavailable)?);
    let listener = UnixListener::bind(&path).map_err(|_| SelectionFailure::Unavailable)?;
    lease.socket_mode(name)?;
    lease.check()?;
    Ok(listener)
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
async fn prompts(
    listener: UnixListener,
    owner: &Owner,
    lease: &Lease,
    file: &SelectedFile,
    scope: &str,
    deadline: Instant,
) -> Result<()> {
    for _ in 0..4 {
        let (mut stream, _) = listener
            .accept()
            .await
            .map_err(|_| SelectionFailure::Unavailable)?;
        let (pid, uid) = process::peer(&stream)?;
        if uid != unsafe { nix::libc::geteuid() }
            || unsafe { nix::libc::getpgid(pid as i32) } != lease.group()? as i32
        {
            return Err(SelectionFailure::Unavailable);
        }
        let mut request = String::new();
        (&mut stream)
            .take(16 * 1024 + 1)
            .read_to_string(&mut request)
            .await
            .map_err(|_| SelectionFailure::Unavailable)?;
        let prefix = format!("chimaera-askpass-scope-v1\n{scope}\n");
        if request.len() > 16 * 1024 || !request.starts_with(&prefix) {
            return Err(SelectionFailure::Unavailable);
        }
        file.check(lease).await?;
        current(owner, lease, deadline)?;
        let answer = (owner.prompt)(
            NativePrompt {
                text: format!("Unlock SSH key {}", file.label()),
                host_key: None,
            },
            owner.guard.clone(),
        )
        .await
        .ok_or(SelectionFailure::Unavailable)?;
        let answer = Zeroizing::new(answer.into_bytes());
        if answer.len() > 16 * 1024
            || answer.contains(&b'\n')
            || answer.contains(&b'\r')
            || answer.contains(&0)
        {
            return Err(SelectionFailure::Unavailable);
        }
        file.check(lease).await?;
        current(owner, lease, deadline)?;
        stream
            .write_all(&answer)
            .await
            .map_err(|_| SelectionFailure::Unavailable)?;
        stream
            .write_all(b"\n")
            .await
            .map_err(|_| SelectionFailure::Unavailable)?;
        stream
            .shutdown()
            .await
            .map_err(|_| SelectionFailure::Unavailable)?;
    }
    // A fourth accepted response can still produce the exact add receipt.
    // A fifth challenge refuses before any further credential UI.
    let _ = listener
        .accept()
        .await
        .map_err(|_| SelectionFailure::Unavailable)?;
    Err(SelectionFailure::Unavailable)
}
async fn packet(stream: &mut UnixStream) -> Result<Zeroizing<Vec<u8>>> {
    let n = stream
        .read_u32()
        .await
        .map_err(|_| SelectionFailure::Unavailable)? as usize;
    if n == 0 || n > chimaera_link::SSH_AUTH_PACKET_MAX {
        return Err(SelectionFailure::Unavailable);
    }
    let mut frame = Zeroizing::new(Vec::with_capacity(n));
    frame.resize(n, 0);
    stream
        .read_exact(&mut frame)
        .await
        .map_err(|_| SelectionFailure::Unavailable)?;
    Ok(frame)
}
async fn run(
    file: SelectedFile,
    lease: Lease,
    owner: Owner,
    deadline: Instant,
    mut stopped: watch::Receiver<bool>,
) -> Result<Vec<u8>> {
    let operation = async {
        let _serial = lease.serialize().await?;
        current(&owner, &lease, deadline)?;
        for name in [c"load", c"askpass", c"askpass.sh"] {
            lease.unlink(name);
        }
        let loader = listener(&lease, c"load")?;
        let askpass = listener(&lease, c"askpass")?;
        let exe = std::env::current_exe().map_err(|_| SelectionFailure::Unavailable)?;
        let exe = exe
            .to_str()
            .filter(|v| !v.chars().any(|c| c.is_control()))
            .ok_or(SelectionFailure::Unavailable)?;
        lease.shim(format!("#!/bin/sh\nexec {} --askpass \"$@\"\n", quote(exe)).as_bytes())?;
        let scope = chimaera_core::generate_token();
        let snapshot = file.snapshot(&lease).await?;
        file.check(&lease).await?;
        current(&owner, &lease, deadline)?;
        let lifetime = deadline
            .saturating_duration_since(Instant::now())
            .as_secs()
            .clamp(1, 180) as u32;
        let mut command = Command::new("/usr/bin/ssh-add");
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LC_ALL", "C")
            .env("SSH_AUTH_SOCK", lease.root().join("load"))
            .env("SSH_ASKPASS", lease.root().join("askpass.sh"))
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("DISPLAY", "native:0")
            .env("CHIMAERA_ASKPASS_SOCK", lease.root().join("askpass"))
            .env(chimaera_remote::ASKPASS_ALIAS_ENV, &scope)
            .args(["-k", "-t", &lifetime.to_string(), "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command.as_std_mut().process_group(lease.group()? as i32);
        current(&owner, &lease, deadline)?;
        let child = command.spawn().map_err(|_| SelectionFailure::Unavailable)?;
        let mut child = Loader {
            child: Some(child),
            lease: lease.clone(),
            completed: false,
        };
        let pid = child.child().id().ok_or(SelectionFailure::Unavailable)?;
        let mut input = child
            .child()
            .stdin
            .take()
            .ok_or(SelectionFailure::Unavailable)?;
        let relay = async {
            input
                .write_all(&snapshot)
                .await
                .map_err(|_| SelectionFailure::Unavailable)?;
            input
                .shutdown()
                .await
                .map_err(|_| SelectionFailure::Unavailable)?;
            drop(input);
            drop(snapshot);
            let (mut source, _) = loader
                .accept()
                .await
                .map_err(|_| SelectionFailure::Unavailable)?;
            if process::peer(&source)? != (pid, unsafe { nix::libc::geteuid() }) {
                return Err(SelectionFailure::Unavailable);
            }
            let frame = packet(&mut source).await?;
            let public = super::add::public_identity(&frame, lifetime)?;
            file.check(&lease).await?;
            current(&owner, &lease, deadline)?;
            let mut target = lease.connect().await?;
            target
                .write_u32(frame.len() as u32)
                .await
                .map_err(|_| SelectionFailure::Unavailable)?;
            target
                .write_all(&frame)
                .await
                .map_err(|_| SelectionFailure::Unavailable)?;
            drop(frame);
            if target
                .read_u32()
                .await
                .map_err(|_| SelectionFailure::Unavailable)?
                != 1
                || target
                    .read_u8()
                    .await
                    .map_err(|_| SelectionFailure::Unavailable)?
                    != 6
            {
                return Err(SelectionFailure::Unavailable);
            }
            file.check(&lease).await?;
            current(&owner, &lease, deadline)?;
            source
                .write_all(&[0, 0, 0, 1, 6])
                .await
                .map_err(|_| SelectionFailure::Unavailable)?;
            source
                .shutdown()
                .await
                .map_err(|_| SelectionFailure::Unavailable)?;
            let mut extra = [0; 1];
            if source
                .read(&mut extra)
                .await
                .map_err(|_| SelectionFailure::Unavailable)?
                != 0
            {
                return Err(SelectionFailure::Unavailable);
            }
            if !child
                .child()
                .wait()
                .await
                .map_err(|_| SelectionFailure::Unavailable)?
                .success()
            {
                return Err(SelectionFailure::Unavailable);
            }
            file.check(&lease).await?;
            current(&owner, &lease, deadline)?;
            child.completed = true;
            Ok(public)
        };
        let result = tokio::select! { biased; value=prompts(askpass,&owner,&lease,&file,&scope,deadline)=>{value?;Err(SelectionFailure::Unavailable)}, value=relay=>value };
        for name in [c"load", c"askpass", c"askpass.sh"] {
            lease.unlink(name);
        }
        result
    };
    let result = tokio::select! { biased;
        _=stopped.wait_for(|v|*v)=>Err(SelectionFailure::Unavailable),
        _=owner.guard.stopped()=>Err(SelectionFailure::Unavailable),
        _=tokio::time::sleep_until(deadline)=>Err(SelectionFailure::Unavailable),
        value=operation=>value,
    };
    if result.is_err() {
        lease.revoke();
    }
    result
}
pub(super) async fn load(
    file: &SelectedFile,
    lease: &Lease,
    owner: &Owner,
    deadline: Instant,
) -> Result<Vec<u8>> {
    current(owner, lease, deadline)?;
    let (cancel, stopped) = watch::channel(false);
    let _observer = Observer(cancel);
    let mut owner = owner.clone();
    owner.guard = owner
        .guard
        .restricted(stopped.clone())
        .map_err(|_| SelectionFailure::Unavailable)?;
    let (done, result) = oneshot::channel();
    let file = file.clone();
    let lease = lease.clone();
    tokio::spawn(async move {
        let result = run(file, lease, owner, deadline, stopped).await;
        let _ = done.send(result);
    });
    result.await.map_err(|_| SelectionFailure::Unavailable)?
}
