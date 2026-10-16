//! Each agent account's plan usage, read live for Agent settings: the same
//! windows a chat's usage popover shows, without a chat open.
//!
//! - claude: a short-lived `claude -p` stream-json process answers the
//!   `initialize` and `get_usage` controls. No model call, no MCP servers
//!   (`--strict-mcp-config`), no hooks (`disableAllHooks` for this process
//!   only); live 2.1.300 answers in under two seconds and writes nothing to
//!   the user's settings.
//! - codex: a short-lived `codex app-server` answers
//!   `account/rateLimits/read` (PROTOCOL.md Pass 46).
//! - grok: a short-lived `grok agent stdio` (ACP) answers `_x.ai/billing`
//!   right after `initialize` — no session, no turn (live 1.0.46).
//! - agy: the Antigravity CLI's own `agy -p /usage --output-format json`,
//!   which its changelog says answers "without starting an agent turn,
//!   spending quota" (live 1.3.4: `num_turns: 0`). Its ACP companion has no
//!   such read.
//! - cursor: nothing a host can read (its usage panel calls Cursor's private
//!   server); 404, like every other agent.
//!
//! The account is host-wide, so the host-scope prelude and `$HOME` as the
//! working directory. Under the daemon-wide probe gate and the minute cache
//! every agent probe shares; only answers are cached, so a sign-in or a
//! network blip is asked again on the next look.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use chimaera_agent::model::UsageWindow;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};

use super::{
    base_command, bin_of, read_rpc_line, wrapped, CodexRpc, GroupKill, ProbePrelude, GATE,
};
use crate::agents::AgentKind;
use crate::AppState;

/// Startup plus one account read (live: claude under 2 s, grok 0.5 s, agy
/// 4 s); the whole process group dies with the read either way.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
/// Antigravity's `/usage` answer is one short JSON line.
const AGY_OUTPUT_CAP: u64 = 1024 * 1024;

/// GET /agents/{id}/usage[?refresh=true] for `claude` / `codex` / `grok` /
/// `agy` — `{agent, windows: [{label, utilization 0-100, resets_at?}],
/// limit_reached, checked_at_ms, error?}`. The other agents report no plan
/// usage a host can read: 404.
pub(crate) async fn usage(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<super::ProbeQuery>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let kind = match id.as_str() {
        "claude" => AgentKind::Claude,
        "codex" => AgentKind::Codex,
        "grok" => AgentKind::Grok,
        "agy" => AgentKind::Antigravity,
        _ => return super::not_found(),
    };
    axum::Json(read(&state, kind, &id, query.refresh).await).into_response()
}

async fn read(state: &Arc<AppState>, kind: AgentKind, agent: &str, refresh: bool) -> Value {
    let key = format!("usage:{agent}");
    if !refresh {
        if let Some(hit) = state.probes.get(&key) {
            return hit;
        }
    }
    let failed = |error: String| {
        json!({
            "agent": agent, "windows": [], "limit_reached": false,
            "checked_at_ms": crate::timeline::now_ms(), "error": error,
        })
    };
    // A signed-out CLI can only fail, and Antigravity's would start its
    // browser sign-in (it ignores BROWSER): never spawn one the sign-in
    // check already says is signed out.
    if crate::agent_auth::cached_verdict(state, kind) == Some(false) {
        return failed("not signed in".to_string());
    }
    let (bin, _version, _retained) = match bin_of(state, kind).await {
        Ok(found) => found,
        Err(error) => return failed(error),
    };
    let _permit = GATE.acquire().await;
    // Whoever held the gate may have just answered the same question.
    if !refresh {
        if let Some(hit) = state.probes.get(&key) {
            return hit;
        }
    }
    let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) else {
        return failed("HOME is not set".to_string());
    };
    let home = PathBuf::from(home);
    let generation = state.probes.generation.load(Ordering::Relaxed);
    let prelude = ProbePrelude::write(state, None).await;
    let answer = match kind {
        AgentKind::Claude => claude_usage(&bin, &home, prelude.path()).await,
        AgentKind::Grok => grok_usage(&bin, &home, prelude.path()).await,
        AgentKind::Antigravity => agy_usage(&bin, &home, prelude.path()).await,
        _ => codex_usage(&bin, &home, prelude.path()).await,
    };
    match answer {
        Ok((windows, limit_reached)) => {
            let value = json!({
                "agent": agent, "windows": windows, "limit_reached": limit_reached,
                "checked_at_ms": crate::timeline::now_ms(),
            });
            state.probes.put(&key, value.clone(), generation);
            value
        }
        Err(error) => failed(error),
    }
}

async fn claude_usage(
    bin: &Path,
    home: &Path,
    prelude: Option<&Path>,
) -> Result<(Vec<UsageWindow>, bool), String> {
    let settings = json!({ "disableAllHooks": true }).to_string();
    let argv = wrapped(
        bin,
        &[
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--strict-mcp-config",
            "--settings",
            &settings,
        ],
    );
    let mut cmd = base_command(&argv, Some(home), prelude);
    cmd.stdin(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start claude: {e}"))?;
    // Dropped before the leader is reaped (PID reuse), like `CodexRpc`.
    let group = GroupKill(child.id().map(|pid| nix::unistd::Pid::from_raw(pid as i32)));
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let mut stdout = BufReader::new(child.stdout.take().ok_or("no stdout")?);
    let exchange = async {
        let mut answer = Value::Null;
        for (id, subtype) in [("init", "initialize"), ("usage", "get_usage")] {
            let mut line = json!({
                "type": "control_request", "request_id": id,
                "request": { "subtype": subtype },
            })
            .to_string();
            line.push('\n');
            stdin
                .write_all(line.as_bytes())
                .await
                .map_err(|e| format!("claude write failed: {e}"))?;
            answer = control_response(&mut stdout, id).await?;
        }
        Ok::<Value, String>(answer)
    };
    let answer = tokio::time::timeout(PROBE_TIMEOUT, exchange)
        .await
        .map_err(|_| "claude get_usage timed out".to_string());
    drop(group);
    let _ = child.start_kill();
    let _ = child.wait().await;
    let windows = chimaera_agent::claude::usage_windows(&answer??);
    let reached = windows.iter().any(|w| w.utilization >= 100.0);
    Ok((windows, reached))
}

/// The `control_response` to `id`: its payload, or its error as an Err.
/// Other frames (a chatty login profile, init system frames) are skipped.
async fn control_response<R: tokio::io::AsyncBufRead + Unpin>(
    stdout: &mut R,
    id: &str,
) -> Result<Value, String> {
    for _ in 0..1024 {
        let Some(line) = read_rpc_line(stdout)
            .await
            .map_err(|e| format!("claude read failed: {e}"))?
        else {
            return Err("claude closed before answering".to_string());
        };
        let Ok(msg) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let response = &msg["response"];
        if msg["type"] != "control_response" || response["request_id"] != id {
            continue;
        }
        if response["subtype"] == "error" {
            let why = response["error"].as_str().unwrap_or("error");
            return Err(format!(
                "claude {id}: {}",
                chimaera_agent::model::truncate_label(why, 200)
            ));
        }
        return Ok(response["response"].clone());
    }
    Err("too many claude frames".to_string())
}

async fn codex_usage(
    bin: &Path,
    home: &Path,
    prelude: Option<&Path>,
) -> Result<(Vec<UsageWindow>, bool), String> {
    let mut rpc = CodexRpc::open(bin, home, prelude).await?;
    let read = rpc
        .request(
            "account/rateLimits/read",
            json!({ "excludeResetCreditDetails": true }),
        )
        .await;
    rpc.close().await;
    Ok(chimaera_agent::codex::rate_limit_usage(&read?))
}

async fn grok_usage(
    bin: &Path,
    home: &Path,
    prelude: Option<&Path>,
) -> Result<(Vec<UsageWindow>, bool), String> {
    let argv = wrapped(bin, &["agent", "--no-leader", "stdio"]);
    let mut cmd = base_command(&argv, Some(home), prelude);
    cmd.stdin(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start grok: {e}"))?;
    let group = GroupKill(child.id().map(|pid| nix::unistd::Pid::from_raw(pid as i32)));
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let mut stdout = BufReader::new(child.stdout.take().ok_or("no stdout")?);
    let exchange = async {
        let mut answer = Value::Null;
        for (id, method, params) in [
            (
                1,
                "initialize",
                json!({ "protocolVersion": 1, "clientCapabilities": {} }),
            ),
            (2, "_x.ai/billing", json!({})),
        ] {
            let mut line =
                json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
                    .to_string();
            line.push('\n');
            stdin
                .write_all(line.as_bytes())
                .await
                .map_err(|e| format!("grok write failed: {e}"))?;
            answer = rpc_result(&mut stdout, id, "grok").await?;
        }
        Ok::<Value, String>(answer)
    };
    let answer = tokio::time::timeout(PROBE_TIMEOUT, exchange)
        .await
        .map_err(|_| "grok billing timed out".to_string());
    drop(group);
    let _ = child.start_kill();
    let _ = child.wait().await;
    Ok(chimaera_agent::acp::grok_billing_usage(&answer??))
}

/// The JSON-RPC result for `id` (its `error` as an Err); notifications and
/// other lines are skipped.
async fn rpc_result<R: tokio::io::AsyncBufRead + Unpin>(
    stdout: &mut R,
    id: u64,
    agent: &str,
) -> Result<Value, String> {
    for _ in 0..1024 {
        let Some(line) = read_rpc_line(stdout)
            .await
            .map_err(|e| format!("{agent} read failed: {e}"))?
        else {
            return Err(format!("{agent} closed before answering"));
        };
        let Ok(msg) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if msg.get("id").and_then(Value::as_u64) != Some(id) || msg.get("method").is_some() {
            continue;
        }
        if let Some(err) = msg.get("error") {
            let why = err["message"].as_str().unwrap_or("error");
            return Err(format!(
                "{agent}: {}",
                chimaera_agent::model::truncate_label(why, 200)
            ));
        }
        return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
    }
    Err(format!("too many {agent} frames"))
}

async fn agy_usage(
    bin: &Path,
    home: &Path,
    prelude: Option<&Path>,
) -> Result<(Vec<UsageWindow>, bool), String> {
    let argv = wrapped(bin, &["-p", "/usage", "--output-format", "json"]);
    let mut cmd = base_command(&argv, Some(home), prelude);
    cmd.stderr(Stdio::null()).process_group(0);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start agy: {e}"))?;
    // The whole group, not just the login shell, dies with a stalled read.
    let group = GroupKill(child.id().map(|pid| nix::unistd::Pid::from_raw(pid as i32)));
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let read = async {
        let mut out = Vec::new();
        (&mut stdout)
            .take(AGY_OUTPUT_CAP)
            .read_to_end(&mut out)
            .await
            .map_err(|e| format!("agy read failed: {e}"))?;
        Ok::<Vec<u8>, String>(out)
    };
    let out = tokio::time::timeout(PROBE_TIMEOUT, read)
        .await
        .map_err(|_| "agy /usage timed out".to_string());
    drop(group);
    let _ = child.start_kill();
    let _ = child.wait().await;
    let out = String::from_utf8_lossy(&out??).into_owned();
    // The JSON is the last line (a login profile may print before it).
    let answer = out
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .ok_or("agy /usage answered no JSON")?;
    if answer["status"] != "SUCCESS" {
        return Err("agy /usage did not succeed".to_string());
    }
    let windows = agy_usage_windows(&answer);
    let reached = windows.iter().any(|w| w.utilization >= 100.0);
    Ok((windows, reached))
}

/// Antigravity's `/usage` (live 1.3.4: `command.data.groups[{name, buckets[
/// {window: "weekly" | "5h", remaining_fraction 0-1, reset_time}]}]`) as
/// windows named by their model group: "Gemini session limit". A bucket at
/// `remaining_fraction: 1` reports "now + window" as its reset time (it
/// moves between reads), so only a used one keeps it.
fn agy_usage_windows(answer: &Value) -> Vec<UsageWindow> {
    const MAX: usize = 16;
    let mut windows: Vec<UsageWindow> = Vec::new();
    let groups = answer["command"]["data"]["groups"].as_array();
    for group in groups.into_iter().flatten().take(MAX) {
        let name = group["name"].as_str().unwrap_or("").trim();
        let name = name
            .strip_suffix(" models")
            .or_else(|| name.strip_suffix(" Models"))
            .unwrap_or(name);
        for bucket in group["buckets"].as_array().into_iter().flatten().take(MAX) {
            let Some(remaining) = bucket["remaining_fraction"].as_f64() else {
                continue;
            };
            let window = match bucket["window"].as_str() {
                Some("5h") => "session limit".to_string(),
                Some("weekly") => "weekly limit".to_string(),
                Some(other) => {
                    format!("{} limit", chimaera_agent::model::truncate_label(other, 16))
                }
                None => "limit".to_string(),
            };
            let label =
                chimaera_agent::model::truncate_label(format!("{name} {window}").trim(), 64);
            if windows.len() >= MAX || windows.iter().any(|w| w.label == label) {
                continue;
            }
            let remaining = remaining.clamp(0.0, 1.0);
            windows.push(UsageWindow {
                label,
                utilization: (1.0 - remaining) * 100.0,
                resets_at: (remaining < 1.0)
                    .then(|| bucket["reset_time"].as_str())
                    .flatten()
                    .map(|at| chimaera_agent::model::truncate_label(at, 40)),
            });
        }
    }
    windows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn control_response_skips_other_frames_and_reads_errors() {
        let frames = [
            json!({"type": "system", "subtype": "init"}),
            json!({"type": "control_response", "response": {
                "subtype": "success", "request_id": "init", "response": {}}}),
            json!({"type": "control_response", "response": {
                "subtype": "success", "request_id": "usage",
                "response": {"rate_limits": {"five_hour": {"utilization": 3}}}}}),
        ];
        let text: String = std::iter::once("profile noise\n".to_string())
            .chain(frames.iter().map(|f| format!("{f}\n")))
            .collect();
        let mut reader = BufReader::new(text.as_bytes());
        let answer = control_response(&mut reader, "usage").await.unwrap();
        let windows = chimaera_agent::claude::usage_windows(&answer);
        assert_eq!(windows[0].label, "session limit");

        let refused = json!({"type": "control_response", "response": {
            "subtype": "error", "request_id": "usage", "error": "not signed in"}});
        let text = format!("{refused}\n");
        let mut reader = BufReader::new(text.as_bytes());
        let err = control_response(&mut reader, "usage").await.unwrap_err();
        assert!(err.contains("not signed in"), "{err}");
    }

    #[test]
    fn agy_usage_names_windows_by_model_group() {
        // Live 1.3.4 shape (trimmed).
        let answer = json!({"status": "SUCCESS", "num_turns": 0, "command": {"name": "usage", "data": {"groups": [
            {"name": "Gemini Models", "buckets": [
                {"id": "gemini-weekly", "window": "weekly", "remaining_fraction": 0.994, "reset_time": "2026-10-23T00:47:14Z"},
                {"id": "gemini-5h", "window": "5h", "remaining_fraction": 0.2, "reset_time": "2026-10-16T05:47:14Z"}]},
            {"name": "Claude and GPT models", "buckets": [
                {"id": "3p-weekly", "window": "weekly", "remaining_fraction": 1, "reset_time": "2026-10-23T00:56:13Z"}]}]}}});
        let windows = agy_usage_windows(&answer);
        let labels: Vec<_> = windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Gemini weekly limit",
                "Gemini session limit",
                "Claude and GPT weekly limit"
            ]
        );
        assert!((windows[1].utilization - 80.0).abs() < 0.01);
        assert_eq!(
            windows[1].resets_at.as_deref(),
            Some("2026-10-16T05:47:14Z")
        );
        // An untouched bucket's "reset" is just now + window: left out.
        assert_eq!(windows[2].utilization, 0.0);
        assert_eq!(windows[2].resets_at, None);
    }

    #[tokio::test]
    async fn rpc_result_skips_notifications_and_reads_errors() {
        let text = concat!(
            "profile noise\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"config\":{\"creditUsagePercent\":12.0}}}\n",
        );
        let mut reader = BufReader::new(text.as_bytes());
        let answer = rpc_result(&mut reader, 2, "grok").await.unwrap();
        let (windows, reached) = chimaera_agent::acp::grok_billing_usage(&answer);
        assert_eq!(windows[0].label, "usage limit");
        assert!(!reached);
        let refused = "{\"jsonrpc\":\"2.0\",\"id\":2,\"error\":{\"message\":\"not signed in\"}}\n";
        let mut reader = BufReader::new(refused.as_bytes());
        let err = rpc_result(&mut reader, 2, "grok").await.unwrap_err();
        assert!(err.contains("not signed in"), "{err}");
    }
}
