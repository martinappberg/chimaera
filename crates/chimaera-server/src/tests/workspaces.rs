use super::support::*;
use crate::*;

#[tokio::test]
async fn workspaces_with_token_is_empty_list() {
    let res = app(test_state())
        .oneshot(
            Request::builder()
                .uri("/api/v1/workspaces")
                .header(header::AUTHORIZATION, "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json, serde_json::json!([]));
}

#[tokio::test]
async fn workspaces_post_get_round_trip() {
    let state = test_state();
    let root = test_dir("ws-root");
    let root_str = root.to_string_lossy().into_owned();

    // POST registers the directory.
    let (status, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root_str})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = ws["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("w-") && id.len() == 10, "bad id {id}");
    assert!(id[2..].chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')));
    assert_eq!(
        ws["name"].as_str().unwrap(),
        root.file_name().unwrap().to_str().unwrap()
    );
    assert_eq!(
        ws["root"].as_str().unwrap(),
        std::fs::canonicalize(&root).unwrap().to_str().unwrap()
    );

    // POST again with the same root is idempotent.
    let (status, again) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root_str})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["id"], ws["id"]);

    // GET lists it.
    let (status, list) = request(&state, Method::GET, "/api/v1/workspaces", None).await;
    assert_eq!(status, StatusCode::OK);
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], ws["id"]);

    // Nonexistent root is a 400 with an error body.
    let (status, err) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": "/definitely/not/a/dir"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(err["error"].is_string());
}

/// A path that exists but is a FILE (not a directory) is a 400. The `is_dir`
/// check now runs off the reactor (spawn_blocking), but still validates.
#[tokio::test]
async fn create_workspace_rejects_a_file_root() {
    let state = test_state();
    let dir = test_dir("ws-file");
    let file = dir.join("not-a-dir.txt");
    std::fs::write(&file, b"x").unwrap();
    let (status, err) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": file.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        err["error"]
            .as_str()
            .unwrap_or_default()
            .contains("is not a directory"),
        "{err:?}"
    );
}

#[tokio::test]
async fn workspaces_open_and_delete() {
    let state = test_state();
    let root = test_dir("ws-open-del");

    let (status, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = ws["id"].as_str().unwrap().to_string();
    let stamped = ws["last_opened_at"].as_u64().unwrap();
    assert!(stamped > 0, "registration stamps last_opened_at");

    // Touch returns the workspace with a fresh (>=) stamp.
    let (status, touched) = request(
        &state,
        Method::POST,
        &format!("/api/v1/workspaces/{id}/open"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(touched["id"], ws["id"]);
    assert!(touched["last_opened_at"].as_u64().unwrap() >= stamped);

    // Unknown ids 404 on both endpoints.
    let (status, _) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces/w-00000000/open",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = request(
        &state,
        Method::DELETE,
        "/api/v1/workspaces/w-00000000",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // DELETE unregisters (files untouched) and the list empties.
    let (status, _) = request(
        &state,
        Method::DELETE,
        &format!("/api/v1/workspaces/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(root.is_dir(), "delete never touches the directory");
    let (status, list) = request(&state, Method::GET, "/api/v1/workspaces", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 0);
}

#[test]
fn workspace_store_missing_file_is_empty_and_round_trips() {
    let path = test_dir("ws-missing").join("workspaces.json");
    let mut store = workspaces::WorkspaceStore::load(path.clone());
    assert!(store.list().is_empty());
    let added = store.add(test_dir("ws-missing-root")).unwrap();
    let reloaded = workspaces::WorkspaceStore::load(path);
    assert_eq!(reloaded.get(&added.id).unwrap().root, added.root);
}

/// Over the cap: an empty list (logged), and the next change replaces the
/// runaway file.
#[test]
fn workspace_store_oversized_file_loads_empty_and_is_replaced() {
    let path = test_dir("ws-big").join("workspaces.json");
    let body = r#"[{"id":"w-00000000","root":"/old","name":"old"}]"#;
    std::fs::write(
        &path,
        super::recents::oversized(body, workspaces::MAX_WORKSPACES_BYTES),
    )
    .unwrap();
    let mut store = workspaces::WorkspaceStore::load(path.clone());
    assert!(store.list().is_empty());
    store.add(test_dir("ws-big-root")).unwrap();
    assert_eq!(workspaces::WorkspaceStore::load(path).list().len(), 1);
}

/// Unreadable: every save refuses ([`workspaces::Held`]) and takes its
/// change back, so a boot that couldn't read the list never replaces it;
/// once the file reads, the next save merges it with this daemon's list.
#[test]
fn workspace_store_unreadable_file_is_held_until_it_reads_then_merged() {
    let dir = test_dir("ws-dir").join("workspaces.json");
    std::fs::create_dir_all(&dir).unwrap();
    let mut store = workspaces::WorkspaceStore::load(dir.clone());
    let err = store.add(test_dir("ws-dir-root")).unwrap_err();
    assert!(err.is::<workspaces::Held>(), "{err}");
    assert!(store.list().is_empty(), "no phantom");
    assert!(dir.is_dir());

    let path = test_dir("ws-perm").join("workspaces.json");
    let body = br#"[{"id":"w-0000aaaa","root":"/kept","name":"kept"}]"#;
    if !super::recents::unreadable_file(&path, body) {
        return; // root reads anything
    }
    let mut store = workspaces::WorkspaceStore::load(path.clone());
    assert!(store.add(test_dir("ws-perm-root")).is_err());
    assert!(store.list().is_empty());
    assert_eq!(super::recents::readable_again(&path), body.to_vec());

    let added = store.add(test_dir("ws-perm-root")).unwrap();
    let ids: Vec<String> = store.list().into_iter().map(|w| w.id).collect();
    assert_eq!(ids, ["w-0000aaaa".to_string(), added.id.clone()]);
    let reloaded = workspaces::WorkspaceStore::load(path);
    assert_eq!(reloaded.list().len(), 2);
    assert!(reloaded.get(&added.id).is_some());
}

/// Over the wire: a held list answers 503 with a plain reason, and the
/// workspace it refused never shows up in the list.
#[tokio::test]
async fn workspaces_add_refused_while_held_answers_503_without_a_phantom() {
    let data = test_dir("ws-held-route");
    std::fs::create_dir_all(data.join("workspaces.json")).unwrap();
    let state = test_state_with_data_dir(0, data);
    let root = test_dir("ws-held-route-root");
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("can't be read"),
        "{body}"
    );
    let (status, list) = request(&state, Method::GET, "/api/v1/workspaces", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list, serde_json::json!([]));
}

/// A refused save of the worktree choice puts back the value it had; a
/// choice that is already set needs no save and stays.
#[test]
fn workspace_new_in_worktree_refused_while_held_rolls_back() {
    let path = test_dir("ws-held-patch").join("workspaces.json");
    std::fs::write(
        &path,
        r#"[{"id":"w-0000bbbb","root":"/r","name":"r"},
            {"id":"w-0000cccc","root":"/s","name":"s","new_in_worktree":true}]"#,
    )
    .unwrap();
    let mut store = workspaces::WorkspaceStore::load(path.clone());
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(store.set_new_in_worktree("w-0000bbbb", true).is_err());
    assert!(!store.get("w-0000bbbb").unwrap().new_in_worktree);
    assert!(store.set_new_in_worktree("w-0000cccc", false).is_err());
    assert!(store.get("w-0000cccc").unwrap().new_in_worktree, "was true");
    store.set_new_in_worktree("w-0000cccc", true).unwrap();
    assert!(store.get("w-0000cccc").unwrap().new_in_worktree);
}
