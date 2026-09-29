//! Git G2: several repositories in one workspace — discovery, `repo=` on
//! the routes (validated, never an arbitrary path), per-repository
//! invalidation, and the file tree as a discovery source.

use super::support::*;
use crate::AppState;

fn git_in(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(dir)
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn init_repo_at(dir: &std::path::Path, file: &str) {
    std::fs::create_dir_all(dir).unwrap();
    git_in(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join(file), "one\n").unwrap();
    git_in(dir, &["add", file]);
    git_in(dir, &["commit", "-qm", "init"]);
}

/// A project folder holding repositories rather than being one:
///   root/pipeline (repo, with a submodule vendor/tool)
///   root/analysis (repo)
///   root/tools/cloned (a clone two levels down)
///   root/deep/a/b (three levels down — not probed; the tree finds it)
///   root/node_modules/pkg (ignored by name)
fn project_folder() -> (PathBuf, PathBuf) {
    let root = test_dir("multi");
    let upstream = test_dir("multi-upstream").join("tool");
    init_repo_at(&upstream, "tool.txt");
    init_repo_at(&root.join("pipeline"), "Snakefile");
    git_in(
        &root.join("pipeline"),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            &upstream.to_string_lossy(),
            "vendor/tool",
        ],
    );
    git_in(&root.join("pipeline"), &["commit", "-qm", "add tool"]);
    init_repo_at(&root.join("analysis"), "notebook.py");
    init_repo_at(&root.join("tools/cloned"), "README");
    init_repo_at(&root.join("deep/a/b"), "x.txt");
    init_repo_at(&root.join("node_modules/pkg"), "index.js");
    (root, upstream)
}

async fn add_workspace(state: &Arc<AppState>, root: &std::path::Path) -> (String, PathBuf) {
    let (status, ws) = request(
        state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    assert!(status.is_success(), "workspace create failed: {ws}");
    (
        ws["id"].as_str().unwrap().to_string(),
        PathBuf::from(ws["root"].as_str().unwrap()),
    )
}

#[tokio::test]
async fn a_folder_of_repositories_lists_each_with_its_kind() {
    let (folder, _) = project_folder();
    let state = test_state();
    let (ws, root) = add_workspace(&state, &folder).await;

    // The root itself is not a repository — and that is not an error.
    let (_, status) = request(
        &state,
        Method::GET,
        &format!("/api/v1/git/status?workspace_id={ws}"),
        None,
    )
    .await;
    assert_eq!(status["repo"], false);

    let (code, body) = request(
        &state,
        Method::GET,
        &format!("/api/v1/git/repos?workspace_id={ws}"),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let repos = body["repos"].as_array().unwrap();
    let by_rel = |rel: &str| {
        repos
            .iter()
            .find(|r| r["rel"] == rel)
            .cloned()
            .unwrap_or_else(|| panic!("{rel} not listed: {body}"))
    };
    assert_eq!(by_rel("analysis")["kind"], "nested");
    assert_eq!(by_rel("pipeline")["kind"], "nested");
    assert_eq!(by_rel("pipeline")["branch"], "main");
    assert_eq!(by_rel("tools/cloned")["kind"], "nested");
    let tool = by_rel("pipeline/vendor/tool");
    assert_eq!(tool["kind"], "submodule");
    assert_eq!(tool["submodule"], true);
    assert_eq!(
        tool["parent"],
        serde_json::json!(root.join("pipeline")),
        "the submodule sits in its superproject"
    );
    assert!(
        !repos.iter().any(|r| r["rel"] == "deep/a/b"),
        "three levels down is not probed"
    );
    assert!(
        !repos
            .iter()
            .any(|r| r["rel"].as_str().unwrap_or("").contains("node_modules")),
        "the ignore list is skipped"
    );
    assert_eq!(body["capped"], false);

    // Listing the deep folder in the file tree finds it (free: the listing
    // already happened).
    let (code, _) = request(
        &state,
        Method::GET,
        &format!(
            "/api/v1/fs/list?path={}&hidden=false",
            urlencode(&root.join("deep/a/b").to_string_lossy())
        ),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    let (_, body) = request(
        &state,
        Method::GET,
        &format!("/api/v1/git/repos?workspace_id={ws}"),
        None,
    )
    .await;
    assert!(
        body["repos"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["rel"] == "deep/a/b"),
        "the tree listing added it: {body}"
    );
}

#[tokio::test]
async fn repo_param_selects_a_known_repository_and_refuses_anything_else() {
    let (folder, _) = project_folder();
    let state = test_state();
    let (ws, root) = add_workspace(&state, &folder).await;
    request(
        &state,
        Method::GET,
        &format!("/api/v1/git/repos?workspace_id={ws}"),
        None,
    )
    .await;
    let analysis = root.join("analysis");
    std::fs::write(analysis.join("notebook.py"), "two\n").unwrap();

    let (code, status) = request(
        &state,
        Method::GET,
        &format!(
            "/api/v1/git/status?workspace_id={ws}&repo={}",
            urlencode(&analysis.to_string_lossy())
        ),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{status}");
    assert_eq!(status["repo"], true);
    assert_eq!(status["toplevel"], serde_json::json!(analysis));
    assert_eq!(status["entries"][0]["rel"], "notebook.py");

    // Never an arbitrary path — not even a real repository elsewhere.
    let elsewhere = init_temp_repo("multi-elsewhere");
    let (code, _) = request(
        &state,
        Method::GET,
        &format!(
            "/api/v1/git/status?workspace_id={ws}&repo={}",
            urlencode(&elsewhere.to_string_lossy())
        ),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);

    // The diff of a nested repository's file needs no `repo`: the innermost
    // repository holding the path is the one diffed.
    let (code, diff) = request(
        &state,
        Method::GET,
        &format!(
            "/api/v1/git/diff?workspace_id={ws}&path={}",
            urlencode(&analysis.join("notebook.py").to_string_lossy())
        ),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{diff}");
    assert_eq!(diff["a"], "one\n");
    assert_eq!(diff["b"], "two\n");
}

#[tokio::test]
async fn a_change_refreshes_only_the_repository_containing_it() {
    let (folder, _) = project_folder();
    let state = test_state();
    let (ws, root) = add_workspace(&state, &folder).await;
    request(
        &state,
        Method::GET,
        &format!("/api/v1/git/repos?workspace_id={ws}"),
        None,
    )
    .await;
    let analysis = root.join("analysis");
    let pipeline = root.join("pipeline");
    let epoch = |top: &std::path::Path| {
        state
            .git
            .repo_epochs_snapshot()
            .get(&ws)
            .and_then(|m| m.get(top).copied())
            .unwrap_or(0)
    };
    let (a0, p0) = (epoch(&analysis), epoch(&pipeline));
    let file = analysis.join("notebook.py");
    std::fs::write(&file, "edited\n").unwrap();
    crate::git::mark_path_dirty(&state, &file.to_string_lossy()).await;
    assert_eq!(epoch(&analysis), a0 + 1);
    assert_eq!(epoch(&pipeline), p0, "the neighbour did not move");

    // A change inside a submodule refreshes its superproject too (whose
    // status marks the submodule).
    let tool = pipeline.join("vendor/tool");
    let t0 = epoch(&tool);
    let p1 = epoch(&pipeline);
    crate::git::mark_path_dirty(&state, &tool.join("tool.txt").to_string_lossy()).await;
    assert_eq!(epoch(&tool), t0 + 1);
    assert_eq!(epoch(&pipeline), p1 + 1);
}
