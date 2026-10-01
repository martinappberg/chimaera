//! Workspace connector inventory and explicit MCP sign-in through the agents'
//! own CLIs. Account login belongs to a separate flow. Never return endpoints,
//! headers, environment values, credential files, or raw probe errors.
use super::*;
use axum::{
    extract::{Path as AxPath, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};

mod auth;
mod auth_pty;
pub(crate) use auth::{cancel, check, input, login, status, AuthState};

const MAX_ROWS: usize = 256;
const PROBE_BUDGET: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Connection {
    name: String,
    kind: String,
    status: String,
    source: String,
    login: bool,
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control)
}

fn connection(name: &str, kind: &str, status: &str, login: bool) -> Connection {
    Connection {
        name: name.into(),
        kind: kind.into(),
        status: status.into(),
        login,
        source: if kind == "app" {
            "ChatGPT"
        } else if name.starts_with("plugin:") {
            "Agent plugin"
        } else {
            "MCP server"
        }
        .into(),
    }
}

fn claude_connections(output: &str) -> Result<Vec<Connection>, &'static str> {
    let mut rows = Vec::new();
    let empty = output.contains("No MCP servers configured");
    for line in output.lines() {
        let Some((definition, state)) = line.trim().rsplit_once(" - ") else {
            continue;
        };
        let Some((name, _)) = definition.split_once(": ") else {
            continue;
        };
        if !valid_name(name) {
            continue;
        }
        let status = match state.trim() {
            "✔ Connected" | "✓ Connected" => "connected",
            "⚠ Needs authentication" | "! Needs authentication" => "needs_auth",
            "✗ Failed to connect" => "failed",
            "⏸ Disabled" => "disabled",
            "⏸ Pending approval" => "needs_approval",
            _ => "unknown",
        };
        let mut row = connection(name, "mcp", status, status == "needs_auth");
        if name.starts_with("claude.ai ") {
            row.source = "claude.ai".into();
        }
        rows.push(row);
        if rows.len() > MAX_ROWS {
            return Err("Too many connections to list.");
        }
    }
    if rows.is_empty() && !empty {
        return Err(
            "Claude couldn't report its connections. Check again or use Claude's /mcp menu.",
        );
    }
    Ok(rows)
}

fn codex_connections(output: &str) -> Result<Vec<Connection>, &'static str> {
    let raw: Value =
        serde_json::from_str(output).map_err(|_| "Codex couldn't report its MCP servers.")?;
    let list = raw
        .as_array()
        .ok_or("Codex couldn't report its MCP servers.")?;
    if list.len() > MAX_ROWS {
        return Err("Too many connections to list.");
    }
    let mut rows = Vec::new();
    for row in list {
        let name = row["name"]
            .as_str()
            .filter(|s| valid_name(s))
            .ok_or("Codex returned an unrecognized connection.")?;
        let status = if row["enabled"] == false {
            "disabled"
        } else {
            match row["auth_status"].as_str() {
                Some("not_logged_in") => "needs_auth",
                Some("o_auth" | "oauth" | "bearer_token") => "authenticated",
                Some("unsupported") => "configured",
                _ => "unknown",
            }
        };
        rows.push(connection(name, "mcp", status, status == "needs_auth"));
    }
    Ok(rows)
}

fn hosted_connections(raw: &Value) -> Result<Vec<Connection>, &'static str> {
    let apps = raw["apps"]
        .as_array()
        .ok_or("Codex couldn't report its ChatGPT apps.")?;
    if apps.len() > MAX_ROWS {
        return Err("Too many ChatGPT apps to list.");
    }
    apps.iter()
        .map(|app| {
            let name = app["runtimeName"]
                .as_str()
                .or_else(|| app["id"].as_str())
                .filter(|s| valid_name(s))
                .ok_or("Codex returned an unrecognized ChatGPT app.")?;
            let status = if app["enabled"] == false {
                "disabled"
            } else if app["callable"] == true {
                "available"
            } else {
                "unavailable"
            };
            Ok(connection(name, "app", status, false))
        })
        .collect()
}

/// CLI listing can start MCP subprocesses. Its whole process group belongs to
/// this bounded probe, including on request cancellation or a timeout.
async fn run_cli(
    bin: &Path,
    args: &[&str],
    root: &Path,
    prelude: Option<&Path>,
) -> Result<String, ()> {
    let mut cmd = base_command(&wrapped(bin, args), Some(root), prelude);
    cmd.stderr(Stdio::null()).process_group(0);
    let mut child = cmd.spawn().map_err(|_| ())?;
    let group = GroupKill(child.id().map(|pid| nix::unistd::Pid::from_raw(pid as i32)));
    let stdout = child.stdout.take().ok_or(())?;
    let work = async {
        let mut out = Vec::new();
        stdout
            .take(CLI_OUTPUT_CAP as u64 + 1)
            .read_to_end(&mut out)
            .await
            .map_err(|_| ())?;
        if out.len() > CLI_OUTPUT_CAP {
            return Err(());
        }
        if !child.wait().await.map_err(|_| ())?.success() {
            return Err(());
        }
        String::from_utf8(out).map_err(|_| ())
    };
    let result = tokio::time::timeout(CLI_TIMEOUT, work)
        .await
        .map_err(|_| ())
        .and_then(|r| r);
    drop(group);
    let _ = child.start_kill();
    let _ = child.wait().await;
    result
}

async fn probe(state: &Arc<AppState>, ws: &str, root: &Path, kind: AgentKind) -> Value {
    let agent = kind.as_str();
    let key = format!("connections:{agent}:{ws}");
    if let Some(hit) = state.probes.get(&key) {
        return hit;
    }
    let work = async {
        let _permit = GATE.acquire().await;
        if let Some(hit) = state.probes.get(&key) {
            return hit;
        }
        let (bin, version) = match bin_of(state, kind).await {
            Ok(found) => found,
            Err(_) => return json!({"agent":agent,"available":false,"connections":[]}),
        };
        let generation = state.probes.generation.load(Ordering::Relaxed);
        let prelude = ProbePrelude::write(state, Some(ws)).await;
        let args: &[&str] = if kind == AgentKind::Codex {
            &["mcp", "list", "--json"]
        } else {
            &["mcp", "list"]
        };
        let listed = run_cli(&bin, args, root, prelude.path()).await;
        let parsed = listed
            .map_err(|_| "Couldn't check connections. Try again.")
            .and_then(|out| {
                if kind == AgentKind::Codex {
                    codex_connections(&out)
                } else {
                    claude_connections(&out)
                }
            });
        let (mut rows, mut errors) = match parsed {
            Ok(rows) => (rows, vec![]),
            Err(error) => (vec![], vec![error]),
        };
        // Older CLIs must never advertise a login command they don't support.
        let local_login =
            |r: &Connection| r.login && !(kind == AgentKind::Claude && r.source == "claude.ai");
        if rows.iter().any(local_login) {
            let supported = run_cli(&bin, &["mcp", "login", "--help"], root, prelude.path())
                .await
                .is_ok_and(|help| help.contains("--no-browser"));
            for row in rows.iter_mut().filter(|r| local_login(r)) {
                row.login &= supported;
            }
        }
        if kind == AgentKind::Codex {
            let hosted = async {
                let mut rpc = CodexRpc::open(&bin, root, prelude.path())
                    .await
                    .map_err(|_| ())?;
                let raw = rpc
                    .request("app/installed", json!({"forceRefresh":true}))
                    .await;
                rpc.close().await;
                hosted_connections(&raw.map_err(|_| ())?).map_err(|_| ())
            }
            .await;
            match hosted {
                Ok(apps) => rows.extend(apps),
                Err(()) => errors.push("Couldn't list ChatGPT apps with this Codex version or account. MCP servers are listed separately."),
            }
        }
        let value = json!({"agent":agent,"available":true,"version":version,"connections":rows,"errors":errors});
        state.probes.put(&key, value.clone(), generation);
        value
    };
    tokio::time::timeout(PROBE_BUDGET, work).await.unwrap_or_else(|_| {
        json!({"agent":agent,"available":true,"connections":[],"errors":["Checking connections timed out. Try again."]})
    })
}

pub(crate) async fn list(
    State(state): State<Arc<AppState>>,
    AxPath(ws): AxPath<String>,
    Query(query): Query<ProbeQuery>,
) -> Response {
    let Some(root) = workspace_root(&state, &ws) else {
        return not_found();
    };
    if query.refresh {
        state.probes.invalidate();
    }
    // Each holds the shared agent-probe gate; the whole request is bounded.
    let (claude, codex) = tokio::join!(
        probe(&state, &ws, &root, AgentKind::Claude),
        probe(&state, &ws, &root, AgentKind::Codex)
    );
    Json(json!({"host":state.hostname,"agents":[claude,codex]})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_reports_never_expose_endpoints_or_credentials() {
        let rows = claude_connections("Checking MCP server health...\nplugin:notes: https://example.test/?token=secret - ✔ Connected\nclaude.ai Drive: https://example.test - ⚠ Needs authentication\n").unwrap();
        let wire = serde_json::to_string(&rows).unwrap();
        assert!(!wire.contains("secret") && !wire.contains("https://"));
        assert_eq!(rows[0].name, "plugin:notes");
        assert_eq!(rows[1].source, "claude.ai");
        assert!(rows[1].login);
        assert!(
            claude_connections("docs: https://example.test - ! Needs authentication\n").unwrap()[0]
                .login
        );
        assert!(claude_connections("unrecognized version output").is_err());
        assert!(claude_connections(
            "No MCP servers configured. Use claude mcp add to add a server."
        )
        .unwrap()
        .is_empty());
        let rows = codex_connections(r#"[{"name":"private","enabled":true,"auth_status":"not_logged_in","transport":{"url":"https://example.test/?token=secret","env":{"KEY":"secret"}}},{"name":"off","enabled":false,"auth_status":"not_logged_in"},{"name":"local","enabled":true,"auth_status":"unsupported"}]"#).unwrap();
        assert!(!serde_json::to_string(&rows).unwrap().contains("secret"));
        assert!(rows[0].login);
        assert!(!rows[1].login);
        assert_eq!(rows[2].status, "configured");
        let codex_named_like_claude =
            codex_connections(r#"[{"name":"claude.ai Docs","auth_status":"not_logged_in"}]"#)
                .unwrap();
        assert_eq!(codex_named_like_claude[0].source, "MCP server");
    }

    #[test]
    fn hosted_apps_keep_policy_separate_from_authentication() {
        let rows = hosted_connections(&json!({"apps":[{"id":"one","runtimeName":"Docs","enabled":true,"callable":true},{"id":"two","enabled":true,"callable":false},{"id":"three","enabled":false,"callable":false}]})).unwrap();
        assert_eq!(
            rows.iter().map(|r| r.status.as_str()).collect::<Vec<_>>(),
            vec!["available", "unavailable", "disabled"]
        );
        assert!(rows.iter().all(|r| !r.login));
        assert!(hosted_connections(&json!({})).is_err());
    }

    #[tokio::test]
    async fn rpc_frames_are_capped_before_allocation() {
        let huge = vec![b'x'; RPC_LINE_CAP + 1];
        assert!(read_rpc_line(&mut huge.as_slice()).await.is_err());
        let mut two = b"{}\n[]\n".as_slice();
        assert_eq!(read_rpc_line(&mut two).await.unwrap().unwrap(), b"{}\n");
        assert_eq!(read_rpc_line(&mut two).await.unwrap().unwrap(), b"[]\n");
    }
}
