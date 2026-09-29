//! What a session changed without git (plan §8): the agent's own edits, per
//! file, in order — from the before/after text its Edit and Write calls and
//! codex's patches carry. Chat sessions read them from the chat journal;
//! claude TUI sessions from claude's transcript, through the existing
//! importer (`chimaera_agent::transcript`). A change a shell command made
//! (a script, `sed`, a pipeline's outputs) has no before and after in either,
//! so it never appears here — the UI says so.
//!
//! Bounded: the journal is capped at 4 MiB and the importer keeps a 2 MiB
//! tail; the answer holds at most `FILES_MAX` files, `EDITS_MAX` edits, each
//! side of a diff at most `TEXT_MAX` bytes, and `ANSWER_MAX` bytes of text.

use std::collections::HashMap;
use std::io::BufRead;
use std::path::Path;

use serde::Serialize;

use chimaera_agent::model::{AgentEvent, ToolContent, ToolKind, ToolStatus};

const FILES_MAX: usize = 100;
const EDITS_MAX: usize = 300;
const TEXT_MAX: usize = 16 * 1024;
const ANSWER_MAX: usize = 1024 * 1024;
/// A journal line past this is skipped (the journal's own entry cap is
/// 256 KiB).
const JOURNAL_LINE_MAX: usize = 512 * 1024;
/// Tool calls tracked while scanning (a runaway journal can't grow the map).
const CALLS_MAX: usize = 4096;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub(crate) struct Edit {
    /// When the edit was announced (ms), when the source says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ts: Option<u64>,
    /// The replaced text; absent for a whole-file write (and codex's
    /// patches, whose hunks ride `new_text`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) old_text: Option<String>,
    pub(crate) new_text: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) truncated: bool,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub(crate) struct FileEdits {
    pub(crate) path: String,
    pub(crate) edits: Vec<Edit>,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub(crate) struct Extracted {
    /// Files in the order the session first changed them.
    pub(crate) files: Vec<FileEdits>,
    pub(crate) edits: usize,
    /// Something was left out by the caps.
    pub(crate) truncated: bool,
    /// The session ran shell commands — changes those made have no
    /// before/after here (the UI says so where no repository shows them).
    pub(crate) ran_commands: bool,
}

struct Call {
    ts: Option<u64>,
    failed: bool,
    diffs: Vec<(String, Option<String>, String, bool)>,
}

fn diffs_of(content: &ToolContent, out: &mut Vec<(String, Option<String>, String, bool)>) {
    match content {
        ToolContent::Diff {
            path,
            old_text,
            new_text,
            truncated,
        } => out.push((path.clone(), old_text.clone(), new_text.clone(), *truncated)),
        ToolContent::Batch { diffs } => {
            for d in diffs {
                diffs_of(d, out);
            }
        }
        _ => {}
    }
}

fn clip(text: String) -> (String, bool) {
    if text.len() <= TEXT_MAX {
        return (text, false);
    }
    let mut end = TEXT_MAX;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

/// Gather the edits from an event stream (`ts` when the source has one).
/// An edit whose tool call failed (denied, or the write itself failed)
/// changed nothing and is left out; the latest diff a call carried wins
/// (claude announces its diff up front, codex on completion).
pub(crate) fn extract(events: impl IntoIterator<Item = (Option<u64>, AgentEvent)>) -> Extracted {
    let mut calls: HashMap<String, Call> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut ran_commands = false;
    for (ts, ev) in events {
        match ev {
            AgentEvent::ToolCall {
                kind: ToolKind::Execute,
                ..
            } => ran_commands = true,
            AgentEvent::ToolCall {
                id,
                kind: ToolKind::Edit,
                status,
                ..
            } => {
                if calls.len() >= CALLS_MAX {
                    continue;
                }
                let call = calls.entry(id.clone()).or_insert_with(|| {
                    order.push(id);
                    Call {
                        ts,
                        failed: false,
                        diffs: Vec::new(),
                    }
                });
                if status == ToolStatus::Failed {
                    call.failed = true;
                }
            }
            AgentEvent::ToolCallUpdate {
                id,
                status,
                content,
            } => {
                let mut diffs = Vec::new();
                if let Some(content) = &content {
                    diffs_of(content, &mut diffs);
                }
                let known = calls.contains_key(&id);
                if !known && (diffs.is_empty() || calls.len() >= CALLS_MAX) {
                    continue;
                }
                let call = calls.entry(id.clone()).or_insert_with(|| {
                    order.push(id);
                    Call {
                        ts,
                        failed: false,
                        diffs: Vec::new(),
                    }
                });
                if status == ToolStatus::Failed {
                    call.failed = true;
                }
                if !diffs.is_empty() {
                    call.diffs = diffs;
                }
            }
            _ => {}
        }
    }

    let mut out = Extracted {
        ran_commands,
        ..Extracted::default()
    };
    let mut at: HashMap<String, usize> = HashMap::new();
    let mut bytes = 0usize;
    for id in order {
        let Some(call) = calls.remove(&id) else {
            continue;
        };
        if call.failed {
            continue;
        }
        for (path, old_text, new_text, truncated) in call.diffs {
            if out.edits >= EDITS_MAX || bytes >= ANSWER_MAX {
                out.truncated = true;
                break;
            }
            let slot = match at.get(&path) {
                Some(&i) => i,
                None => {
                    if out.files.len() >= FILES_MAX {
                        out.truncated = true;
                        continue;
                    }
                    at.insert(path.clone(), out.files.len());
                    out.files.push(FileEdits {
                        path: path.clone(),
                        edits: Vec::new(),
                    });
                    out.files.len() - 1
                }
            };
            let (new_text, cut_new) = clip(new_text);
            let (old_text, cut_old) = match old_text {
                Some(t) => {
                    let (t, cut) = clip(t);
                    (Some(t), cut)
                }
                None => (None, false),
            };
            bytes += new_text.len() + old_text.as_ref().map_or(0, String::len);
            out.files[slot].edits.push(Edit {
                ts: call.ts,
                old_text,
                new_text,
                truncated: truncated || cut_new || cut_old,
            });
            out.edits += 1;
        }
    }
    out
}

/// A chat journal's events, streamed line by line. BLOCKING.
pub(crate) fn journal_events(path: &Path) -> Vec<(Option<u64>, AgentEvent)> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut reader = std::io::BufReader::new(file);
    let mut out = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if line.len() > JOURNAL_LINE_MAX {
            continue;
        }
        // Only tool rows matter (edits, and whether commands ran); skip the
        // parse for everything else.
        let is_tool = line
            .windows(b"\"tool_call".len())
            .any(|w| w == b"\"tool_call");
        if !is_tool {
            continue;
        }
        if let Ok(entry) = serde_json::from_slice::<chimaera_agent::journal::SeqEvent>(&line) {
            out.push((Some(entry.ts), entry.ev));
        }
    }
    out
}

/// A claude transcript's events through the shared importer (bounded tail).
/// BLOCKING.
pub(crate) fn transcript_events(path: &Path) -> Vec<(Option<u64>, AgentEvent)> {
    chimaera_agent::transcript::import_transcript(path)
        .into_iter()
        .map(|ev| (None, ev))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str) -> AgentEvent {
        AgentEvent::ToolCall {
            id: id.into(),
            kind: ToolKind::Edit,
            title: "Edit".into(),
            locations: vec![],
            status: ToolStatus::InProgress,
            cross_turn: false,
            command: None,
        }
    }

    fn diff(path: &str, old: Option<&str>, new: &str) -> ToolContent {
        ToolContent::Diff {
            path: path.into(),
            old_text: old.map(str::to_string),
            new_text: new.into(),
            truncated: false,
        }
    }

    fn update(id: &str, status: ToolStatus, content: Option<ToolContent>) -> AgentEvent {
        AgentEvent::ToolCallUpdate {
            id: id.into(),
            status,
            content,
        }
    }

    #[test]
    fn edits_group_per_file_in_order_and_skip_failures() {
        let events = vec![
            (Some(1), call("a")),
            (
                Some(1),
                update(
                    "a",
                    ToolStatus::InProgress,
                    Some(diff("/w/qc.py", Some("x"), "y")),
                ),
            ),
            (Some(2), update("a", ToolStatus::Completed, None)),
            (Some(3), call("b")),
            (
                Some(3),
                update(
                    "b",
                    ToolStatus::InProgress,
                    Some(diff("/w/b.py", None, "new")),
                ),
            ),
            (Some(4), call("c")),
            (
                Some(4),
                update(
                    "c",
                    ToolStatus::InProgress,
                    Some(diff("/w/qc.py", Some("y"), "z")),
                ),
            ),
            (Some(5), call("d")),
            (
                Some(5),
                update(
                    "d",
                    ToolStatus::InProgress,
                    Some(diff("/w/denied.py", None, "no")),
                ),
            ),
            (Some(6), update("d", ToolStatus::Failed, None)),
            // Codex: the patch arrives on completion, as a batch.
            (
                Some(7),
                update(
                    "e",
                    ToolStatus::Completed,
                    Some(ToolContent::Batch {
                        diffs: vec![diff("/w/b.py", None, "@@ hunk")],
                    }),
                ),
            ),
        ];
        let out = extract(events);
        let paths: Vec<&str> = out.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["/w/qc.py", "/w/b.py"]);
        assert_eq!(out.files[0].edits.len(), 2);
        assert_eq!(out.files[0].edits[0].old_text.as_deref(), Some("x"));
        assert_eq!(out.files[0].edits[1].new_text, "z");
        assert_eq!(out.files[1].edits.len(), 2);
        assert_eq!(out.files[1].edits[1].ts, Some(7));
        assert_eq!(out.edits, 4);
        assert!(!out.truncated);
        assert!(!out.ran_commands, "no shell command ran");

        let ran = extract(vec![(
            None,
            AgentEvent::ToolCall {
                id: "x".into(),
                kind: ToolKind::Execute,
                title: "Bash: make".into(),
                locations: vec![],
                status: ToolStatus::Completed,
                cross_turn: false,
                command: Some("make".into()),
            },
        )]);
        assert!(ran.ran_commands);
    }

    #[test]
    fn big_diffs_are_clipped_and_marked() {
        let big = "x".repeat(TEXT_MAX + 10);
        let out = extract(vec![
            (None, call("a")),
            (
                None,
                update("a", ToolStatus::Completed, Some(diff("/w/a", None, &big))),
            ),
        ]);
        let edit = &out.files[0].edits[0];
        assert_eq!(edit.new_text.len(), TEXT_MAX);
        assert!(edit.truncated);
    }

    #[test]
    fn journal_lines_parse_and_other_rows_are_skipped() {
        let dir = std::env::temp_dir().join(format!("chimaera-edits-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let rows = [
            serde_json::json!({"seq": 1, "ts": 10, "ev": {"type": "turn_started", "turn_id": "t"}}),
            serde_json::json!({"seq": 2, "ts": 11, "ev": {"type": "tool_call", "id": "a", "kind": "edit", "title": "Edit", "status": "in_progress"}}),
            serde_json::json!({"seq": 3, "ts": 12, "ev": {"type": "tool_call_update", "id": "a", "status": "completed",
                "content": {"kind": "diff", "path": "/w/a.py", "old_text": "1", "new_text": "2"}}}),
        ];
        let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
        std::fs::write(&path, body).unwrap();
        let events = journal_events(&path);
        assert_eq!(events.len(), 2, "{events:?}");
        let out = extract(events);
        assert_eq!(out.files[0].path, "/w/a.py");
        assert_eq!(out.files[0].edits[0].ts, Some(11));
        let _ = std::fs::remove_dir_all(dir);
    }
}
