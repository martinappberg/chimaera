//! The daemon's MCP server: linked-terminal tools for agent sessions.
//!
//! Speaks MCP's streamable-HTTP transport statelessly — every message is a
//! JSON-RPC POST answered with a plain JSON body (the spec allows servers
//! to skip SSE entirely). Each agent session gets its own endpoint
//! `/api/v1/mcp/{agent_id}?key={secret}` wired in via a generated
//! `--mcp-config`; the per-session key authorizes it (same pattern as hook
//! ingestion — the agent cannot know the daemon token).
//!
//! Scope is the whole point: the tools see exactly the terminals the user
//! linked to this agent (see `links`), resolved by session id or display
//! name. `@term:NAME` mentions in user prompts auto-link (the mention *is*
//! the consent — it arrives through the UserPromptSubmit hook, which only
//! fires for the human's own composer input).

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::AppState;

/// Protocol version offered when the client's is unknown to us.
const PROTOCOL_FALLBACK: &str = "2025-06-18";
/// Journal entries returned by read_terminal when unspecified.
const DEFAULT_READ_COMMANDS: usize = 5;
/// Screen lines returned by read_terminal in screen mode.
const SCREEN_LINES: usize = 120;

/// Instructions injected into the agent's context at initialize.
const INSTRUCTIONS: &str = "\
Chimaera linked terminals: the user can link live terminal sessions (often \
long-lived shells with modules loaded, conda envs active, or an ssh session \
to a cluster) to this agent. Linked terminals are this session's entire \
scope — if a terminal isn't linked, ask the user to link it (they drag a \
terminal onto this pane, use its top-bar menu, or mention it as @term:NAME \
in a message; you cannot link one yourself).\n\
When the user writes @term:NAME they mean a linked terminal: resolve it \
with list_terminals, run commands there with run_in_terminal, and inspect \
recent activity with read_terminal (prefer reading before acting). \
Commands run in a real interactive shell the user is watching and sharing: \
run one thing at a time, keep commands short-running, and start long jobs \
with the shell's own facilities (sbatch, nohup ... &) or a larger \
timeout_ms. If the shell is busy your exec queues until its prompt \
returns. State changes (cd, module load, exports) persist in the shell — \
that is usually why the user linked it.";

#[derive(serde::Deserialize)]
pub(crate) struct McpQuery {
    #[serde(default)]
    key: String,
}

/// POST /api/v1/mcp/{agent_id}?key={secret} — the MCP endpoint.
pub(crate) async fn mcp(
    State(state): State<Arc<AppState>>,
    Path(agent_id): Path<String>,
    Query(query): Query<McpQuery>,
    Json(message): Json<Value>,
) -> Response {
    {
        let agents = crate::lock(&state.agents);
        let Some(record) = agents.get(&agent_id) else {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("unknown agent session {agent_id}")})),
            )
                .into_response();
        };
        if record.key != query.key {
            return (StatusCode::FORBIDDEN, Json(json!({"error": "bad key"}))).into_response();
        }
    }

    let method = message.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = message.get("id").cloned();
    // Notifications (no id) are acknowledged and otherwise ignored.
    let Some(id) = id else {
        return StatusCode::ACCEPTED.into_response();
    };
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));

    let result = match method {
        "initialize" => Ok(initialize_result(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_defs() })),
        "tools/call" => tools_call(&state, &agent_id, &params).await,
        other => Err((-32601, format!("method not found: {other}"))),
    };

    let body = match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, message)) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": code, "message": message},
        }),
    };
    Json(body).into_response()
}

fn initialize_result(params: &Value) -> Value {
    // Echo a protocol version we can serve; the shapes we use are stable
    // across all published revisions.
    let requested = params
        .get("protocolVersion")
        .and_then(|v| v.as_str())
        .unwrap_or(PROTOCOL_FALLBACK);
    json!({
        "protocolVersion": requested,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "chimaera", "version": chimaera_core::VERSION },
        "instructions": INSTRUCTIONS,
    })
}

fn tool_defs() -> Value {
    json!([
        {
            "name": "list_terminals",
            "description": "List the terminal sessions linked to this agent: name, id, \
                            working directory, shell state (ready/running), and the most \
                            recent command. Linked terminals are the only ones reachable.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
        },
        {
            "name": "run_in_terminal",
            "description": "Type a command into a linked terminal's live shell and wait for \
                            it to finish; returns its output and exit code. The shell's \
                            state (cwd, env, modules, remote ssh) applies and persists. If \
                            a command is already running, this queues until the prompt \
                            returns (bounded by queue_timeout_ms).",
            "inputSchema": {
                "type": "object",
                "required": ["terminal", "command"],
                "properties": {
                    "terminal": {
                        "type": "string",
                        "description": "Linked terminal: session id or display name (as in @term:NAME)",
                    },
                    "command": {
                        "type": "string",
                        "description": "Single-line shell command (join steps with ';' or '&&')",
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Max runtime once typed (default 30000, cap 3600000). \
                                        On timeout the command keeps running and partial \
                                        output is returned.",
                    },
                    "queue_timeout_ms": {
                        "type": "integer",
                        "description": "Max wait for the shell's prompt before typing \
                                        (default 15000, cap 600000; 0 = only if free now)",
                    },
                },
                "additionalProperties": false,
            },
        },
        {
            "name": "read_terminal",
            "description": "Read a linked terminal's recent activity: the command journal \
                            (commands, outputs, exit codes) by default, or the visible \
                            screen text with screen=true (for TUIs, boot output, or shells \
                            without integration marks).",
            "inputSchema": {
                "type": "object",
                "required": ["terminal"],
                "properties": {
                    "terminal": {
                        "type": "string",
                        "description": "Linked terminal: session id or display name",
                    },
                    "commands": {
                        "type": "integer",
                        "description": "How many recent journal entries (default 5, cap 50)",
                    },
                    "screen": {
                        "type": "boolean",
                        "description": "Return the visible screen text instead of the journal",
                    },
                },
                "additionalProperties": false,
            },
        },
    ])
}

/// Result content for a successful tool call.
fn tool_text(text: String) -> Value {
    json!({ "content": [{ "type": "text", "text": text }] })
}

/// Result content for a failed tool call (isError; the model sees the text).
fn tool_error(text: String) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": true })
}

async fn tools_call(
    state: &Arc<AppState>,
    agent_id: &str,
    params: &Value,
) -> Result<Value, (i64, String)> {
    let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    match name {
        "list_terminals" => Ok(list_terminals(state, agent_id)),
        "run_in_terminal" => Ok(run_in_terminal(state, agent_id, &args).await),
        "read_terminal" => Ok(read_terminal(state, agent_id, &args)),
        other => Err((-32602, format!("unknown tool: {other}"))),
    }
}

/// Display name of a (shell) session, matching what the UI shows.
pub(crate) fn display_name_of(state: &AppState, id: &str) -> Option<String> {
    let info = state.sessions.get(id)?;
    if info.renamed {
        return Some(info.name);
    }
    let polled = crate::lock(&state.display_names).get(id).cloned();
    Some(crate::naming::shell_display_name(&info, polled.as_deref()))
}

/// Resolve a terminal reference (session id or display name, with an
/// optional `@term:` prefix and quotes) against `candidates`.
pub(crate) fn resolve_terminal(
    state: &AppState,
    token: &str,
    candidates: &[String],
) -> Result<String, String> {
    let token = token
        .trim()
        .trim_start_matches("@term:")
        .trim_matches(|c| c == '"' || c == '\'');
    if token.is_empty() {
        return Err("empty terminal reference".to_string());
    }
    if candidates.iter().any(|id| id == token) {
        return Ok(token.to_string());
    }
    let wanted = token.to_lowercase();
    let matches: Vec<&String> = candidates
        .iter()
        .filter(|id| {
            display_name_of(state, id).is_some_and(|name| name.to_lowercase() == wanted)
        })
        .collect();
    match matches.as_slice() {
        [only] => Ok((*only).clone()),
        [] => Err(format!("no terminal '{token}' found")),
        several => Err(format!(
            "'{token}' is ambiguous — use a session id: {}",
            several
                .iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Resolve within the agent's linked scope, with a helpful error when the
/// agent has no links at all.
fn resolve_linked(state: &AppState, agent_id: &str, args: &Value) -> Result<String, String> {
    let scope = crate::links::terminals_of(state, agent_id);
    if scope.is_empty() {
        return Err(
            "no terminals are linked to this agent session. Ask the user to link one: \
             they can drag a terminal onto this pane, use the terminal's top-bar menu \
             ('Link to agent…'), or mention it as @term:NAME in a message."
                .to_string(),
        );
    }
    let token = args
        .get("terminal")
        .and_then(|t| t.as_str())
        .unwrap_or_default();
    resolve_terminal(state, token, &scope).map_err(|err| {
        format!(
            "{err} among linked terminals ({})",
            scope
                .iter()
                .map(|id| {
                    let name = display_name_of(state, id).unwrap_or_default();
                    format!("'{name}' [{id}]")
                })
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

fn list_terminals(state: &AppState, agent_id: &str) -> Value {
    let scope = crate::links::terminals_of(state, agent_id);
    if scope.is_empty() {
        return tool_text(
            "No linked terminals. The user can link one by dragging a terminal onto \
             this pane, via the terminal's top-bar menu, or by mentioning @term:NAME."
                .to_string(),
        );
    }
    let execs = crate::lock(&state.exec_status).clone();
    let mut out = String::new();
    for id in &scope {
        let Some(info) = state.sessions.get(id) else {
            continue;
        };
        let name = display_name_of(state, id).unwrap_or_else(|| info.name.clone());
        let marks = state.sessions.marks(id);
        let phase = marks
            .as_ref()
            .map(|m| m.phase().as_str())
            .unwrap_or("unknown");
        let stage = execs
            .get(id)
            .map(|s| format!(" · agent exec {}", json!(s).as_str().unwrap_or("?")))
            .unwrap_or_default();
        out.push_str(&format!(
            "'{name}' [{id}] — {phase}{stage} · cwd {}\n",
            info.cwd.display()
        ));
        if let Some(last) = marks.and_then(|m| m.journal(1).pop()) {
            let cmd = last.command.as_deref().unwrap_or("(unknown command)");
            if last.running {
                out.push_str(&format!("  running: $ {cmd}\n"));
            } else {
                let exit = last
                    .exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "?".to_string());
                out.push_str(&format!("  last: $ {cmd} → exit {exit}\n"));
            }
        }
    }
    tool_text(out.trim_end().to_string())
}

async fn run_in_terminal(state: &Arc<AppState>, agent_id: &str, args: &Value) -> Value {
    let id = match resolve_linked(state, agent_id, args) {
        Ok(id) => id,
        Err(err) => return tool_error(err),
    };
    let Some(command) = args.get("command").and_then(|c| c.as_str()) else {
        return tool_error("missing required argument: command".to_string());
    };
    let timeout_ms = args.get("timeout_ms").and_then(|v| v.as_u64());
    let queue_timeout_ms = args.get("queue_timeout_ms").and_then(|v| v.as_u64());

    match crate::api::run_exec(state, &id, command.to_string(), timeout_ms, queue_timeout_ms)
        .await
    {
        Ok(outcome) => {
            let mode = json!(outcome.mode);
            let mode = mode.as_str().unwrap_or("?");
            let secs = outcome
                .record
                .ended_at_ms
                .map(|end| end.saturating_sub(outcome.record.started_at_ms))
                .unwrap_or(0) as f64
                / 1000.0;
            let mut head = if outcome.timed_out {
                format!(
                    "TIMED OUT — still running in the terminal ({mode} mode); \
                     output so far below. Check later with read_terminal."
                )
            } else {
                let exit = outcome
                    .record
                    .exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                format!("exit {exit} · {secs:.1}s · {mode} mode")
            };
            if outcome.waited_ms > 500 {
                head.push_str(&format!(
                    " · queued {:.1}s for the prompt",
                    outcome.waited_ms as f64 / 1000.0
                ));
            }
            let output = if outcome.record.output.is_empty() {
                "(no output)".to_string()
            } else {
                outcome.record.output.clone()
            };
            tool_text(format!("{head}\n{output}"))
        }
        Err(err) => tool_error(err.to_string()),
    }
}

fn read_terminal(state: &AppState, agent_id: &str, args: &Value) -> Value {
    let id = match resolve_linked(state, agent_id, args) {
        Ok(id) => id,
        Err(err) => return tool_error(err),
    };
    if args.get("screen").and_then(|s| s.as_bool()).unwrap_or(false) {
        let screen = state
            .sessions
            .screen_text(&id, SCREEN_LINES)
            .unwrap_or_default();
        return tool_text(if screen.is_empty() {
            "(screen is empty)".to_string()
        } else {
            screen
        });
    }
    let Some(marks) = state.sessions.marks(&id) else {
        return tool_error(format!("terminal {id} is gone"));
    };
    let limit = args
        .get("commands")
        .and_then(|v| v.as_u64())
        .map_or(DEFAULT_READ_COMMANDS, |v| (v as usize).clamp(1, 50));
    let entries = marks.journal(limit);
    let phase = marks.phase().as_str();
    if entries.is_empty() {
        return tool_text(format!(
            "phase: {phase} — journal empty (no commands seen yet, or this shell \
             has no integration marks). Use screen=true to read the visible screen."
        ));
    }
    let mut out = format!("phase: {phase} — last {} command(s):\n", entries.len());
    for entry in entries {
        let cmd = entry.command.as_deref().unwrap_or("(unknown command)");
        let cwd = entry
            .cwd
            .as_deref()
            .map(|c| format!(" @ {c}"))
            .unwrap_or_default();
        if entry.running {
            out.push_str(&format!("\n[{}] $ {cmd}{cwd} — RUNNING\n", entry.seq));
        } else {
            let exit = entry
                .exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "?".to_string());
            let secs = entry
                .ended_at_ms
                .map(|end| end.saturating_sub(entry.started_at_ms))
                .unwrap_or(0) as f64
                / 1000.0;
            out.push_str(&format!(
                "\n[{}] $ {cmd}{cwd} → exit {exit} ({secs:.1}s)\n",
                entry.seq
            ));
        }
        if !entry.output.is_empty() {
            out.push_str(&entry.output);
            out.push('\n');
        }
    }
    tool_text(out.trim_end().to_string())
}

/// Scan a user prompt for `@term:NAME` mentions and link every resolvable
/// one to `agent_id` (the mention is the user's consent). Returns
/// human-readable notes for the agent's context; empty when nothing matched.
pub(crate) fn autolink_mentions(state: &AppState, agent_id: &str, prompt: &str) -> Vec<String> {
    let mut notes = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for token in mention_tokens(prompt) {
        if !seen.insert(token.clone()) {
            continue;
        }
        // Mentions resolve across every non-agent session in the daemon —
        // the whole point is granting access to a not-yet-linked terminal.
        let candidates: Vec<String> = {
            let agents = crate::lock(&state.agents);
            state
                .sessions
                .list()
                .into_iter()
                .filter(|info| !agents.contains_key(&info.id))
                .map(|info| info.id)
                .collect()
        };
        match resolve_terminal(state, &token, &candidates) {
            Ok(terminal_id) => {
                let already = crate::lock(&state.links)
                    .get(&terminal_id)
                    .is_some_and(|a| a == agent_id);
                if already {
                    continue;
                }
                let name = display_name_of(state, &terminal_id).unwrap_or_default();
                match crate::links::link(state, &terminal_id, agent_id) {
                    Ok(_) => notes.push(format!(
                        "[chimaera] Linked terminal '{name}' [{terminal_id}] to this \
                         session — reachable via run_in_terminal/read_terminal."
                    )),
                    Err((_, err)) => notes.push(format!(
                        "[chimaera] Could not link @term:{token}: {err}"
                    )),
                }
            }
            Err(err) => notes.push(format!("[chimaera] @term:{token}: {err}")),
        }
    }
    notes
}

/// Extract mention tokens: `@term:name`, `@term:"name with spaces"`.
fn mention_tokens(prompt: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut rest = prompt;
    while let Some(idx) = rest.find("@term:") {
        rest = &rest[idx + "@term:".len()..];
        let token = match rest.chars().next() {
            Some(quote @ ('"' | '\'')) => {
                let inner = &rest[1..];
                match inner.find(quote) {
                    Some(end) => &inner[..end],
                    None => "",
                }
            }
            // The name starts right after the colon — `@term: end` is a
            // dangling mention, not a reference to "end".
            _ => rest
                .split(char::is_whitespace)
                .next()
                .unwrap_or("")
                .trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']']),
        };
        if !token.is_empty() {
            tokens.push(token.to_string());
        }
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mention_tokens_plain_quoted_and_punctuated() {
        assert_eq!(
            mention_tokens("run ls in @term:cluster please"),
            vec!["cluster"]
        );
        assert_eq!(
            mention_tokens("check @term:\"gpu shell\" and @term:s-1234abcd."),
            vec!["gpu shell", "s-1234abcd"]
        );
        assert_eq!(
            mention_tokens("(@term:a, @term:b) @term:'c d'!"),
            vec!["a", "b", "c d"]
        );
        assert!(mention_tokens("no mentions here, term: nope").is_empty());
        assert!(mention_tokens("dangling @term: end").is_empty());
    }
}
