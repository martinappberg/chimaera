//! Installs, versions and updates, over the wire and against a local fake
//! releases server (the GitHub releases API shape: a release's JSON plus its
//! `plugin.wasm`, `plugin.toml` and `SHA256SUMS`, and the same assets at
//! their direct download URLs, which a first-party install uses). The
//! components are the host's fixture (`plugins/test-fixture`) and its "next
//! release" — the same crate built with its `v2` feature, one more tool
//! (`version`) — and the first-party releases the lock pins, all laid out in
//! `plugins/dist-test` by `scripts/build-plugins.sh`. Every test installs
//! under its own id (or its own state), so no test sees another's copies.

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

    fn downloads(&self) -> String {
        format!("{}/gh", self.base)
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
        // The API's asset URLs, and the direct download URLs
        // (`{base}/{owner}/{repo}/releases/download/v{version}/{asset}`).
        let direct = format!("/gh/{github}/releases/download/v{version}");
        for dir in [&dl, &direct] {
            files.insert(format!("{dir}/plugin.wasm"), wasm.to_vec());
            files.insert(format!("{dir}/plugin.toml"), toml.as_bytes().to_vec());
            files.insert(format!("{dir}/SHA256SUMS"), sums.as_bytes().to_vec());
        }
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

/// `manifest` without its `[release]` section: a plugin with nowhere to
/// check or update from.
fn unreleased(v2: bool, id: &str, version: &str) -> String {
    let full = manifest(v2, id, version, "");
    let cut = full.find("\n[release]").expect("manifest names a release");
    full[..cut].to_string()
}

fn sums_of(wasm: &[u8], toml: &str) -> String {
    format!(
        "{}  plugin.wasm\n{}  plugin.toml\n",
        crate::fs::sha256_hex(wasm),
        crate::fs::sha256_hex(toml.as_bytes())
    )
}

/// A local build's directory: its manifest, its component and, when given,
/// a `SHA256SUMS`.
fn local_build(src: &std::path::Path, toml: &str, wasm: &[u8], sums: Option<&str>) {
    std::fs::write(src.join("plugin.toml"), toml).unwrap();
    std::fs::write(src.join("plugin.wasm"), wasm).unwrap();
    match sums {
        Some(sums) => std::fs::write(src.join("SHA256SUMS"), sums).unwrap(),
        None => {
            let _ = std::fs::remove_file(src.join("SHA256SUMS"));
        }
    }
}

/// The first-party release the lock pins for Agent notes, as the build
/// script laid it out: its lock entry, manifest, component and SHA256SUMS.
fn agent_notes_release() -> (&'static crate::plugins::Locked, String, Vec<u8>, String) {
    (
        crate::plugins::lock_entry("agent-notes").unwrap(),
        test_catalog::dist_test_text("agent-notes/plugin.toml"),
        test_catalog::dist_test_bytes("agent-notes/plugin.wasm"),
        test_catalog::dist_test_text("agent-notes/SHA256SUMS"),
    )
}

/// A test daemon whose plugin release fetches go to `fake`.
fn state_for(fake: &FakeReleases) -> Arc<AppState> {
    let state = test_state();
    state.plugin_releases.set_api_for_tests(&fake.api());
    state
        .plugin_releases
        .set_downloads_for_tests(&fake.downloads());
    state
}

/// Re-read the installed copies, as the catalog does after a change.
async fn reload(state: &Arc<AppState>) {
    let reloading = state.clone();
    tokio::task::spawn_blocking(move || reloading.plugin_catalog.reload())
        .await
        .unwrap();
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

    // The release's SHA256SUMS is kept beside the two files.
    assert_eq!(
        std::fs::read_to_string(dir.join("0.1.0/SHA256SUMS")).unwrap(),
        sums_of(&v1_wasm(), &toml)
    );
    assert!(!dir.join("0.1.0/local-path").exists());

    let entry = listed(&state, "up-install").await;
    assert_eq!(entry["version"], "0.1.0");
    assert_eq!(entry["api"], "0.1");
    assert_eq!(entry["source"], "installed");
    assert_eq!(entry["installed"], true);
    assert_eq!(entry["first_party"], false, "not in the lock");
    assert_eq!(
        entry["verified"], true,
        "its files match the kept SHA256SUMS"
    );
    assert_eq!(entry["sha256_wasm"], crate::fs::sha256_hex(&v1_wasm()));
    assert_eq!(entry["repo"], "acme/up-install");
    assert_eq!(entry["path"], json!(dir.join("0.1.0")));
    for absent in [
        "pinned_version",
        "local_path",
        "previous",
        "update",
        "fault",
        "embedded_version",
        "installed_version",
        "stale",
    ] {
        assert!(entry.get(absent).is_none(), "{absent}: {entry}");
    }
    // The first-party plugins, nothing installed for them: available.
    let notes = listed(&state, "agent-notes").await;
    assert_eq!(notes["source"], "available");
    assert_eq!(notes["installed"], false);
    assert_eq!(notes["first_party"], true);
    assert_eq!(
        notes["pinned_version"],
        crate::plugins::lock_entry("agent-notes")
            .unwrap()
            .version
            .as_str()
    );
    assert!(notes.get("path").is_none() && notes.get("sha256_wasm").is_none());

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
        (Method::POST, "/api/v1/plugins/x/install"),
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

    // A plugin whose manifest names no release source (a local build):
    // nothing to check, nothing to update from.
    let src = test_dir("up-norel");
    local_build(
        &src,
        &unreleased(false, "up-norel", "0.1.0"),
        &v1_wasm(),
        None,
    );
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/plugins/install",
        Some(json!({"path": src})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = post(&state, "/api/v1/plugins/up-norel/check").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"],
        "Test fixture names no release source ([release] in its plugin.toml)"
    );
    let (status, body) = post(&state, "/api/v1/plugins/up-norel/update").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        link(&state, "up-norel", "current").as_deref(),
        Some("0.1.0")
    );
}

#[tokio::test]
async fn a_first_party_plugin_installs_the_pinned_release_checked_twice() {
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);
    let (l, toml, wasm, sums) = agent_notes_release();
    fake.publish_with_sums(&l.repo, &l.version, &toml, &wasm, &sums);
    let before = listed(&state, "agent-notes").await;
    assert_eq!(before["source"], "available");

    let (status, body) = post(&state, "/api/v1/plugins/agent-notes/install").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["version"], l.version.as_str());
    assert_eq!(body["previous"], Value::Null);
    assert_eq!(body["sha256"]["plugin.wasm"], l.sha256_wasm.as_str());
    assert_eq!(body["sha256"]["plugin.toml"], l.sha256_toml.as_str());
    let e = &body["plugin"];
    assert_eq!(e["source"], "installed");
    assert_eq!(e["first_party"], true);
    assert_eq!(e["verified"], true, "{e}");
    assert_eq!(e["sha256_wasm"], l.sha256_wasm.as_str());
    assert_eq!(e["pinned_version"], l.version.as_str());
    assert_eq!(e["repo"], l.repo.as_str());
    assert!(e.get("local_path").is_none());
    let vdir = plugin_dir(&state, "agent-notes").join(&l.version);
    assert_eq!(std::fs::read(vdir.join("plugin.wasm")).unwrap(), wasm);
    assert_eq!(
        std::fs::read_to_string(vdir.join("SHA256SUMS")).unwrap(),
        sums
    );
    no_temp_left(&state, "agent-notes");
    let (status, body) = post(&state, "/api/v1/plugins/agent-notes/install").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("already installed"));

    // An ordinary plugin now: switched on, its tools are offered.
    let (_ws, sid) = workspace_with(&state, "fp-install", "kfp1", &["agent-notes"]).await;
    assert!(tool_names(&state, &sid, "kfp1")
        .await
        .contains(&"post_note".to_string()));

    // A newer release from the same repository: offered and installed like
    // any update, and still first-party and verified past the pin.
    let newer = toml.replacen(
        &format!("version = \"{}\"", l.version),
        "version = \"9.0.0\"",
        1,
    );
    fake.publish(&l.repo, "9.0.0", &newer, &wasm);
    let (_, body) = post(&state, "/api/v1/plugins/agent-notes/check").await;
    assert_eq!(body["update"]["version"], "9.0.0", "{body}");
    let (status, body) = post(&state, "/api/v1/plugins/agent-notes/update").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let e = listed(&state, "agent-notes").await;
    assert_eq!(e["version"], "9.0.0");
    assert_eq!(e["first_party"], true);
    assert_eq!(e["verified"], true);
    assert_eq!(
        e["pinned_version"],
        l.version.as_str(),
        "the pin is still named"
    );
    assert_eq!(e["previous"], l.version.as_str());

    // Removed: available again.
    let (status, body) = request(&state, Method::DELETE, "/api/v1/plugins/agent-notes", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["plugin"]["source"], "available");
    assert!(!plugin_dir(&state, "agent-notes").exists());

    // Installing by its repository is the pinned release too, whatever the
    // latest is.
    let (status, body) = install(&state, &l.repo, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["version"], l.version.as_str());
    assert_eq!(body["plugin"]["verified"], true);
    // …unless another version is asked for by name.
    let (status, body) = install(&state, &l.repo, Some("9.0.0")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["version"], "9.0.0");
    assert_eq!(body["plugin"]["first_party"], true);
    state.sessions.kill(&sid).ok();
}

#[tokio::test]
async fn a_release_that_is_not_what_the_lock_pins_is_refused() {
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);
    let (l, toml, _wasm, sums) = agent_notes_release();

    // The release lists other bytes than the lock: nothing is downloaded.
    fake.publish(&l.repo, &l.version, &toml, &v1_wasm());
    let (status, body) = post(&state, "/api/v1/plugins/agent-notes/install").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let error = body["error"].as_str().unwrap();
    assert!(
        error.contains("does not match what chimaera pins"),
        "{error}"
    );
    assert!(error.contains(&l.sha256_wasm), "{error}");

    // It lists the lock's sha256s, but serves another component.
    fake.publish_with_sums(&l.repo, &l.version, &toml, &v1_wasm(), &sums);
    let (status, body) = post(&state, "/api/v1/plugins/agent-notes/install").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("plugin.wasm does not match the release's SHA256SUMS"));
    assert!(!plugin_dir(&state, "agent-notes").join(&l.version).exists());
    no_temp_left(&state, "agent-notes");
    assert_eq!(listed(&state, "agent-notes").await["source"], "available");

    // Only a plugin the lock names installs by id.
    let (status, body) = post(&state, "/api/v1/plugins/test-fixture/install").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
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
    assert!(plugin_dir(&state, "up-swap")
        .join("0.2.0/SHA256SUMS")
        .is_file());

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
    assert_eq!(
        body["error"],
        "Agent notes isn't installed — install it first"
    );
    state.sessions.kill(&sid).ok();
}

#[tokio::test]
async fn a_first_party_id_from_another_repository_is_not_first_party() {
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);
    // `manifest` releases it from `acme/<id>`, not the lock's repository.
    fake.publish(
        "acme/agent-notes",
        "9.0.0",
        &manifest(false, "agent-notes", "9.0.0", ""),
        &v1_wasm(),
    );
    let (status, body) = install(&state, "acme/agent-notes", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let e = listed(&state, "agent-notes").await;
    assert_eq!(e["source"], "installed");
    assert_eq!(e["version"], "9.0.0");
    assert_eq!(e["first_party"], false, "{e}");
    assert_eq!(e["verified"], true, "it is what its own release lists");
    assert!(e.get("pinned_version").is_none());
    assert_eq!(e["repo"], "acme/agent-notes");
}

#[tokio::test]
async fn the_kept_sums_are_checked_at_every_load() {
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);
    let gh = "acme/up-tamper";
    fake.publish(
        gh,
        "0.1.0",
        &manifest(false, "up-tamper", "0.1.0", ""),
        &v1_wasm(),
    );
    assert_eq!(install(&state, gh, None).await.0, StatusCode::OK);
    assert_eq!(listed(&state, "up-tamper").await["verified"], true);
    let ws = make_workspace(&state, "up-tamper").await;
    let v1 = plugin_dir(&state, "up-tamper").join("0.1.0");

    // A byte of the component changed after the install: listed with the
    // fault, never loaded, its switch refused.
    std::fs::write(v1.join("plugin.wasm"), v2_wasm()).unwrap();
    reload(&state).await;
    let e = listed(&state, "up-tamper").await;
    assert_eq!(
        e["fault"],
        "its files do not match its release's SHA256SUMS"
    );
    assert_eq!(e["verified"], false);
    let (status, body) = request(
        &state,
        Method::PUT,
        &format!("/api/v1/workspaces/{ws}/plugins/up-tamper"),
        Some(json!({"on": true})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(crate::plugins::active(&state, &ws).await.is_empty());

    // Use previous never goes back to such a copy.
    fake.publish(
        gh,
        "0.2.0",
        &manifest(true, "up-tamper", "0.2.0", ""),
        &v2_wasm(),
    );
    assert_eq!(
        post(&state, "/api/v1/plugins/up-tamper/update").await.0,
        StatusCode::OK
    );
    let (status, body) = post(&state, "/api/v1/plugins/up-tamper/rollback").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("its files do not match its release's SHA256SUMS"));
    assert_eq!(
        link(&state, "up-tamper", "current").as_deref(),
        Some("0.2.0")
    );

    // No SHA256SUMS beside it (a copy installed before they were kept):
    // loads, unverified, no fault.
    let v2 = plugin_dir(&state, "up-tamper").join("0.2.0");
    std::fs::remove_file(v2.join("SHA256SUMS")).unwrap();
    reload(&state).await;
    let e = listed(&state, "up-tamper").await;
    assert_eq!(e["verified"], false);
    assert!(e.get("fault").is_none(), "{e}");
}

#[tokio::test]
async fn a_local_build_installs_from_a_directory_and_replaces_itself() {
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);
    let path_install = |src: serde_json::Value| {
        let state = state.clone();
        async move {
            request(
                &state,
                Method::POST,
                "/api/v1/plugins/install",
                Some(json!({"path": src})),
            )
            .await
        }
    };
    let src = test_dir("up-local");
    local_build(
        &src,
        &unreleased(false, "up-local", "0.1.0"),
        &v1_wasm(),
        None,
    );
    let (status, body) = path_install(json!(src)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let canonical = src.canonicalize().unwrap();
    let e = &body["plugin"];
    assert_eq!(e["local_path"], json!(canonical));
    assert_eq!(e["verified"], false, "no SHA256SUMS beside it");
    assert_eq!(e["first_party"], false);
    assert_eq!(
        body["sha256"]["plugin.wasm"],
        crate::fs::sha256_hex(&v1_wasm())
    );
    let vdir = plugin_dir(&state, "up-local").join("0.1.0");
    assert_eq!(
        std::fs::read_to_string(vdir.join("local-path")).unwrap(),
        canonical.to_string_lossy()
    );
    assert!(!vdir.join("SHA256SUMS").exists());
    let (_ws, sid) = workspace_with(&state, "up-local", "kl1", &["up-local"]).await;
    assert!(!tool_names(&state, &sid, "kl1")
        .await
        .contains(&"version".to_string()));

    // Rebuilt at the same version and installed again: replaced in place,
    // and the same session's next tools/list is the new build.
    let rebuilt = unreleased(true, "up-local", "0.1.0");
    local_build(
        &src,
        &rebuilt,
        &v2_wasm(),
        Some(&sums_of(&v2_wasm(), &rebuilt)),
    );
    let (status, body) = path_install(json!(src)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["previous"], Value::Null, "the same version, replaced");
    assert_eq!(body["plugin"]["verified"], true, "its SHA256SUMS matches");
    assert!(vdir.join("SHA256SUMS").is_file());
    assert!(tool_names(&state, &sid, "kl1")
        .await
        .contains(&"version".to_string()));
    no_temp_left(&state, "up-local");

    // A SHA256SUMS that doesn't match refuses the install; the copy stays.
    local_build(
        &src,
        &rebuilt,
        &v1_wasm(),
        Some(&sums_of(&v2_wasm(), &rebuilt)),
    );
    let (status, body) = path_install(json!(src)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("plugin.wasm does not match the SHA256SUMS beside it"));
    assert_eq!(std::fs::read(vdir.join("plugin.wasm")).unwrap(), v2_wasm());
    no_temp_left(&state, "up-local");

    // A local build of a first-party plugin at the pinned version, with
    // other bytes than the lock's: first-party, installed, not verified.
    let (l, toml, _, _) = agent_notes_release();
    let fp = test_dir("fp-local");
    local_build(&fp, &toml, &v1_wasm(), Some(&sums_of(&v1_wasm(), &toml)));
    let (status, body) = path_install(json!(fp)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let e = &body["plugin"];
    assert_eq!(e["version"], l.version.as_str());
    assert_eq!(e["first_party"], true);
    assert_eq!(e["verified"], false, "{e}");
    assert!(e.get("fault").is_none());

    // What a path install refuses.
    for bad in [
        json!("relative/dir"),
        json!("/nonexistent/chimaera-plugin"),
        json!(vdir.join("plugin.toml")),
    ] {
        let (status, body) = path_install(bad.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }
    for body in [json!({}), json!({"github": "a/b", "path": "/tmp"})] {
        let (status, _) = request(
            &state,
            Method::POST,
            "/api/v1/plugins/install",
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
    state.sessions.kill(&sid).ok();
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
    install_first_party(&state, "agent-notes").await;
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
