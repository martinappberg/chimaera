//! Auth subprocesses never forward their output to logs or HTTP errors.
use serde_json::{json, Value};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

pub(super) const LIMIT: usize = 64 * 1024;
pub(super) const TIMEOUT: Duration = Duration::from_secs(8);
pub(super) struct Child {
    pub child: tokio::process::Child,
    #[cfg(unix)]
    group: Option<rustix::process::Pid>,
}
impl Child {
    pub fn spawn(command: &mut tokio::process::Command) -> Result<Self, &'static str> {
        #[cfg(unix)]
        command.process_group(0);
        let child = command
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| "start_failed")?;
        #[cfg(unix)]
        let group = child
            .id()
            .and_then(|id| rustix::process::Pid::from_raw(id as i32));
        Ok(Self {
            child,
            #[cfg(unix)]
            group,
        })
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(group) = self.group.take() {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        let _ = self.child.start_kill();
    }
}
pub(super) fn command(bin: &Path, args: &[&str], cwd: &Path) -> tokio::process::Command {
    let argv = crate::launcher::wrap_login_shell(
        &crate::launcher::login_shell(),
        std::iter::once(bin.to_string_lossy().into_owned())
            .chain(args.iter().map(|s| s.to_string()))
            .collect(),
    );
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(cwd)
        // The worker's explicit provider home owns auth. A different ambient
        // desktop HOME/config override must never redirect an auth mutation.
        // Claude keeps its default layout under that HOME (`~/.claude.json`
        // beside `~/.claude/`), exactly what its sessions read: a
        // CLAUDE_CONFIG_DIR here would move its global config to
        // `~/.claude/.claude.json`, so sign-in and sessions would disagree.
        .env("HOME", cwd)
        .env("CODEX_HOME", cwd.join(".codex"))
        .env_remove("CLAUDE_CONFIG_DIR")
        .env("GH_CONFIG_DIR", cwd.join(".config/gh"))
        .env("XDG_CONFIG_HOME", cwd.join(".config"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in crate::api::launcher_context_env() {
        cmd.env_remove(name);
    }
    cmd
}
pub(super) struct Output {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
}
pub(super) async fn output(cmd: &mut tokio::process::Command) -> Result<Output, &'static str> {
    output_tracked(cmd, |_| {}).await
}
pub(super) async fn output_tracked(
    cmd: &mut tokio::process::Command,
    started: impl FnOnce(u32),
) -> Result<Output, &'static str> {
    let mut child = Child::spawn(cmd)?;
    if let Some(pid) = child.child.id() {
        started(pid);
    }
    let stdout = child.child.stdout.take().ok_or("start_failed")?;
    let stderr = child.child.stderr.take().ok_or("start_failed")?;
    let work = async {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut stdout = stdout.take((LIMIT + 1) as u64);
        let mut stderr = stderr.take((LIMIT + 1) as u64);
        let (a, b) = tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err));
        a.map_err(|_| "probe_failed")?;
        b.map_err(|_| "probe_failed")?;
        if out.len() > LIMIT || err.len() > LIMIT {
            return Err("output_limit");
        }
        let status = child.child.wait().await.map_err(|_| "probe_failed")?;
        Ok(Output {
            success: status.success(),
            code: status.code(),
            stdout: out,
        })
    };
    tokio::time::timeout(TIMEOUT, work)
        .await
        .map_err(|_| "probe_timeout")?
}

pub(super) struct Rpc {
    _child: Child,
    input: tokio::process::ChildStdin,
    output: BufReader<tokio::process::ChildStdout>,
    next_id: u64,
}
impl Rpc {
    pub fn process_id(&self) -> Option<u32> {
        self._child.child.id()
    }
    pub async fn open(bin: &Path, cwd: &Path) -> Result<Self, &'static str> {
        let mut cmd = command(bin, &["app-server"], cwd);
        cmd.stdin(Stdio::piped()).stderr(Stdio::null());
        let mut child = Child::spawn(&mut cmd)?;
        let mut rpc = Self {
            input: child.child.stdin.take().ok_or("start_failed")?,
            output: BufReader::new(child.child.stdout.take().ok_or("start_failed")?),
            _child: child,
            next_id: 0,
        };
        rpc.request(
            "initialize",
            json!({"clientInfo":{"name":"chimaera-provider","version":chimaera_core::VERSION}}),
        )
        .await?;
        rpc.send(json!({"method":"initialized"})).await?;
        Ok(rpc)
    }
    async fn send(&mut self, value: Value) -> Result<(), &'static str> {
        let mut bytes = serde_json::to_vec(&value).map_err(|_| "protocol_error")?;
        bytes.push(b'\n');
        self.input
            .write_all(&bytes)
            .await
            .map_err(|_| "connection_closed")
    }
    pub async fn next(&mut self) -> Result<Value, &'static str> {
        // read_until on a limited adapter caps allocation before parsing;
        // checking String::len after lines().next_line() would be too late.
        let mut bytes = Vec::new();
        let n = (&mut self.output)
            .take((LIMIT + 1) as u64)
            .read_until(b'\n', &mut bytes)
            .await
            .map_err(|_| "connection_closed")?;
        if n == 0 {
            return Err("connection_closed");
        }
        if n > LIMIT {
            return Err("output_limit");
        }
        serde_json::from_slice(&bytes).map_err(|_| "protocol_error")
    }
    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value, &'static str> {
        self.next_id += 1;
        let id = self.next_id;
        let work = async {
            self.send(json!({"id":id,"method":method,"params":params}))
                .await?;
            // Notifications are bounded too: a noisy child cannot monopolize
            // an auth request indefinitely even before the wall-time fence.
            for _ in 0..64 {
                let msg = self.next().await?;
                if msg["id"].as_u64() != Some(id) {
                    continue;
                }
                if msg.get("error").is_some() {
                    return Err("provider_rejected");
                }
                return msg.get("result").cloned().ok_or("protocol_error");
            }
            Err("output_limit")
        };
        tokio::time::timeout(TIMEOUT, work)
            .await
            .map_err(|_| "probe_timeout")?
    }
}

/// Only used for a child process group that this service created and killed.
/// A reused PID makes cleanup conservatively fail closed rather than releasing
/// the provider's single-writer reservation while its status is uncertain.
pub(super) fn group_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        rustix::process::Pid::from_raw(pid as i32).is_some_and(|pid| {
            !matches!(
                rustix::process::test_kill_process_group(pid),
                Err(rustix::io::Errno::SRCH)
            )
        })
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}
