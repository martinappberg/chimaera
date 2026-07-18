//! Offline import of a Claude Code transcript into normalized `AgentEvent`s.
//!
//! When a "recent" is opened whose conversation only ever lived as a claude
//! TUI session (or a pre-chimaera one), the daemon mints a fresh chimaera
//! session and spawns `claude --resume <native>`. The agent replays no history
//! over the wire, and the native-id index has no chimaera journal to copy — so
//! this module reads claude's OWN transcript (`~/.claude/projects/…/<id>.jsonl`)
//! and translates it into the same `AgentEvent` stream a live session would
//! have produced, which the caller seeds into the new journal
//! ([`crate::journal::seed_journal`]) so `attach` replays the full conversation
//! before the fresh `Init`.
//!
//! **Reuse over reimplementation.** The per-block translation (tool kind,
//! title, locations, edit diffs, result text) is the *same* code the live
//! `claude` driver runs — [`crate::claude`]'s `tool_kind`/`tool_title`/
//! `tool_locations`/`edit_diff_content`/`tool_result_text` — so imported history
//! renders identically to a live session and stays correct as that mapping
//! evolves. We do NOT drive the driver's stateful per-frame handlers offline:
//! `ClaudeMapper::on_user_frame` only emits tool results (a live user turn's
//! text is echoed by the `Send` command path, absent here), and its turn
//! boundaries key off `result` frames that transcripts don't contain. So this
//! module owns only the turn-bracketing + user-message reconstruction that the
//! transcript shape needs; every content block goes through the shared helpers.
//!
//! **Bounded.** Transcripts reach tens of MB. The reader caps each line, keeps
//! only a byte/record-budgeted TAIL (the most recent messages), and prepends a
//! [`AgentEvent::Truncated`] marker when it drops older history — never an
//! unbounded buffer. Per-event size caps come for free from the shared helpers
//! (`cap_output`, `edit_diff_content`'s diff budgets).

use std::collections::HashMap;
use std::io::BufRead;
use std::path::Path;

use serde_json::Value;

use crate::claude::{
    edit_diff_content, opt_label, plan_status, tool_kind, tool_locations, tool_result_text,
    tool_title, TaskTracker,
};
use crate::model::{
    cap_output, truncate_label, AgentEvent, PlanEntry, ToolContent, ToolKind, ToolStatus, Usage,
    PLAN_LABEL_MAX, PLAN_TASKS_CAP,
};

/// A single transcript line longer than this is a pathological outlier (a
/// giant embedded blob); read past the cap is discarded so its parse fails and
/// the line is skipped, rather than allocating the whole line.
const MAX_LINE_BYTES: usize = 1024 * 1024;
/// Retained-tail budgets: at most this many of the newest records, and at most
/// this many raw bytes, are translated. Keeps memory bounded and the seeded
/// journal comfortably under the file cap even for a huge transcript.
const TAIL_RECORD_BUDGET: usize = 4000;
const TAIL_BYTE_BUDGET: usize = 2 * 1024 * 1024;

/// Read a claude transcript file and translate its retained tail into the
/// normalized event stream. Returns an empty vec for a missing/empty/unreadable
/// file or one with nothing user-visible. Blocking fs — call off the reactor.
pub fn import_transcript(path: &Path) -> Vec<AgentEvent> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut reader = std::io::BufReader::new(file);

    // Bounded tail: keep only the newest records within both budgets, dropping
    // older ones off the front (a coherent, most-recent slice).
    let mut tail: std::collections::VecDeque<Vec<u8>> = std::collections::VecDeque::new();
    let mut tail_bytes = 0usize;
    let mut dropped = false;
    let mut line: Vec<u8> = Vec::new();
    loop {
        line.clear();
        match read_line_capped(&mut reader, &mut line, MAX_LINE_BYTES) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        tail_bytes += line.len();
        tail.push_back(std::mem::take(&mut line));
        // Keep at least one record; evict from the front while over budget.
        while tail.len() > 1 && (tail.len() > TAIL_RECORD_BUDGET || tail_bytes > TAIL_BYTE_BUDGET) {
            if let Some(front) = tail.pop_front() {
                tail_bytes -= front.len();
                dropped = true;
            }
        }
    }

    let mut out: Vec<AgentEvent> = Vec::new();
    // A dropped front means the UI should show history was clipped (the agent's
    // own transcript on disk remains the full source of truth).
    if dropped {
        out.push(AgentEvent::Truncated);
    }
    let mut tx = Translator::default();
    for raw in &tail {
        if let Ok(value) = serde_json::from_slice::<Value>(raw) {
            tx.on_record(&value, &mut out);
        }
    }
    tx.finish(&mut out);
    out
}

/// Read one `\n`-terminated line, capping `out` at `cap` bytes: the rest of an
/// over-long line is consumed from the reader and discarded (its truncated
/// `out` then fails to parse and is skipped). Returns raw bytes consumed;
/// `Ok(0)` = EOF.
fn read_line_capped<R: BufRead>(
    r: &mut R,
    out: &mut Vec<u8>,
    cap: usize,
) -> std::io::Result<usize> {
    let mut consumed = 0usize;
    loop {
        let available = match r.fill_buf() {
            Ok(buf) => buf,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        if available.is_empty() {
            break; // EOF
        }
        let (used, done) = match available.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (available.len(), false),
        };
        if out.len() < cap {
            let room = cap - out.len();
            out.extend_from_slice(&available[..room.min(used)]);
        }
        r.consume(used);
        consumed += used;
        if done {
            break;
        }
    }
    Ok(consumed)
}

/// Transcript → event translator. A logical turn spans from a user prompt to
/// the next one (or end): it opens on the first assistant activity after a
/// prompt and closes before the next prompt, mirroring the live driver's one
/// turn per user message (so `MessageChunk`s group by turn and the seeded
/// journal has clean `TurnStarted` compaction boundaries).
#[derive(Default)]
struct Translator {
    turn_n: u64,
    turn_open: bool,
    /// tool_use id → kind, so a tool_result renders the same way the live
    /// driver does (edit acks are suppressed, everything else shows output).
    tool_kinds: HashMap<String, ToolKind>,
    /// The same task-list state machine the live driver runs, so imported
    /// history rebuilds its plan panel instead of replaying the bookkeeping
    /// as rows. Shared (not reimplemented) so the two cannot drift.
    task_list: TaskTracker,
}

impl Translator {
    fn turn_id(&self) -> String {
        format!("t{}", self.turn_n)
    }

    fn open_turn(&mut self, out: &mut Vec<AgentEvent>) {
        if !self.turn_open {
            self.turn_n += 1;
            self.turn_open = true;
            out.push(AgentEvent::TurnStarted {
                turn_id: self.turn_id(),
            });
        }
    }

    fn close_turn(&mut self, out: &mut Vec<AgentEvent>) {
        if self.turn_open {
            // Default usage: no cost/tokens/duration is known for imported
            // history, so the UI's end-of-turn strip is elided (it only draws
            // for a real duration) — the turn boundary still groups blocks.
            out.push(AgentEvent::TurnCompleted {
                turn_id: self.turn_id(),
                usage: Usage::default(),
            });
            self.turn_open = false;
        }
    }

    fn on_record(&mut self, rec: &Value, out: &mut Vec<AgentEvent>) {
        // Subagent (sidechain) and injected-meta records are noise the live
        // surface hides too — skip before any translation (mirrors the
        // launcher's `first_prompt_text` filter).
        if rec.get("isSidechain").and_then(Value::as_bool) == Some(true)
            || rec.get("isMeta").and_then(Value::as_bool) == Some(true)
        {
            return;
        }
        match rec.get("type").and_then(Value::as_str) {
            Some("user") => self.on_user(&rec["message"], out),
            Some("assistant") => self.on_assistant(&rec["message"], out),
            // summary / title / system records are handled elsewhere or ignored.
            _ => {}
        }
    }

    fn on_user(&mut self, message: &Value, out: &mut Vec<AgentEvent>) {
        let content = &message["content"];
        // A user record is EITHER a tool-result carrier (part of the current
        // turn) or a human prompt (which opens the next turn) — never both.
        let has_tool_result = content
            .as_array()
            .is_some_and(|blocks| blocks.iter().any(|b| b["type"] == "tool_result"));
        if has_tool_result {
            for block in content.as_array().into_iter().flatten() {
                if block["type"] == "tool_result" {
                    self.on_tool_result(block, out);
                }
            }
            return;
        }
        let Some(text) = user_prompt_text(content) else {
            return;
        };
        self.close_turn(out);
        let attachments = content
            .as_array()
            .map(|blocks| blocks.iter().filter(|b| b["type"] == "image").count() as u32)
            .unwrap_or(0);
        // Seeded history is already-delivered by definition: no delivery id,
        // never queued.
        out.push(AgentEvent::UserMessage {
            text,
            attachments,
            id: None,
            queued: false,
        });
    }

    fn on_assistant(&mut self, message: &Value, out: &mut Vec<AgentEvent>) {
        let Some(blocks) = message["content"].as_array() else {
            return;
        };
        if blocks.is_empty() {
            return;
        }
        self.open_turn(out);
        let turn = self.turn_id();
        for block in blocks {
            match block["type"].as_str() {
                Some("text") => {
                    if let Some(text) = block["text"].as_str() {
                        if !text.is_empty() {
                            out.push(AgentEvent::MessageChunk {
                                turn_id: turn.clone(),
                                text: text.to_string(),
                            });
                        }
                    }
                }
                Some("thinking") => {
                    if let Some(text) = block["thinking"].as_str() {
                        if !text.is_empty() {
                            out.push(AgentEvent::ThoughtChunk {
                                turn_id: turn.clone(),
                                text: text.to_string(),
                            });
                        }
                    }
                }
                Some("tool_use") => self.on_tool_use(block, out),
                _ => {}
            }
        }
    }

    /// Mirror of the driver's `on_tool_use`, but the imported call is already
    /// finished, so it lands `Completed` (its `tool_result` refines status +
    /// output). Edit-family inputs still surface their diff immediately.
    fn on_tool_use(&mut self, block: &Value, out: &mut Vec<AgentEvent>) {
        let id = block["id"].as_str().unwrap_or_default().to_string();
        if id.is_empty() {
            return;
        }
        let name = block["name"].as_str().unwrap_or_default();
        let input = &block["input"];

        // The todo list is a plan panel, not a tool card (as in the driver):
        // `TodoWrite` on older CLIs, the `Task*` family from 2.1.207 on. The
        // latter's snapshot lands on its tool_result, where the ids live.
        if name == "TodoWrite" {
            if let Some(todos) = input["todos"].as_array() {
                let entries = todos
                    .iter()
                    .filter_map(|t| {
                        Some(PlanEntry {
                            content: truncate_label(t["content"].as_str()?, PLAN_LABEL_MAX),
                            status: plan_status(t["status"].as_str().unwrap_or_default()),
                            active_form: opt_label(&t["activeForm"], PLAN_LABEL_MAX),
                            ..Default::default()
                        })
                    })
                    .take(PLAN_TASKS_CAP)
                    .collect();
                out.push(AgentEvent::Plan { entries });
            }
            self.tool_kinds.insert(id, ToolKind::Think);
            return;
        }
        if TaskTracker::is_task_tool(name) {
            self.task_list.on_tool_use(&id, name, input);
            self.tool_kinds.insert(id, ToolKind::Think);
            return;
        }

        let kind = tool_kind(name);
        self.tool_kinds.insert(id.clone(), kind);
        out.push(AgentEvent::ToolCall {
            id: id.clone(),
            kind,
            title: tool_title(name, input),
            locations: tool_locations(input),
            status: ToolStatus::Completed,
        });
        if let Some(diff) = edit_diff_content(name, input) {
            out.push(AgentEvent::ToolCallUpdate {
                id,
                status: ToolStatus::Completed,
                content: Some(diff),
            });
        }
    }

    /// Mirror of the driver's `on_tool_results` (per block): edit acks are
    /// suppressed (the diff card already shows the change); everything else
    /// shows the capped output; `is_error` marks the call failed.
    fn on_tool_result(&mut self, block: &Value, out: &mut Vec<AgentEvent>) {
        let id = block["tool_use_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if id.is_empty() {
            return;
        }
        let failed = block["is_error"].as_bool() == Some(true);
        let kind = self.tool_kinds.get(&id).copied();
        // A task-list call carries its id/state only in the result text; the
        // `tracks` gate keeps that work off every other tool's result.
        if self.task_list.tracks(&id) {
            if let Some(entries) =
                self.task_list
                    .on_tool_result(&id, failed, &tool_result_text(block))
            {
                out.push(AgentEvent::Plan { entries });
            }
        }
        let content = if matches!(kind, Some(ToolKind::Edit)) && !failed {
            None
        } else {
            let (text, truncated) = cap_output(&tool_result_text(block));
            Some(ToolContent::Output { text, truncated })
        };
        out.push(AgentEvent::ToolCallUpdate {
            id,
            status: if failed {
                ToolStatus::Failed
            } else {
                ToolStatus::Completed
            },
            content,
        });
    }

    fn finish(mut self, out: &mut Vec<AgentEvent>) {
        self.close_turn(out);
    }
}

/// The human text of a user record, if it is a real typed prompt: content is a
/// string or text blocks; command invocations (`<…>`) and claude's injected
/// `Caveat:` preamble are not prompts (mirrors the launcher's
/// `first_prompt_text`). `None` = not a prompt (skip).
fn user_prompt_text(content: &Value) -> Option<String> {
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    let text = text.trim();
    if text.is_empty() || text.starts_with('<') || text.starts_with("Caveat:") {
        return None;
    }
    Some(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small but representative transcript: a typed prompt, an assistant
    /// message with thinking + prose + an Edit + a Bash tool_use, their
    /// tool_results (one success, one failure), a second turn, plus records
    /// that MUST be skipped — a sidechain, an injected meta, and a
    /// command-invocation user line.
    fn fixture() -> String {
        // NDJSON: exactly one record per line (no embedded newlines), as the
        // real `~/.claude/projects/*.jsonl` transcripts are written.
        let lines = [
            // Injected preamble — skipped (starts with "Caveat:").
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"Caveat: injected"}}"#,
            // A command invocation — skipped (starts with '<').
            r#"{"type":"user","message":{"role":"user","content":"<command-name>/init</command-name>"}}"#,
            // Real typed prompt (turn 1).
            r#"{"type":"user","uuid":"u1","message":{"role":"user","content":"fix the bug"}}"#,
            // Assistant: thinking + prose + Edit + Bash.
            r#"{"type":"assistant","uuid":"a1","message":{"id":"m1","model":"claude","content":[{"type":"thinking","thinking":"let me look"},{"type":"text","text":"On it."},{"type":"tool_use","id":"tu_edit","name":"Edit","input":{"file_path":"/tmp/x.rs","old_string":"a","new_string":"b"}},{"type":"tool_use","id":"tu_bash","name":"Bash","input":{"command":"cargo test"}}]}}"#,
            // A subagent frame — skipped (isSidechain).
            r#"{"type":"assistant","isSidechain":true,"message":{"id":"sub","content":[{"type":"text","text":"secret subagent"}]}}"#,
            // Tool results: the edit ack (suppressed) and a failing bash.
            r#"{"type":"user","uuid":"u2","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu_edit","content":"File updated"},{"type":"tool_result","tool_use_id":"tu_bash","content":"boom","is_error":true}]}}"#,
            // Assistant closing prose for turn 1.
            r#"{"type":"assistant","uuid":"a2","message":{"id":"m2","content":[{"type":"text","text":"Done."}]}}"#,
            // Second typed prompt (turn 2).
            r#"{"type":"user","uuid":"u3","message":{"role":"user","content":[{"type":"text","text":"thanks"}]}}"#,
            r#"{"type":"assistant","uuid":"a3","message":{"id":"m3","content":[{"type":"text","text":"Anytime."}]}}"#,
        ];
        lines.join("\n") + "\n"
    }

    fn import_str(s: &str) -> Vec<AgentEvent> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, s).unwrap();
        import_transcript(&path)
    }

    #[test]
    fn translates_turns_tools_and_skips_noise() {
        let events = import_str(&fixture());

        // No sidechain/meta/command content leaked in.
        for ev in &events {
            if let AgentEvent::MessageChunk { text, .. } | AgentEvent::ThoughtChunk { text, .. } =
                ev
            {
                assert!(!text.contains("subagent"), "sidechain leaked: {text:?}");
                assert!(!text.contains("Caveat"), "meta leaked: {text:?}");
            }
        }

        // Turn 1 opens after the first real prompt, brackets its blocks, and
        // closes before turn 2's prompt.
        assert_eq!(
            events[0],
            AgentEvent::UserMessage {
                text: "fix the bug".into(),
                attachments: 0,
                id: None,
                queued: false,
            }
        );
        assert!(matches!(&events[1], AgentEvent::TurnStarted { turn_id } if turn_id == "t1"));
        assert_eq!(
            events[2],
            AgentEvent::ThoughtChunk {
                turn_id: "t1".into(),
                text: "let me look".into()
            }
        );
        assert_eq!(
            events[3],
            AgentEvent::MessageChunk {
                turn_id: "t1".into(),
                text: "On it.".into()
            }
        );
        // The Edit tool_use lands Completed with its diff surfaced from inputs.
        match &events[4] {
            AgentEvent::ToolCall {
                id,
                kind,
                status,
                title,
                ..
            } => {
                assert_eq!(id, "tu_edit");
                assert_eq!(*kind, ToolKind::Edit);
                assert_eq!(*status, ToolStatus::Completed);
                assert!(title.starts_with("Edit: /tmp/x.rs"));
            }
            other => panic!("expected Edit ToolCall, got {other:?}"),
        }
        assert!(matches!(
            &events[5],
            AgentEvent::ToolCallUpdate {
                id,
                content: Some(ToolContent::Diff { .. }),
                ..
            } if id == "tu_edit"
        ));
        assert!(matches!(
            &events[6],
            AgentEvent::ToolCall { id, kind: ToolKind::Execute, .. } if id == "tu_bash"
        ));

        // Tool results: the edit ack is suppressed (content None, still
        // Completed), the bash failure carries its output and Failed status.
        let edit_result = events.iter().find(|e| {
            matches!(e, AgentEvent::ToolCallUpdate { id, content: None, status: ToolStatus::Completed } if id == "tu_edit")
        });
        assert!(edit_result.is_some(), "edit ack should be suppressed");
        let bash_result = events.iter().find_map(|e| match e {
            AgentEvent::ToolCallUpdate {
                id,
                status: ToolStatus::Failed,
                content: Some(ToolContent::Output { text, .. }),
            } if id == "tu_bash" => Some(text.clone()),
            _ => None,
        });
        assert_eq!(bash_result.as_deref(), Some("boom"));

        // Turn 1 closes, turn 2 opens with its own id, and the stream ends with
        // a closing TurnCompleted (so replayed `running` settles to false).
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnCompleted { turn_id, .. } if turn_id == "t1")));
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::UserMessage { text, .. } if text == "thanks")));
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnStarted { turn_id } if turn_id == "t2")));
        assert!(matches!(
            events.last(),
            Some(AgentEvent::TurnCompleted { turn_id, .. }) if turn_id == "t2"
        ));

        // No Truncated marker for a small transcript.
        assert!(!events.iter().any(|e| matches!(e, AgentEvent::Truncated)));
    }

    #[test]
    fn empty_or_missing_transcript_yields_no_events() {
        assert!(import_transcript(Path::new("/nonexistent/does-not-exist.jsonl")).is_empty());
        assert!(import_str("").is_empty());
        // A transcript with only skippable noise imports nothing.
        let noise = "\n \n{\"type\":\"summary\",\"summary\":\"x\"}\n";
        assert!(import_str(noise).is_empty());
    }

    #[test]
    fn oversized_transcript_imports_a_bounded_truncated_tail() {
        // Far more records than the record budget: the import must keep only a
        // bounded tail and prepend exactly one Truncated marker.
        let mut s = String::new();
        for i in 0..(TAIL_RECORD_BUDGET + 500) {
            s.push_str(&format!(
                r#"{{"type":"user","message":{{"role":"user","content":"msg {i}"}}}}"#
            ));
            s.push('\n');
        }
        let events = import_str(&s);
        assert_eq!(
            events.first(),
            Some(&AgentEvent::Truncated),
            "clipped history is marked"
        );
        let users = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::UserMessage { .. }))
            .count();
        assert!(users <= TAIL_RECORD_BUDGET, "tail is bounded: {users}");
        // The newest message survives; the oldest was dropped.
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::UserMessage { text, .. } if text == &format!("msg {}", TAIL_RECORD_BUDGET + 499)
        )));
        assert!(!events
            .iter()
            .any(|e| matches!(e, AgentEvent::UserMessage { text, .. } if text == "msg 0")));
    }

    #[test]
    fn a_single_oversized_line_is_skipped_not_buffered() {
        // A line past the per-line cap fails to parse (truncated) and is
        // skipped, while surrounding real records import fine.
        let big = "z".repeat(MAX_LINE_BYTES + 4096);
        let s = format!(
            "{}\n{}\n",
            format_args!(
                r#"{{"type":"assistant","message":{{"id":"m1","content":[{{"type":"text","text":"{big}"}}]}}}}"#
            ),
            r#"{"type":"user","message":{"role":"user","content":"still here"}}"#
        );
        let events = import_str(&s);
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::UserMessage { text, .. } if text == "still here")));
        // The giant assistant text never made it into an event.
        assert!(!events.iter().any(
            |e| matches!(e, AgentEvent::MessageChunk { text, .. } if text.len() > MAX_LINE_BYTES)
        ));
    }
}
