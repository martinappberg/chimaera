//! A fixed same-binary supervisor watches the app pipe and absolute deadline.
//! Its fixed foreground agent shares its pinned group, so owner loss kills all
//! descendants before reaping and releasing the retained four-agent budget.
use super::super::{selection::SelectionFailure, trust::Owner};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
            process::CommandExt,
        },
    },
    path::PathBuf,
    process::{Child as BlockingChild, Stdio},
    sync::{Arc, LazyLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    sync::{watch, Semaphore},
    time::Instant,
};

static SLOTS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(4)));
const READY: &[u8; 8] = b"agent-v1";
const PREFIX: &str = "cx-native-key-agent-";
type Result<T> = std::result::Result<T, SelectionFailure>;
struct Directory(PathBuf, File);
impl Directory {
    fn create() -> Result<Self> {
        let root = std::fs::canonicalize("/tmp").map_err(|_| SelectionFailure::Unavailable)?;
        let path = root.join(format!(
            "{PREFIX}{}",
            &chimaera_core::generate_token()[..24]
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| SelectionFailure::Unavailable)?;
        let pin = OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
            .open(&path)
            .map_err(|_| SelectionFailure::Unavailable)?;
        Ok(Self(path, pin))
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        for name in [c"agent", c"askpass", c"askpass.sh"] {
            unsafe {
                nix::libc::unlinkat(self.1.as_raw_fd(), name.as_ptr(), 0);
            }
        }
        if let (Ok(pin), Ok(entry)) = (self.1.metadata(), std::fs::symlink_metadata(&self.0)) {
            if entry.is_dir() && pin.dev() == entry.dev() && pin.ino() == entry.ino() {
                let _ = std::fs::remove_dir(&self.0);
            }
        }
    }
}
#[derive(Clone)]
pub(super) struct Lease(Arc<Retained>);
struct Retained {
    socket: PathBuf,
    stop: watch::Sender<bool>,
}
impl Drop for Retained {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}
impl Lease {
    pub(super) fn path(&self) -> &std::path::Path {
        &self.0.socket
    }
    pub(super) async fn spawn(owner: &Owner, deadline: Instant) -> Result<Self> {
        if !owner.guard.active() || !(owner.current)() || deadline <= Instant::now() {
            return Err(SelectionFailure::Unavailable);
        }
        let permit = SLOTS
            .clone()
            .try_acquire_owned()
            .map_err(|_| SelectionFailure::Unavailable)?;
        let directory = Directory::create()?;
        let milliseconds = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(180_000);
        if milliseconds == 0 {
            return Err(SelectionFailure::Unavailable);
        }
        let expires = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| SelectionFailure::Unavailable)?
            .as_millis()
            .checked_add(milliseconds)
            .ok_or(SelectionFailure::Unavailable)?;
        let mut command =
            Command::new(std::env::current_exe().map_err(|_| SelectionFailure::Unavailable)?);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LC_ALL", "C")
            .arg("--native-key-agent")
            .arg(&directory.0)
            .arg(expires.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        command.as_std_mut().process_group(0);
        let mut child = command.spawn().map_err(|_| SelectionFailure::Unavailable)?;
        let pipe = child.stdin.take();
        let ready = child.stdout.take();
        let (stop, mut stopped) = watch::channel(false);
        let lease = Self(Arc::new(Retained {
            socket: directory.0.join("agent"),
            stop,
        }));
        let owner_task = owner.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _directory = directory;
            let _pipe = pipe;
            loop {
                if exited(&child).unwrap_or(true)
                    || !owner_task.guard.active()
                    || !(owner_task.current)()
                {
                    break;
                }
                tokio::select! { biased;
                    _=stopped.wait_for(|value| *value)=>break,
                    _=owner_task.guard.stopped()=>break,
                    _=tokio::time::sleep_until(deadline)=>break,
                    _=tokio::time::sleep(Duration::from_millis(25))=>{},
                }
            }
            kill(&mut child);
            let _ = child.wait().await;
        });
        let mut ready = ready.ok_or(SelectionFailure::Unavailable)?;
        let mut frame = [0u8; 8];
        tokio::select! { biased;
            _=owner.guard.stopped()=>return Err(SelectionFailure::Unavailable),
            _=tokio::time::sleep_until(deadline.min(Instant::now()+Duration::from_secs(5)))=>return Err(SelectionFailure::Unavailable),
            result=ready.read_exact(&mut frame)=>{result.map_err(|_| SelectionFailure::Unavailable)?;}
        }
        if &frame != READY || !owner.guard.active() || !(owner.current)() {
            return Err(SelectionFailure::Unavailable);
        }
        Ok(lease)
    }
}
fn exited(child: &Child) -> Result<bool> {
    let pid = child.id().ok_or(SelectionFailure::Unavailable)?;
    let mut info = std::mem::MaybeUninit::<nix::libc::siginfo_t>::zeroed();
    if unsafe {
        nix::libc::waitid(
            nix::libc::P_PID,
            pid,
            info.as_mut_ptr(),
            nix::libc::WEXITED | nix::libc::WNOHANG | nix::libc::WNOWAIT,
        )
    } != 0
    {
        return Err(SelectionFailure::Unavailable);
    }
    let info = unsafe { info.assume_init() };
    if info.si_signo == 0 {
        return Ok(false);
    }
    if info.si_signo != nix::libc::SIGCHLD || unsafe { info.si_pid() } != pid as i32 {
        return Err(SelectionFailure::Unavailable);
    }
    Ok(true)
}
fn kill(child: &mut Child) {
    if let Some(pid) = child.id() {
        unsafe {
            nix::libc::kill(-(pid as i32), nix::libc::SIGKILL);
        }
    }
    let _ = child.start_kill();
}
struct Agent(BlockingChild);
impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn helper() -> std::io::Result<()> {
    let mut args = std::env::args_os();
    let _ = args.next();
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--native-key-agent")) {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    let path = PathBuf::from(args.next().ok_or(std::io::ErrorKind::InvalidInput)?);
    let expires = args
        .next()
        .and_then(|v| v.to_str().and_then(|v| v.parse::<u128>().ok()))
        .ok_or(std::io::ErrorKind::InvalidInput)?;
    let base = std::fs::canonicalize("/tmp")?;
    let name = path
        .file_name()
        .and_then(|v| v.to_str())
        .and_then(|v| v.strip_prefix(PREFIX))
        .ok_or(std::io::ErrorKind::InvalidInput)?;
    if args.next().is_some()
        || path.parent() != Some(base.as_path())
        || name.len() != 24
        || !name.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(&path)?;
    let stat = root.metadata()?;
    if stat.uid() != unsafe { nix::libc::geteuid() } || stat.mode() & 0o777 != 0o700 {
        return Err(std::io::ErrorKind::PermissionDenied.into());
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| std::io::ErrorKind::InvalidInput)?
        .as_millis();
    let remaining = expires
        .checked_sub(now)
        .filter(|v| *v > 0 && *v <= 180_000)
        .ok_or(std::io::ErrorKind::InvalidInput)?;
    let deadline = std::time::Instant::now() + Duration::from_millis(remaining as u64);
    let mut input = std::io::stdin().lock();
    let mut poll = nix::libc::pollfd {
        fd: 0,
        events: nix::libc::POLLIN | nix::libc::POLLHUP,
        revents: 0,
    };
    let input_stat = {
        let mut stat = std::mem::MaybeUninit::<nix::libc::stat>::uninit();
        if unsafe { nix::libc::fstat(0, stat.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        unsafe { stat.assume_init() }
    };
    if input_stat.st_mode & nix::libc::S_IFMT != nix::libc::S_IFIFO {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    // No provider libraries can be loaded into this software-key-only agent.
    let socket = path.join("agent");
    if socket.exists() {
        return Err(std::io::ErrorKind::AlreadyExists.into());
    }
    let agent = Agent(
        std::process::Command::new("/usr/bin/ssh-agent")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LC_ALL", "C")
            .args(["-D", "-P", ""])
            .arg("-a")
            .arg(&socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let startup = std::time::Instant::now() + Duration::from_secs(3);
    let mut announced = false;
    loop {
        if std::time::Instant::now() >= deadline
            || (!announced && std::time::Instant::now() >= startup)
        {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        if unsafe { nix::libc::poll(&mut poll, 1, 25) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if poll.revents
            & (nix::libc::POLLIN | nix::libc::POLLHUP | nix::libc::POLLERR | nix::libc::POLLNVAL)
            != 0
        {
            let mut byte = [0u8; 1];
            let _ = input.read(&mut byte);
            return Err(std::io::ErrorKind::BrokenPipe.into());
        }
        let mut info = std::mem::MaybeUninit::<nix::libc::siginfo_t>::zeroed();
        if unsafe {
            nix::libc::waitid(
                nix::libc::P_PID,
                agent.0.id(),
                info.as_mut_ptr(),
                nix::libc::WEXITED | nix::libc::WNOHANG | nix::libc::WNOWAIT,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        if unsafe { info.assume_init() }.si_signo != 0 {
            return Err(std::io::ErrorKind::BrokenPipe.into());
        }
        let current = std::fs::symlink_metadata(&path)?;
        if !current.is_dir()
            || current.dev() != stat.dev()
            || current.ino() != stat.ino()
            || current.uid() != stat.uid()
            || current.mode() & 0o777 != 0o700
        {
            return Err(std::io::ErrorKind::PermissionDenied.into());
        }
        if !announced && socket.exists() {
            use std::os::unix::fs::FileTypeExt;
            let socket_stat = std::fs::symlink_metadata(&socket)?;
            if !socket_stat.file_type().is_socket() || socket_stat.uid() != stat.uid() {
                return Err(std::io::ErrorKind::PermissionDenied.into());
            }
            std::io::stdout().write_all(READY)?;
            std::io::stdout().flush()?;
            announced = true;
        }
    }
}
pub(super) fn run_helper() -> i32 {
    if helper().is_ok() {
        0
    } else {
        2
    }
}
