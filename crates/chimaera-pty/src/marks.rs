//! Shell-integration marks: a lightweight scanner that runs beside the vte
//! parser on the PTY output stream, recognizing OSC 133 semantic prompts
//! (FinalTerm protocol: A=prompt, B=input, C=output, D;code=done), OSC 633;E
//! command-line reports (VS Code convention), and OSC 7 cwd reports.
//!
//! From those marks each session derives a [`ShellPhase`] (is the shell at
//! its prompt or running a command?) and a **command journal**: a bounded
//! ring of [`CommandView`] records — command text, captured output, exit
//! code, cwd, duration — which is what agents read instead of raw scrollback.
//!
//! The scanner is deliberately independent of the alacritty grid: it filters
//! escape sequences out at capture time and stores plain text, so journal
//! reads never need the term lock.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::watch;

use crate::lock_unpoisoned;

/// Max bytes of an OSC payload we accumulate; longer ones are skipped whole.
const OSC_CAP: usize = 8192;
/// Captured output kept from the start of a command.
const HEAD_CAP: usize = 64 * 1024;
/// Captured output kept from the end of a command (once the head is full).
const TAIL_CAP: usize = 16 * 1024;
/// Max completed records kept per session.
const MAX_RECORDS: usize = 500;
/// Total captured-output budget across a session's journal.
const TOTAL_OUTPUT_BUDGET: usize = 8 * 1024 * 1024;

/// What the shell is doing, derived from OSC 133 marks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellPhase {
    /// No marks seen yet: no shell integration on this session.
    Unknown,
    /// At (or drawing) the prompt — safe to type a command.
    Ready,
    /// A command is executing (133;C seen, no 133;D yet).
    Running,
}

impl ShellPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            ShellPhase::Unknown => "unknown",
            ShellPhase::Ready => "ready",
            ShellPhase::Running => "running",
        }
    }
}

/// Who typed a journal command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandSource {
    /// Typed by a human (or unattributed).
    User,
    /// Typed by a linked agent through the exec engine.
    Agent,
}

/// One journal entry, materialized for readers.
#[derive(Clone, Debug, serde::Serialize)]
pub struct CommandView {
    pub seq: u64,
    /// Command line when known (OSC 633;E report or an exec stamp).
    pub command: Option<String>,
    pub source: CommandSource,
    /// Working directory at command start (last OSC 7 report), when known.
    pub cwd: Option<String>,
    /// `None` while running, or when the shell never reported one.
    pub exit_code: Option<i32>,
    /// Captured output as plain text (escapes filtered, CR runs collapsed).
    pub output: String,
    /// Bytes dropped from the middle of an over-budget capture.
    pub truncated_bytes: u64,
    pub started_at_ms: u64,
    pub ended_at_ms: Option<u64>,
    /// True for the currently-executing command (present at most once, last).
    pub running: bool,
}

/// Bounded head+tail capture of one command's output.
#[derive(Debug, Default)]
struct Capture {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    truncated: u64,
}

impl Capture {
    fn push(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if self.head.len() < HEAD_CAP {
                self.head.push(b);
            } else {
                if self.tail.len() == TAIL_CAP {
                    self.tail.pop_front();
                    self.truncated += 1;
                }
                self.tail.push_back(b);
            }
        }
    }

    fn len(&self) -> usize {
        self.head.len() + self.tail.len()
    }

    /// Plain-text rendering: lossy UTF-8, CR-overwrite collapse (progress
    /// bars keep only their final state), trailing blank lines trimmed.
    fn render(&self) -> String {
        let mut text = collapse_cr(&String::from_utf8_lossy(&self.head));
        if self.truncated > 0 || !self.tail.is_empty() {
            let tail: Vec<u8> = self.tail.iter().copied().collect();
            if self.truncated > 0 {
                text.push_str(&format!("\n… {} bytes omitted …\n", self.truncated));
            }
            text.push_str(&collapse_cr(&String::from_utf8_lossy(&tail)));
        }
        let trimmed_len = text.trim_end_matches('\n').len();
        text.truncate(trimmed_len);
        text
    }
}

/// Collapse carriage-return overwrites: `\r\n` is a plain newline; within
/// each line, only the content after the last remaining `\r` survives (so a
/// progress bar `50%\r100%` renders as `100%`).
fn collapse_cr(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line.rsplit('\r').next().unwrap_or(line));
    }
    out
}

/// One command's mutable record while in the journal.
#[derive(Debug)]
struct Record {
    seq: u64,
    command: Option<String>,
    source: CommandSource,
    cwd: Option<String>,
    exit_code: Option<i32>,
    capture: Capture,
    started_at_ms: u64,
    ended_at_ms: Option<u64>,
    /// Correlation token for exec waits; never exposed.
    agent_token: Option<u64>,
}

impl Record {
    fn view(&self, running: bool) -> CommandView {
        CommandView {
            seq: self.seq,
            command: self.command.clone(),
            source: self.source,
            cwd: self.cwd.clone(),
            exit_code: self.exit_code,
            output: self.capture.render(),
            truncated_bytes: self.capture.truncated,
            started_at_ms: self.started_at_ms,
            ended_at_ms: self.ended_at_ms,
            running,
        }
    }
}

/// An agent's stamp on the next command to start (set just before the exec
/// engine types it, so the journal can attribute and correlate it).
struct AgentExpect {
    command: String,
    token: u64,
}

/// Escape-stream scanner state (one per session, reader-thread only).
#[derive(Clone, Copy, Debug, Default)]
enum ScanState {
    #[default]
    Ground,
    Esc,
    /// ESC + intermediate bytes (0x20-0x2F): charset selections and friends.
    EscInter,
    Csi,
    Osc,
    /// ESC seen inside an OSC payload: `\` completes ST, else the OSC dies.
    OscEsc,
    /// DCS/SOS/PM/APC string: skipped until ST or BEL.
    SkipString,
    SkipStringEsc,
}

struct Scanner {
    state: ScanState,
    osc: Vec<u8>,
    osc_overflow: bool,
}

impl Scanner {
    fn new() -> Self {
        Scanner {
            state: ScanState::Ground,
            osc: Vec::new(),
            osc_overflow: false,
        }
    }
}

/// Marks recognized in the output stream.
#[derive(Debug, PartialEq)]
enum MarkEvent {
    PromptStart,
    CommandStart,
    OutputStart,
    CommandDone(Option<i32>),
    CommandLine(String),
    Cwd(String),
}

/// Per-session marks state: scanner + phase + journal, fed by the reader
/// thread, read by the server and the exec engine.
pub struct Marks {
    inner: Mutex<Inner>,
    phase_tx: watch::Sender<ShellPhase>,
    /// Notified on every journal change (start, output, completion).
    update: tokio::sync::Notify,
}

struct Inner {
    scanner: Scanner,
    phase: ShellPhase,
    next_seq: u64,
    next_token: u64,
    cwd: Option<String>,
    /// Command line reported by the shell (OSC 633;E) for the next command.
    reported_command: Option<String>,
    /// Agent stamp for the next command (takes precedence over 633;E).
    agent_expect: Option<AgentExpect>,
    running: Option<Record>,
    done: VecDeque<Record>,
    total_output: usize,
    last_byte_at: Instant,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Marks {
    pub(crate) fn new() -> Self {
        let (phase_tx, _) = watch::channel(ShellPhase::Unknown);
        Marks {
            inner: Mutex::new(Inner {
                scanner: Scanner::new(),
                phase: ShellPhase::Unknown,
                next_seq: 1,
                next_token: 1,
                cwd: None,
                reported_command: None,
                agent_expect: None,
                running: None,
                done: VecDeque::new(),
                total_output: 0,
                last_byte_at: Instant::now(),
            }),
            phase_tx,
            update: tokio::sync::Notify::new(),
        }
    }

    /// Feed one chunk of PTY output. Called from the session reader thread.
    pub(crate) fn feed(&self, bytes: &[u8]) {
        let mut inner = lock_unpoisoned(&self.inner);
        inner.last_byte_at = Instant::now();
        let mut changed = false;

        // The scanner walks the chunk byte-wise, emitting text spans (for
        // capture) and complete marks. Text spans are only materialized while
        // a command is running — the common idle case does no extra work.
        let mut span_start: Option<usize> = None;
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            match inner.scanner.state {
                ScanState::Ground => {
                    let printable = b >= 0x20 && b != 0x7f || matches!(b, b'\n' | b'\r' | b'\t');
                    if printable {
                        if span_start.is_none() {
                            span_start = Some(i);
                        }
                    } else {
                        if let Some(start) = span_start.take() {
                            changed |= inner.capture_text(&bytes[start..i]);
                        }
                        if b == 0x1b {
                            inner.scanner.state = ScanState::Esc;
                        }
                        // Other C0 controls are dropped from capture.
                    }
                }
                ScanState::Esc => {
                    inner.scanner.state = match b {
                        b']' => {
                            inner.scanner.osc.clear();
                            inner.scanner.osc_overflow = false;
                            ScanState::Osc
                        }
                        b'[' => ScanState::Csi,
                        b'P' | b'X' | b'^' | b'_' => ScanState::SkipString,
                        0x20..=0x2f => ScanState::EscInter,
                        _ => ScanState::Ground,
                    };
                }
                ScanState::EscInter => {
                    if !(0x20..=0x2f).contains(&b) {
                        inner.scanner.state = ScanState::Ground;
                    }
                }
                ScanState::Csi => match b {
                    0x20..=0x3f => {}
                    0x1b => inner.scanner.state = ScanState::Esc,
                    _ => inner.scanner.state = ScanState::Ground,
                },
                ScanState::Osc => match b {
                    0x07 => {
                        changed |= inner.dispatch_osc();
                        inner.scanner.state = ScanState::Ground;
                    }
                    0x1b => inner.scanner.state = ScanState::OscEsc,
                    _ => {
                        if inner.scanner.osc.len() < OSC_CAP {
                            inner.scanner.osc.push(b);
                        } else {
                            inner.scanner.osc_overflow = true;
                        }
                    }
                },
                ScanState::OscEsc => {
                    if b == b'\\' {
                        changed |= inner.dispatch_osc();
                        inner.scanner.state = ScanState::Ground;
                    } else {
                        // Broken OSC; discard and reprocess as a fresh escape.
                        inner.scanner.osc.clear();
                        inner.scanner.state = ScanState::Esc;
                        continue;
                    }
                }
                ScanState::SkipString => match b {
                    0x07 => inner.scanner.state = ScanState::Ground,
                    0x1b => inner.scanner.state = ScanState::SkipStringEsc,
                    _ => {}
                },
                ScanState::SkipStringEsc => {
                    inner.scanner.state = if b == b'\\' {
                        ScanState::Ground
                    } else {
                        ScanState::SkipString
                    };
                }
            }
            i += 1;
        }
        if let Some(start) = span_start {
            changed |= inner.capture_text(&bytes[start..]);
        }

        let phase = inner.phase;
        drop(inner);
        // Late/idempotent sends are fine: watch dedups by value.
        self.phase_tx.send_if_modified(|p| {
            if *p != phase {
                *p = phase;
                true
            } else {
                false
            }
        });
        if changed {
            self.update.notify_waiters();
        }
    }

    pub fn phase(&self) -> ShellPhase {
        lock_unpoisoned(&self.inner).phase
    }

    pub fn phase_watch(&self) -> watch::Receiver<ShellPhase> {
        self.phase_tx.subscribe()
    }

    /// Age of the most recent PTY output byte (quiet-period detection).
    pub fn last_byte_age(&self) -> Duration {
        lock_unpoisoned(&self.inner).last_byte_at.elapsed()
    }

    /// A future that resolves on the next journal change. Grab it *before*
    /// checking state, then await (the standard Notify pattern).
    pub fn updated(&self) -> tokio::sync::futures::Notified<'_> {
        self.update.notified()
    }

    /// Stamp the next command to start as agent-typed. Returns a correlation
    /// token; the record created at the next 133;C carries it.
    pub fn expect_agent_command(&self, command: String) -> u64 {
        let mut inner = lock_unpoisoned(&self.inner);
        let token = inner.next_token;
        inner.next_token += 1;
        inner.agent_expect = Some(AgentExpect { command, token });
        token
    }

    /// Withdraw an agent stamp that never started (exec aborted pre-typing).
    pub fn clear_agent_expect(&self, token: u64) {
        let mut inner = lock_unpoisoned(&self.inner);
        if inner
            .agent_expect
            .as_ref()
            .is_some_and(|e| e.token == token)
        {
            inner.agent_expect = None;
        }
    }

    /// The completed record carrying `token`, if it finished.
    pub fn find_by_token(&self, token: u64) -> Option<CommandView> {
        let inner = lock_unpoisoned(&self.inner);
        inner
            .done
            .iter()
            .rev()
            .find(|r| r.agent_token == Some(token))
            .map(|r| r.view(false))
    }

    /// The running record carrying `token`, if it started but hasn't finished.
    pub fn running_by_token(&self, token: u64) -> Option<CommandView> {
        let inner = lock_unpoisoned(&self.inner);
        inner
            .running
            .as_ref()
            .filter(|r| r.agent_token == Some(token))
            .map(|r| r.view(true))
    }

    /// Last `limit` journal entries, oldest first; the running command (if
    /// any) is always included last.
    pub fn journal(&self, limit: usize) -> Vec<CommandView> {
        let inner = lock_unpoisoned(&self.inner);
        let mut out: Vec<CommandView> = Vec::new();
        let done_count = limit.saturating_sub(usize::from(inner.running.is_some()));
        let skip = inner.done.len().saturating_sub(done_count);
        out.extend(inner.done.iter().skip(skip).map(|r| r.view(false)));
        if let Some(r) = &inner.running {
            out.push(r.view(true));
        }
        out
    }
}

impl Inner {
    /// Append a text span to the running command's capture, if any.
    /// Returns whether the journal changed.
    fn capture_text(&mut self, span: &[u8]) -> bool {
        let Some(running) = &mut self.running else {
            return false;
        };
        let before = running.capture.len();
        running.capture.push(span);
        let after = running.capture.len();
        self.total_output += after - before;
        after != before
    }

    /// Handle a complete OSC payload. Returns whether the journal changed.
    fn dispatch_osc(&mut self) -> bool {
        if self.osc_overflowed() {
            return false;
        }
        let Some(event) = parse_osc(&self.scanner.osc) else {
            return false;
        };
        match event {
            MarkEvent::PromptStart | MarkEvent::CommandStart => {
                self.phase = ShellPhase::Ready;
                // A new prompt with a command still "running" means its D was
                // lost (e.g. a program repainted over it): close it honestly.
                if self.running.is_some() {
                    self.finish_running(None);
                    return true;
                }
                false
            }
            MarkEvent::OutputStart => {
                self.phase = ShellPhase::Running;
                if self.running.is_some() {
                    self.finish_running(None);
                }
                let (command, source, agent_token) = match self.agent_expect.take() {
                    Some(e) => (Some(e.command), CommandSource::Agent, Some(e.token)),
                    None => (self.reported_command.take(), CommandSource::User, None),
                };
                let seq = self.next_seq;
                self.next_seq += 1;
                self.running = Some(Record {
                    seq,
                    command,
                    source,
                    cwd: self.cwd.clone(),
                    exit_code: None,
                    capture: Capture::default(),
                    started_at_ms: now_ms(),
                    ended_at_ms: None,
                    agent_token,
                });
                true
            }
            MarkEvent::CommandDone(code) => {
                self.phase = ShellPhase::Ready;
                if self.running.is_some() {
                    self.finish_running(code);
                    true
                } else {
                    // D without C: an empty command (bare Enter / Ctrl-C).
                    self.reported_command = None;
                    false
                }
            }
            MarkEvent::CommandLine(cmd) => {
                self.reported_command = Some(cmd);
                false
            }
            MarkEvent::Cwd(cwd) => {
                self.cwd = Some(cwd);
                false
            }
        }
    }

    fn osc_overflowed(&mut self) -> bool {
        let overflowed = self.scanner.osc_overflow;
        self.scanner.osc_overflow = false;
        if overflowed {
            self.scanner.osc.clear();
        }
        overflowed
    }

    fn finish_running(&mut self, exit_code: Option<i32>) {
        if let Some(mut r) = self.running.take() {
            r.exit_code = exit_code;
            r.ended_at_ms = Some(now_ms());
            self.done.push_back(r);
            self.enforce_budgets();
        }
    }

    fn enforce_budgets(&mut self) {
        while self.done.len() > MAX_RECORDS
            || (self.total_output > TOTAL_OUTPUT_BUDGET && self.done.len() > 1)
        {
            if let Some(old) = self.done.pop_front() {
                self.total_output = self.total_output.saturating_sub(old.capture.len());
            } else {
                break;
            }
        }
    }
}

/// Parse one complete OSC payload into a mark, if it is one we care about.
fn parse_osc(payload: &[u8]) -> Option<MarkEvent> {
    let text = std::str::from_utf8(payload).ok()?;
    if let Some(rest) = text.strip_prefix("133;") {
        let mut parts = rest.splitn(2, ';');
        let sub = parts.next()?;
        let param = parts.next();
        return match sub {
            "A" => Some(MarkEvent::PromptStart),
            "B" => Some(MarkEvent::CommandStart),
            "C" => Some(MarkEvent::OutputStart),
            "D" => Some(MarkEvent::CommandDone(
                // Accept `D;0`, `D;0;aid=…` (iTerm2 extensions), and bare `D`.
                param
                    .and_then(|p| p.split(';').next())
                    .and_then(|c| c.parse::<i32>().ok()),
            )),
            _ => None,
        };
    }
    if let Some(rest) = text.strip_prefix("633;E;") {
        // VS Code escaping: `\\` for backslash, `\x3b` for semicolon; an
        // optional `;nonce` field follows the (escaped) command line.
        let cmd = rest.split(';').next().unwrap_or(rest);
        return Some(MarkEvent::CommandLine(unescape_633(cmd)));
    }
    if let Some(rest) = text.strip_prefix("7;") {
        return parse_file_url(rest).map(MarkEvent::Cwd);
    }
    None
}

/// Unescape a 633;E command-line field.
fn unescape_633(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match (chars.next(), chars.as_str()) {
            (Some('\\'), _) => out.push('\\'),
            (Some('x'), rest) if rest.len() >= 2 => {
                let (hex, _) = rest.split_at(2);
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v as char);
                    chars.next();
                    chars.next();
                } else {
                    out.push('x');
                }
            }
            (Some(other), _) => {
                out.push('\\');
                out.push(other);
            }
            (None, _) => out.push('\\'),
        }
    }
    out
}

/// Extract the percent-decoded path from a `file://host/path` URL.
fn parse_file_url(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    let path = match rest.find('/') {
        Some(idx) => &rest[idx..],
        None => return None,
    };
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Some(v) = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_str(marks: &Marks, s: &str) {
        marks.feed(s.as_bytes());
    }

    /// A full integrated-shell round trip: prompt, command, output, done.
    #[test]
    fn integrated_command_round_trip() {
        let marks = Marks::new();
        assert_eq!(marks.phase(), ShellPhase::Unknown);

        feed_str(&marks, "\x1b]133;A\x07$ ");
        assert_eq!(marks.phase(), ShellPhase::Ready);

        feed_str(&marks, "\x1b]133;B\x07");
        feed_str(&marks, "echo hi\r\n"); // echoed typing: not captured
        feed_str(&marks, "\x1b]633;E;echo hi\x07");
        feed_str(&marks, "\x1b]133;C\x07");
        assert_eq!(marks.phase(), ShellPhase::Running);

        feed_str(&marks, "hi\r\n");
        feed_str(&marks, "\x1b]133;D;0\x07\x1b]133;A\x07$ ");
        assert_eq!(marks.phase(), ShellPhase::Ready);

        let journal = marks.journal(10);
        assert_eq!(journal.len(), 1);
        let entry = &journal[0];
        assert_eq!(entry.command.as_deref(), Some("echo hi"));
        assert_eq!(entry.exit_code, Some(0));
        assert_eq!(entry.output, "hi");
        assert_eq!(entry.source, CommandSource::User);
        assert!(!entry.running);
        assert!(entry.ended_at_ms.is_some());
    }

    #[test]
    fn split_across_chunks_and_st_terminator() {
        let marks = Marks::new();
        // OSC split across feeds, terminated by ESC \ instead of BEL.
        marks.feed(b"\x1b]133");
        marks.feed(b";C\x1b");
        marks.feed(b"\\output");
        assert_eq!(marks.phase(), ShellPhase::Running);
        marks.feed(b" more\x1b]133;D;1\x1b\\");
        let journal = marks.journal(10);
        assert_eq!(journal.len(), 1);
        assert_eq!(journal[0].output, "output more");
        assert_eq!(journal[0].exit_code, Some(1));
    }

    #[test]
    fn escapes_are_filtered_from_capture() {
        let marks = Marks::new();
        feed_str(&marks, "\x1b]133;C\x07");
        // SGR color, cursor movement, a title OSC, and a DCS string embedded
        // in output — none may leak into the captured text.
        feed_str(
            &marks,
            "\x1b[31mred\x1b[0m\x1b[2Aplain\x1b]2;title\x07\x1bPskipme\x1b\\done\r\n",
        );
        feed_str(&marks, "\x1b]133;D;0\x07");
        assert_eq!(marks.journal(10)[0].output, "redplaindone");
    }

    #[test]
    fn cr_overwrites_collapse() {
        let marks = Marks::new();
        feed_str(&marks, "\x1b]133;C\x07");
        feed_str(&marks, "10%\r50%\r100%\r\ndone\r\n");
        feed_str(&marks, "\x1b]133;D;0\x07");
        assert_eq!(marks.journal(10)[0].output, "100%\ndone");
    }

    #[test]
    fn agent_stamp_beats_shell_report_and_correlates() {
        let marks = Marks::new();
        feed_str(&marks, "\x1b]133;A\x07");
        let token = marks.expect_agent_command("sbatch job.sh".into());
        // The shell still reports the echoed line; the agent stamp wins.
        feed_str(&marks, "\x1b]633;E;sbatch job.sh\x07\x1b]133;C\x07");
        assert!(marks.find_by_token(token).is_none());
        let running = marks.running_by_token(token).expect("running record");
        assert!(running.running);
        assert_eq!(running.source, CommandSource::Agent);

        feed_str(&marks, "Submitted batch job 42\r\n\x1b]133;D;0\x07");
        let done = marks.find_by_token(token).expect("completed record");
        assert_eq!(done.command.as_deref(), Some("sbatch job.sh"));
        assert_eq!(done.exit_code, Some(0));
        assert_eq!(done.output, "Submitted batch job 42");
        assert_eq!(done.source, CommandSource::Agent);
    }

    #[test]
    fn cleared_stamp_falls_back_to_user_attribution() {
        let marks = Marks::new();
        let token = marks.expect_agent_command("never typed".into());
        marks.clear_agent_expect(token);
        feed_str(&marks, "\x1b]633;E;ls\x07\x1b]133;C\x07\x1b]133;D;0\x07");
        let journal = marks.journal(10);
        assert_eq!(journal[0].command.as_deref(), Some("ls"));
        assert_eq!(journal[0].source, CommandSource::User);
        assert!(marks.find_by_token(token).is_none());
    }

    #[test]
    fn empty_command_leaves_no_record() {
        let marks = Marks::new();
        // Bare Enter at an integrated prompt: A, B, D — no C.
        feed_str(&marks, "\x1b]133;A\x07\x1b]133;B\x07\x1b]133;D;0\x07");
        assert!(marks.journal(10).is_empty());
        assert_eq!(marks.phase(), ShellPhase::Ready);
    }

    #[test]
    fn lost_done_mark_closes_record_on_next_prompt() {
        let marks = Marks::new();
        feed_str(&marks, "\x1b]133;C\x07some output");
        feed_str(&marks, "\x1b]133;A\x07"); // D never arrived
        let journal = marks.journal(10);
        assert_eq!(journal.len(), 1);
        assert_eq!(journal[0].exit_code, None);
        assert!(!journal[0].running);
        assert_eq!(marks.phase(), ShellPhase::Ready);
    }

    #[test]
    fn cwd_reports_attach_to_records() {
        let marks = Marks::new();
        feed_str(&marks, "\x1b]7;file://host/home/user/my%20dir\x07");
        feed_str(&marks, "\x1b]133;C\x07\x1b]133;D;0\x07");
        assert_eq!(
            marks.journal(10)[0].cwd.as_deref(),
            Some("/home/user/my dir")
        );
    }

    #[test]
    fn journal_limit_and_running_entry() {
        let marks = Marks::new();
        for i in 0..5 {
            feed_str(&marks, &format!("\x1b]633;E;cmd{i}\x07"));
            feed_str(&marks, "\x1b]133;C\x07out\x1b]133;D;0\x07");
        }
        feed_str(
            &marks,
            "\x1b]633;E;tail -f log\x07\x1b]133;C\x07following...",
        );
        let journal = marks.journal(3);
        assert_eq!(journal.len(), 3);
        assert_eq!(journal[0].command.as_deref(), Some("cmd3"));
        assert_eq!(journal[1].command.as_deref(), Some("cmd4"));
        assert_eq!(journal[2].command.as_deref(), Some("tail -f log"));
        assert!(journal[2].running);
        assert_eq!(journal[2].output, "following...");
    }

    #[test]
    fn record_cap_evicts_oldest() {
        let marks = Marks::new();
        for i in 0..(MAX_RECORDS + 10) {
            feed_str(&marks, &format!("\x1b]633;E;cmd{i}\x07"));
            feed_str(&marks, "\x1b]133;C\x07\x1b]133;D;0\x07");
        }
        let journal = marks.journal(usize::MAX);
        assert_eq!(journal.len(), MAX_RECORDS);
        assert_eq!(journal[0].command.as_deref(), Some("cmd10"));
    }

    #[test]
    fn giant_output_keeps_head_and_tail() {
        let marks = Marks::new();
        feed_str(&marks, "\x1b]133;C\x07");
        let chunk = "x".repeat(1024);
        for _ in 0..((HEAD_CAP + TAIL_CAP) / 1024 + 32) {
            feed_str(&marks, &chunk);
        }
        feed_str(&marks, "THE-END");
        feed_str(&marks, "\x1b]133;D;0\x07");
        let entry = &marks.journal(1)[0];
        assert!(entry.truncated_bytes > 0);
        assert!(entry.output.contains("bytes omitted"));
        assert!(entry.output.ends_with("THE-END"));
    }

    #[test]
    fn oversized_osc_is_skipped_whole() {
        let marks = Marks::new();
        let mut giant = Vec::from(&b"\x1b]133;"[..]);
        giant.extend(std::iter::repeat_n(b'A', OSC_CAP + 100));
        giant.push(0x07);
        marks.feed(&giant);
        assert_eq!(marks.phase(), ShellPhase::Unknown);
        // The scanner recovers cleanly afterwards.
        marks.feed(b"\x1b]133;A\x07");
        assert_eq!(marks.phase(), ShellPhase::Ready);
    }

    #[test]
    fn unescape_633_handles_vscode_escaping() {
        assert_eq!(unescape_633(r"echo a\x3bb"), "echo a;b");
        assert_eq!(unescape_633(r"a\\b"), r"a\b");
        assert_eq!(unescape_633("plain"), "plain");
    }

    #[tokio::test]
    async fn phase_watch_and_update_notify() {
        let marks = std::sync::Arc::new(Marks::new());
        let mut watch = marks.phase_watch();
        assert_eq!(*watch.borrow(), ShellPhase::Unknown);

        let notified = marks.updated();
        marks.feed(b"\x1b]133;C\x07output");
        watch.changed().await.expect("phase change");
        assert_eq!(*watch.borrow(), ShellPhase::Running);
        // The journal changed (record started + output captured).
        tokio::time::timeout(Duration::from_secs(1), notified)
            .await
            .expect("journal update notification");
    }
}
