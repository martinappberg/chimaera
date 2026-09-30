//! A plugin whose job moved into chimaera (`plugins::retired`; Agent notes
//! is Agent communication now): the copy an older daemon installed is
//! listed with why and Remove, never runs, its switch refuses and a kept
//! one is dropped at boot, and no way in installs it again.

use serde_json::{json, Value};

use super::plugin_updates::*;
use super::support::*;
use crate::{lock, AppState};

const WHY: &str =
    "Built into Chimaera now: Agent communication (Settings → Agents). Remove this copy.";
const REPO: &str = "martinappberg/chimaera-plugin-agent-notes";

/// What a daemon from before left under `root` (`<data dir>/plugins`):
/// Agent notes installed from its releases — 0.1.4 current, 0.1.3 kept as
/// previous — each with the release's SHA256SUMS and the source marker. The
/// component is the fixture's: nothing ever loads it.
fn plant_agent_notes(root: &std::path::Path) {
    let dir = root.join("agent-notes");
    for version in ["0.1.3", "0.1.4"] {
        let toml = manifest(false, "agent-notes", version, "")
            .replace("acme/agent-notes", REPO)
            .replace("name = \"Test fixture\"", "name = \"Agent notes\"");
        let v = dir.join(version);
        std::fs::create_dir_all(&v).unwrap();
        local_build(&v, &toml, &v1_wasm(), Some(&sums_of(&v1_wasm(), &toml)));
        std::fs::write(v.join("source-github"), REPO).unwrap();
    }
    std::os::unix::fs::symlink("0.1.4", dir.join("current")).unwrap();
    std::os::unix::fs::symlink("0.1.3", dir.join("previous")).unwrap();
}

async fn card(state: &Arc<AppState>, ws: &str, id: &str) -> Value {
    let (status, out) = request(
        state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/plugins"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    out["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == id)
        .cloned()
        .unwrap_or(Value::Null)
}

/// The upgrade: a daemon starting over an installed Agent notes that was
/// switched on lists it with the reason and nothing else to do but Remove —
/// its switch gone from `workspaces.json`, not active, no tools, no update,
/// no trust question — and Remove takes it away for good.
#[tokio::test]
async fn a_kept_copy_is_listed_off_with_why_and_removable() {
    let data = test_dir("retired-boot");
    plant_agent_notes(&data.join("plugins"));
    let root = test_dir("retired-boot-ws");
    let list = json!([{"id": "w-retired", "root": root, "name": "retired",
        "plugins_on": ["agent-notes", "mycelium"]}]);
    std::fs::write(data.join("workspaces.json"), list.to_string()).unwrap();

    let state = test_state_with_data_dir(0, data.clone());
    assert_eq!(state.plugin_catalog.root, data.join("plugins"));
    let ws = "w-retired";
    let on = || lock(&state.workspaces).get(ws).unwrap().plugins_on;
    assert_eq!(on(), ["mycelium"], "the retired switch went at load");
    let saved = std::fs::read_to_string(data.join("workspaces.json")).unwrap();
    assert!(!saved.contains("agent-notes"), "{saved}");

    let c = card(&state, ws, "agent-notes").await;
    assert_eq!(c["installed"], true, "{c}");
    assert_eq!(c["source"], "installed");
    assert_eq!(c["version"], "0.1.4");
    assert_eq!(c["previous"], "0.1.3");
    assert_eq!(c["fault"], WHY);
    assert_eq!(c["first_party"], false, "no longer in the lock");
    assert!(
        c["hold"].is_null(),
        "the fault is the one thing it says: {c}"
    );
    assert_eq!(c["on"], false);
    assert_eq!(c["active"], false);
    assert!(c.get("pinned_version").is_none() && c.get("update").is_none());
    assert_eq!(listed(&state, "agent-notes").await["fault"], WHY);

    // A switch kept anyway (set after load) still runs nothing.
    lock(&state.workspaces)
        .set_plugin_on(ws, "agent-notes", true)
        .unwrap();
    let sid = inject_agent(&state, "kret");
    lock(&state.session_workspaces).insert(sid.clone(), ws.to_string());
    assert!(crate::plugins::active(&state, ws).await.is_empty());
    let names = tool_names(&state, &sid, "kret").await;
    assert!(!names.contains(&"echo".to_string()), "{names:?}");
    assert_eq!(card(&state, ws, "agent-notes").await["on"], false);

    // Its switch refuses on, in the same words; off is fine.
    for on in [true, false] {
        let (status, body) = request(
            &state,
            Method::PUT,
            &format!("/api/v1/workspaces/{ws}/plugins/agent-notes"),
            Some(json!({"on": on})),
        )
        .await;
        if on {
            assert_eq!(status, StatusCode::CONFLICT, "{body}");
            assert_eq!(body["error"], WHY);
        } else {
            assert_eq!(status, StatusCode::OK, "{body}");
        }
    }

    // Remove: gone, nothing left to list or switch.
    lock(&state.workspaces)
        .set_plugin_on(ws, "agent-notes", true)
        .unwrap();
    let (status, body) = request(&state, Method::DELETE, "/api/v1/plugins/agent-notes", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["plugin"], Value::Null, "{body}");
    assert!(!plugin_dir(&state, "agent-notes").exists());
    assert_eq!(listed(&state, "agent-notes").await, Value::Null);
    assert_eq!(card(&state, ws, "agent-notes").await, Value::Null);
    assert_eq!(on(), ["mycelium"], "its switch went with it");
    state.sessions.kill(&sid).ok();
}

/// No way in takes it: by id, by its repository (before anything is
/// fetched), a release or a local build whose manifest names it — and a
/// kept copy's Update, Use previous and Check. Each answers 409 in the
/// card's words.
#[tokio::test]
async fn every_way_in_refuses_a_retired_plugin() {
    let fake = FakeReleases::start().await;
    let state = state_for(&fake);
    let refused = |(status, body): (StatusCode, Value)| {
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], WHY, "{body}");
    };

    // By id (`chimaera plugin add agent-notes`), and by its repository.
    refused(post(&state, "/api/v1/plugins/agent-notes/install").await);
    refused(install(&state, REPO, None).await);
    refused(
        install(
            &state,
            &format!("https://github.com/{}/", REPO.to_uppercase()),
            None,
        )
        .await,
    );
    refused(install(&state, REPO, Some("0.1.4")).await);
    assert_eq!(fake.hits(), 0, "nothing was fetched");

    // Another repository's release of it: refused on its manifest, before
    // the component is fetched (the release, its SHA256SUMS, its
    // plugin.toml — never plugin.wasm). Its preview already says why.
    let fork = manifest(false, "agent-notes", "0.2.0", "");
    fake.publish("acme/agent-notes", "0.2.0", &fork, &v1_wasm());
    refused(install(&state, "acme/agent-notes", None).await);
    assert_eq!(fake.hits(), 3);
    let (status, p) = request(
        &state,
        Method::POST,
        "/api/v1/plugins/preview",
        Some(json!({"github": "acme/agent-notes"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{p}");
    assert_eq!(p["fault"], WHY);

    // A local build of it.
    let src = test_dir("retired-local");
    local_build(
        &src,
        &unreleased(false, "agent-notes", "0.1.5"),
        &v1_wasm(),
        None,
    );
    refused(
        request_trusting(
            &state,
            Method::POST,
            "/api/v1/plugins/install",
            Some(json!({"path": src})),
        )
        .await,
    );
    assert!(!plugin_dir(&state, "agent-notes").exists());

    // A kept copy can't move either way, nor ask for a newer release.
    plant_agent_notes(&state.plugin_catalog.root);
    reload(&state).await;
    let before = fake.hits();
    for route in ["update", "rollback", "check", "install"] {
        refused(post(&state, &format!("/api/v1/plugins/agent-notes/{route}")).await);
    }
    assert_eq!(fake.hits(), before, "nothing asked of GitHub");
    assert_eq!(
        link(&state, "agent-notes", "current").as_deref(),
        Some("0.1.4")
    );
    assert_eq!(
        link(&state, "agent-notes", "previous").as_deref(),
        Some("0.1.3")
    );
    assert_eq!(listed(&state, "agent-notes").await["fault"], WHY);
}
