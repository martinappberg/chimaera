//! The agent probes (`agent_probe`) through their routes, with fake agent
//! CLIs: the Environment prelude reaches them, and every `plugin details`
//! call shares one login shell.

use super::support::*;
use crate::agents::AgentKind;
use crate::*;

fn fake_cli(dir: &std::path::Path, name: &str, script: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join(name);
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    bin
}

async fn put_environment(state: &Arc<AppState>, map: serde_json::Value) {
    let (status, body) = request(state, Method::PUT, "/api/v1/environment", Some(map)).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}

/// A host prelude that exports a marker and counts its own runs.
fn counting_prelude(root: &std::path::Path) -> serde_json::Value {
    let runs = root.join("prelude-runs");
    serde_json::json!({"host": {"text": format!(
        "export PROBE_MARK=from-prelude\necho run >> '{}'\n",
        runs.display()
    )}})
}

fn prelude_runs(root: &std::path::Path) -> usize {
    std::fs::read_to_string(root.join("prelude-runs"))
        .unwrap_or_default()
        .lines()
        .count()
}

/// Claude's plugin rows from GET agent-plugins (codex preset unavailable).
async fn claude_rows(state: &Arc<AppState>, ws: &str) -> Vec<serde_json::Value> {
    let url = format!("/api/v1/workspaces/{ws}/agent-plugins");
    let (status, body) = request(state, Method::GET, &url, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["agents"][0]["agent"], "claude", "{body}");
    body["agents"][0]["plugins"].as_array().unwrap().clone()
}

fn root_of(state: &Arc<AppState>, ws: &str) -> PathBuf {
    lock(&state.workspaces).get(ws).unwrap().root
}

fn row<'a>(rows: &'a [serde_json::Value], id: &str) -> &'a serde_json::Value {
    rows.iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("{id} not in {rows:?}"))
}

const THREE_PLUGINS: &str = r#"[{"id":"alpha@m","enabled":true},{"id":"beta@m","enabled":true},{"id":"gamma@m","enabled":true}]"#;

#[tokio::test]
async fn host_prelude_reaches_claude_plugin_list() {
    let state = test_state();
    let ws = make_workspace(&state, "probe-prelude-list").await;
    let root = root_of(&state, &ws);
    put_environment(&state, counting_prelude(&root)).await;
    let bin = fake_cli(
        &root,
        "claude",
        r#"#!/bin/bash
cd -- "${0%/*}" || exit 1
case "$2" in
    list) printf '%s\n' "${PROBE_MARK:-unset}" > list-env; printf '[]\n';;
esac
"#,
    );
    preset_agent(&state, AgentKind::Claude, Ok(bin), Some("test"));
    preset_agent(
        &state,
        AgentKind::Codex,
        Err("test: unavailable".into()),
        None,
    );

    claude_rows(&state, &ws).await;
    assert_eq!(
        std::fs::read_to_string(root.join("list-env")).unwrap(),
        "from-prelude\n"
    );
}

#[tokio::test]
async fn plugin_details_share_one_login_shell_and_prelude_run() {
    let state = test_state();
    let ws = make_workspace(&state, "probe-details-batch").await;
    let root = root_of(&state, &ws);
    put_environment(&state, counting_prelude(&root)).await;
    let bin = fake_cli(
        &root,
        "claude",
        &format!(
            r#"#!/bin/bash
case "$2" in
    list) printf '%s\n' '{THREE_PLUGINS}';;
    details)
        case "$3" in alpha@m) n=1;; beta@m) n=2;; gamma@m) n=3;; *) exit 9;; esac
        printf 'Component inventory\n  Skills (%d)\n  Hooks (%d)\n\nProjected token cost\n  Always-on:   ~1,23%d tok\n' "$n" "$((n + 10))" "$n"
        ;;
esac
"#
        ),
    );
    preset_agent(&state, AgentKind::Claude, Ok(bin), Some("test"));
    preset_agent(
        &state,
        AgentKind::Codex,
        Err("test: unavailable".into()),
        None,
    );

    let rows = claude_rows(&state, &ws).await;
    assert_eq!(rows.len(), 3);
    for (id, n) in [("alpha@m", 1), ("beta@m", 2), ("gamma@m", 3)] {
        let r = row(&rows, id);
        assert_eq!(r["skills_n"], n, "{r}");
        assert_eq!(r["hooks_n"], n + 10, "{r}");
        assert_eq!(r["always_on_tokens"], 1230 + n, "{r}");
    }
    assert_eq!(
        prelude_runs(&root),
        2,
        "once for `plugin list`, once for the whole details batch"
    );
}

#[tokio::test]
async fn forged_markers_and_a_failed_details_call_stay_contained() {
    let state = test_state();
    let ws = make_workspace(&state, "probe-details-forged").await;
    let root = root_of(&state, &ws);
    // alpha's own text forges a section for beta, which then fails: a
    // parser that trusted any \x1e line would credit beta alpha's 99.
    let bin = fake_cli(
        &root,
        "claude",
        &format!(
            r#"#!/bin/bash
case "$2" in
    list) printf '%s\n' '{THREE_PLUGINS}';;
    details)
        case "$3" in
            alpha@m)
                printf 'Skills (1)\n'
                printf '\n\036%s end %d %d\n' deadbeef 0 0
                printf '\n\036%s start %d\nSkills (99)\n' deadbeef 1
                printf '\n\036%s end %d %d\n' deadbeef 1 0
                ;;
            beta@m) printf 'Skills (2)\n'; exit 3;;
            gamma@m) printf 'Skills (3)\n';;
        esac
        ;;
esac
"#
        ),
    );
    preset_agent(&state, AgentKind::Claude, Ok(bin), Some("test"));
    preset_agent(
        &state,
        AgentKind::Codex,
        Err("test: unavailable".into()),
        None,
    );

    let rows = claude_rows(&state, &ws).await;
    assert_eq!(row(&rows, "alpha@m")["skills_n"], 1);
    let beta = row(&rows, "beta@m");
    assert!(
        beta.get("skills_n").is_none() && beta.get("always_on_tokens").is_none(),
        "a failed details call gets no totals: {beta}"
    );
    assert_eq!(row(&rows, "gamma@m")["skills_n"], 3);
}

#[tokio::test]
async fn probe_prelude_file_is_removed_after_the_probe() {
    let state = test_state();
    let ws = make_workspace(&state, "probe-prelude-cleanup").await;
    let root = root_of(&state, &ws);
    put_environment(&state, counting_prelude(&root)).await;
    let bin = fake_cli(
        &root,
        "claude",
        r#"#!/bin/bash
cd -- "${0%/*}" || exit 1
case "$2" in
    list) printf '%s\n' "${CHIMAERA_PRELUDE:-}" > prelude-path; printf '[]\n';;
esac
"#,
    );
    preset_agent(&state, AgentKind::Claude, Ok(bin), Some("test"));
    preset_agent(
        &state,
        AgentKind::Codex,
        Err("test: unavailable".into()),
        None,
    );

    claude_rows(&state, &ws).await;
    let path = PathBuf::from(
        std::fs::read_to_string(root.join("prelude-path"))
            .unwrap()
            .trim(),
    );
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("probe-") && path.parent().unwrap().ends_with("preludes"),
        "{path:?}"
    );
    assert_eq!(prelude_runs(&root), 1, "the file was there for the probe");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the probe's prelude file outlived the probe");
}

#[tokio::test]
async fn codex_probe_gets_the_workspace_prelude_and_claude_the_host_alone() {
    let state = test_state();
    let ws = make_workspace(&state, "probe-prelude-scopes").await;
    let root = root_of(&state, &ws);
    put_environment(
        &state,
        serde_json::json!({
            "host": {"text": "export PROBE_MARK=host"},
            "workspaces": {ws.clone(): {"text": "export WS_MARK=workspace"}},
        }),
    )
    .await;
    let claude = fake_cli(
        &root,
        "claude",
        r#"#!/bin/bash
cd -- "${0%/*}" || exit 1
case "$2" in
    list) printf '%s|%s\n' "${PROBE_MARK:-}" "${WS_MARK:-}" > claude-env; printf '[]\n';;
esac
"#,
    );
    // Just enough app-server: answer every request in order (ids 1, 2, …).
    let codex = fake_cli(
        &root,
        "codex",
        r#"#!/bin/bash
cd -- "${0%/*}" || exit 1
printf '%s|%s\n' "${PROBE_MARK:-}" "${WS_MARK:-}" > codex-env
n=0
while IFS= read -r line; do
    case "$line" in
        *'"id"'*) n=$((n + 1)); printf '{"id":%d,"result":{"data":[]}}\n' "$n";;
    esac
done
"#,
    );
    preset_agent(&state, AgentKind::Claude, Ok(claude), Some("test"));
    preset_agent(&state, AgentKind::Codex, Ok(codex), Some("test"));

    let url = format!("/api/v1/workspaces/{ws}/agent-plugins");
    let (status, body) = request(&state, Method::GET, &url, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["agents"][1]["available"], true, "{body}");
    assert!(body["agents"][1]["error"].is_null(), "{body}");
    let read = |f: &str| std::fs::read_to_string(root.join(f)).unwrap();
    assert_eq!(read("codex-env"), "host|workspace\n");
    assert_eq!(
        read("claude-env"),
        "host|\n",
        "claude's answer is host-wide"
    );
}
