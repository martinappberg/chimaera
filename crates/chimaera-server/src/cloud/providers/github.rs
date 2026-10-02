//! GitHub's official CLI owns the device flow, token exchange and storage.
//! Only its one-time code and GitHub's verification page leave this adapter:
//! no terminal, workspace or browser on the cloud machine is involved.
//!
//! `gh auth login --web` with piped I/O is its non-interactive web flow: it
//! prints `! First copy your one-time code: XXXX-XXXX` and then `Open this URL
//! to continue in your web browser: https://github.com/login/device` (both on
//! stderr), polls GitHub itself and exits once access is approved, denied or
//! the code expires. It never waits for Enter there, but an interactive
//! "Press Enter to open github.com in your browser" is answered anyway.
use super::{home, process, Action, Attempt, Phase};
use crate::AppState;
use std::{path::Path, process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{ChildStderr, ChildStdin, ChildStdout},
};

/// GitHub's device page, for a CLI that asked for Enter instead of printing it
/// (it would then have opened this page itself).
const DEVICE_PAGE: &str = "https://github.com/login/device";
/// How long the CLI has to print its one-time code: it starts through the
/// login shell and asks GitHub for the code over the network.
#[cfg(not(test))]
const CODE_WAIT: Duration = Duration::from_secs(30);
#[cfg(test)]
const CODE_WAIT: Duration = Duration::from_secs(3);
/// After the code, how long the page line that follows it may take.
const PAGE_WAIT: Duration = Duration::from_secs(2);

/// Colour and hyperlink escape sequences never reach the parsers.
fn plain(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameters, then one final byte.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: until BEL or the string terminator (ESC \).
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// A GitHub one-time code: two groups of four capital letters or digits.
fn user_code(token: &str) -> bool {
    let bytes = token.as_bytes();
    bytes.len() == 9
        && bytes.iter().enumerate().all(|(i, &b)| {
            if i == 4 {
                b == b'-'
            } else {
                b.is_ascii_uppercase() || b.is_ascii_digit()
            }
        })
}

/// Only GitHub's own device page, on an origin in the shared catalog.
fn device_page(candidate: &str) -> bool {
    if candidate.len() > 4096 || candidate.contains('#') {
        return false;
    }
    let Ok(uri) = candidate.parse::<axum::http::Uri>() else {
        return false;
    };
    let Some(authority) = uri.authority() else {
        return false;
    };
    let origin = format!(
        "https://{}",
        authority
            .as_str()
            .strip_suffix(":443")
            .unwrap_or(authority.as_str())
    );
    uri.scheme_str() == Some("https")
        && chimaera_core::cloud_providers::provider_auth_origins("github").contains(&origin)
        && uri.path().starts_with("/login/device")
}

#[derive(Debug, Default, PartialEq)]
struct Prompt {
    code: Option<String>,
    page: Option<String>,
    enter: bool,
}

/// What the CLI has asked for so far. The code and page come from complete
/// lines only, so a pipe read that ends mid-line never publishes half of one.
fn prompt(bytes: &[u8]) -> Prompt {
    let text = plain(bytes);
    let mut found = Prompt {
        enter: text.contains("Press Enter"),
        ..Prompt::default()
    };
    let complete = text.rfind('\n').map_or("", |end| &text[..end]);
    for line in complete.lines() {
        if found.code.is_none() && line.to_ascii_lowercase().contains("one-time code") {
            found.code = line
                .split_whitespace()
                .map(|token| token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-'))
                .find(|token| user_code(token))
                .map(str::to_owned);
        }
        if found.page.is_none() {
            found.page = line
                .match_indices("https://")
                .filter_map(|(start, _)| line[start..].split_whitespace().next())
                .find(|candidate| device_page(candidate))
                .map(str::to_owned);
        }
    }
    found
}

async fn read<R: AsyncRead + Unpin>(
    stream: &mut Option<R>,
    buffer: &mut [u8],
) -> std::io::Result<usize> {
    match stream {
        Some(stream) => stream.read(buffer).await,
        None => std::future::pending().await,
    }
}

/// The CLI's stdout and stderr as one stream: gh prompts on stderr, and a
/// login shell's profile may print on either.
struct Output {
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    out: [u8; 2048],
    err: [u8; 2048],
}
impl Output {
    fn open(&self) -> bool {
        self.stdout.is_some() || self.stderr.is_some()
    }
    /// Appends the next chunk from either stream to `into`; false once both
    /// have closed.
    async fn next(&mut self, into: &mut Vec<u8>) -> Result<bool, &'static str> {
        while self.open() {
            let (read, stdout) = tokio::select! {
                n = read(&mut self.stdout, &mut self.out), if self.stdout.is_some() => (n, true),
                n = read(&mut self.stderr, &mut self.err), if self.stderr.is_some() => (n, false),
                else => return Ok(false),
            };
            let n = read.map_err(|_| "connection_closed")?;
            match (n, stdout) {
                (0, true) => self.stdout = None,
                (0, false) => self.stderr = None,
                (n, true) => {
                    into.extend_from_slice(&self.out[..n]);
                    return Ok(true);
                }
                (n, false) => {
                    into.extend_from_slice(&self.err[..n]);
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

/// Reads until the CLI shows its one-time code (and the page it names, if
/// it prints one), answering an Enter prompt once.
async fn code(
    output: &mut Output,
    input: &mut ChildStdin,
    captured: &mut Vec<u8>,
) -> Result<(String, String), &'static str> {
    let mut deadline = tokio::time::Instant::now() + CODE_WAIT;
    let mut page_wait = false;
    let mut answered = false;
    loop {
        let found = prompt(captured);
        if let (Some(code), Some(page)) = (&found.code, &found.page) {
            return Ok((code.clone(), page.clone()));
        }
        if found.enter && !answered {
            answered = true;
            let _ = tokio::time::timeout(Duration::from_secs(3), async {
                input.write_all(b"\n").await?;
                input.flush().await
            })
            .await;
        }
        if found.code.is_some() && !page_wait {
            page_wait = true;
            deadline = deadline.min(tokio::time::Instant::now() + PAGE_WAIT);
        }
        match tokio::time::timeout_at(deadline, output.next(captured)).await {
            // A code without its page line: GitHub's own device page.
            Err(_) => {
                return found
                    .code
                    .map(|code| (code, DEVICE_PAGE.to_owned()))
                    .ok_or("sign_in_unavailable")
            }
            // A CLI that exited is no longer waiting for anyone's approval.
            Ok(Ok(false)) => return Err("sign_in_unavailable"),
            Ok(Ok(true)) if captured.len() > process::LIMIT => return Err("output_limit"),
            Ok(Ok(true)) => {}
            Ok(Err(error)) => return Err(error),
        }
    }
}

/// The flag only matters for SSH setup, which this HTTPS login never does;
/// it is passed when the installed CLI knows it, never guessed.
async fn skips_ssh_key(bin: &Path, home: &Path) -> bool {
    process::output(&mut process::command(
        bin,
        &["auth", "login", "--help"],
        home,
    ))
    .await
    .is_ok_and(|help| {
        help.success
            && help
                .stdout
                .windows(b"--skip-ssh-key".len())
                .any(|w| w == b"--skip-ssh-key")
    })
}

fn quiet(command: &mut tokio::process::Command) -> &mut tokio::process::Command {
    // Nothing on the cloud machine may prompt, open a browser or add notices.
    command
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("NO_COLOR", "1")
        .env("BROWSER", "true")
        .env("GH_BROWSER", "true")
}

pub(super) async fn login(
    state: &Arc<AppState>,
    bin: &Path,
    attempt: &Attempt,
) -> Result<(), &'static str> {
    let home = home(state);
    let mut args = vec![
        "auth",
        "login",
        "--hostname",
        "github.com",
        "--git-protocol",
        "https",
        "--web",
    ];
    if skips_ssh_key(bin, &home).await {
        args.push("--skip-ssh-key");
    }
    let mut command = process::command(bin, &args, &home);
    quiet(&mut command).stdin(Stdio::piped());
    let mut child = process::Child::spawn(&mut command)?;
    *crate::lock(&attempt.process) = child.child.id();
    let mut input = child.child.stdin.take().ok_or("start_failed")?;
    let mut output = Output {
        stdout: Some(child.child.stdout.take().ok_or("start_failed")?),
        stderr: Some(child.child.stderr.take().ok_or("start_failed")?),
        out: [0; 2048],
        err: [0; 2048],
    };
    let mut captured = Vec::new();
    let (user_code, verification_url) = code(&mut output, &mut input, &mut captured).await?;
    attempt.update(
        Phase::Waiting,
        Some(Action::DeviceCode {
            verification_url,
            user_code,
        }),
        None,
    );
    // Keep draining bounded output while GitHub waits for the person: the
    // CLI polls GitHub itself and exits once access is approved, denied or
    // the code expires. Its words never leave this adapter.
    let mut read = captured.len();
    drop(captured);
    let mut scratch = Vec::new();
    let status = {
        let wait = child.wait();
        tokio::pin!(wait);
        loop {
            tokio::select! {
                status = &mut wait => break status.map_err(|_| "sign_in_failed")?,
                more = output.next(&mut scratch), if output.open() => {
                    more?;
                    read += scratch.len();
                    scratch.clear();
                    if read > process::LIMIT {
                        return Err("output_limit");
                    }
                }
            }
        }
    };
    // Group cleanup precedes reaping; no process identity survives into Git setup.
    drop((input, output, child));
    if !status.success() {
        return Err("sign_in_failed");
    }
    setup_git(state, bin, attempt).await
}

/// A signed-in CLI alone does not let Git use the account: gh becomes Git's
/// credential helper for github.com only when asked (an interactive login
/// asks; this one cannot). Idempotent, so an already signed-in CLI gets it too.
pub(super) async fn setup_git(
    state: &Arc<AppState>,
    bin: &Path,
    attempt: &Attempt,
) -> Result<(), &'static str> {
    let mut command = process::command(
        bin,
        &["auth", "setup-git", "--hostname", "github.com"],
        &home(state),
    );
    let done = process::output_tracked(quiet(&mut command), |pid| {
        *crate::lock(&attempt.process) = Some(pid);
    })
    .await
    .map_err(|_| "git_setup_failed")?;
    if done.success {
        Ok(())
    } else {
        Err("git_setup_failed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_code_and_page_come_only_from_complete_known_lines() {
        let text = "! First copy your one-time code: ABCD-12E4\nOpen this URL to continue in your web browser: https://github.com/login/device\n";
        assert_eq!(
            prompt(text.as_bytes()),
            Prompt {
                code: Some("ABCD-12E4".into()),
                page: Some(DEVICE_PAGE.into()),
                enter: false
            }
        );
        // Colour, a clipboard wording and a profile's noise are tolerated.
        let coloured = "motd: welcome\n\x1b[0;33m!\x1b[0m One-time code (\x1b[1mWXYZ-9876\x1b[0m) copied to clipboard\n";
        assert_eq!(
            prompt(coloured.as_bytes()).code.as_deref(),
            Some("WXYZ-9876")
        );
        // Half a line is not a code yet.
        assert!(prompt(b"! First copy your one-time code: ABCD-1234")
            .code
            .is_none());
        // A code-shaped token needs the CLI's own words beside it.
        assert!(prompt(b"build ABCD-1234 finished\n").code.is_none());
        for code in ["abcd-1234", "ABCD1234", "ABCD-12345", "AB-CD1234"] {
            let line = format!("! First copy your one-time code: {code}\n");
            assert!(prompt(line.as_bytes()).code.is_none(), "{code}");
        }
        let enter = "! First copy your one-time code: ABCD-1234\nPress Enter to open github.com in your browser... ";
        let asked = prompt(enter.as_bytes());
        assert!(asked.enter && asked.page.is_none());
    }

    #[test]
    fn only_githubs_own_device_page_is_published() {
        assert!(device_page("https://github.com/login/device"));
        assert!(device_page("https://github.com:443/login/device"));
        for url in [
            "http://github.com/login/device",
            "https://github.com.evil.test/login/device",
            "https://user@github.com/login/device",
            "https://github.com:444/login/device",
            "https://github.com/login/device#code",
            "https://github.com/settings/tokens",
            "https://gist.github.com/login/device",
        ] {
            assert!(!device_page(url), "{url}");
            let line = format!("Open this URL to continue in your web browser: {url}\n");
            assert!(prompt(line.as_bytes()).page.is_none(), "{url}");
        }
    }
}
