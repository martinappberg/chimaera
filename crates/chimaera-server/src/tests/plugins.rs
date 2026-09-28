//! Workbench plugins over the wire: the first-party plugins listed as
//! available until installed, the per-workspace switch, footprint
//! detection, the MCP tools a plugin adds (offered AND gated only where it is
//! active), and Agent notes end to end. The daemon carries no plugin, so
//! each test installs the first-party ones it uses by path, from the
//! releases `scripts/build-plugins.sh` laid out in `plugins/dist-test`.

use super::support::*;
use crate::{lock, AppState};

/// Exercise the route's exact shell program in a real PTY. A fast success
/// or failure must still be readable by a client attaching after completion.
#[tokio::test]
async fn agent_plugin_install_keeps_success_and_failure_until_acknowledged() {
    use crate::agents::AgentKind;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    for (kind, verb, code) in [
        (AgentKind::Claude, "install", 0),
        (AgentKind::Codex, "add", 7),
    ] {
        let state = test_state();
        install_first_party(&state, "mycelium").await;
        let ws = make_workspace(&state, "plugin-install-result").await;
        let root = root_of(&state, &ws);
        let git_dir = root.join("module-bin");
        std::fs::create_dir(&git_dir).unwrap();
        let git = git_dir.join("git");
        std::fs::write(
            &git,
            "#!/bin/bash\nprintf '%s\\n' '--[no-]shallow-submodules'\n",
        )
        .unwrap();
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o700)).unwrap();
        let git_dir = git_dir.to_string_lossy().replace('\'', "'\"'\"'");
        let (status, _) = request(
            &state,
            Method::PUT,
            "/api/v1/environment",
            Some(serde_json::json!({
                "host": {"text": "export CHIMAERA_TEST_INSTALL_ENV=host"},
                "workspaces": {(&ws): {"text": format!(
                    "export CHIMAERA_TEST_INSTALL_ENV=\"$CHIMAERA_TEST_INSTALL_ENV:workspace\"\nexport PATH='{git_dir}':\"$PATH\""
                )}},
            })),
        ).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let bin = root.join("agent's cli");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/bash\n[ \"$CHIMAERA_TEST_INSTALL_ENV\" = host:workspace ] || exit 90\n\
                 printf '%s\\n' \"$*\" >> calls\n\
                 if [ \"$2\" = marketplace ]; then exit 1; fi\n\
                 printf 'agent install output\\n'\nexit {code}\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        preset_agent(&state, kind, Ok(bin), Some("test"));
        let (status, _) = request(
            &state,
            Method::POST,
            &format!("/api/v1/workspaces/{ws}/plugins/mycelium/install"),
            Some(
                serde_json::json!({"agent": kind.as_str(), "agent_plugin_id": "undeclared@market"}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let mut body = serde_json::json!({"agent": kind.as_str()});
        if kind == AgentKind::Codex {
            body["agent_plugin_id"] = serde_json::json!("mycelium@mycelium");
        }
        let (status, result) = request(
            &state,
            Method::POST,
            &format!("/api/v1/workspaces/{ws}/plugins/mycelium/install"),
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        let sid = result["session_id"].as_str().unwrap();
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let info = state
                    .sessions
                    .get(sid)
                    .expect("install vanished before acknowledgement");
                if info.title.as_deref() == Some("Plugin installation finished") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("install never finished");
        assert_eq!(
            std::fs::read_to_string(root.join("calls")).unwrap(),
            format!("plugin marketplace add arjunrajlaboratory/mycelium\nplugin {verb} mycelium@mycelium\n")
        );
        let sessions = state.sessions.clone();
        let attach_id = sid.to_string();
        let attachment = tokio::task::spawn_blocking(move || sessions.attach(&attach_id))
            .await
            .unwrap()
            .unwrap();
        let screen = String::from_utf8_lossy(&attachment.snapshot);
        assert!(screen.contains("agent install output"), "{screen}");
        assert!(screen.contains("Press Enter to close"), "{screen}");
        let outcome = if code == 0 {
            "Installed."
        } else {
            "Install failed (exit 7)"
        };
        assert!(screen.contains(outcome), "{screen}");
        attachment
            .input
            .send(bytes::Bytes::from_static(b"\r"))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while state.sessions.get(sid).is_some() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            state.sessions.last_words(sid).unwrap().info.exit_status,
            Some(code)
        );
    }
}

#[test]
fn agent_plugin_install_treats_names_as_data() {
    let root = test_dir("plugin-install-quoting");
    let completion = root.join("completion");
    let name = "Mycelium's $(touch injected) `touch injected-too`";
    let output = std::process::Command::new("/bin/bash")
        .args([
            "-c",
            include_str!("../plugins/install-agent.sh"),
            "chimaera-plugin-install",
            name,
            "claude",
            "/usr/bin/true",
            "arjunrajlaboratory/mycelium",
            "mycelium@mycelium",
            "install",
            completion.to_str().unwrap(),
        ])
        .current_dir(&root)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(name));
    assert_eq!(std::fs::read(&completion).unwrap(), b"1");
    assert!(!root.join("injected").exists());
    assert!(!root.join("injected-too").exists());
}

#[test]
fn agent_plugin_install_explains_claude_git_without_blocking_cached_installs() {
    use std::os::unix::fs::PermissionsExt;

    for (agent, git_help, expected, hint) in [
        ("claude", Some("usage: git clone [--recursive]"), 1, true),
        ("claude", None, 1, true),
        ("claude", Some("usage: git clone [--recursive]"), 0, true),
        ("codex", Some("usage: git clone [--recursive]"), 0, false),
        ("claude", Some("--[no-]shallow-submodules"), 0, false),
    ] {
        let root = test_dir("plugin-install-git");
        let bin = root.join("agent");
        std::fs::write(&bin, format!("#!/bin/bash\nprintf '%s\\n' \"$*\" >> calls\nif [ \"$2\" = marketplace ]; then exit 1; fi\nexit {expected}\n")).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        if let Some(help) = git_help {
            let git = root.join("git");
            std::fs::write(&git, format!(
                "#!/bin/bash\nif [ \"$1\" = --version ]; then printf 'git version 1.8.3.1\\n'; else printf '%s\\n' '{help}'; exit 129; fi\n"
            )).unwrap();
            std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let completion = root.join("completion");
        let output = std::process::Command::new("/bin/bash")
            .args([
                "-c",
                include_str!("../plugins/install-agent.sh"),
                "chimaera-plugin-install",
                "Mycelium",
                agent,
                bin.to_str().unwrap(),
                "arjunrajlaboratory/mycelium",
                "mycelium@mycelium",
                "install",
                completion.to_str().unwrap(),
            ])
            .env("PATH", &root)
            .current_dir(&root)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        let screen = String::from_utf8_lossy(&output.stdout);
        assert_eq!(output.status.code(), Some(expected), "{screen}");
        assert_eq!(
            std::fs::read_to_string(root.join("calls"))
                .unwrap()
                .lines()
                .count(),
            2
        );
        assert_eq!(screen.contains("Settings > Environment"), hint, "{screen}");
        if hint && git_help.is_some() {
            assert!(screen.contains("1.8.3.1"), "{screen}");
            assert!(screen.contains("--shallow-submodules"), "{screen}");
        }
        assert_eq!(std::fs::read(completion).unwrap(), b"1");
    }
}

/// The git chimaera resolved (the Git binary path setting) reaches the
/// agent's plugin manager ahead of an old system git — seen on a login node
/// whose /usr/bin/git is 1.8.3 while the setting pointed at a module's 2.45.
#[test]
fn agent_plugin_install_puts_the_resolved_git_first_on_path() {
    use std::os::unix::fs::PermissionsExt;

    let root = test_dir("plugin-install-git-dir");
    let exe = |path: &std::path::Path, body: &str| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    };
    exe(
        &root.join("old/git"),
        "#!/bin/bash\nprintf 'usage: git clone [--recursive]\\n'; exit 129\n",
    );
    exe(
        &root.join("new/git"),
        "#!/bin/bash\nprintf '%s\\n' '--[no-]shallow-submodules'; exit 129\n",
    );
    let bin = root.join("agent");
    exe(
        &bin,
        "#!/bin/bash\ncommand -v git >> seen\nif [ \"$2\" = marketplace ]; then exit 1; fi\nexit 0\n",
    );
    let completion = root.join("completion");
    let output = std::process::Command::new("/bin/bash")
        .args([
            "-c",
            include_str!("../plugins/install-agent.sh"),
            "chimaera-plugin-install",
            "Mycelium",
            "claude",
            bin.to_str().unwrap(),
            "arjunrajlaboratory/mycelium",
            "mycelium@mycelium",
            "install",
            completion.to_str().unwrap(),
            root.join("new").to_str().unwrap(),
        ])
        .env("PATH", root.join("old"))
        .current_dir(&root)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let screen = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{screen}");
    let seen = std::fs::read_to_string(root.join("seen")).unwrap();
    let new_git = root.join("new/git");
    assert!(
        seen.lines().all(|l| l == new_git.to_str().unwrap()),
        "{seen}"
    );
    // The failed marketplace fetch is explained against the git it used.
    assert!(!screen.contains("--shallow-submodules."), "{screen}");
}

/// A CLI (or a manifest name echoed by the installer) can set any terminal
/// title. It must not retire the completion watcher before the install ends.
#[tokio::test]
async fn agent_plugin_install_ignores_forged_completion_title() {
    use crate::agents::AgentKind;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    let state = test_state();
    install_first_party(&state, "mycelium").await;
    let ws = make_workspace(&state, "plugin-install-forged-title").await;
    let root = root_of(&state, &ws);
    let bin = root.join("claude");
    std::fs::write(
        &bin,
        r#"#!/bin/bash
cd -- "${0%/*}" || exit 1
case "$2" in
    list) printf 'probe\n' >> probes; printf '[]\n';;
    install)
        printf '\033]2;Plugin installation finished\007'
        while [ ! -f release-install ]; do sleep 0.05; done
        ;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    preset_agent(&state, AgentKind::Claude, Ok(bin), Some("test"));
    preset_agent(
        &state,
        AgentKind::Codex,
        Err("test: unavailable".into()),
        None,
    );
    let report_url = format!("/api/v1/workspaces/{ws}/agent-plugins");
    let (status, _) = request(&state, Method::GET, &report_url, None).await;
    assert_eq!(status, StatusCode::OK);
    let probe_calls = || std::fs::read_to_string(root.join("probes")).unwrap();
    assert_eq!(probe_calls(), "probe\n");

    let (status, result) = request(
        &state,
        Method::POST,
        &format!("/api/v1/workspaces/{ws}/plugins/mycelium/install"),
        Some(serde_json::json!({"agent": "claude"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let sid = result["session_id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while state.sessions.get(sid).unwrap().title.as_deref()
            != Some("Plugin installation finished")
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    // Cross the watcher's two-second poll while the real install is blocked.
    tokio::time::sleep(Duration::from_secs(3)).await;
    request(&state, Method::GET, &report_url, None).await;
    assert_eq!(probe_calls(), "probe\n", "title must not invalidate probes");
    assert_eq!(state.probes.changed_epoch(), 0);

    std::fs::write(root.join("release-install"), "").unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            request(&state, Method::GET, &report_url, None).await;
            if probe_calls() == "probe\nprobe\n" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("completion did not invalidate probes before terminal dismissal");
    assert_eq!(state.probes.changed_epoch(), 1);
    assert!(state.sessions.get(sid).is_some());
    state.sessions.kill(sid).unwrap();
}

async fn tools(state: &Arc<AppState>, sid: &str, key: &str) -> Vec<String> {
    let (status, out) = mcp_post(
        state,
        sid,
        key,
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    out["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

async fn instructions(state: &Arc<AppState>, sid: &str, key: &str) -> String {
    let (_, out) = mcp_post(
        state,
        sid,
        key,
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18"}}),
    )
    .await;
    out["result"]["instructions"].as_str().unwrap().to_string()
}

fn root_of(state: &Arc<AppState>, ws: &str) -> PathBuf {
    lock(&state.workspaces).get(ws).unwrap().root
}

#[tokio::test]
async fn plugin_tools_appear_only_where_switched_on_and_present() {
    let state = test_state();
    install_first_party(&state, "mycelium").await;
    install_first_party(&state, "agent-notes").await;
    let ws = make_workspace(&state, "plugins-gate").await;
    let worker = inject_agent(&state, "wk");
    lock(&state.session_workspaces).insert(worker.clone(), ws.clone());
    let base = tools(&state, &worker, "wk").await;

    // Switched on, no footprint yet: nothing changes.
    let (status, _) = request(
        &state,
        Method::PUT,
        &format!("/api/v1/workspaces/{ws}/plugins/mycelium"),
        Some(serde_json::json!({"on": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(tools(&state, &worker, "wk").await, base);

    // The footprint appears (mycelium set up): the tools and the paragraph
    // arrive on the very next connect — no cold-cache miss.
    std::fs::create_dir_all(root_of(&state, &ws).join(".living/findings")).unwrap();
    crate::plugins::refresh_detect(&state, &ws).await;
    let with = tools(&state, &worker, "wk").await;
    assert!(with.contains(&"knowledge_search".to_string()), "{with:?}");
    assert!(with.contains(&"knowledge_get".to_string()));
    assert!(instructions(&state, &worker, "wk")
        .await
        .contains("Project knowledge (the mycelium plugin)"));

    // A tool of a plugin that isn't on is refused at the call gate too.
    let (status, out) = mcp_post(
        &state,
        &worker,
        "wk",
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "post_note", "arguments": {"text": "hi"}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(out["error"]["message"]
        .as_str()
        .unwrap()
        .contains("isn't switched on"));

    // Switched back off: back to exactly the base list.
    request(
        &state,
        Method::PUT,
        &format!("/api/v1/workspaces/{ws}/plugins/mycelium"),
        Some(serde_json::json!({"on": false})),
    )
    .await;
    assert_eq!(tools(&state, &worker, "wk").await, base);
    state.sessions.kill(&worker).ok();
}

#[tokio::test]
async fn workspace_plugins_route_reports_on_detected_active() {
    let state = test_state();
    install_first_party(&state, "mycelium").await;
    let ws = make_workspace(&state, "plugins-route").await;
    std::fs::write(root_of(&state, &ws).join("MYCELIUM.md"), "# protocol").unwrap();
    lock(&state.workspaces)
        .set_plugin_on(&ws, "mycelium", true)
        .unwrap();
    let (status, out) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/plugins"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let myc = out["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "mycelium")
        .unwrap()
        .clone();
    assert_eq!(myc["on"], true);
    assert_eq!(myc["detected"], true);
    assert_eq!(myc["active"], true);
    assert!(!myc["adds"]["agents"].as_array().unwrap().is_empty());
    assert_eq!(myc["source"], "installed");
    assert_eq!(myc["first_party"], true);
    assert_eq!(myc["verified"], true, "{myc}");
    let notes = out["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "agent-notes")
        .unwrap()
        .clone();
    assert_eq!(notes["source"], "available", "not installed here: {notes}");
    assert_eq!(notes["active"], false);
    assert_eq!(notes["requires"], serde_json::json!([]));
    assert_eq!(notes["recommends"], serde_json::json!([]));
}

/// The author's own words reach the card: the description, and the sentence
/// about the agent-side plugin it recommends (beside the agents' rows).
#[tokio::test]
async fn the_card_carries_the_authors_description_and_agent_plugin_summary() {
    crate::plugins::test_catalog::fixture();
    let state = test_state();
    let ws = make_workspace(&state, "plugins-words").await;
    let (status, out) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/plugins"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let card = out["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "test-fixture")
        .unwrap()
        .clone();
    assert!(
        card["description"]
            .as_str()
            .unwrap()
            .starts_with("A plugin that exists only for chimaera's own tests."),
        "{card}"
    );
    assert!(card["recommends_summary"]
        .as_str()
        .unwrap()
        .starts_with("The fixture's agent-side helper"));
    assert_eq!(card["requires_summary"], serde_json::Value::Null);
    assert_eq!(
        card["recommends"],
        serde_json::json!([{
            "agent": "claude",
            "id": "fixture-helper@fixture",
            "marketplace": "acme/fixture-helper",
        }])
    );
    // The catalog route says the same.
    let (_, all) = request(&state, Method::GET, "/api/v1/plugins", None).await;
    let listed = all["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "test-fixture")
        .unwrap()
        .clone();
    assert_eq!(listed["description"], card["description"]);
    assert_eq!(listed["recommends_summary"], card["recommends_summary"]);
}

/// Nothing ships inside the daemon: each first-party plugin is listed as
/// available (the lock's name, summary, pinned version and repository),
/// can't be switched on, and becomes an ordinary installed plugin when
/// installed — and available again when removed.
#[tokio::test]
async fn first_party_plugins_are_available_until_installed() {
    let state = test_state();
    let ws = make_workspace(&state, "plugins-available").await;
    let worker = inject_agent(&state, "kav");
    lock(&state.session_workspaces).insert(worker.clone(), ws.clone());
    let base = tools(&state, &worker, "kav").await;
    let entry = |state: &Arc<AppState>, id: &'static str| {
        let state = state.clone();
        async move {
            let (status, out) = request(&state, Method::GET, "/api/v1/plugins", None).await;
            assert_eq!(status, StatusCode::OK);
            out["plugins"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["id"] == id)
                .cloned()
                .unwrap()
        }
    };
    for l in crate::plugins::lock_entries() {
        let id = l.id.as_str();
        let (_, out) = request(&state, Method::GET, "/api/v1/plugins", None).await;
        let e = out["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .cloned()
            .unwrap();
        assert_eq!(
            e,
            serde_json::json!({
                "id": id,
                "name": l.name,
                "summary": l.summary,
                "description": null,
                "homepage": null,
                "adds": {"ui": [], "agents": []},
                "provides": {"knowledge": null, "mcp_tools": [], "views": []},
                "setup": null,
                "detect": [],
                "requires_summary": null,
                "recommends_summary": null,
                "version": l.version,
                "api": null,
                "source": "available",
                "installed": false,
                "first_party": true,
                "verified": false,
                "repo": l.repo,
                "pinned_version": l.version,
            })
        );
    }

    // Nothing to switch on yet; switching off is harmless.
    let put = |on: bool| {
        let (state, ws) = (state.clone(), ws.clone());
        async move {
            request(
                &state,
                Method::PUT,
                &format!("/api/v1/workspaces/{ws}/plugins/agent-notes"),
                Some(serde_json::json!({"on": on})),
            )
            .await
        }
    };
    let (status, body) = put(true).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"],
        "Agent notes isn't installed — install it first"
    );
    assert_eq!(put(false).await.0, StatusCode::OK);
    for route in ["update", "check", "rollback"] {
        let (status, body) = request(
            &state,
            Method::POST,
            &format!("/api/v1/plugins/agent-notes/{route}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{route}: {body}");
    }
    let (status, _) = request(&state, Method::DELETE, "/api/v1/plugins/agent-notes", None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = request(&state, Method::POST, "/api/v1/plugins/nothing/check", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(tools(&state, &worker, "kav").await, base);

    // Installed (by path, with the release's SHA256SUMS): an ordinary
    // plugin, first-party and verified, whose switch now holds.
    install_first_party(&state, "agent-notes").await;
    let e = entry(&state, "agent-notes").await;
    let l = crate::plugins::lock_entry("agent-notes").unwrap();
    assert_eq!(e["source"], "installed");
    assert_eq!(e["installed"], true);
    assert_eq!(e["first_party"], true);
    assert_eq!(e["verified"], true, "{e}");
    assert_eq!(e["sha256_wasm"], l.sha256_wasm.as_str());
    assert_eq!(e["pinned_version"], l.version.as_str());
    assert_eq!(e["repo"], l.repo.as_str());
    assert!(e["local_path"]
        .as_str()
        .unwrap()
        .ends_with("dist-test/agent-notes"));
    let dir = state
        .plugin_catalog
        .root
        .join("agent-notes")
        .join(&l.version);
    for file in ["plugin.toml", "plugin.wasm", "SHA256SUMS", "local-path"] {
        assert!(dir.join(file).is_file(), "{file}");
    }
    assert_eq!(put(true).await.0, StatusCode::OK);
    let with = tools(&state, &worker, "kav").await;
    assert!(with.contains(&"post_note".to_string()), "{with:?}");

    // Removed: available again, its tools gone, its switch kept for later.
    let (status, body) = request(&state, Method::DELETE, "/api/v1/plugins/agent-notes", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["plugin"]["source"], "available");
    assert_eq!(entry(&state, "agent-notes").await["source"], "available");
    assert_eq!(tools(&state, &worker, "kav").await, base);
    assert!(lock(&state.workspaces)
        .get(&ws)
        .unwrap()
        .plugins_on
        .contains(&"agent-notes".to_string()));
    install_first_party(&state, "agent-notes").await;
    assert!(
        tools(&state, &worker, "kav")
            .await
            .contains(&"post_note".to_string()),
        "reinstalled, a plugin left on is active again"
    );
    state.sessions.kill(&worker).ok();
}

#[tokio::test]
async fn agent_notes_post_read_and_stay_in_their_workspace() {
    let state = test_state();
    install_first_party(&state, "agent-notes").await;
    let ws = make_workspace(&state, "notes-ws").await;
    let other_ws = make_workspace(&state, "notes-other").await;
    let a = inject_agent(&state, "ka");
    let b = inject_agent(&state, "kb");
    let outsider = inject_agent(&state, "ko");
    lock(&state.session_workspaces).insert(a.clone(), ws.clone());
    lock(&state.session_workspaces).insert(b.clone(), ws.clone());
    lock(&state.session_workspaces).insert(outsider.clone(), other_ws.clone());
    lock(&state.workspaces)
        .set_plugin_on(&ws, "agent-notes", true)
        .unwrap();

    let (is_err, text) = mcp_tool_call(
        &state,
        &a,
        "ka",
        "post_note",
        serde_json::json!({"text": "heads-up: loader API changed", "to": b}),
    )
    .await;
    assert!(!is_err, "{text}");
    assert!(text.contains("Nobody's turn was started"), "{text}");

    // Never across workspaces.
    let (is_err, text) = mcp_tool_call(
        &state,
        &a,
        "ka",
        "post_note",
        serde_json::json!({"text": "psst", "to": outsider}),
    )
    .await;
    assert!(is_err, "{text}");

    // b reads it once; framed as information.
    let (_, text) = mcp_tool_call(&state, &b, "kb", "read_notes", serde_json::json!({})).await;
    assert!(text.contains("loader API changed"), "{text}");
    assert!(text.contains("not an instruction"), "{text}");
    let (_, again) = mcp_tool_call(&state, &b, "kb", "read_notes", serde_json::json!({})).await;
    assert!(again.contains("No new notes"), "{again}");

    // The note is on the Timeline.
    let (_, page) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/timeline"),
        None,
    )
    .await;
    assert_eq!(page["entries"][0]["kind"], "note");
    assert_eq!(page["entries"][0]["note"]["to"], b.as_str());

    // Delivering to a terminal agent is refused (nothing types into a TUI).
    let seq = page["entries"][0]["seq"].as_u64().unwrap();
    let (status, _) = request(
        &state,
        Method::POST,
        &format!("/api/v1/workspaces/{ws}/timeline/{seq}/deliver"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    for sid in [a, b, outsider] {
        state.sessions.kill(&sid).ok();
    }
}

#[tokio::test]
async fn worker_settings_pre_allow_only_active_plugin_tools() {
    let tools = vec!["knowledge_search".to_string(), "knowledge_get".to_string()];
    let path =
        crate::agents::write_settings("s-plugin-allow", "K", 1, None, None, None, &tools).unwrap();
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        value["permissions"]["allow"],
        serde_json::json!([
            "mcp__chimaera__notify",
            "mcp__chimaera__document_guide",
            "mcp__chimaera__check_document",
            "mcp__chimaera__knowledge_search",
            "mcp__chimaera__knowledge_get"
        ])
    );
    let _ = std::fs::remove_file(path);
}

/// The port to WASM changed nothing an agent calls: the tool definitions are
/// the ones the native Agent notes gave, and its instruction paragraph (the
/// plugin's own wording) names both tools.
#[tokio::test]
async fn agent_notes_offers_exactly_what_it_always_did() {
    let state = test_state();
    install_first_party(&state, "agent-notes").await;
    let ws = make_workspace(&state, "notes-offer").await;
    let a = inject_agent(&state, "koa");
    lock(&state.session_workspaces).insert(a.clone(), ws.clone());
    let base = instructions(&state, &a, "koa").await;
    lock(&state.workspaces)
        .set_plugin_on(&ws, "agent-notes", true)
        .unwrap();
    let (_, out) = mcp_post(
        &state,
        &a,
        "koa",
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    let tools = out["result"]["tools"].as_array().unwrap();
    let added: Vec<&serde_json::Value> = tools
        .iter()
        .filter(|t| t["name"] == "post_note" || t["name"] == "read_notes")
        .collect();
    assert_eq!(
        serde_json::Value::Array(added.into_iter().cloned().collect()),
        serde_json::json!([
            {
                "name": "post_note",
                "description": "Leave a short note on the workspace Timeline. `to` is a \
                                session id, \"mastermind\", or omitted for everyone. Never \
                                starts anyone's turn.",
                "inputSchema": {
                    "type": "object",
                    "required": ["text"],
                    "properties": {
                        "text": {"type": "string", "description": "The note (under 2 KB)"},
                        "to": {"type": "string", "description": "Session id or \"mastermind\""},
                    },
                    "additionalProperties": false,
                },
            },
            {
                "name": "read_notes",
                "description": "Notes other sessions left — by default the ones for you \
                                (and for everyone) that you haven't read yet.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "all": {"type": "boolean", "description": "Include notes you already read"},
                    },
                    "additionalProperties": false,
                },
            },
        ])
    );
    // The instruction paragraph is the plugin's own text (it owns its
    // wording since 0.1.2): appended after the base, naming both tools.
    let with = instructions(&state, &a, "koa").await;
    let paragraph = with
        .strip_prefix(&format!("{base}\n\n"))
        .unwrap_or_else(|| panic!("the plugin's paragraph follows the base: {with}"));
    assert!(!paragraph.trim().is_empty(), "{with}");
    assert!(
        paragraph.contains("post_note") && paragraph.contains("read_notes"),
        "{paragraph}"
    );
    state.sessions.kill(&a).ok();
}

/// Mail waits to be read: the hook the agent already fires carries a
/// one-line hint, and a read clears it; a session that ends is forgotten.
#[tokio::test]
async fn agent_notes_hint_rides_the_hook_and_clears_on_read() {
    let state = test_state();
    install_first_party(&state, "agent-notes").await;
    let ws = make_workspace(&state, "notes-hint").await;
    let a = inject_agent(&state, "kha");
    let b = inject_agent(&state, "khb");
    lock(&state.session_workspaces).insert(a.clone(), ws.clone());
    lock(&state.session_workspaces).insert(b.clone(), ws.clone());
    lock(&state.workspaces)
        .set_plugin_on(&ws, "agent-notes", true)
        .unwrap();
    let hook = |sid: String, key: &'static str| {
        let state = state.clone();
        async move {
            let (status, out) = request(
                &state,
                Method::POST,
                &format!("/api/v1/agent-events/{sid}?key={key}"),
                Some(serde_json::json!({"hook_event_name": "SessionStart"})),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            out["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap_or("")
                .to_string()
        }
    };
    assert_eq!(hook(b.clone(), "khb").await, "", "no mail, no hint");
    for text in ["one", "two"] {
        let (is_err, out) = mcp_tool_call(
            &state,
            &a,
            "kha",
            "post_note",
            serde_json::json!({"text": text, "to": b}),
        )
        .await;
        assert!(!is_err, "{out}");
    }
    assert_eq!(
        hook(b.clone(), "khb").await,
        "2 unread notes from other sessions in this workspace — read_notes shows them."
    );
    assert_eq!(
        hook(a.clone(), "kha").await,
        "",
        "your own notes aren't mail"
    );
    let (_, text) = mcp_tool_call(&state, &b, "khb", "read_notes", serde_json::json!({})).await;
    assert!(text.contains("> one") && text.contains("> two"), "{text}");
    assert_eq!(hook(b.clone(), "khb").await, "", "read mail has no hint");
    let (_, all) = mcp_tool_call(
        &state,
        &b,
        "khb",
        "read_notes",
        serde_json::json!({"all": true}),
    )
    .await;
    assert!(all.contains("> one"), "all includes what was read: {all}");

    // b ends: its read cursor goes with it.
    let cursors = || {
        lock(&state.plugin_state)
            .get("agent-notes", &ws, "cursors")
            .unwrap_or_default()
    };
    assert!(cursors().contains(&b), "{}", cursors());
    crate::plugins::runtime::session_ended(&state, &b);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while cursors().contains(&b) {
        assert!(std::time::Instant::now() < deadline, "{}", cursors());
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    for sid in [a, b] {
        state.sessions.kill(&sid).ok();
    }
}
