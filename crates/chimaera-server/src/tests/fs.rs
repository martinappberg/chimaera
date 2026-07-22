use super::support::*;
use crate::*;

#[tokio::test]
async fn fs_home_returns_real_home() {
    let (status, json) = request(&test_state(), Method::GET, "/api/v1/fs/home", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json["path"].as_str().unwrap(),
        std::env::var("HOME").unwrap()
    );
}

#[tokio::test]
async fn fs_mkdir_creates_nested_idempotently_and_rejects_empty() {
    let state = test_state();
    let root = test_dir("fs-mkdir");
    let target = root.join("nested/newproj");
    let target_str = target.to_string_lossy().into_owned();
    assert!(!target.exists());

    // Creates the path and any missing parents, returns the canonical path.
    let (status, json) = request(
        &state,
        Method::POST,
        "/api/v1/fs/mkdir",
        Some(serde_json::json!({ "path": target_str })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(target.is_dir());
    assert_eq!(
        json["path"].as_str().unwrap(),
        std::fs::canonicalize(&target).unwrap().to_string_lossy()
    );

    // Idempotent: an existing directory is a success, not a conflict.
    let (status, _) = request(
        &state,
        Method::POST,
        "/api/v1/fs/mkdir",
        Some(serde_json::json!({ "path": target_str })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // An empty path is a 400, not a silent create of the cwd.
    let (status, json) = request(
        &state,
        Method::POST,
        "/api/v1/fs/mkdir",
        Some(serde_json::json!({ "path": "" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json["error"].as_str().unwrap().contains("empty path"));

    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn fs_create_makes_parents_and_conflicts_on_existing() {
    let state = test_state();
    let root = test_dir("fs-create");

    // A nested file name creates the intermediate directories (the inline
    // "new file" input accepts a/b/c.txt), echoing the canonical path.
    let file = root.join("a/b/c.txt");
    let (status, json) = request(
        &state,
        Method::POST,
        "/api/v1/fs/create",
        Some(serde_json::json!({ "path": file.to_string_lossy(), "kind": "file" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert!(file.is_file());
    assert_eq!(
        json["path"].as_str().unwrap(),
        std::fs::canonicalize(&file).unwrap().to_string_lossy()
    );

    // An explicit New File on an existing path is a conflict, never a
    // silent truncate-or-succeed.
    let (status, json) = request(
        &state,
        Method::POST,
        "/api/v1/fs/create",
        Some(serde_json::json!({ "path": file.to_string_lossy(), "kind": "file" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(json["error"].as_str().unwrap().contains("already exists"));

    // Directories: nested create works, an existing one conflicts (unlike
    // the idempotent /fs/mkdir — this is an explicit New Folder).
    let dir = root.join("x/y");
    let (status, _) = request(
        &state,
        Method::POST,
        "/api/v1/fs/create",
        Some(serde_json::json!({ "path": dir.to_string_lossy(), "kind": "dir" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(dir.is_dir());
    let (status, _) = request(
        &state,
        Method::POST,
        "/api/v1/fs/create",
        Some(serde_json::json!({ "path": dir.to_string_lossy(), "kind": "dir" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Empty path is a 400.
    let (status, _) = request(
        &state,
        Method::POST,
        "/api/v1/fs/create",
        Some(serde_json::json!({ "path": "", "kind": "file" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn fs_rename_moves_files_dirs_symlinks_and_guards() {
    let state = test_state();
    let root = test_dir("fs-rename");
    let rename = |from: &std::path::Path, to: &std::path::Path| {
        let body = serde_json::json!({
            "from": from.to_string_lossy(),
            "to": to.to_string_lossy(),
        });
        let state = state.clone();
        async move { request(&state, Method::POST, "/api/v1/fs/rename", Some(body)).await }
    };

    // File rename: old gone, new exists, canonical path echoed.
    let old = root.join("old.txt");
    let new = root.join("new.txt");
    std::fs::write(&old, "data").unwrap();
    let (status, json) = rename(&old, &new).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert!(!old.exists());
    assert_eq!(std::fs::read_to_string(&new).unwrap(), "data");
    // The echoed path is parent-canonical (on macOS /var resolves to
    // /private/var), so canonicalize the expectation too.
    assert_eq!(
        json["path"].as_str().unwrap(),
        std::fs::canonicalize(&new).unwrap().to_string_lossy()
    );

    // Dir rename carries children along.
    let dir = root.join("proj");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub/f.txt"), "x").unwrap();
    let moved = root.join("renamed-proj");
    let (status, _) = rename(&dir, &moved).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        std::fs::read_to_string(moved.join("sub/f.txt")).unwrap(),
        "x"
    );

    // Renaming onto an existing path is a conflict.
    let other = root.join("other.txt");
    std::fs::write(&other, "keep").unwrap();
    let (status, json) = rename(&new, &other).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(json["error"].as_str().unwrap().contains("already exists"));
    assert_eq!(std::fs::read_to_string(&other).unwrap(), "keep");

    // A symlink renames as itself; its target stays put.
    let target = root.join("target.txt");
    std::fs::write(&target, "t").unwrap();
    let link = root.join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let moved_link = root.join("moved-link");
    let (status, _) = rename(&link, &moved_link).await;
    assert_eq!(status, StatusCode::OK);
    assert!(target.is_file());
    assert!(moved_link.symlink_metadata().unwrap().is_symlink());
    assert!(!link.exists());

    // Missing source and missing destination parent are 400s.
    let (status, _) = rename(&root.join("nope.txt"), &root.join("x.txt")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = rename(&new, &root.join("no-such-dir/x.txt")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn fs_delete_removes_files_dirs_symlinks_and_refuses_home() {
    let state = test_state();
    let root = test_dir("fs-delete");
    let del = |path: &std::path::Path| {
        let body = serde_json::json!({ "path": path.to_string_lossy() });
        let state = state.clone();
        async move { request(&state, Method::POST, "/api/v1/fs/delete", Some(body)).await }
    };

    // File: 204 and gone.
    let file = root.join("f.txt");
    std::fs::write(&file, "x").unwrap();
    let (status, _) = del(&file).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!file.exists());

    // Dir: recursive.
    let dir = root.join("proj");
    std::fs::create_dir_all(dir.join("deep/deeper")).unwrap();
    std::fs::write(dir.join("deep/deeper/f.txt"), "x").unwrap();
    let (status, _) = del(&dir).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!dir.exists());

    // Symlink: the link is unlinked, the target survives.
    let target = root.join("target.txt");
    std::fs::write(&target, "t").unwrap();
    let link = root.join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let (status, _) = del(&link).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(target.is_file());
    assert!(std::fs::symlink_metadata(&link).is_err());

    // Guards: the home directory is refused; a missing path is a plain 400.
    let home = std::path::PathBuf::from(std::env::var("HOME").unwrap());
    let (status, json) = del(&home).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json["error"].as_str().unwrap().contains("refusing"));
    let (status, _) = del(&root.join("nope")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn fs_copy_files_dirs_symlinks_unique_and_guards() {
    let state = test_state();
    let root = test_dir("fs-copy");
    let copy = |from: &std::path::Path, to: &std::path::Path, on_conflict: Option<&str>| {
        let mut body = serde_json::json!({
            "from": from.to_string_lossy(),
            "to": to.to_string_lossy(),
        });
        if let Some(oc) = on_conflict {
            body["on_conflict"] = serde_json::json!(oc);
        }
        let state = state.clone();
        async move { request(&state, Method::POST, "/api/v1/fs/copy", Some(body)).await }
    };

    // File copy: source survives, target is a byte-for-byte duplicate.
    let src = root.join("a.txt");
    std::fs::write(&src, "data").unwrap();
    let dst = root.join("b.txt");
    let (status, json) = copy(&src, &dst, None).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(std::fs::read_to_string(&src).unwrap(), "data");
    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "data");

    // Directory copy recurses; the source tree is untouched.
    let dir = root.join("proj");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub/f.txt"), "x").unwrap();
    let dir_dst = root.join("proj-copy");
    let (status, _) = copy(&dir, &dir_dst, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        std::fs::read_to_string(dir_dst.join("sub/f.txt")).unwrap(),
        "x"
    );
    assert!(dir.join("sub/f.txt").is_file());

    // A symlink is copied AS a link, never followed.
    let target = root.join("t.txt");
    std::fs::write(&target, "t").unwrap();
    let link = root.join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let link_copy = root.join("link-copy");
    let (status, _) = copy(&link, &link_copy, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(link_copy.symlink_metadata().unwrap().is_symlink());

    // Copy onto an existing path: 409 by default, a free " copy" sibling with
    // on_conflict=unique.
    let (status, _) = copy(&src, &dst, None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, json) = copy(&src, &dst, Some("unique")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json["path"].as_str().unwrap(),
        std::fs::canonicalize(root.join("b copy.txt"))
            .unwrap()
            .to_string_lossy()
    );

    // Refuse copying a directory into its own subtree.
    let (status, json) = copy(&dir, &dir.join("nested"), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json["error"].as_str().unwrap().contains("into itself"));

    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn fs_move_relocates_and_guards() {
    let state = test_state();
    let root = test_dir("fs-move");
    let mv = |from: &std::path::Path, to: &std::path::Path| {
        let body = serde_json::json!({
            "from": from.to_string_lossy(),
            "to": to.to_string_lossy(),
        });
        let state = state.clone();
        async move { request(&state, Method::POST, "/api/v1/fs/move", Some(body)).await }
    };

    // Same-filesystem move: source gone, target has the bytes.
    let src = root.join("a.txt");
    std::fs::write(&src, "data").unwrap();
    let dst = root.join("sub").join("a.txt");
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    let (status, json) = mv(&src, &dst).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert!(!src.exists());
    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "data");

    // Moving onto an existing path is a conflict; both files survive.
    let keep = root.join("keep.txt");
    std::fs::write(&keep, "keep").unwrap();
    let (status, _) = mv(&dst, &keep).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(std::fs::read_to_string(&keep).unwrap(), "keep");

    // Home and dir-into-itself are refused; a missing source is a 400.
    let home = std::path::PathBuf::from(std::env::var("HOME").unwrap());
    let (status, _) = mv(&home, &root.join("x")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = mv(&root.join("nope"), &root.join("y")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn fs_dirs_lists_only_directories_sorted() {
    let state = test_state();
    let root = test_dir("fs-list");
    std::fs::create_dir(root.join("Zebra")).unwrap();
    std::fs::create_dir(root.join("apple")).unwrap();
    std::fs::create_dir(root.join("Mango")).unwrap();
    std::fs::create_dir(root.join(".config")).unwrap();
    std::fs::write(root.join("notes.txt"), "not a dir").unwrap();
    std::os::unix::fs::symlink(root.join("apple"), root.join("orchard")).unwrap();
    std::os::unix::fs::symlink(root.join("notes.txt"), root.join("shortcut")).unwrap();
    std::os::unix::fs::symlink(root.join("nowhere"), root.join("dangling")).unwrap();

    let canonical = std::fs::canonicalize(&root).unwrap();
    let names = |json: &serde_json::Value| -> Vec<String> {
        json["dirs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["name"].as_str().unwrap().to_string())
            .collect()
    };

    // Default: dot-directories hidden; files and non-dir symlinks never
    // listed; case-insensitive order (byte order would put Mango first).
    let uri = format!("/api/v1/fs/dirs?path={}", root.to_string_lossy());
    let (status, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["path"].as_str().unwrap(), canonical.to_str().unwrap());
    assert_eq!(
        json["parent"].as_str().unwrap(),
        canonical.parent().unwrap().to_str().unwrap()
    );
    assert_eq!(names(&json), ["apple", "Mango", "orchard", "Zebra"]);
    assert_eq!(
        json["dirs"][0]["path"].as_str().unwrap(),
        canonical.join("apple").to_str().unwrap()
    );

    // hidden=true adds the dot-directory; still no files.
    let uri = format!(
        "/api/v1/fs/dirs?path={}&hidden=true",
        root.to_string_lossy()
    );
    let (status, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        names(&json),
        [".config", "apple", "Mango", "orchard", "Zebra"]
    );
}

#[tokio::test]
async fn fs_dirs_expands_tilde() {
    let (status, json) = request(&test_state(), Method::GET, "/api/v1/fs/dirs?path=~", None).await;
    assert_eq!(status, StatusCode::OK);
    let home = std::fs::canonicalize(std::env::var("HOME").unwrap()).unwrap();
    assert_eq!(json["path"].as_str().unwrap(), home.to_str().unwrap());
    assert!(json["parent"].is_string());
    assert!(json["dirs"].is_array());
}

#[tokio::test]
async fn fs_dirs_rejects_files_and_missing_paths() {
    let state = test_state();
    let root = test_dir("fs-bad");
    let file = root.join("plain.txt");
    std::fs::write(&file, "x").unwrap();

    let uri = format!("/api/v1/fs/dirs?path={}", file.to_string_lossy());
    let (status, err) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(err["error"].is_string());

    let (status, err) = request(
        &state,
        Method::GET,
        "/api/v1/fs/dirs?path=/definitely/not/a/dir",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(err["error"].is_string());
}

#[tokio::test]
async fn fs_endpoints_without_token_are_401() {
    for uri in [
        "/api/v1/fs/home",
        "/api/v1/fs/dirs?path=/",
        "/api/v1/fs/list?path=/",
        "/api/v1/fs/file?path=/etc/hosts",
        "/api/v1/fs/markdown?path=/x.md",
        "/api/v1/fs/table?path=/x.csv",
        "/api/v1/fs/quickopen?workspace_id=w-x&q=main",
    ] {
        let res = app(test_state())
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{uri}");
    }
    // The ticket mint and the link-provider validation are POSTs and
    // equally protected.
    for (uri, body) in [
        ("/api/v1/fs/ticket", r#"{"path":"/etc/hosts"}"#),
        (
            "/api/v1/fs/validate",
            r#"{"candidates":["hosts"],"base":"/etc"}"#,
        ),
        ("/api/v1/fs/create", r#"{"path":"/tmp/x","kind":"file"}"#),
        ("/api/v1/fs/rename", r#"{"from":"/tmp/x","to":"/tmp/y"}"#),
        ("/api/v1/fs/delete", r#"{"path":"/tmp/x"}"#),
    ] {
        let res = app(test_state())
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{uri}");
    }
    // So is the file write.
    let res = app(test_state())
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri("/api/v1/fs/file?path=/tmp/x.txt")
                .body(Body::from("data"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn fs_validate_resolves_relative_absolute_missing_and_dirs() {
    let state = test_state();
    let base = test_dir("validate-base");
    std::fs::create_dir(base.join("sub")).unwrap();
    std::fs::write(base.join("sub").join("real.txt"), "x").unwrap();
    std::fs::write(base.join("top.rs"), "x").unwrap();
    // The base may itself be uncanonical (macOS /var -> /private/var);
    // resolved paths in the answer are always canonical.
    let canon = std::fs::canonicalize(&base).unwrap();
    let abs = canon.join("top.rs").to_string_lossy().into_owned();

    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/fs/validate",
        Some(serde_json::json!({
            "candidates": [
                "sub/real.txt",     // relative file
                "sub",              // relative dir
                abs,                // absolute file
                "missing.txt",      // nonexistent -> absent
                "./sub/../top.rs",  // dot segments resolve away
                "",                 // empty -> absent
            ],
            "base": base.to_string_lossy(),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let valid = body["valid"].as_object().unwrap();
    assert_eq!(valid.len(), 4, "{body}");
    assert_eq!(
        valid["sub/real.txt"]["path"],
        serde_json::json!(canon.join("sub").join("real.txt").to_string_lossy())
    );
    assert_eq!(valid["sub/real.txt"]["kind"], "file");
    assert_eq!(
        valid["sub"]["path"],
        serde_json::json!(canon.join("sub").to_string_lossy())
    );
    assert_eq!(valid["sub"]["kind"], "dir");
    assert_eq!(valid[&abs]["path"], serde_json::json!(abs));
    assert_eq!(valid[&abs]["kind"], "file");
    assert_eq!(valid["./sub/../top.rs"]["path"], serde_json::json!(abs));
    assert!(!valid.contains_key("missing.txt"), "{body}");
}

#[tokio::test]
async fn fs_validate_caps_candidates_and_rejects_relative_base() {
    let state = test_state();
    let base = test_dir("validate-cap");
    std::fs::write(base.join("real.txt"), "x").unwrap();

    // Candidates past the 50 cap are ignored, even valid ones.
    let mut candidates: Vec<String> = (0..50).map(|i| format!("nope-{i}")).collect();
    candidates.push("real.txt".to_string());
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/fs/validate",
        Some(serde_json::json!({
            "candidates": candidates,
            "base": base.to_string_lossy(),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["valid"].as_object().unwrap().is_empty(), "{body}");

    // A non-absolute base is a 400 (candidates would resolve nowhere).
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/fs/validate",
        Some(serde_json::json!({
            "candidates": ["real.txt"],
            "base": "relative/base",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["error"].is_string(), "{body}");
}

/// One /fs/validate call with a workspace_id, returning the `valid` map.
async fn validate_in_workspace(
    state: &Arc<AppState>,
    base: &std::path::Path,
    workspace_id: &str,
    candidates: &[&str],
) -> serde_json::Value {
    let (status, body) = request(
        state,
        Method::POST,
        "/api/v1/fs/validate",
        Some(serde_json::json!({
            "candidates": candidates,
            "base": base.to_string_lossy(),
            "workspace_id": workspace_id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["valid"].clone()
}

#[tokio::test]
async fn fs_validate_bare_basename_falls_back_to_unique_workspace_file() {
    let state = test_state();
    let root = test_dir("validate-bare");
    // The reported scenario: the agent says "FIGURE_PLAN.md", the file lives
    // at paper/FIGURE_PLAN.md — direct-child resolution can never confirm it.
    std::fs::create_dir_all(root.join("paper")).unwrap();
    std::fs::write(root.join("paper/FIGURE_PLAN.md"), "x").unwrap();
    // Ambiguous basename: two dup.md files — must refuse.
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::create_dir_all(root.join("b")).unwrap();
    std::fs::write(root.join("a/dup.md"), "x").unwrap();
    std::fs::write(root.join("b/dup.md"), "x").unwrap();
    // Direct child shadows the fallback: top.md exists both at the base and
    // in a subdirectory — the direct hit must win (old behavior unchanged).
    std::fs::write(root.join("top.md"), "x").unwrap();
    std::fs::write(root.join("a/top.md"), "x").unwrap();
    // Never fallback-eligible: extension-less, dotfile, and a directory
    // whose name is extension-shaped (the fallback matches FILES only).
    std::fs::write(root.join("a/justfile"), "x").unwrap();
    std::fs::write(root.join("a/.env"), "x").unwrap();
    std::fs::create_dir_all(root.join("a/notes.md")).unwrap();
    // Ignored dirs stay invisible to the fallback (quickopen's rules).
    std::fs::create_dir_all(root.join("node_modules")).unwrap();
    std::fs::write(root.join("node_modules/hidden.md"), "x").unwrap();
    // Deeper than the index depth guard: never indexed, so never linked.
    let mut deep = root.clone();
    for i in 0..=quickopen::MAX_INDEX_DEPTH {
        deep = deep.join(format!("d{i}"));
    }
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join("buried.md"), "x").unwrap();

    let (status, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ws}");
    let ws_id = ws["id"].as_str().unwrap();
    let canon = std::fs::canonicalize(&root).unwrap();

    let valid = validate_in_workspace(
        &state,
        &root,
        ws_id,
        &[
            "FIGURE_PLAN.md", // unique in a subdir -> resolves
            "dup.md",         // ambiguous -> refused
            "top.md",         // direct child -> wins over the subdir copy
            "missing.md",     // nonexistent anywhere -> absent
            "justfile",       // no extension -> not eligible
            ".env",           // dotfile -> not eligible
            "notes.md",       // only a DIRECTORY has this name -> absent
            "hidden.md",      // only inside an ignored dir -> absent
            "buried.md",      // past the depth guard -> absent
        ],
    )
    .await;
    let valid = valid.as_object().unwrap();
    assert_eq!(
        valid["FIGURE_PLAN.md"]["path"],
        serde_json::json!(canon.join("paper/FIGURE_PLAN.md").to_string_lossy()),
        "{valid:?}"
    );
    assert_eq!(valid["FIGURE_PLAN.md"]["kind"], "file");
    assert_eq!(
        valid["top.md"]["path"],
        serde_json::json!(canon.join("top.md").to_string_lossy())
    );
    assert_eq!(valid.len(), 2, "only the two hits answer: {valid:?}");

    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn fs_validate_bare_basename_without_workspace_keeps_old_behavior() {
    let state = test_state();
    let root = test_dir("validate-bare-nows");
    std::fs::create_dir_all(root.join("paper")).unwrap();
    std::fs::write(root.join("paper/FIGURE_PLAN.md"), "x").unwrap();

    // No workspace_id: the fallback never fires (an older client's request).
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/fs/validate",
        Some(serde_json::json!({
            "candidates": ["FIGURE_PLAN.md"],
            "base": root.to_string_lossy(),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["valid"].as_object().unwrap().is_empty(), "{body}");

    // An unknown workspace_id degrades silently to the same miss — no error.
    let valid = validate_in_workspace(&state, &root, "w-nope", &["FIGURE_PLAN.md"]).await;
    assert!(valid.as_object().unwrap().is_empty(), "{valid}");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn quickopen_walk_guards_cap_entries_depth_and_time() {
    let root = test_dir("walk-guards");
    std::fs::create_dir_all(root.join("l1/l2/l3")).unwrap();
    for i in 0..10 {
        std::fs::write(root.join(format!("f{i}.txt")), "x").unwrap();
    }
    std::fs::write(root.join("l1/one.txt"), "x").unwrap();
    std::fs::write(root.join("l1/l2/two.txt"), "x").unwrap();
    let far = std::time::Instant::now() + std::time::Duration::from_secs(60);

    // Entry cap: the walk stops at max_files entries (partial, not an error).
    let files = quickopen::walk_bounded(&root, None, 3, quickopen::MAX_INDEX_DEPTH, far);
    assert_eq!(files.len(), 3);

    // Depth cap: max_depth levels of directories are read, nothing deeper.
    // With max_depth=2 the root and l1 are read (one.txt indexed), l2 is
    // recorded as an entry but never descended (two.txt absent).
    let files = quickopen::walk_bounded(&root, None, 100_000, 2, far);
    let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
    assert!(names.contains(&"one.txt"), "{names:?}");
    assert!(names.contains(&"l2"), "{names:?}");
    assert!(!names.contains(&"two.txt"), "{names:?}");

    // Time cap: an already-expired deadline yields an empty (partial) index.
    let files = quickopen::walk_bounded(
        &root,
        None,
        100_000,
        quickopen::MAX_INDEX_DEPTH,
        std::time::Instant::now() - std::time::Duration::from_secs(1),
    );
    assert!(files.is_empty());

    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn fs_list_dirs_first_sorted_with_metadata() {
    let state = test_state();
    let root = test_dir("fs-full-list");
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::create_dir(root.join("Docs")).unwrap();
    std::fs::create_dir(root.join(".git")).unwrap();
    std::fs::write(root.join("README.md"), "hello").unwrap();
    std::fs::write(root.join("app.rs"), "fn main() {}").unwrap();
    std::fs::write(root.join(".env"), "SECRET=1").unwrap();
    // A symlink to a dir (kind "dir", symlink true), one to a file (kind
    // "file"), and a dangling one (kind "file", broken true) — all marked.
    std::os::unix::fs::symlink(root.join("src"), root.join("link-dir")).unwrap();
    std::os::unix::fs::symlink(root.join("app.rs"), root.join("link-file")).unwrap();
    std::os::unix::fs::symlink(root.join("nowhere"), root.join("dangling")).unwrap();

    let canonical = std::fs::canonicalize(&root).unwrap();
    let uri = format!("/api/v1/fs/list?path={}", root.to_string_lossy());
    let (status, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["path"].as_str().unwrap(), canonical.to_str().unwrap());
    assert_eq!(
        json["parent"].as_str().unwrap(),
        canonical.parent().unwrap().to_str().unwrap()
    );

    // Dirs first (case-insensitive), then files; dot entries excluded. A
    // symlink-to-dir sorts with the dirs (its resolved kind), links and the
    // broken one sort among the files.
    let entries = json["entries"].as_array().unwrap();
    let by_name = |n: &str| entries.iter().find(|e| e["name"] == n).unwrap();
    let names: Vec<&str> = entries
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "Docs",
            "link-dir",
            "src",
            "app.rs",
            "dangling",
            "link-file",
            "README.md"
        ]
    );
    assert_eq!(by_name("README.md")["kind"], "file");
    assert_eq!(by_name("README.md")["size"], 5); // "hello"
                                                 // A symlink-to-dir keeps kind "dir" (navigation is unchanged) but is
                                                 // marked, with its raw target text for the "→" hover.
    assert_eq!(by_name("link-dir")["kind"], "dir");
    assert_eq!(by_name("link-dir")["symlink"], true);
    // `target` is the raw readlink text, exactly as it was created (not
    // canonicalized), so it's the pre-/private path we passed to symlink().
    assert_eq!(
        by_name("link-dir")["target"].as_str().unwrap(),
        root.join("src").to_str().unwrap()
    );
    assert!(!by_name("link-dir")["broken"].as_bool().unwrap_or(false));
    // A symlink-to-file: kind "file", marked, not broken.
    assert_eq!(by_name("link-file")["kind"], "file");
    assert_eq!(by_name("link-file")["symlink"], true);
    // A dangling symlink is now VISIBLE (so it can be removed from the UI):
    // kind "file", symlink true, broken true, its target text preserved.
    assert_eq!(by_name("dangling")["kind"], "file");
    assert_eq!(by_name("dangling")["symlink"], true);
    assert_eq!(by_name("dangling")["broken"], true);
    assert_eq!(
        by_name("dangling")["target"].as_str().unwrap(),
        root.join("nowhere").to_str().unwrap()
    );
    // A plain file omits the additive symlink fields entirely.
    assert!(by_name("README.md").get("symlink").is_none());

    // hidden=true adds the dot entries in their sorted spots.
    let uri = format!(
        "/api/v1/fs/list?path={}&hidden=true",
        root.to_string_lossy()
    );
    let (status, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            ".git",
            "Docs",
            "link-dir",
            "src",
            ".env",
            "app.rs",
            "dangling",
            "link-file",
            "README.md"
        ]
    );
}

#[tokio::test]
async fn fs_list_caps_large_directories_and_reports_truncation() {
    let state = test_state();
    let root = test_dir("fs-list-cap");
    for n in 0..=fs::MAX_DIR_ENTRIES {
        std::fs::File::create(root.join(format!("entry-{n:04}"))).unwrap();
    }

    let uri = format!("/api/v1/fs/list?path={}", root.to_string_lossy());
    let (status, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json["entries"].as_array().unwrap().len(),
        fs::MAX_DIR_ENTRIES
    );
    assert_eq!(json["truncated"], true);
}

#[tokio::test]
async fn fs_list_rejects_files_and_missing_paths() {
    let state = test_state();
    let root = test_dir("fs-list-bad");
    let file = root.join("plain.txt");
    std::fs::write(&file, "x").unwrap();

    for path in [
        file.to_string_lossy().into_owned(),
        "/definitely/not/a/dir".into(),
    ] {
        let uri = format!("/api/v1/fs/list?path={path}");
        let (status, err) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert!(err["error"].is_string());
    }
}

#[tokio::test]
async fn fs_file_serves_slices_with_size_headers() {
    let state = test_state();
    let root = test_dir("fs-file");
    let path = root.join("notes.txt");
    std::fs::write(&path, "0123456789").unwrap();
    let path = path.to_string_lossy();

    // Whole file by default.
    let uri = format!("/api/v1/fs/file?path={path}");
    let (status, headers, body) =
        request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"0123456789");
    assert!(header_str(&headers, "content-type").starts_with("text/plain"));
    assert_eq!(header_str(&headers, "x-file-size"), "10");
    assert_eq!(header_str(&headers, "x-truncated"), "false");

    // A middle slice reports truncation.
    let uri = format!("/api/v1/fs/file?path={path}&offset=3&limit=4");
    let (status, headers, body) =
        request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"3456");
    assert_eq!(header_str(&headers, "x-file-size"), "10");
    assert_eq!(header_str(&headers, "x-truncated"), "true");

    // A slice ending exactly at EOF is not truncated.
    let uri = format!("/api/v1/fs/file?path={path}&offset=6&limit=4");
    let (status, headers, body) =
        request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"6789");
    assert_eq!(header_str(&headers, "x-truncated"), "false");

    // An offset past EOF yields an empty, non-truncated body.
    let uri = format!("/api/v1/fs/file?path={path}&offset=100");
    let (status, headers, body) =
        request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty());
    assert_eq!(header_str(&headers, "x-truncated"), "false");
}

#[tokio::test]
async fn fs_file_limit_is_capped_at_2mb() {
    let state = test_state();
    let root = test_dir("fs-file-cap");
    let path = root.join("big.bin");
    std::fs::write(&path, vec![0x42u8; 3 * 1024 * 1024]).unwrap();

    let uri = format!(
        "/api/v1/fs/file?path={}&limit=99999999",
        path.to_string_lossy()
    );
    let (status, headers, body) =
        request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.len(), 2 * 1024 * 1024);
    assert_eq!(
        header_str(&headers, "x-file-size"),
        (3 * 1024 * 1024).to_string()
    );
    assert_eq!(header_str(&headers, "x-truncated"), "true");
}

#[tokio::test]
async fn fs_file_rejects_dirs_and_missing_paths() {
    let state = test_state();
    let root = test_dir("fs-file-bad");

    for path in [
        root.to_string_lossy().into_owned(),
        "/no/such/file.txt".into(),
    ] {
        let uri = format!("/api/v1/fs/file?path={path}");
        let (status, err) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert!(err["error"].is_string());
    }
}

#[tokio::test]
async fn fs_markdown_renders_gfm_and_sanitizes() {
    let state = test_state();
    let root = test_dir("fs-md");
    let path = root.join("doc.md");
    std::fs::write(
        &path,
        concat!(
            "# Title\n\n",
            "~~old~~ new, see https://example.com\n\n",
            "| a | b |\n|---|---|\n| 1 | 2 |\n\n",
            "<script>alert('xss')</script>\n\n",
            "<img src=\"x.png\" onerror=\"alert('xss')\">\n",
        ),
    )
    .unwrap();

    let uri = format!("/api/v1/fs/markdown?path={}", path.to_string_lossy());
    let (status, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    let html = json["html"].as_str().unwrap();

    // GFM features render.
    assert!(html.contains("<h1>Title</h1>"), "no heading in {html}");
    assert!(
        html.contains("<del>old</del>"),
        "no strikethrough in {html}"
    );
    assert!(html.contains("<table>"), "no table in {html}");
    assert!(
        html.contains("<a href=\"https://example.com\""),
        "no autolink in {html}"
    );
    // Sanitization strips script tags and event handlers but keeps the img.
    assert!(!html.contains("<script"), "script survived in {html}");
    assert!(!html.contains("onerror"), "onerror survived in {html}");
    assert!(!html.contains("alert("), "alert survived in {html}");
    assert!(
        html.contains("<img src=\"x.png\""),
        "img stripped in {html}"
    );
}

/// `fs/xlsx` parses a spreadsheet into the same paged `TablePage` shape the CSV
/// viewer renders, plus the workbook's sheet list — the first row is the header,
/// a named sheet is selectable, paging past the data is an empty (not error)
/// page, and an unknown sheet is a 400. Reads a committed 2-sheet fixture.
#[tokio::test]
async fn fs_xlsx_pages_sheets_of_a_workbook() {
    let state = test_state();
    let fixture = format!(
        "{}/src/tests/fixtures/sample.xlsx",
        env!("CARGO_MANIFEST_DIR")
    );

    // Default sheet (the first) carries the whole sheet list.
    let (status, json) = request(
        &state,
        Method::GET,
        &format!("/api/v1/fs/xlsx?path={fixture}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["sheets"], serde_json::json!(["Alpha", "Beta"]));
    assert_eq!(json["sheet"], "Alpha");
    assert_eq!(json["columns"], serde_json::json!(["id", "value"]));
    assert_eq!(json["rows"], serde_json::json!([["a", "1"], ["b", "2"]]));
    assert_eq!(json["truncated"], false);

    // A named sheet.
    let (status, json) = request(
        &state,
        Method::GET,
        &format!("/api/v1/fs/xlsx?path={fixture}&sheet=Beta"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["sheet"], "Beta");
    assert_eq!(json["columns"], serde_json::json!(["k"]));
    assert_eq!(json["rows"], serde_json::json!([["x"]]));

    // Paging past the data is an empty page, not an error (the grid stops).
    let (status, json) = request(
        &state,
        Method::GET,
        &format!("/api/v1/fs/xlsx?path={fixture}&sheet=Alpha&offset_rows=100"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["rows"], serde_json::json!([]));

    // An unknown sheet is a 400.
    let (status, _) = request(
        &state,
        Method::GET,
        &format!("/api/v1/fs/xlsx?path={fixture}&sheet=Nope"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn fs_markdown_rejects_oversize_dirs_and_missing() {
    let state = test_state();
    let root = test_dir("fs-md-bad");

    // One byte over the 4MB limit is a 400.
    let big = root.join("big.md");
    std::fs::write(&big, "a".repeat(4 * 1024 * 1024 + 1)).unwrap();
    let uri = format!("/api/v1/fs/markdown?path={}", big.to_string_lossy());
    let (status, err) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(err["error"].as_str().unwrap().contains("too large"));

    for path in [
        root.to_string_lossy().into_owned(),
        "/no/such/doc.md".into(),
    ] {
        let uri = format!("/api/v1/fs/markdown?path={path}");
        let (status, err) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert!(err["error"].is_string());
    }
}

#[tokio::test]
async fn fs_table_pages_csv_with_header() {
    let state = test_state();
    let root = test_dir("fs-table");
    let path = root.join("data.csv");
    let mut csv = String::from("name,value,note\n");
    for i in 0..8 {
        csv.push_str(&format!("row{i},{i},\"has, comma\"\n"));
    }
    std::fs::write(&path, csv).unwrap();
    let path = path.to_string_lossy();

    // Defaults: all 8 rows fit in one 200-row page.
    let uri = format!("/api/v1/fs/table?path={path}");
    let (status, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json["columns"],
        serde_json::json!(["name", "value", "note"])
    );
    assert_eq!(json["rows"].as_array().unwrap().len(), 8);
    assert_eq!(
        json["rows"][0],
        serde_json::json!(["row0", "0", "has, comma"])
    );
    assert_eq!(json["offset"], 0);
    assert_eq!(json["truncated"], false);

    // A limited page is truncated.
    let uri = format!("/api/v1/fs/table?path={path}&limit_rows=3");
    let (_, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(json["rows"].as_array().unwrap().len(), 3);
    assert_eq!(json["rows"][2][0], "row2");
    assert_eq!(json["truncated"], true);

    // The final short page is not.
    let uri = format!("/api/v1/fs/table?path={path}&offset_rows=6&limit_rows=3");
    let (_, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(json["rows"].as_array().unwrap().len(), 2);
    assert_eq!(json["rows"][0][0], "row6");
    assert_eq!(json["offset"], 6);
    assert_eq!(json["truncated"], false);
}

#[tokio::test]
async fn fs_table_sniffs_delimiters() {
    let state = test_state();
    let root = test_dir("fs-table-sniff");

    // .tsv extension forces tabs.
    let tsv = root.join("data.tsv");
    std::fs::write(&tsv, "a\tb\n1\t2\n").unwrap();
    let uri = format!("/api/v1/fs/table?path={}", tsv.to_string_lossy());
    let (status, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["columns"], serde_json::json!(["a", "b"]));
    assert_eq!(json["rows"][0], serde_json::json!(["1", "2"]));

    // Unknown extension: a tab in the first line wins over commas.
    let weird = root.join("export.data");
    std::fs::write(&weird, "x\ty\n3\t4\n").unwrap();
    let uri = format!("/api/v1/fs/table?path={}", weird.to_string_lossy());
    let (status, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["columns"], serde_json::json!(["x", "y"]));

    // Explicit delim=tab overrides a .csv extension.
    let mixed = root.join("tabs.csv");
    std::fs::write(&mixed, "p\tq\n5\t6\n").unwrap();
    let uri = format!(
        "/api/v1/fs/table?path={}&delim=tab",
        mixed.to_string_lossy()
    );
    let (status, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["columns"], serde_json::json!(["p", "q"]));

    // An unsupported delim value is a 400.
    let uri = format!("/api/v1/fs/table?path={}&delim=pipe", tsv.to_string_lossy());
    let (status, err) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(err["error"].as_str().unwrap().contains("delimiter"));
}

#[tokio::test]
async fn fs_table_caps_rows_and_rejects_corrupt_gz_dirs_missing() {
    let state = test_state();
    let root = test_dir("fs-table-bad");

    // limit_rows above the 1000 cap clamps to 1000.
    let big = root.join("big.csv");
    let mut csv = String::from("n\n");
    for i in 0..1200 {
        csv.push_str(&format!("{i}\n"));
    }
    std::fs::write(&big, csv).unwrap();
    let uri = format!(
        "/api/v1/fs/table?path={}&limit_rows=1200",
        big.to_string_lossy()
    );
    let (status, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["rows"].as_array().unwrap().len(), 1000);
    assert_eq!(json["truncated"], true);

    // A .gz that is not actually gzip is a clean 400, not a hang or 500.
    let gz = root.join("data.csv.gz");
    std::fs::write(&gz, b"totally not gzip bytes").unwrap();
    let uri = format!("/api/v1/fs/table?path={}", gz.to_string_lossy());
    let (status, err) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(err["error"].is_string());

    for path in [
        root.to_string_lossy().into_owned(),
        "/no/such/data.csv".into(),
    ] {
        let uri = format!("/api/v1/fs/table?path={path}");
        let (status, err) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert!(err["error"].is_string());
    }
}

#[tokio::test]
async fn fs_table_pages_tsv_gz_including_multimember() {
    let state = test_state();
    let root = test_dir("fs-table-gz");

    // Single member: pages exactly like the plain-file test.
    let mut tsv = String::from("name\tvalue\n");
    for i in 0..8 {
        tsv.push_str(&format!("row{i}\t{i}\n"));
    }
    let single = root.join("data.tsv.gz");
    std::fs::write(&single, gzip_bytes(tsv.as_bytes(), None)).unwrap();
    let path = single.to_string_lossy();

    let uri = format!("/api/v1/fs/table?path={path}");
    let (status, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["columns"], serde_json::json!(["name", "value"]));
    assert_eq!(json["rows"].as_array().unwrap().len(), 8);
    assert_eq!(json["rows"][0], serde_json::json!(["row0", "0"]));
    assert_eq!(json["truncated"], false);

    let uri = format!("/api/v1/fs/table?path={path}&limit_rows=3");
    let (_, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(json["rows"].as_array().unwrap().len(), 3);
    assert_eq!(json["truncated"], true);

    let uri = format!("/api/v1/fs/table?path={path}&offset_rows=6&limit_rows=3");
    let (_, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(json["rows"].as_array().unwrap().len(), 2);
    assert_eq!(json["rows"][0][0], "row6");
    assert_eq!(json["offset"], 6);
    assert_eq!(json["truncated"], false);

    // Multi-member (bgzip-style concatenated gzip streams), with the
    // member boundary cutting a row in half: the decode is seamless.
    let mut multi = gzip_bytes(b"a\tb\nrow0\t0\nro", None);
    multi.extend(gzip_bytes(b"w1\t1\nrow2\t2\n", None));
    let multi_path = root.join("multi.tsv.gz");
    std::fs::write(&multi_path, multi).unwrap();
    let uri = format!("/api/v1/fs/table?path={}", multi_path.to_string_lossy());
    let (status, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["columns"], serde_json::json!(["a", "b"]));
    assert_eq!(
        json["rows"],
        serde_json::json!([["row0", "0"], ["row1", "1"], ["row2", "2"]])
    );

    // .bgz reads the same as .gz.
    let bgz = root.join("data.tsv.bgz");
    std::fs::write(&bgz, gzip_bytes(b"x\ty\n1\t2\n", None)).unwrap();
    let uri = format!("/api/v1/fs/table?path={}", bgz.to_string_lossy());
    let (status, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["columns"], serde_json::json!(["x", "y"]));
}

#[tokio::test]
async fn fs_table_gz_sniffs_inner_name() {
    let state = test_state();
    let root = test_dir("fs-table-gz-sniff");

    // Outer name says nothing ("blob.gz"), but the member FNAME says
    // .csv — comma wins even though the first line contains a tab
    // (content-sniffing alone would have picked tab).
    let blob = root.join("blob.gz");
    std::fs::write(&blob, gzip_bytes(b"a,b\tc\n1,2\t3\n", Some("data.csv"))).unwrap();
    let uri = format!("/api/v1/fs/table?path={}", blob.to_string_lossy());
    let (status, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["columns"], serde_json::json!(["a", "b\tc"]));

    // No FNAME, no inner extension: the first decoded line is sniffed.
    let mystery = root.join("mystery.gz");
    std::fs::write(&mystery, gzip_bytes(b"x\ty\n3\t4\n", None)).unwrap();
    let uri = format!("/api/v1/fs/table?path={}", mystery.to_string_lossy());
    let (status, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["columns"], serde_json::json!(["x", "y"]));
}

#[tokio::test]
async fn fs_file_gz_serves_decompressed_slices() {
    let state = test_state();
    let root = test_dir("fs-file-gz");
    let path = root.join("notes.txt.gz");
    std::fs::write(&path, gzip_bytes(b"abcdefghij", None)).unwrap();
    let path = path.to_string_lossy();

    // Whole file: decompressed bytes, inner-name content type, exact size.
    let uri = format!("/api/v1/fs/file?path={path}");
    let (status, headers, body) =
        request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"abcdefghij");
    assert!(header_str(&headers, "content-type").starts_with("text/plain"));
    assert_eq!(header_str(&headers, "x-truncated"), "false");
    assert_eq!(header_str(&headers, "x-file-size"), "10");
    assert!(header_str(&headers, "x-mtime").parse::<u128>().unwrap() > 0);

    // A head slice: truncated, and the total size is honestly unknown.
    let uri = format!("/api/v1/fs/file?path={path}&limit=4");
    let (status, headers, body) =
        request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"abcd");
    assert_eq!(header_str(&headers, "x-truncated"), "true");
    assert!(headers.get("x-file-size").is_none());

    // Offsets address decompressed bytes (sequential skip).
    let uri = format!("/api/v1/fs/file?path={path}&offset=4&limit=4");
    let (_, headers, body) = request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
    assert_eq!(&body[..], b"efgh");
    assert_eq!(header_str(&headers, "x-truncated"), "true");

    // A slice ending exactly at EOF is not truncated, and knows the size.
    let uri = format!("/api/v1/fs/file?path={path}&offset=6&limit=4");
    let (_, headers, body) = request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
    assert_eq!(&body[..], b"ghij");
    assert_eq!(header_str(&headers, "x-truncated"), "false");
    assert_eq!(header_str(&headers, "x-file-size"), "10");

    // An offset past decompressed EOF: empty, non-truncated.
    let uri = format!("/api/v1/fs/file?path={path}&offset=100");
    let (_, headers, body) = request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
    assert!(body.is_empty());
    assert_eq!(header_str(&headers, "x-truncated"), "false");
    assert_eq!(header_str(&headers, "x-file-size"), "10");

    // Multi-member decodes seamlessly here too.
    let multi_path = root.join("hello.txt.gz");
    let mut multi = gzip_bytes(b"hello ", None);
    multi.extend(gzip_bytes(b"world", None));
    std::fs::write(&multi_path, multi).unwrap();
    let uri = format!("/api/v1/fs/file?path={}", multi_path.to_string_lossy());
    let (status, _, body) = request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"hello world");
}

#[tokio::test]
async fn fs_put_file_round_trip_atomic_with_mtime_chain() {
    let state = test_state();
    let root = test_dir("fs-put");
    let path = root.join("notes.txt");
    let uri = |extra: &str| format!("/api/v1/fs/file?path={}{extra}", path.to_string_lossy());

    // Create (parent exists, file does not): 204 + the new mtime token.
    let (status, headers, body) = put_raw(&state, &uri(""), b"hello v1".to_vec()).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(body.is_empty());
    let mtime1 = header_str(&headers, "x-mtime").to_string();
    assert!(mtime1.parse::<u128>().unwrap() > 0);

    // GET reports the same token, so the editor can start a save chain.
    let (status, headers, body) =
        request_bytes(&state, Method::GET, &uri(""), Some("test-token")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"hello v1");
    assert_eq!(header_str(&headers, "x-mtime"), mtime1);

    // Save with a matching expect_mtime: accepted, token advances.
    let (status, headers, _) = put_raw(
        &state,
        &uri(&format!("&expect_mtime={mtime1}")),
        b"hello v2".to_vec(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let mtime2 = header_str(&headers, "x-mtime").to_string();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello v2");

    // Chained save against the returned token still works.
    let (status, _, _) = put_raw(
        &state,
        &uri(&format!("&expect_mtime={mtime2}")),
        b"hello v3".to_vec(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello v3");

    // Atomicity hygiene: no tmp siblings survive the writes.
    let names: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["notes.txt"], "leftover files: {names:?}");
}

#[tokio::test]
async fn fs_put_file_conflict_is_409_and_leaves_disk_untouched() {
    let state = test_state();
    let root = test_dir("fs-put-conflict");
    let path = root.join("doc.md");
    std::fs::write(&path, "original").unwrap();

    let uri = format!("/api/v1/fs/file?path={}", path.to_string_lossy());
    let (_, headers, _) = request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
    let stale = header_str(&headers, "x-mtime").to_string();

    // Another writer replaces the bytes without changing their length. The
    // opaque token still advances even on a filesystem with coarse mtimes.
    std::fs::write(&path, "external").unwrap();
    let (_, headers, _) = request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
    assert_ne!(header_str(&headers, "x-mtime"), stale);

    let (status, _, body) = put_raw(
        &state,
        &format!("{uri}&expect_mtime={stale}"),
        b"my edit".to_vec(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let err: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(err, serde_json::json!({"error": "file changed on disk"}));
    // The refused write changed nothing on disk.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "external");

    // A file deleted since the editor loaded it is a conflict too.
    let gone = root.join("gone.txt");
    let (status, _, _) = put_raw(
        &state,
        &format!(
            "/api/v1/fs/file?path={}&expect_mtime=12345",
            gone.to_string_lossy()
        ),
        b"x".to_vec(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(!gone.exists());

    // Without expect_mtime the check is skipped (explicit overwrite).
    let (status, _, _) = put_raw(&state, &uri, b"forced".to_vec()).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "forced");
}

#[tokio::test]
async fn fs_put_file_rejects_dirs_and_missing_parents() {
    let state = test_state();
    let root = test_dir("fs-put-bad");

    // Writing over a directory is refused.
    let uri = format!("/api/v1/fs/file?path={}", root.to_string_lossy());
    let (status, _, body) = put_raw(&state, &uri, b"x".to_vec()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let err: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(err["error"].as_str().unwrap().contains("directory"));

    // Creating a file whose parent directory does not exist is refused
    // (no implicit mkdir -p).
    let orphan = root.join("no/such/dir/file.txt");
    let uri = format!("/api/v1/fs/file?path={}", orphan.to_string_lossy());
    let (status, _, _) = put_raw(&state, &uri, b"x".to_vec()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!orphan.exists());
}

#[tokio::test]
async fn fs_put_file_caps_at_1mb() {
    let state = test_state();
    let root = test_dir("fs-put-cap");
    let path = root.join("big.txt");
    let uri = format!("/api/v1/fs/file?path={}", path.to_string_lossy());

    // Exactly 1MB is fine.
    let (status, _, _) = put_raw(&state, &uri, vec![b'a'; 1024 * 1024]).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 1024 * 1024);

    // One byte over is a 413, and the file is untouched.
    let (status, _, body) = put_raw(&state, &uri, vec![b'b'; 1024 * 1024 + 1]).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    let err: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(err["error"].as_str().unwrap().contains("too large"));
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 1024 * 1024);
}

#[tokio::test]
async fn fs_quickopen_ranks_matches_and_ignores() {
    let state = test_state();
    let root = test_dir("quickopen");
    for dir in [
        "src",
        "map",
        "docs",
        "node_modules",
        "target",
        ".git",
        "work",
        "dist",
        "__pycache__",
        ".venv",
        "venv",
        ".snakemake",
    ] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    // Tier 0 (name-prefix), newer beats older within the tier.
    std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
    std::fs::write(root.join("src/main_test.rs"), "#[test]").unwrap();
    age_file(&root.join("src/main_test.rs"), 3600);
    // Tier 1 (name-substring): "domain" contains "main".
    std::fs::write(root.join("src/domain.rs"), "struct D;").unwrap();
    // Tier 2 (path-subsequence): m-a-i-n spread across "map/init.txt".
    std::fs::write(root.join("map/init.txt"), "x").unwrap();
    // Non-match.
    std::fs::write(root.join("docs/other.txt"), "y").unwrap();
    // Ignored directories, all with tempting matches inside.
    for ignored in [
        "node_modules/main.js",
        "target/main.rs",
        ".git/main",
        "work/main.txt",
        "dist/main.css",
        "__pycache__/main.pyc",
        ".venv/main.py",
        "venv/main.py",
        ".snakemake/main.log",
    ] {
        std::fs::write(root.join(ignored), "z").unwrap();
    }

    let (status, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ws_id = ws["id"].as_str().unwrap().to_string();

    // Ranked: prefix (mtime-tiebroken) > substring > subsequence, and
    // nothing from the ignored directories leaks in.
    let uri = format!("/api/v1/fs/quickopen?workspace_id={ws_id}&q=main");
    let (status, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    let rels: Vec<&str> = json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["rel"].as_str().unwrap())
        .collect();
    assert_eq!(
        rels,
        [
            "src/main.rs",
            "src/main_test.rs",
            "src/domain.rs",
            "map/init.txt"
        ]
    );
    let first = &json["entries"][0];
    assert_eq!(first["name"], "main.rs");
    assert_eq!(
        first["path"].as_str().unwrap(),
        std::fs::canonicalize(&root)
            .unwrap()
            .join("src/main.rs")
            .to_str()
            .unwrap()
    );
    assert!(first["mtime"].as_u64().unwrap() > 0);

    // Matching is case-insensitive.
    let uri = format!("/api/v1/fs/quickopen?workspace_id={ws_id}&q=MAIN");
    let (_, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(json["entries"][0]["rel"], "src/main.rs");

    // Empty query: every indexed file, most recent first. Directories
    // stay out unless asked for (the Cmd+P palette is a file finder).
    let uri = format!("/api/v1/fs/quickopen?workspace_id={ws_id}&q=");
    let (_, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(json["entries"].as_array().unwrap().len(), 5);
    assert!(json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["kind"] == "file"));

    // dirs=true admits directories (chat @-mentions tag folders too) —
    // and ignored dirs still never appear, even as entries themselves.
    let uri = format!("/api/v1/fs/quickopen?workspace_id={ws_id}&q=&dirs=true");
    let (_, json) = request(&state, Method::GET, &uri, None).await;
    let entries = json["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 8); // 5 files + src, map, docs
    let dir_rels: Vec<&str> = entries
        .iter()
        .filter(|e| e["kind"] == "dir")
        .map(|e| e["rel"].as_str().unwrap())
        .collect();
    assert_eq!(dir_rels.len(), 3);
    for rel in ["src", "map", "docs"] {
        assert!(dir_rels.contains(&rel), "missing dir {rel}");
    }
    let uri = format!("/api/v1/fs/quickopen?workspace_id={ws_id}&q=src&dirs=true");
    let (_, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(json["entries"][0]["rel"], "src");
    assert_eq!(json["entries"][0]["kind"], "dir");

    // limit is honored.
    let uri = format!("/api/v1/fs/quickopen?workspace_id={ws_id}&q=main&limit=2");
    let (_, json) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(json["entries"].as_array().unwrap().len(), 2);

    // Unknown workspaces are 404s.
    let (status, err) = request(
        &state,
        Method::GET,
        "/api/v1/fs/quickopen?workspace_id=w-nope&q=x",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(err["error"].as_str().unwrap().contains("w-nope"));
}

#[tokio::test]
async fn raw_serves_byte_ranges() {
    let state = test_state();
    let root = test_dir("fs-raw-range");
    let path = root.join("doc.pdf");
    std::fs::write(&path, b"0123456789").unwrap();

    let (_, json) = request(
        &state,
        Method::POST,
        "/api/v1/fs/ticket",
        Some(serde_json::json!({"path": path.to_string_lossy()})),
    )
    .await;
    let ticket = json["ticket"].as_str().unwrap();
    let uri = format!("/raw/{ticket}");

    let ranged = |range: &'static str| {
        let state = state.clone();
        let uri = uri.clone();
        async move {
            let res = app(state)
                .oneshot(
                    Request::builder()
                        .uri(&uri)
                        .header(header::RANGE, range)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = res.status();
            let headers = res.headers().clone();
            let bytes = res.into_body().collect().await.unwrap().to_bytes();
            (status, headers, bytes)
        }
    };

    // Full fetch advertises range support.
    let (status, headers, body) = request_bytes(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"0123456789");
    assert_eq!(header_str(&headers, "accept-ranges"), "bytes");

    // bounded, open-ended, and suffix forms.
    let (status, headers, body) = ranged("bytes=2-5").await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(&body[..], b"2345");
    assert_eq!(header_str(&headers, "content-range"), "bytes 2-5/10");
    assert_eq!(header_str(&headers, "content-type"), "application/pdf");

    let (status, headers, body) = ranged("bytes=7-").await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(&body[..], b"789");
    assert_eq!(header_str(&headers, "content-range"), "bytes 7-9/10");

    let (status, _, body) = ranged("bytes=-3").await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(&body[..], b"789");

    // An end past EOF clamps.
    let (status, headers, body) = ranged("bytes=8-999").await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(&body[..], b"89");
    assert_eq!(header_str(&headers, "content-range"), "bytes 8-9/10");

    // A start past EOF is unsatisfiable.
    let (status, headers, _) = ranged("bytes=100-").await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(header_str(&headers, "content-range"), "bytes */10");

    // Malformed and multipart ranges fall back to the whole file.
    for odd in ["bytes=nope", "bytes=1-2,4-5", "chapters=1-2"] {
        let res = app(state.clone())
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .header(header::RANGE, odd)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{odd}");
    }
}

#[tokio::test]
async fn fs_ticket_mints_and_raw_serves_without_auth() {
    let state = test_state();
    let root = test_dir("fs-ticket");
    let path = root.join("pic.png");
    std::fs::write(&path, b"\x89PNG fake image bytes").unwrap();

    // Mint (bearer-authed).
    let (status, json) = request(
        &state,
        Method::POST,
        "/api/v1/fs/ticket",
        Some(serde_json::json!({"path": path.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ticket = json["ticket"].as_str().unwrap().to_string();
    assert!(ticket.starts_with("t-"), "bad ticket {ticket}");
    assert_eq!(ticket.len(), 34, "bad ticket {ticket}");
    assert!(ticket[2..]
        .chars()
        .all(|c| matches!(c, '0'..='9' | 'a'..='f')));

    // Fetch with NO Authorization header.
    let uri = format!("/raw/{ticket}");
    let (status, headers, body) = request_bytes(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"\x89PNG fake image bytes");
    assert_eq!(header_str(&headers, "content-type"), "image/png");
    assert!(headers.get("content-security-policy").is_none());

    // Tickets are reusable within their TTL (an <img> may refetch).
    let (status, _, _) = request_bytes(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);

    // Unknown tickets are 404s.
    let (status, _, _) = request_bytes(
        &state,
        Method::GET,
        "/raw/t-00000000000000000000000000000000",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A file that vanished after minting is a 404 too.
    std::fs::remove_file(&path).unwrap();
    let (status, _, _) = request_bytes(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Minting for a missing path is a 400. (Directories mint fine now —
    // folder downloads — but /raw itself stays file-only; see the
    // download tests.)
    let (status, err) = request(
        &state,
        Method::POST,
        "/api/v1/fs/ticket",
        Some(serde_json::json!({"path": "/no/such/pic.png"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(err["error"].is_string());
}

#[tokio::test]
async fn fs_ticket_expires() {
    let state = test_state();
    let root = test_dir("fs-ticket-expiry");
    let path = root.join("page.txt");
    std::fs::write(&path, "still here").unwrap();

    let (status, json) = request(
        &state,
        Method::POST,
        "/api/v1/fs/ticket",
        Some(serde_json::json!({"path": path.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ticket = json["ticket"].as_str().unwrap().to_string();

    let uri = format!("/raw/{ticket}");
    let (status, _, _) = request_bytes(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);

    // Once expired the ticket is gone for good, even though the file
    // still exists.
    lock(&state.tickets).expire(&ticket);
    let (status, _, _) = request_bytes(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn raw_html_is_sandboxed() {
    let state = test_state();
    let root = test_dir("fs-raw-html");
    let path = root.join("report.html");
    std::fs::write(&path, "<h1>hi</h1><script>runs_in_sandbox()</script>").unwrap();

    let (_, json) = request(
        &state,
        Method::POST,
        "/api/v1/fs/ticket",
        Some(serde_json::json!({"path": path.to_string_lossy()})),
    )
    .await;
    let ticket = json["ticket"].as_str().unwrap();

    let uri = format!("/raw/{ticket}");
    let (status, headers, body) = request_bytes(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header_str(&headers, "content-type"), "text/html");
    assert_eq!(
        header_str(&headers, "content-security-policy"),
        "sandbox allow-scripts"
    );
    assert_eq!(header_str(&headers, "referrer-policy"), "no-referrer");
    // Raw bytes pass through unmodified — the sandbox does the confining.
    assert_eq!(&body[..], b"<h1>hi</h1><script>runs_in_sandbox()</script>");
}
