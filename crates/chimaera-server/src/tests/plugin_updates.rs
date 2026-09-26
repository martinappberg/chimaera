//! Versions, the installed directory and updates, over the wire and against
//! a local fake releases server (the GitHub releases API shape: a release's
//! JSON plus its `plugin.wasm`, `plugin.toml` and `SHA256SUMS`). The
//! components are the host's fixture (`plugins/test-fixture`) and its "next
//! release" — the same crate built with its `v2` feature, one more tool
//! (`version`) — both laid out in `plugins/dist-test` by
//! `scripts/build-plugins.sh`. Every test installs under its own id, so the
//! process-wide test catalog never makes one test's precedence another's.

use std::collections::HashMap;

use axum::response::IntoResponse;
use serde_json::{json, Value};

use super::support::*;
use crate::plugins::test_catalog;
use crate::{lock, AppState};

type Files = Arc<std::sync::Mutex<HashMap<String, Vec<u8>>>>;

/// A releases API on 127.0.0.1: `{api}/{owner}/{repo}/releases/latest`, the
/// `…/releases/tags/v{version}` of every published version, and the assets
/// under `/dl/`.
struct FakeReleases {
    base: String,
    files: Files,
}

impl FakeReleases {
    async fn start() -> Self {
        let files: Files = Arc::default();
        let served = files.clone();
        let app = axum::Router::new().fallback(move |uri: axum::http::Uri| {
            let files = served.clone();
            async move {
                match lock(&files).get(uri.path()).cloned() {
                    Some(body) => (StatusCode::OK, body).into_response(),
                    None => StatusCode::NOT_FOUND.into_response(),
                }
            }
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        FakeReleases { base, files }
    }

    fn api(&self) -> String {
        format!("{}/api", self.base)
    }

    /// Publish `version` of `github` as its latest release.
    fn publish(&self, github: &str, version: &str, toml: &str, wasm: &[u8]) {
        let sums = format!(
            "{}  plugin.wasm\n{}  plugin.toml\n",
            crate::fs::sha256_hex(wasm),
            crate::fs::sha256_hex(toml.as_bytes())
        );
        self.publish_with_sums(github, version, toml, wasm, &sums);
    }

    fn publish_with_sums(&self, github: &str, version: &str, toml: &str, wasm: &[u8], sums: &str) {
        let dl = format!("/dl/{github}/{version}");
        let asset = |name: &str| json!({"name": name, "browser_download_url": format!("{}{dl}/{name}", self.base)});
        let release = json!({
            "tag_name": format!("v{version}"),
            "html_url": format!("https://github.com/{github}/releases/tag/v{version}"),
            "assets": [asset("plugin.wasm"), asset("plugin.toml"), asset("SHA256SUMS")],
        })
        .to_string()
        .into_bytes();
        let mut files = lock(&self.files);
        files.insert(format!("{dl}/plugin.wasm"), wasm.to_vec());
        files.insert(format!("{dl}/plugin.toml"), toml.as_bytes().to_vec());
        files.insert(format!("{dl}/SHA256SUMS"), sums.as_bytes().to_vec());
        files.insert(
            format!("/api/{github}/releases/tags/v{version}"),
            release.clone(),
        );
        files.insert(format!("/api/{github}/releases/latest"), release);
    }
}

fn v1_wasm() -> Vec<u8> {
    test_catalog::fixture_wasm()
}

fn v2_wasm() -> Vec<u8> {
    test_catalog::dist_test_bytes("test-fixture-v2/plugin.wasm")
}

/// The fixture's manifest (`v2`: its next release's) as plugin `id` at
/// `version`, released from `acme/<id>`, plus `extra` TOML at the end.
fn manifest(v2: bool, id: &str, version: &str, extra: &str) -> String {
    let base = if v2 {
        test_catalog::dist_test_text("test-fixture-v2/plugin.toml")
    } else {
        test_catalog::fixture_manifest()
    };
    let body: Vec<String> = base
        .lines()
        .map(|line| {
            if line.starts_with("id = ") {
                format!("id = \"{id}\"")
            } else if line.starts_with("version = ") {
                format!("version = \"{version}\"")
            } else {
                line.to_string()
            }
        })
        .collect();
    format!(
        "{}\n{extra}\n[release]\ngithub = \"acme/{id}\"\n",
        body.join("\n")
    )
}

/// A test daemon whose plugin release fetches go to `fake`.
fn state_for(fake: &FakeReleases) -> Arc<AppState> {
    let state = test_state();
    state.plugin_releases.set_api_for_tests(&fake.api());
    state
}

async fn install(
    state: &Arc<AppState>,
    github: &str,
    version: Option<&str>,
) -> (StatusCode, Value) {
    request(
        state,
        Method::POST,
        "/api/v1/plugins/install",
        Some(json!({"github": github, "version": version})),
    )
    .await
}

async fn post(state: &Arc<AppState>, uri: &str) -> (StatusCode, Value) {
    request(state, Method::POST, uri, None).await
}

/// `id`'s entry in GET /plugins (Null when it isn't listed).
async fn listed(state: &Arc<AppState>, id: &str) -> Value {
    let (status, body) = request(state, Method::GET, "/api/v1/plugins", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == id)
        .cloned()
        .unwrap_or(Value::Null)
}

fn plugin_dir(state: &Arc<AppState>, id: &str) -> PathBuf {
    state.plugin_catalog.root.join(id)
}

fn link(state: &Arc<AppState>, id: &str, name: &str) -> Option<String> {
    std::fs::read_link(plugin_dir(state, id).join(name))
        .ok()
        .map(|t| t.to_string_lossy().into_owned())
}

/// No staging left behind under the plugin's directory.
fn no_temp_left(state: &Arc<AppState>, id: &str) {
    let left: Vec<String> = std::fs::read_dir(plugin_dir(state, id))
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with(".tmp-"))
                .collect()
        })
        .unwrap_or_default();
    assert!(left.is_empty(), "staging left behind: {left:?}");
}

/// A workspace with `plugins` switched on and one agent session in it.
async fn workspace_with(
    state: &Arc<AppState>,
    label: &str,
    key: &str,
    plugins: &[&str],
) -> (String, String) {
    let ws = make_workspace(state, label).await;
    let sid = inject_agent(state, key);
    lock(&state.session_workspaces).insert(sid.clone(), ws.clone());
    for id in plugins {
        let (status, body) = request(
            state,
            Method::PUT,
            &format!("/api/v1/workspaces/{ws}/plugins/{id}"),
            Some(json!({"on": true})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{id}: {body}");
    }
    (ws, sid)
}

async fn tool_names(state: &Arc<AppState>, sid: &str, key: &str) -> Vec<String> {
    let (_, out) = mcp_post(
        state,
        sid,
        key,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    out["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn install_writes_the_version_dir_and_current_and_lists_it_installed() {
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);
    let toml = manifest(false, "up-install", "0.1.0", "");
    fake.publish("acme/up-install", "0.1.0", &toml, &v1_wasm());

    let (status, body) = install(&state, "acme/up-install", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["id"], "up-install");
    assert_eq!(body["version"], "0.1.0");
    assert_eq!(body["previous"], Value::Null);
    assert_eq!(
        body["sha256"]["plugin.wasm"],
        crate::fs::sha256_hex(&v1_wasm())
    );
    assert_eq!(body["plugin"]["source"], "installed");

    let dir = plugin_dir(&state, "up-install");
    assert_eq!(
        std::fs::read(dir.join("0.1.0/plugin.wasm")).unwrap(),
        v1_wasm()
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("0.1.0/plugin.toml")).unwrap(),
        toml
    );
    assert_eq!(
        link(&state, "up-install", "current").as_deref(),
        Some("0.1.0")
    );
    assert_eq!(link(&state, "up-install", "previous"), None);
    no_temp_left(&state, "up-install");

    let entry = listed(&state, "up-install").await;
    assert_eq!(entry["version"], "0.1.0");
    assert_eq!(entry["api"], "0.1");
    assert_eq!(entry["source"], "installed");
    assert_eq!(entry["installed_version"], "0.1.0");
    assert_eq!(entry["stale"], false);
    assert_eq!(entry["path"], json!(dir.join("0.1.0")));
    for absent in ["embedded_version", "previous", "update", "fault"] {
        assert!(entry.get(absent).is_none(), "{absent}: {entry}");
    }
    // The shipped plugins say where they came from too.
    let notes = listed(&state, "agent-notes").await;
    assert_eq!(notes["source"], "embedded");
    assert_eq!(notes["version"], "0.1.0");
    assert!(notes.get("path").is_none());

    let (status, body) = install(&state, "acme/up-install", Some("0.1.0")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("already installed"));

    // A pinned version the source never released.
    let (status, _) = install(&state, "acme/up-install", Some("9.9.9")).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let (status, _) = install(&state, "../etc", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_change_routes_are_bearer_authed() {
    let state = test_state();
    for (method, uri) in [
        (Method::POST, "/api/v1/plugins/install"),
        (Method::POST, "/api/v1/plugins/x/update"),
        (Method::POST, "/api/v1/plugins/x/rollback"),
        (Method::POST, "/api/v1/plugins/x/check"),
        (Method::DELETE, "/api/v1/plugins/x"),
    ] {
        let res = crate::app(state.clone())
            .oneshot(
                Request::builder()
                    .method(method.clone())
                    .uri(uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"github":"a/b"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
}

#[tokio::test]
async fn a_newer_compatible_release_is_offered_and_an_older_or_incompatible_one_is_not() {
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);
    let gh = "acme/up-offer";
    fake.publish(
        gh,
        "0.1.0",
        &manifest(false, "up-offer", "0.1.0", ""),
        &v1_wasm(),
    );
    assert_eq!(install(&state, gh, None).await.0, StatusCode::OK);

    let check = || async {
        let (status, body) = post(&state, "/api/v1/plugins/up-offer/check").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    };
    // The latest is what runs: nothing to offer.
    assert_eq!(check().await["update"], Value::Null);

    // Older: never offered.
    fake.publish(
        gh,
        "0.0.9",
        &manifest(false, "up-offer", "0.0.9", ""),
        &v1_wasm(),
    );
    assert_eq!(check().await["update"], Value::Null);
    assert!(listed(&state, "up-offer").await.get("update").is_none());

    // Newer and runnable here: offered, on the card too.
    fake.publish(
        gh,
        "0.2.0",
        &manifest(true, "up-offer", "0.2.0", ""),
        &v2_wasm(),
    );
    let body = check().await;
    assert_eq!(body["update"]["version"], "0.2.0");
    assert_eq!(body["plugin"]["update"]["version"], "0.2.0");
    let entry = listed(&state, "up-offer").await;
    assert_eq!(entry["update"]["version"], "0.2.0");
    assert!(entry["update"]["url"]
        .as_str()
        .unwrap()
        .ends_with("/tag/v0.2.0"));
    assert!(entry["update"]["checked_ms"].as_u64().unwrap() > 0);
    // The checker never downloads a component on its own.
    assert!(!plugin_dir(&state, "up-offer").join("0.2.0").exists());

    // Newer, but for a plugin API this host doesn't serve: not offered.
    let api = manifest(true, "up-offer", "0.3.0", "").replace("api = \"0.1\"", "api = \"0.2\"");
    fake.publish(gh, "0.3.0", &api, &v2_wasm());
    assert_eq!(check().await["update"], Value::Null);
    assert!(listed(&state, "up-offer").await.get("update").is_none());

    // Newer, but it needs a newer chimaera than this one: not offered.
    state.plugin_catalog.set_daemon_version_for_tests("0.4.1");
    let req = manifest(
        true,
        "up-offer",
        "0.3.0",
        "[requires]\nchimaera = \">=0.5.0\"",
    );
    fake.publish(gh, "0.3.0", &req, &v2_wasm());
    assert_eq!(check().await["update"], Value::Null);
    // …and offered once it matches.
    let req = manifest(
        true,
        "up-offer",
        "0.3.0",
        "[requires]\nchimaera = \">=0.4.0\"",
    );
    fake.publish(gh, "0.3.0", &req, &v2_wasm());
    assert_eq!(check().await["update"]["version"], "0.3.0");

    // A plugin that only ships with chimaera updates with chimaera.
    let (status, body) = post(&state, "/api/v1/plugins/agent-notes/check").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("ships with chimaera"));
    let (status, _) = post(&state, "/api/v1/plugins/agent-notes/update").await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn an_update_swaps_current_drops_instances_and_the_next_tools_list_is_the_new_one() {
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);
    let gh = "acme/up-swap";
    fake.publish(
        gh,
        "0.1.0",
        &manifest(false, "up-swap", "0.1.0", ""),
        &v1_wasm(),
    );
    assert_eq!(install(&state, gh, None).await.0, StatusCode::OK);
    let (_ws, sid) = workspace_with(&state, "up-swap", "ks1", &["up-swap"]).await;

    let before = tool_names(&state, &sid, "ks1").await;
    assert!(before.contains(&"echo".to_string()), "{before:?}");
    assert!(!before.contains(&"version".to_string()), "{before:?}");
    let (is_err, text) = mcp_tool_call(&state, &sid, "ks1", "echo", json!({})).await;
    assert!(!is_err, "{text}");
    assert_eq!(state.plugin_runtime.live_instances("up-swap"), 1);

    fake.publish(
        gh,
        "0.2.0",
        &manifest(true, "up-swap", "0.2.0", ""),
        &v2_wasm(),
    );
    let (status, body) = post(&state, "/api/v1/plugins/up-swap/update").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["version"], "0.2.0");
    assert_eq!(body["previous"], "0.1.0");
    assert_eq!(
        body["sha256"]["plugin.wasm"],
        crate::fs::sha256_hex(&v2_wasm())
    );
    assert_eq!(
        state.plugin_runtime.live_instances("up-swap"),
        0,
        "moving current drops the plugin's instances"
    );
    assert_eq!(link(&state, "up-swap", "current").as_deref(), Some("0.2.0"));
    assert_eq!(
        link(&state, "up-swap", "previous").as_deref(),
        Some("0.1.0")
    );
    no_temp_left(&state, "up-swap");

    let after = tool_names(&state, &sid, "ks1").await;
    assert!(after.contains(&"version".to_string()), "{after:?}");
    let (is_err, text) = mcp_tool_call(&state, &sid, "ks1", "version", json!({})).await;
    assert!(!is_err, "{text}");
    assert_eq!(text, "0.2.0", "the new build answers");

    let entry = listed(&state, "up-swap").await;
    assert_eq!(entry["version"], "0.2.0");
    assert_eq!(entry["previous"], "0.1.0");

    let (status, body) = post(&state, "/api/v1/plugins/up-swap/update").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["error"].as_str().unwrap().contains("up to date"));
    state.sessions.kill(&sid).ok();
}

#[tokio::test]
async fn a_bad_checksum_leaves_the_old_version_current() {
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);
    let gh = "acme/up-sum";
    fake.publish(
        gh,
        "0.1.0",
        &manifest(false, "up-sum", "0.1.0", ""),
        &v1_wasm(),
    );
    assert_eq!(install(&state, gh, None).await.0, StatusCode::OK);

    // The component's checksum is wrong.
    let toml = manifest(true, "up-sum", "0.2.0", "");
    let sums = format!(
        "{}  plugin.wasm\n{}  plugin.toml\n",
        "0".repeat(64),
        crate::fs::sha256_hex(toml.as_bytes())
    );
    fake.publish_with_sums(gh, "0.2.0", &toml, &v2_wasm(), &sums);
    let (status, body) = post(&state, "/api/v1/plugins/up-sum/update").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let error = body["error"].as_str().unwrap();
    assert!(
        error.contains("plugin.wasm does not match") && error.contains(&"0".repeat(64)),
        "{error}"
    );
    assert_eq!(link(&state, "up-sum", "current").as_deref(), Some("0.1.0"));
    assert!(!plugin_dir(&state, "up-sum").join("0.2.0").exists());
    no_temp_left(&state, "up-sum");
    assert_eq!(listed(&state, "up-sum").await["version"], "0.1.0");

    // The manifest's checksum is wrong: refused before any download.
    let sums = format!(
        "{}  plugin.wasm\n{}  plugin.toml\n",
        crate::fs::sha256_hex(&v2_wasm()),
        "1".repeat(64)
    );
    fake.publish_with_sums(gh, "0.2.0", &toml, &v2_wasm(), &sums);
    let (status, body) = post(&state, "/api/v1/plugins/up-sum/update").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("plugin.toml does not match"));

    // No checksum for it at all.
    fake.publish_with_sums(gh, "0.2.0", &toml, &v2_wasm(), "");
    let (status, body) = post(&state, "/api/v1/plugins/up-sum/update").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(link(&state, "up-sum", "current").as_deref(), Some("0.1.0"));
    no_temp_left(&state, "up-sum");
}

#[tokio::test]
async fn use_previous_swaps_back_and_is_reversible() {
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);
    let gh = "acme/up-back";
    fake.publish(
        gh,
        "0.1.0",
        &manifest(false, "up-back", "0.1.0", ""),
        &v1_wasm(),
    );
    assert_eq!(install(&state, gh, None).await.0, StatusCode::OK);
    let (status, body) = post(&state, "/api/v1/plugins/up-back/rollback").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("no previous version"));

    fake.publish(
        gh,
        "0.2.0",
        &manifest(true, "up-back", "0.2.0", ""),
        &v2_wasm(),
    );
    assert_eq!(
        post(&state, "/api/v1/plugins/up-back/update").await.0,
        StatusCode::OK
    );
    let (_ws, sid) = workspace_with(&state, "up-back", "kb1", &["up-back"]).await;
    assert!(tool_names(&state, &sid, "kb1")
        .await
        .contains(&"version".to_string()));

    let (status, body) = post(&state, "/api/v1/plugins/up-back/rollback").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["version"], "0.1.0");
    assert_eq!(body["previous"], "0.2.0");
    assert_eq!(body["plugin"]["version"], "0.1.0");
    assert_eq!(link(&state, "up-back", "current").as_deref(), Some("0.1.0"));
    assert_eq!(
        link(&state, "up-back", "previous").as_deref(),
        Some("0.2.0")
    );
    assert_eq!(state.plugin_runtime.live_instances("up-back"), 0);
    assert!(!tool_names(&state, &sid, "kb1")
        .await
        .contains(&"version".to_string()));

    // And forward again: the newer version was kept.
    let (status, body) = post(&state, "/api/v1/plugins/up-back/rollback").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["version"], "0.2.0");
    assert!(tool_names(&state, &sid, "kb1")
        .await
        .contains(&"version".to_string()));

    let (status, _) = post(&state, "/api/v1/plugins/agent-notes/rollback").await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = post(&state, "/api/v1/plugins/nothing-here/rollback").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    state.sessions.kill(&sid).ok();
}

#[tokio::test]
async fn remove_deletes_the_plugins_directory() {
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);
    let gh = "acme/up-rm";
    fake.publish(
        gh,
        "0.1.0",
        &manifest(false, "up-rm", "0.1.0", ""),
        &v1_wasm(),
    );
    assert_eq!(install(&state, gh, None).await.0, StatusCode::OK);
    fake.publish(
        gh,
        "0.2.0",
        &manifest(true, "up-rm", "0.2.0", ""),
        &v2_wasm(),
    );
    assert_eq!(
        post(&state, "/api/v1/plugins/up-rm/check").await.0,
        StatusCode::OK
    );
    assert_eq!(
        post(&state, "/api/v1/plugins/up-rm/update").await.0,
        StatusCode::OK
    );
    let (_ws, sid) = workspace_with(&state, "up-rm", "kr1", &["up-rm"]).await;
    assert!(tool_names(&state, &sid, "kr1")
        .await
        .contains(&"echo".to_string()));

    let (status, body) = request(&state, Method::DELETE, "/api/v1/plugins/up-rm", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["plugin"], Value::Null, "nothing ships under that id");
    assert!(!plugin_dir(&state, "up-rm").exists(), "every version goes");
    assert_eq!(listed(&state, "up-rm").await, Value::Null);
    assert!(!tool_names(&state, &sid, "kr1")
        .await
        .contains(&"echo".to_string()));
    assert_eq!(state.plugin_runtime.live_instances("up-rm"), 0);

    let (status, _) = request(&state, Method::DELETE, "/api/v1/plugins/up-rm", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = request(&state, Method::DELETE, "/api/v1/plugins/agent-notes", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("ships with chimaera"));
    state.sessions.kill(&sid).ok();
}

#[tokio::test]
async fn precedence_picks_the_higher_version_and_names_both() {
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);
    // "Embedded" at 0.1.0 (a test build's catalog extra), installed at 0.2.0.
    test_catalog::add(&manifest(false, "prec-high", "0.1.0", ""), v1_wasm());
    let gh = "acme/prec-high";
    fake.publish(
        gh,
        "0.2.0",
        &manifest(true, "prec-high", "0.2.0", ""),
        &v2_wasm(),
    );
    assert_eq!(install(&state, gh, None).await.0, StatusCode::OK);
    let entry = listed(&state, "prec-high").await;
    assert_eq!(entry["version"], "0.2.0");
    assert_eq!(entry["source"], "installed");
    assert_eq!(entry["embedded_version"], "0.1.0");
    assert_eq!(entry["installed_version"], "0.2.0");
    assert_eq!(entry["stale"], false);

    // Removed: the embedded copy takes over.
    let (status, body) = request(&state, Method::DELETE, "/api/v1/plugins/prec-high", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["plugin"]["source"], "embedded");
    assert_eq!(body["plugin"]["version"], "0.1.0");
    assert!(body["plugin"].get("installed_version").is_none());

    // Equal versions: the embedded copy loads; the installed one is named.
    test_catalog::add(&manifest(false, "prec-equal", "0.1.0", ""), v1_wasm());
    let gh = "acme/prec-equal";
    fake.publish(
        gh,
        "0.1.0",
        &manifest(false, "prec-equal", "0.1.0", ""),
        &v1_wasm(),
    );
    assert_eq!(install(&state, gh, None).await.0, StatusCode::OK);
    let entry = listed(&state, "prec-equal").await;
    assert_eq!(entry["source"], "embedded");
    assert_eq!(entry["installed_version"], "0.1.0");
    assert_eq!(entry["stale"], false);
}

#[tokio::test]
async fn an_older_installed_copy_is_flagged_stale() {
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);
    test_catalog::add(&manifest(false, "stale-copy", "0.3.0", ""), v1_wasm());
    let gh = "acme/stale-copy";
    fake.publish(
        gh,
        "0.2.0",
        &manifest(false, "stale-copy", "0.2.0", ""),
        &v1_wasm(),
    );
    assert_eq!(install(&state, gh, None).await.0, StatusCode::OK);
    let entry = listed(&state, "stale-copy").await;
    assert_eq!(entry["version"], "0.3.0");
    assert_eq!(entry["source"], "embedded");
    assert_eq!(entry["stale"], true);
    assert_eq!(entry["installed_version"], "0.2.0");
    assert_eq!(
        entry["path"],
        json!(plugin_dir(&state, "stale-copy").join("0.2.0"))
    );
    // Its release is still asked: something newer than what runs is offered.
    fake.publish(
        gh,
        "0.4.0",
        &manifest(false, "stale-copy", "0.4.0", ""),
        &v1_wasm(),
    );
    let (_, body) = post(&state, "/api/v1/plugins/stale-copy/check").await;
    assert_eq!(body["update"]["version"], "0.4.0");
}

#[tokio::test]
async fn the_gates_refuse_an_install_and_turn_off_an_installed_copy_that_stops_passing() {
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);

    let api = manifest(false, "gate-api", "0.1.0", "").replace("api = \"0.1\"", "api = \"0.2\"");
    fake.publish("acme/gate-api", "0.1.0", &api, &v1_wasm());
    let (status, body) = install(&state, "acme/gate-api", None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("needs a newer chimaera"),
        "{body}"
    );
    assert!(!plugin_dir(&state, "gate-api").join("0.1.0").exists());

    let req = manifest(
        false,
        "gate-req",
        "0.1.0",
        "[requires]\nchimaera = \">=0.5.0\"",
    );
    fake.publish("acme/gate-req", "0.1.0", &req, &v1_wasm());
    state.plugin_catalog.set_daemon_version_for_tests("0.4.1");
    let (status, body) = install(&state, "acme/gate-req", None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("needs chimaera ≥ 0.5.0 (this is 0.4.1)"),
        "{body}"
    );

    // Installed on a daemon it runs on, then that daemon is older (a
    // downgrade): listed, off, and saying why.
    state.plugin_catalog.set_daemon_version_for_tests("0.5.0");
    assert_eq!(
        install(&state, "acme/gate-req", None).await.0,
        StatusCode::OK
    );
    let (ws, sid) = workspace_with(&state, "gate-req", "kg1", &["gate-req"]).await;
    assert!(tool_names(&state, &sid, "kg1")
        .await
        .contains(&"echo".to_string()));
    state.plugin_catalog.set_daemon_version_for_tests("0.4.1");
    let (_, out) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/plugins"),
        None,
    )
    .await;
    let card = out["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "gate-req")
        .unwrap()
        .clone();
    assert_eq!(card["on"], false, "{card}");
    assert_eq!(card["active"], false);
    assert_eq!(card["fault"], "needs chimaera ≥ 0.5.0 (this is 0.4.1)");
    assert!(!tool_names(&state, &sid, "kg1")
        .await
        .contains(&"echo".to_string()));
    let (status, body) = request(
        &state,
        Method::PUT,
        &format!("/api/v1/workspaces/{ws}/plugins/gate-req"),
        Some(json!({"on": true})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    // The switch was kept: it holds again once the gate passes.
    state.plugin_catalog.set_daemon_version_for_tests("0.5.0");
    assert!(tool_names(&state, &sid, "kg1")
        .await
        .contains(&"echo".to_string()));
    state.sessions.kill(&sid).ok();
}

#[tokio::test]
async fn events_reach_only_the_plugins_that_declared_them() {
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);
    let gh = "acme/up-events";
    fake.publish(
        gh,
        "0.1.0",
        &manifest(false, "up-events", "0.1.0", ""),
        &v1_wasm(),
    );
    assert_eq!(install(&state, gh, None).await.0, StatusCode::OK);
    let (ws, a) = workspace_with(&state, "up-events", "ke1", &["agent-notes", "up-events"]).await;
    let b = inject_agent(&state, "ke2");
    lock(&state.session_workspaces).insert(b.clone(), ws.clone());

    let (is_err, text) = mcp_tool_call(
        &state,
        &a,
        "ke1",
        "post_note",
        json!({"text": "hi", "to": b}),
    )
    .await;
    assert!(!is_err, "{text}");
    let (status, out) = request(
        &state,
        Method::POST,
        &format!("/api/v1/agent-events/{b}?key=ke2"),
        Some(json!({"hook_event_name": "SessionStart"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        out["hookSpecificOutput"]["additionalContext"],
        "1 unread note from other sessions in this workspace — read_notes shows it.",
        "agent notes declared `hook` and still answers it"
    );
    assert_eq!(
        state.plugin_runtime.live_instances("up-events"),
        0,
        "a plugin that declared no events is never instantiated by a hook"
    );
    assert!(crate::plugins::active(&state, &ws)
        .await
        .iter()
        .any(|m| m.id == "up-events"));
    for sid in [a, b] {
        state.sessions.kill(&sid).ok();
    }
}
