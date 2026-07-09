use super::support::*;

/// End-to-end against a REAL repo: status must resolve the branch and
/// classify a modified tracked file vs an untracked one. A directory that
/// is not a repo answers `{"repo":false}` rather than failing.
#[tokio::test]
async fn git_status_reads_a_real_repo() {
    let repo = test_dir("gitrepo");
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(&repo)
            // Hermetic: never read the developer's global/system config.
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .args(args)
            .output()
            .expect("git must be installed");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("tracked.txt"), "one\n").unwrap();
    git(&["add", "tracked.txt"]);
    git(&["commit", "-qm", "init"]);
    // A tracked file modified in the worktree, plus a brand-new file.
    std::fs::write(repo.join("tracked.txt"), "two\n").unwrap();
    std::fs::write(repo.join("new.txt"), "hi\n").unwrap();

    let state = test_state();
    let (status, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": repo.to_string_lossy()})),
    )
    .await;
    assert!(status.is_success(), "workspace create failed: {ws}");
    let ws_id = ws["id"].as_str().unwrap().to_string();

    let (status, body) = request(
        &state,
        Method::GET,
        &format!("/api/v1/git/status?workspace_id={ws_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["repo"], true);
    assert_eq!(body["branch"], "main");
    assert_eq!(body["detached"], false);

    let entries = body["entries"].as_array().unwrap();
    let modified = entries
        .iter()
        .find(|e| e["rel"] == "tracked.txt")
        .expect("modified file present");
    assert_eq!(modified["unstaged"], true);
    assert_eq!(modified["staged"], false);
    assert_eq!(modified["untracked"], false);
    assert_eq!(modified["y"], "M");

    let untracked = entries
        .iter()
        .find(|e| e["rel"] == "new.txt")
        .expect("untracked file present");
    assert_eq!(untracked["untracked"], true);
    assert_eq!(body["counts"]["untracked"], 1);
    assert_eq!(body["counts"]["unstaged"], 1);

    // The diff endpoint returns both blob sides for the modified file.
    let path = repo.join("tracked.txt");
    let (status, diff) = request(
        &state,
        Method::GET,
        &format!(
            "/api/v1/git/diff?workspace_id={ws_id}&path={}&mode=unstaged",
            urlencode(&path.to_string_lossy())
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(diff["binary"], false);
    assert_eq!(diff["a"], "one\n");
    assert_eq!(diff["b"], "two\n");
}

/// A workspace opened AT A LINKED WORKTREE — how Chimaera itself is
/// developed, so the common case, not the edge. Status must resolve that
/// worktree's own branch, and the worktree list must see the whole repo.
#[tokio::test]
async fn git_worktrees_from_a_linked_worktree() {
    let repo = test_dir("gitwtrepo");
    let linked = test_dir("gitwtlinked").join("linked");
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(&repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .args(args)
            .output()
            .expect("git must be installed");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&["add", "a.txt"]);
    git(&["commit", "-qm", "init"]);
    git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "feat/x",
        &linked.to_string_lossy(),
    ]);

    // The workspace is the LINKED worktree, not the main checkout.
    let state = test_state();
    let (status, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": linked.to_string_lossy()})),
    )
    .await;
    assert!(status.is_success(), "workspace create failed: {ws}");
    let ws_id = ws["id"].as_str().unwrap().to_string();

    // Status reports the LINKED worktree's branch, not main's.
    let (status, body) = request(
        &state,
        Method::GET,
        &format!("/api/v1/git/status?workspace_id={ws_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["repo"], true);
    assert_eq!(body["branch"], "feat/x");

    // The worktree list sees the whole repo, and marks OUR worktree current.
    let (status, body) = request(
        &state,
        Method::GET,
        &format!("/api/v1/git/worktrees?workspace_id={ws_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let list = body["worktrees"].as_array().unwrap();
    assert_eq!(list.len(), 2, "main + linked");
    let branches: Vec<&str> = list.iter().filter_map(|w| w["branch"].as_str()).collect();
    assert!(branches.contains(&"main"));
    assert!(branches.contains(&"feat/x"));
    let current: Vec<&str> = list
        .iter()
        .filter(|w| w["current"] == true)
        .filter_map(|w| w["branch"].as_str())
        .collect();
    assert_eq!(current, vec!["feat/x"], "the opened worktree is current");
}

/// Worktree CREATE: lands under the managed root, checks out the new branch,
/// and registers a workspace so the branch is immediately openable.
#[tokio::test]
async fn git_worktree_create_is_managed_and_registered() {
    let repo = init_temp_repo("wtcreate");
    let state = test_state();
    let (_, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": repo.to_string_lossy()})),
    )
    .await;
    let ws_id = ws["id"].as_str().unwrap().to_string();

    // A name git rejects never reaches `worktree add`.
    let (status, _) = request(
        &state,
        Method::POST,
        "/api/v1/git/worktrees",
        Some(serde_json::json!({"workspace_id": ws_id, "branch": "bad..name"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "invalid branch rejected");

    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/git/worktrees",
        Some(serde_json::json!({"workspace_id": ws_id, "branch": "feat/x"})),
    )
    .await;
    assert!(status.is_success(), "create failed: {body}");
    let wt_path = PathBuf::from(body["worktree"]["path"].as_str().unwrap());
    assert_eq!(body["worktree"]["branch"], "feat/x");
    assert!(wt_path.join("a.txt").exists(), "worktree is checked out");
    // Confined to the managed root.
    let managed = std::fs::canonicalize(&state.worktrees_root).unwrap();
    assert!(
        wt_path.starts_with(&managed),
        "{wt_path:?} under {managed:?}"
    );
    // And registered as a workspace you can open.
    let new_ws = body["workspace"]["id"].as_str().unwrap();
    assert!(crate::lock(&state.workspaces).get(new_ws).is_some());

    // The list marks it managed (the UI only offers remove where the daemon allows it).
    let (_, list) = request(
        &state,
        Method::GET,
        &format!("/api/v1/git/worktrees?workspace_id={ws_id}"),
        None,
    )
    .await;
    let entry = list["worktrees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["branch"] == "feat/x")
        .expect("new worktree listed");
    assert_eq!(entry["managed"], true);
    let main_entry = list["worktrees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["branch"] == "main")
        .unwrap();
    assert_eq!(
        main_entry["managed"], false,
        "the user's checkout is not ours"
    );

    // A second create for the same branch collides rather than clobbering.
    let (status, _) = request(
        &state,
        Method::POST,
        "/api/v1/git/worktrees",
        Some(serde_json::json!({"workspace_id": ws_id, "branch": "feat/x"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

/// Worktree REMOVE is destructive, so every fence gets a test: unmanaged
/// paths are refused outright, uncommitted work blocks it, and only then
/// does it delete (leaving the branch, and unregistering the workspace).
#[tokio::test]
async fn git_worktree_remove_is_fenced() {
    let repo = init_temp_repo("wtremove");
    let state = test_state();
    let (_, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": repo.to_string_lossy()})),
    )
    .await;
    let ws_id = ws["id"].as_str().unwrap().to_string();
    let (_, body) = request(
        &state,
        Method::POST,
        "/api/v1/git/worktrees",
        Some(serde_json::json!({"workspace_id": ws_id, "branch": "feat/y"})),
    )
    .await;
    let wt_path = body["worktree"]["path"].as_str().unwrap().to_string();
    let new_ws = body["workspace"]["id"].as_str().unwrap().to_string();

    // Fence 1: a checkout chimaera did not create is never removed — even
    // though it IS a real worktree of this repo.
    let (status, err) = request(
        &state,
        Method::DELETE,
        "/api/v1/git/worktrees",
        Some(serde_json::json!({"workspace_id": ws_id, "path": repo.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{err}");
    assert!(repo.join("a.txt").exists(), "the user's checkout survived");

    // Fence 4: uncommitted work blocks removal.
    std::fs::write(PathBuf::from(&wt_path).join("scratch.txt"), "wip\n").unwrap();
    let (status, err) = request(
        &state,
        Method::DELETE,
        "/api/v1/git/worktrees",
        Some(serde_json::json!({"workspace_id": ws_id, "path": wt_path})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");
    assert!(PathBuf::from(&wt_path).exists(), "dirty worktree survived");

    // Clean it, and the removal goes through.
    std::fs::remove_file(PathBuf::from(&wt_path).join("scratch.txt")).unwrap();
    let (status, err) = request(
        &state,
        Method::DELETE,
        "/api/v1/git/worktrees",
        Some(serde_json::json!({"workspace_id": ws_id, "path": wt_path})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{err}");
    assert!(!PathBuf::from(&wt_path).exists(), "worktree dir removed");
    assert!(
        crate::lock(&state.workspaces).get(&new_ws).is_none(),
        "its workspace registration is gone"
    );
    // The branch itself is untouched — removing a worktree is not rm -rf history.
    let out = std::process::Command::new("git")
        .current_dir(&repo)
        .args(["rev-parse", "--verify", "--quiet", "refs/heads/feat/y"])
        .output()
        .unwrap();
    assert!(out.status.success(), "branch feat/y still exists");
}

/// Fence 3: a live session inside a managed worktree blocks removal — pulling
/// the directory out from under someone's shell is never acceptable.
#[tokio::test]
async fn git_worktree_remove_refuses_with_a_live_session_inside() {
    let repo = init_temp_repo("wtsession");
    let state = test_state();
    let (_, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": repo.to_string_lossy()})),
    )
    .await;
    let ws_id = ws["id"].as_str().unwrap().to_string();
    let (_, body) = request(
        &state,
        Method::POST,
        "/api/v1/git/worktrees",
        Some(serde_json::json!({"workspace_id": ws_id, "branch": "feat/z"})),
    )
    .await;
    let wt_path = body["worktree"]["path"].as_str().unwrap().to_string();
    let new_ws = body["workspace"]["id"].as_str().unwrap().to_string();

    // A shell living in the new worktree.
    let (status, session) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": new_ws, "kind": "shell"})),
    )
    .await;
    assert!(status.is_success(), "spawn failed: {session}");
    let sid = session["id"].as_str().unwrap().to_string();

    let (status, err) = request(
        &state,
        Method::DELETE,
        "/api/v1/git/worktrees",
        Some(serde_json::json!({"workspace_id": ws_id, "path": wt_path, "force": true})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "even force must not evict a session"
    );
    assert!(err["error"].as_str().unwrap().contains("live session"));
    assert!(PathBuf::from(&wt_path).exists());

    state.sessions.kill(&sid).ok();
}

#[tokio::test]
async fn git_status_on_a_non_repo_says_so() {
    let plain = test_dir("notarepo");
    let state = test_state();
    let (status, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": plain.to_string_lossy()})),
    )
    .await;
    assert!(status.is_success(), "workspace create failed: {ws}");
    let ws_id = ws["id"].as_str().unwrap();
    let (status, body) = request(
        &state,
        Method::GET,
        &format!("/api/v1/git/status?workspace_id={ws_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["repo"], false);
}

/// `mark_path_dirty` canonicalizes before the `starts_with(ws.root)` prefix
/// check, so a path that reaches the workspace through a symlink (sharing no
/// component prefix with the canonical root) still bumps the git epoch. Before
/// the fix that path failed the prefix test and a real change went unannounced.
#[tokio::test]
async fn mark_path_dirty_canonicalizes_symlinked_paths() {
    let root = test_dir("git-dirty");
    let sub = root.join("subdir");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("file.rs"), "x").unwrap();

    let state = test_state();
    let (status, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ws_id = ws["id"].as_str().unwrap().to_string();
    let canonical_root = std::path::PathBuf::from(ws["root"].as_str().unwrap());

    // A symlink into the workspace: `<link>/subdir/file.rs` shares no component
    // prefix with the canonical root, but canonicalizes to inside it.
    let link_home = test_dir("git-link");
    let link = link_home.join("ws-link");
    std::os::unix::fs::symlink(&canonical_root, &link).unwrap();
    let via_link = link.join("subdir").join("file.rs");
    assert!(
        !via_link.starts_with(&canonical_root),
        "precondition: the symlinked path must not prefix-match the root"
    );

    let before = state
        .git
        .epochs_snapshot()
        .get(&ws_id)
        .copied()
        .unwrap_or(0);
    crate::git::mark_path_dirty(&state, &via_link.to_string_lossy()).await;
    let after = state
        .git
        .epochs_snapshot()
        .get(&ws_id)
        .copied()
        .unwrap_or(0);
    assert!(
        after > before,
        "a symlinked in-workspace path must bump the epoch after canonicalize (before={before}, after={after})"
    );
}
