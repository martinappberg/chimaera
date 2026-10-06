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

#[tokio::test]
async fn internal_setup_is_persisted_and_hidden_without_hiding_similarly_named_user_projects() {
    let data = test_dir("internal-workspaces-data");
    let state = test_state_with_data_dir(0, data.clone());
    let projects = test_dir("internal-workspaces-roots");
    let internal = crate::lock(&state.workspaces)
        .add_internal(projects.join("managed-login"))
        .unwrap();
    let normal = crate::lock(&state.workspaces)
        .add(projects.join(".chimaera-setup"))
        .unwrap();
    let (status, rows) = request(&state, Method::GET, "/api/v1/workspaces", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["id"], normal.id);
    let restarted = test_state_with_data_dir(0, data);
    assert!(
        crate::lock(&restarted.workspaces)
            .get(&internal.id)
            .unwrap()
            .cloud_internal
    );
    assert!(
        !crate::lock(&restarted.workspaces)
            .get(&normal.id)
            .unwrap()
            .cloud_internal
    );
    let (_, rows) = request(&restarted, Method::GET, "/api/v1/workspaces", None).await;
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["id"], normal.id);
}

/// A project Pro enrolled records its workspace id in its folder, and the
/// id follows the folder: a new daemon (a reinstall, a state reset, a second
/// computer) reopens the same project; a local duplicate is its own; a moved
/// folder keeps its workspace. A project Pro never enrolled gets nothing.
mod folder_identity {
    use super::*;
    use crate::workspaces::identity;
    use std::path::Path;

    fn folder(label: &str) -> PathBuf {
        std::fs::canonicalize(test_dir(label)).unwrap()
    }

    async fn register(state: &Arc<AppState>, root: &Path) -> serde_json::Value {
        let (status, ws) = request(
            state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": root.to_string_lossy()})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{ws}");
        ws
    }

    /// The marker is written off the request: wait for it briefly.
    async fn marker_naming(root: &Path, id: &str) {
        for _ in 0..100 {
            if identity::read(root).is_some_and(|m| m.id == id) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("{} never named {id}", root.display());
    }

    /// Registered, then enrolled by Pro (an account bound it).
    async fn enrolled(state: &Arc<AppState>, root: &Path) -> serde_json::Value {
        let ws = register(state, root).await;
        let id = ws["id"].as_str().unwrap();
        crate::pro::enroll_fixture(state, id);
        marker_naming(root, id).await;
        ws
    }

    /// Every file under `root` with its bytes, sorted.
    fn contents(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path.clone());
                    out.push((path, Vec::new()));
                } else {
                    out.push((path.clone(), std::fs::read(&path).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    /// The free contract: registering and opening a project on a daemon
    /// without Pro, or with the extension but no account, leaves its folder
    /// byte-identical, and nothing Pro is noted.
    #[tokio::test]
    async fn a_project_pro_never_enrolled_is_left_byte_identical() {
        for state in [test_state(), test_state_with_extension()] {
            for repo in [false, true] {
                let root = folder("ident-free");
                std::fs::write(root.join("notes.md"), b"mine").unwrap();
                if repo {
                    std::fs::create_dir(root.join(".git")).unwrap();
                    std::fs::write(root.join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();
                }
                let before = contents(&root);
                let ws = register(&state, &root).await;
                let id = ws["id"].as_str().unwrap();
                for _ in 0..2 {
                    let (status, _) = request(
                        &state,
                        Method::POST,
                        &format!("/api/v1/workspaces/{id}/open"),
                        None,
                    )
                    .await;
                    assert_eq!(status, StatusCode::OK);
                }
                register(&state, &root).await;
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                assert_eq!(contents(&root), before, "the folder gained nothing");
                assert!(identity::read(&root).is_none());
                if state.daemon_extension.is_none() {
                    assert!(!crate::pro::opened_here(&state, id));
                }
            }
        }
    }

    /// Without the extension a marker another installation left in the
    /// folder is not read: the folder registers under a fresh id, as it
    /// would on a daemon without Pro code.
    #[tokio::test]
    async fn a_daemon_without_pro_ignores_a_folder_marker() {
        let root = folder("ident-free-read");
        std::fs::write(
            root.join(".chimaera-workspace"),
            r#"{"id":"w-fromanotherinstall","written_at":1}"#,
        )
        .unwrap();
        let ws = register(&test_state(), &root).await;
        assert_ne!(ws["id"], "w-fromanotherinstall");
        let ws = register(&test_state_with_extension(), &root).await;
        assert_eq!(ws["id"], "w-fromanotherinstall");
    }

    #[tokio::test]
    async fn enrolling_writes_the_marker_in_git_or_a_dotfile() {
        let state = test_state_with_extension();
        let plain = folder("ident-plain");
        let ws = enrolled(&state, &plain).await;
        assert_eq!(
            identity::read(&plain).unwrap().id,
            ws["id"].as_str().unwrap()
        );
        assert!(plain.join(".chimaera-workspace").is_file());

        let repo = folder("ident-repo");
        std::fs::create_dir(repo.join(".git")).unwrap();
        let ws = enrolled(&state, &repo).await;
        assert_eq!(
            identity::read(&repo).unwrap().id,
            ws["id"].as_str().unwrap()
        );
        assert!(repo.join(".git/chimaera-workspace").is_file());
        assert!(!repo.join(".chimaera-workspace").exists());
    }

    #[tokio::test]
    async fn a_new_daemon_reopens_the_same_folder_as_the_same_project() {
        let root = folder("ident-reinstall");
        let first = enrolled(&test_state_with_extension(), &root).await;
        // A different daemon with an empty registry: a reinstall, a state
        // reset, or the same folder on another computer.
        let second_state = test_state_with_extension();
        let second = register(&second_state, &root).await;
        assert_eq!(second["id"], first["id"]);
        assert_eq!(second["root"], first["root"]);
        // The user opened an existing project here: it may come home to this
        // computer (a fresh registration is not that).
        let first_state = test_state_with_extension();
        let fresh = folder("ident-fresh");
        let fresh_ws = register(&first_state, &fresh).await;
        assert!(!crate::pro::opened_here(
            &first_state,
            fresh_ws["id"].as_str().unwrap()
        ));
        assert!(crate::pro::opened_here(
            &second_state,
            second["id"].as_str().unwrap()
        ));
        // Reopening is idempotent and lists one project.
        assert_eq!(register(&second_state, &root).await["id"], first["id"]);
        let (_, list) = request(&second_state, Method::GET, "/api/v1/workspaces", None).await;
        assert_eq!(list.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_local_duplicate_is_its_own_project_and_a_move_keeps_the_project() {
        let state = test_state_with_extension();
        let parent = folder("ident-dup");
        let original = parent.join("thesis");
        std::fs::create_dir(&original).unwrap();
        let first = enrolled(&state, &original).await;
        let first_id = first["id"].as_str().unwrap().to_owned();

        // A copy of the folder (the marker comes along), the original still there.
        let duplicate = parent.join("thesis-copy");
        std::fs::create_dir(&duplicate).unwrap();
        std::fs::copy(
            original.join(".chimaera-workspace"),
            duplicate.join(".chimaera-workspace"),
        )
        .unwrap();
        let copy = register(&state, &duplicate).await;
        let copy_id = copy["id"].as_str().unwrap().to_owned();
        assert_ne!(copy_id, first_id);
        assert!(!crate::pro::opened_here(&state, &copy_id));
        // Not enrolled: its folder is left as the user copied it until Pro
        // takes it on, which tells it its own id.
        assert_eq!(identity::read(&duplicate).unwrap().id, first_id);
        crate::pro::enroll_fixture(&state, &copy_id);
        marker_naming(&duplicate, &copy_id).await;
        assert_eq!(
            identity::read(&original).unwrap().id,
            first_id,
            "the original is untouched"
        );

        // Move the original: its old path is gone.
        let moved = parent.join("thesis-renamed");
        std::fs::rename(&original, &moved).unwrap();
        let after = register(&state, &moved).await;
        assert_eq!(after["id"], first["id"], "Pro state survives a move");
        assert!(crate::pro::opened_here(&state, &first_id));
        assert_eq!(after["root"], moved.to_string_lossy().as_ref());
        assert_eq!(after["name"], "thesis-renamed");
        let (_, list) = request(&state, Method::GET, "/api/v1/workspaces", None).await;
        let list = list.as_array().unwrap();
        assert_eq!(list.len(), 2);
        assert!(list
            .iter()
            .all(|w| w["root"] != original.to_string_lossy().as_ref()));
        assert_eq!(identity::read(&moved).unwrap().id, first_id);
    }

    #[tokio::test]
    async fn an_unreadable_marker_is_no_marker_and_a_wrong_one_is_repaired() {
        let state = test_state_with_extension();
        let root = folder("ident-broken");
        std::fs::write(root.join(".chimaera-workspace"), "x".repeat(10_000)).unwrap();
        let ws = enrolled(&state, &root).await;
        let id = ws["id"].as_str().unwrap();

        // An enrolled root whose marker names another id is repaired.
        std::fs::write(
            root.join(".chimaera-workspace"),
            r#"{"id":"w-someoneelse","written_at":1}"#,
        )
        .unwrap();
        let again = register(&state, &root).await;
        assert_eq!(again["id"], ws["id"]);
        marker_naming(&root, id).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_folder_that_cannot_be_written_still_registers() {
        use std::os::unix::fs::PermissionsExt;
        let state = test_state();
        let root = folder("ident-readonly");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();
        let ws = register(&state, &root).await;
        let refused = std::fs::write(root.join("probe"), b"x").is_err();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(ws["id"].as_str().unwrap().starts_with("w-"));
        if refused {
            assert!(identity::read(&root).is_none(), "no marker, no failure");
        }
    }

    #[tokio::test]
    async fn opening_an_enrolled_workspace_backfills_a_missing_marker_only() {
        let state = test_state_with_extension();
        let root = folder("ident-backfill");
        let ws = enrolled(&state, &root).await;
        let id = ws["id"].as_str().unwrap();
        std::fs::remove_file(root.join(".chimaera-workspace")).unwrap();
        let (status, _) = request(
            &state,
            Method::POST,
            &format!("/api/v1/workspaces/{id}/open"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        // Best effort and off the request: wait for it briefly.
        for _ in 0..100 {
            if identity::read(&root).is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(identity::read(&root).unwrap().id, id);

        // An existing marker is never rewritten by an open.
        std::fs::write(
            root.join(".chimaera-workspace"),
            r#"{"id":"w-keepthisid","written_at":1}"#,
        )
        .unwrap();
        request(
            &state,
            Method::POST,
            &format!("/api/v1/workspaces/{id}/open"),
            None,
        )
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(identity::read(&root).unwrap().id, "w-keepthisid");
    }
}
