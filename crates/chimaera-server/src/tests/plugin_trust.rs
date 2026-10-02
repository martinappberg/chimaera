//! Trust (docs/design/plugin-platform-plan.md §2, phase P6), over the wire: the
//! trust prompt for a plugin the maintainers haven't verified, an update
//! that asks for more, Skip this version, withdrawing trust, the kill
//! switch's two levels, the admin policy, the activity log, and `[access]`
//! enforced by the host. The fake releases server and the fixture builds
//! are the update tests' (`plugin_updates`).

use serde_json::{json, Value};

use super::plugin_updates::*;
use super::support::*;
use crate::plugins::revoke::{Entry, Level};
use crate::plugins::{test_catalog, trust};
use crate::{lock, AppState};

async fn raw_install(state: &Arc<AppState>, body: Value) -> (StatusCode, Value) {
    request(state, Method::POST, "/api/v1/plugins/install", Some(body)).await
}

async fn switch(state: &Arc<AppState>, ws: &str, id: &str, on: bool) -> (StatusCode, Value) {
    request(
        state,
        Method::PUT,
        &format!("/api/v1/workspaces/{ws}/plugins/{id}"),
        Some(json!({"on": on})),
    )
    .await
}

async fn active_ids(state: &Arc<AppState>, ws: &str) -> Vec<String> {
    crate::plugins::active(state, ws)
        .await
        .iter()
        .map(|m| m.id.clone())
        .collect()
}

fn texts(lines: &Value) -> Vec<String> {
    lines
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["text"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn an_unverified_plugin_installs_only_with_the_digest_the_user_saw() {
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);
    let gh = "acme/tr-ask";
    fake.publish(
        gh,
        "0.1.0",
        &manifest(false, "tr-ask", "0.1.0", ""),
        &v1_wasm(),
    );
    let hits = fake.hits();

    // Asked, not installed: the refusal carries what the prompt shows.
    let (status, body) = raw_install(&state, json!({"github": gh})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let t = &body["trust"];
    assert_eq!(t["id"], "tr-ask");
    assert_eq!(t["source"], "github.com/acme/tr-ask");
    assert_eq!(t["tier"], "sandboxed");
    assert_eq!(
        t["confirm"],
        Value::Null,
        "only a privileged plugin is typed out"
    );
    assert!(t["grown"].is_null(), "nothing runs under that id yet");
    let can = texts(&t["can"]);
    assert!(
        can.contains(&"Reads files in this workspace".to_string()),
        "{can:?}"
    );
    assert!(
        can.iter().any(|l| l.starts_with("Gives agents 8 tools")),
        "{can:?}"
    );
    assert!(body["error"].as_str().unwrap().contains("isn't verified"));
    assert!(!plugin_dir(&state, "tr-ask").join("0.1.0").exists());
    assert!(
        fake.hits() - hits <= 3,
        "no component was fetched before the answer (release, sums, manifest)"
    );

    // A digest other than the one shown is not an answer.
    let (status, body) = raw_install(&state, json!({"github": gh, "trust": "0".repeat(64)})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("changed since you looked"),
        "{body}"
    );

    // The digest shown is: installed, trusted, and a record says so.
    let caps = t["caps"].as_str().unwrap().to_string();
    let (status, body) = raw_install(&state, json!({"github": gh, "trust": caps})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["plugin"]["standing"], "trusted");
    assert_eq!(body["plugin"]["hold"], Value::Null);
    assert_eq!(body["plugin"]["first_party"], false);
    let records = trust::records_for_tests(&state);
    assert!(records.iter().any(|r| r.id == "tr-ask"
        && r.source == "github:acme/tr-ask"
        && r.caps == caps
        && r.how == "prompt"));

    // It switches on and runs.
    let (ws, sid) = workspace_with(&state, "tr-ask", "kt1", &["tr-ask"]).await;
    assert_eq!(active_ids(&state, &ws).await, ["tr-ask"]);
    let (is_err, text) = mcp_tool_call(&state, &sid, "kt1", "echo", json!({})).await;
    assert!(!is_err, "{text}");

    // The activity log has both.
    let (status, log) = request(&state, Method::GET, "/api/v1/plugins/tr-ask/activity", None).await;
    assert_eq!(status, StatusCode::OK);
    let kinds: Vec<&str> = log["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["install", "trust"], "newest first");
    state.sessions.kill(&sid).ok();
}

#[tokio::test]
async fn an_update_that_asks_for_more_waits_and_the_old_build_keeps_running() {
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);
    let gh = "acme/tr-grow";
    fake.publish(
        gh,
        "0.1.0",
        &manifest(false, "tr-grow", "0.1.0", ""),
        &v1_wasm(),
    );
    assert_eq!(install(&state, gh, None).await.0, StatusCode::OK);
    let (ws, sid) = workspace_with(&state, "tr-grow", "kt2", &["tr-grow"]).await;

    // The same capabilities under a new version: no question.
    fake.publish(
        gh,
        "0.1.1",
        &manifest(false, "tr-grow", "0.1.1", ""),
        &v1_wasm(),
    );
    let (status, body) =
        request(&state, Method::POST, "/api/v1/plugins/tr-grow/update", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // One more tool: the update is refused with what it would add, and
    // the running build keeps answering.
    fake.publish(
        gh,
        "0.2.0",
        &manifest(true, "tr-grow", "0.2.0", ""),
        &v2_wasm(),
    );
    let (status, body) =
        request(&state, Method::POST, "/api/v1/plugins/tr-grow/update", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("would do more than 0.1.1"),
        "{body}"
    );
    assert_eq!(body["trust"]["from_version"], "0.1.1");
    let grown = texts(&body["trust"]["grown"]);
    assert_eq!(grown, ["Gives agents 1 tool: version"]);
    assert_eq!(link(&state, "tr-grow", "current").as_deref(), Some("0.1.1"));
    assert_eq!(active_ids(&state, &ws).await, ["tr-grow"]);
    let (is_err, _) = mcp_tool_call(&state, &sid, "kt2", "echo", json!({})).await;
    assert!(!is_err);

    // Skip this version: not offered again (the running build stays).
    let (status, _) = post(&state, "/api/v1/plugins/tr-grow/check").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        listed(&state, "tr-grow").await["update"]["version"],
        "0.2.0"
    );
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/plugins/tr-grow/skip",
        Some(json!({"version": "0.2.0"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entry = listed(&state, "tr-grow").await;
    assert!(entry.get("update").is_none(), "{entry}");
    assert_eq!(entry["skipped_version"], "0.2.0");

    // Allow: the update lands and the skip is gone.
    let caps = body["plugin"]["caps"].as_str().unwrap().to_string();
    let (status, refused) =
        request(&state, Method::POST, "/api/v1/plugins/tr-grow/update", None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let new_caps = refused["trust"]["caps"].as_str().unwrap().to_string();
    assert_ne!(caps, new_caps);
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/plugins/tr-grow/update",
        Some(json!({"trust": new_caps})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(link(&state, "tr-grow", "current").as_deref(), Some("0.2.0"));
    assert!(listed(&state, "tr-grow")
        .await
        .get("skipped_version")
        .is_none());

    // Going back asks for less than what runs: no question.
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/plugins/tr-grow/rollback",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    state.sessions.kill(&sid).ok();
}

#[tokio::test]
async fn withdrawn_trust_turns_a_plugin_off_until_trusted_again() {
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);
    let gh = "acme/tr-untrust";
    fake.publish(
        gh,
        "0.1.0",
        &manifest(false, "tr-untrust", "0.1.0", ""),
        &v1_wasm(),
    );
    assert_eq!(install(&state, gh, None).await.0, StatusCode::OK);
    let (ws, sid) = workspace_with(&state, "tr-untrust", "kt3", &["tr-untrust"]).await;
    assert_eq!(active_ids(&state, &ws).await, ["tr-untrust"]);

    let (status, body) = request(
        &state,
        Method::DELETE,
        "/api/v1/plugins/tr-untrust/trust",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["plugin"]["standing"], "untrusted");
    assert_eq!(body["plugin"]["hold"]["kind"], "untrusted");
    assert!(active_ids(&state, &ws).await.is_empty(), "off at once");
    let names = tool_names(&state, &sid, "kt3").await;
    assert!(!names.contains(&"echo".to_string()), "{names:?}");

    // Switching it on asks the question again.
    switch(&state, &ws, "tr-untrust", false).await;
    let (status, body) = switch(&state, &ws, "tr-untrust", true).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let caps = body["trust"]["caps"].as_str().unwrap().to_string();

    // Trust: an empty ask answers with what there is to trust (an installed
    // build "needs your trust to run"); the digest brings it back, sent as
    // `caps` (the card) or `trust` (the CLI, as install and update name it).
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/plugins/tr-untrust/trust",
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["trust"]["caps"], caps.as_str());
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("needs your trust to run"),
        "{body}"
    );
    let (status, _) = request(
        &state,
        Method::POST,
        "/api/v1/plugins/tr-untrust/trust",
        Some(json!({"trust": caps})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the CLI's key");
    let (status, _) = request(
        &state,
        Method::DELETE,
        "/api/v1/plugins/tr-untrust/trust",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/plugins/tr-untrust/trust",
        Some(json!({"caps": caps})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["plugin"]["standing"], "trusted");
    assert_eq!(
        switch(&state, &ws, "tr-untrust", true).await.0,
        StatusCode::OK
    );
    assert_eq!(active_ids(&state, &ws).await, ["tr-untrust"]);
    state.sessions.kill(&sid).ok();
}

#[tokio::test]
async fn a_local_build_asks_once_per_capability_digest() {
    let state = test_state();
    let src = test_dir("tr-local");
    let toml = unreleased(false, "tr-local", "0.1.0");
    local_build(&src, &toml, &v1_wasm(), None);
    let path_install = |trust: Option<String>| {
        let (state, src) = (state.clone(), src.clone());
        async move {
            let mut body = json!({"path": src});
            if let Some(t) = trust {
                body["trust"] = json!(t);
            }
            raw_install(&state, body).await
        }
    };
    let (status, body) = path_install(None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["trust"]["source"]
        .as_str()
        .unwrap()
        .starts_with("a local build in "));
    let caps = body["trust"]["caps"].as_str().unwrap().to_string();
    assert_eq!(path_install(Some(caps)).await.0, StatusCode::OK);
    // Rebuilt, added again: no question.
    let (status, body) = path_install(None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // A build that asks for more is a new question.
    local_build(
        &src,
        &unreleased(true, "tr-local", "0.1.0"),
        &v2_wasm(),
        None,
    );
    let (status, body) = path_install(None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        texts(&body["trust"]["grown"]),
        ["Gives agents 1 tool: version"]
    );
}

#[tokio::test]
async fn the_kill_switch_stops_a_build_at_once_hard_for_good_soft_until_allowed() {
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);
    let gh = "acme/tr-kill";
    fake.publish(
        gh,
        "0.1.0",
        &manifest(false, "tr-kill", "0.1.0", ""),
        &v1_wasm(),
    );
    assert_eq!(install(&state, gh, None).await.0, StatusCode::OK);
    let (ws, sid) = workspace_with(&state, "tr-kill", "kt4", &["tr-kill"]).await;
    let (is_err, _) = mcp_tool_call(&state, &sid, "kt4", "echo", json!({})).await;
    assert!(!is_err);
    assert_eq!(state.plugin_runtime.live_instances("tr-kill"), 1);

    let block = |level: Level| Entry {
        id: "tr-kill".into(),
        versions: vec!["0.1.0".into()],
        sha256: vec![],
        level,
        reason: "it misbehaves".into(),
    };
    // Soft: off at once, instances dropped; the user may switch it back on.
    crate::plugins::write(&state.plugin_guard.revoked)
        .set_fetched_for_tests(vec![block(Level::Soft)]);
    crate::plugins::revoke::apply(&state).await;
    assert_eq!(state.plugin_runtime.live_instances("tr-kill"), 0);
    assert!(active_ids(&state, &ws).await.is_empty());
    let entry = listed(&state, "tr-kill").await;
    assert_eq!(
        entry["hold"],
        json!({"kind": "blocked", "level": "soft", "reason": "it misbehaves"})
    );
    let (status, body) = switch(&state, &ws, "tr-kill", true).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["blocked"]["level"], "soft");
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/plugins/tr-kill/trust",
        Some(json!({"allow_block": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(active_ids(&state, &ws).await, ["tr-kill"]);

    // Hard: off, and no override.
    crate::plugins::write(&state.plugin_guard.revoked)
        .set_fetched_for_tests(vec![block(Level::Hard)]);
    crate::plugins::revoke::apply(&state).await;
    assert!(active_ids(&state, &ws).await.is_empty());
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/plugins/tr-kill/trust",
        Some(json!({"allow_block": true})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["error"].as_str().unwrap().contains("for good"));
    // An update past the block restores it.
    crate::plugins::write(&state.plugin_guard.revoked).set_fetched_for_tests(vec![]);
    crate::plugins::revoke::apply(&state).await;
    assert_eq!(active_ids(&state, &ws).await, ["tr-kill"]);
    let log = crate::plugins::activity::recent(&state.plugin_catalog.root, "tr-kill", 50);
    assert!(log.iter().any(|e| e["kind"] == "blocked"), "{log:?}");
    assert!(log.iter().any(|e| e["kind"] == "allow-block"), "{log:?}");
    state.sessions.kill(&sid).ok();
}

#[tokio::test]
async fn a_blocked_release_is_refused_before_it_is_fetched() {
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);
    let gh = "acme/tr-banned";
    fake.publish(
        gh,
        "0.1.0",
        &manifest(false, "tr-banned", "0.1.0", ""),
        &v1_wasm(),
    );
    crate::plugins::write(&state.plugin_guard.revoked).set_fetched_for_tests(vec![Entry {
        id: "tr-banned".into(),
        versions: vec![],
        sha256: vec![],
        level: Level::Hard,
        reason: "steals keys".into(),
    }]);
    let (status, body) = raw_install(&state, json!({"github": gh})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        body["error"],
        "Chimaera blocked Test fixture 0.1.0: steals keys"
    );
    assert!(!plugin_dir(&state, "tr-banned").join("0.1.0").exists());
}

#[tokio::test]
async fn an_admin_policy_tightens_and_a_broken_one_fails_closed() {
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);
    let gh = "acme/tr-policy";
    fake.publish(
        gh,
        "0.1.0",
        &manifest(false, "tr-policy", "0.1.0", ""),
        &v1_wasm(),
    );
    assert_eq!(install(&state, gh, None).await.0, StatusCode::OK);
    let (ws, sid) = workspace_with(&state, "tr-policy", "kt5", &["tr-policy"]).await;
    assert_eq!(active_ids(&state, &ws).await, ["tr-policy"]);

    let dir = test_dir("policy");
    let file = dir.join("policy.json");
    std::fs::write(&file, r#"{"plugins": {"allowUnverified": false}}"#).unwrap();
    state.plugin_guard.set_policy_path_for_tests(file.clone());
    assert!(
        active_ids(&state, &ws).await.is_empty(),
        "the trusted plugin is held"
    );
    let entry = listed(&state, "tr-policy").await;
    assert_eq!(entry["hold"]["kind"], "policy");
    let (_, list) = request(&state, Method::GET, "/api/v1/plugins", None).await;
    assert_eq!(list["policy"]["allow_unverified"], false);
    assert_eq!(list["policy"]["managed"], true);
    fake.publish(
        "acme/tr-policy2",
        "0.1.0",
        &manifest(false, "tr-policy2", "0.1.0", ""),
        &v1_wasm(),
    );
    let (status, body) = install(&state, "acme/tr-policy2", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("only allows verified plugins"));

    // A file that doesn't parse: closed, and it says why.
    std::fs::write(&file, "{ not json").unwrap();
    state.plugin_guard.set_policy_path_for_tests(file.clone());
    let (_, list) = request(&state, Method::GET, "/api/v1/plugins", None).await;
    assert_eq!(list["policy"]["allow_unverified"], false);
    assert_eq!(list["policy"]["allow_privileged"], "none");
    assert!(list["policy"]["error"].is_string());

    // The policy's own block list.
    std::fs::write(&file, r#"{"plugins": {"blocked": ["tr-policy"]}}"#).unwrap();
    state.plugin_guard.set_policy_path_for_tests(file.clone());
    assert_eq!(
        listed(&state, "tr-policy").await["hold"]["reason"],
        "this host's policy blocks it"
    );

    // Gone: back to open.
    std::fs::remove_file(&file).unwrap();
    state.plugin_guard.set_policy_path_for_tests(file);
    assert_eq!(active_ids(&state, &ws).await, ["tr-policy"]);
    state.sessions.kill(&sid).ok();
}

#[tokio::test]
async fn the_first_daemon_trusts_what_was_already_installed_and_a_broken_file_trusts_nothing() {
    // Installed by an earlier daemon (planted, no trust file yet).
    let data = test_dir("tr-grandfather");
    let state = test_state_with_data_dir(0, data.clone());
    let src = test_dir("tr-grandfather-src");
    local_build(
        &src,
        &unreleased(false, "tr-old", "0.1.0"),
        &v1_wasm(),
        None,
    );
    assert_eq!(
        request_trusting(
            &state,
            Method::POST,
            "/api/v1/plugins/install",
            Some(json!({"path": src}))
        )
        .await
        .0,
        StatusCode::OK
    );
    drop(state);
    let trust_file = data.join("plugins/trust.json");
    std::fs::remove_file(&trust_file).unwrap();

    let state = test_state_with_data_dir(0, data.clone());
    let entry = listed(&state, "tr-old").await;
    assert_eq!(entry["standing"], "trusted", "{entry}");
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&trust_file).unwrap()).unwrap();
    assert_eq!(written["records"][0]["how"], "grandfathered");
    drop(state);

    std::fs::write(&trust_file, "{ torn").unwrap();
    let state = test_state_with_data_dir(0, data);
    let entry = listed(&state, "tr-old").await;
    assert_eq!(entry["standing"], "untrusted", "{entry}");
    assert_eq!(entry["hold"]["kind"], "untrusted");
}

#[tokio::test]
async fn the_host_refuses_what_access_does_not_allow() {
    let manifest = test_catalog::fixture_manifest()
        .replace("id = \"test-fixture\"", "id = \"fixture-sealed\"")
        .replace(
            "api = \"0.1\"",
            "api = \"0.1\"\n[access]\nfiles = \"none\"\ntimeline = \"none\"\nsessions = \"none\"",
        );
    let m = test_catalog::add(&manifest, test_catalog::fixture_wasm());
    let can = m.caps.lines();
    assert!(can.iter().all(|l| !l.text.contains("Reads")), "{can:?}");

    let state = test_state();
    let ws = make_workspace(&state, "sealed").await;
    std::fs::write(
        lock(&state.workspaces).get(&ws).unwrap().root.join("a.txt"),
        "secret",
    )
    .unwrap();
    let sid = inject_agent(&state, "kt6");
    lock(&state.session_workspaces).insert(sid.clone(), ws.clone());
    lock(&state.workspaces)
        .set_plugin_on(&ws, "fixture-sealed", true)
        .unwrap();

    let (is_err, text) = mcp_tool_call(&state, &sid, "kt6", "read", json!({"path": "a.txt"})).await;
    assert!(is_err && text.contains("[access] files"), "{text}");
    let (is_err, text) = mcp_tool_call(&state, &sid, "kt6", "append", json!({"text": "hi"})).await;
    assert!(is_err && text.contains("[access] timeline"), "{text}");
    let (is_err, text) = mcp_tool_call(&state, &sid, "kt6", "recent", json!({})).await;
    assert!(!is_err && text == "[]", "{text}");
    state.sessions.kill(&sid).ok();
}

#[tokio::test]
async fn a_remove_forgets_the_plugins_state_and_the_users_trust() {
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);
    let gh = "acme/tr-rm";
    fake.publish(
        gh,
        "0.1.0",
        &manifest(false, "tr-rm", "0.1.0", ""),
        &v1_wasm(),
    );
    assert_eq!(install(&state, gh, None).await.0, StatusCode::OK);
    let (ws, sid) = workspace_with(&state, "tr-rm", "kt7", &["tr-rm"]).await;
    let (is_err, _) = mcp_tool_call(
        &state,
        &sid,
        "kt7",
        "state",
        json!({"key": "k", "value": 1}),
    )
    .await;
    assert!(!is_err);
    assert!(lock(&state.plugin_state).holds("tr-rm", &ws));

    let (status, _) = request(&state, Method::DELETE, "/api/v1/plugins/tr-rm", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!lock(&state.plugin_state).holds("tr-rm", &ws));
    assert!(trust::records_for_tests(&state)
        .iter()
        .all(|r| r.id != "tr-rm"));
    let (status, body) = raw_install(&state, json!({"github": gh})).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a new install is a new question: {body}"
    );
    state.sessions.kill(&sid).ok();
}

#[tokio::test]
async fn a_privileged_first_party_release_past_the_pin_asks() {
    let (l, toml, wasm, sums) = locked_release("latex");
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);
    fake.publish_with_sums(&l.repo, &l.version, &toml, &wasm, &sums);
    let (status, body) = request(&state, Method::POST, "/api/v1/plugins/latex/install", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["plugin"]["standing"], "verified");
    assert_eq!(body["plugin"]["tier"], "privileged");
    // The next release asks for nothing more, but the lock vouches for the
    // pin alone: a privileged update comes through the lock or asks.
    let next = toml.replacen(
        &format!("version = \"{}\"", l.version),
        "version = \"9.9.9\"",
        1,
    );
    fake.publish(&l.repo, "9.9.9", &next, &wasm);
    let (status, body) = request(&state, Method::POST, "/api/v1/plugins/latex/update", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["trust"]["tier"], "privileged", "{body}");
    assert_eq!(body["trust"]["grown"], json!([]), "{body}");
    assert_eq!(
        link(&state, "latex", "current").as_deref(),
        Some(l.version.as_str())
    );
}

#[tokio::test]
async fn a_first_party_plugin_is_verified_and_asks_nothing() {
    let (l, toml, wasm, sums) = locked_release("mycelium");
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);
    fake.publish_with_sums(&l.repo, &l.version, &toml, &wasm, &sums);
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/plugins/mycelium/install",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let p = &body["plugin"];
    assert_eq!(p["standing"], "verified");
    assert_eq!(p["first_party"], true);
    assert_eq!(p["tier"], "sandboxed");
    assert_eq!(p["caps"], l.caps.as_str());
    assert!(texts(&p["can"])
        .iter()
        .any(|t| t.contains("knowledge_search")));
    assert!(
        trust::records_for_tests(&state).is_empty(),
        "the lock covers it: no record"
    );
}
