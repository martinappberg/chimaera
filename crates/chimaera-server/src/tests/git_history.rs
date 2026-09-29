//! Git G3: history — the log (paged, per file with renames followed), one
//! commit's files, `rev=` on the diff, and "Changes on this branch".

use super::support::*;
use crate::AppState;

fn git_in(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Ada")
        .env("GIT_AUTHOR_EMAIL", "ada@example.com")
        .env("GIT_COMMITTER_NAME", "Ada")
        .env("GIT_COMMITTER_EMAIL", "ada@example.com")
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

fn commit(dir: &std::path::Path, file: &str, body: &str, message: &str) -> String {
    if let Some(parent) = std::path::Path::new(file).parent() {
        std::fs::create_dir_all(dir.join(parent)).unwrap();
    }
    std::fs::write(dir.join(file), body).unwrap();
    git_in(dir, &["add", "-A"]);
    git_in(dir, &["commit", "-qm", message]);
    git_in(dir, &["rev-parse", "HEAD"])
}

async fn workspace(state: &Arc<AppState>, root: &std::path::Path) -> String {
    let (status, ws) = request(
        state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    assert!(status.is_success(), "{ws}");
    ws["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn log_pages_and_follows_a_file_across_a_rename() {
    let repo = init_temp_repo("hist-log");
    commit(&repo, "notes.md", "v1\n", "docs: notes");
    for i in 0..5 {
        commit(
            &repo,
            "notes.md",
            &format!("v{}\n", i + 2),
            &format!("docs: revision {i}"),
        );
    }
    git_in(&repo, &["mv", "notes.md", "guide.md"]);
    git_in(&repo, &["commit", "-qm", "docs: rename notes to guide"]);
    commit(&repo, "other.txt", "x\n", "chore: unrelated");
    let state = test_state();
    let ws = workspace(&state, &repo).await;

    let (code, page) = request(
        &state,
        Method::GET,
        &format!("/api/v1/git/log?workspace_id={ws}&limit=3"),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{page}");
    let commits = page["commits"].as_array().unwrap();
    assert_eq!(commits.len(), 3);
    assert_eq!(commits[0]["subject"], "chore: unrelated");
    assert_eq!(commits[0]["author"], "Ada");
    assert_eq!(page["has_more"], true);
    let (_, next) = request(
        &state,
        Method::GET,
        &format!("/api/v1/git/log?workspace_id={ws}&limit=3&skip=3"),
        None,
    )
    .await;
    assert_ne!(next["commits"][0]["sha"], commits[0]["sha"]);

    // One file, followed back past its rename.
    let (_, hist) = request(
        &state,
        Method::GET,
        &format!(
            "/api/v1/git/log?workspace_id={ws}&path={}",
            urlencode(&repo.join("guide.md").to_string_lossy())
        ),
        None,
    )
    .await;
    let subjects: Vec<&str> = hist["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["subject"].as_str().unwrap())
        .collect();
    assert_eq!(subjects.first(), Some(&"docs: rename notes to guide"));
    assert!(subjects.contains(&"docs: notes"), "followed: {subjects:?}");
    assert!(!subjects.contains(&"chore: unrelated"));

    // Never more than 50 a page; a flag-shaped rev never reaches git.
    let (_, capped) = request(
        &state,
        Method::GET,
        &format!("/api/v1/git/log?workspace_id={ws}&limit=500"),
        None,
    )
    .await;
    assert!(capped["commits"].as_array().unwrap().len() <= 50);
    let (code, _) = request(
        &state,
        Method::GET,
        &format!("/api/v1/git/log?workspace_id={ws}&rev=--all"),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn show_lists_a_commits_files_and_diff_opens_them_by_rev() {
    let repo = init_temp_repo("hist-show");
    let before = commit(&repo, "src/qc.py", "a = 1\nb = 2\n", "feat: qc");
    std::fs::write(repo.join("src/qc.py"), "a = 1\nb = 3\nc = 4\n").unwrap();
    std::fs::write(repo.join("new.txt"), "hello\n").unwrap();
    git_in(&repo, &["add", "-A"]);
    git_in(
        &repo,
        &["commit", "-qm", "fix: rounding\n\nLonger explanation."],
    );
    let sha = git_in(&repo, &["rev-parse", "HEAD"]);
    let state = test_state();
    let ws = workspace(&state, &repo).await;

    let (code, body) = request(
        &state,
        Method::GET,
        &format!("/api/v1/git/show?workspace_id={ws}&rev={sha}"),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(body["subject"], "fix: rounding");
    assert_eq!(body["body"], "Longer explanation.");
    assert_eq!(body["parents"][0], before);
    let files = body["files"].as_array().unwrap();
    let qc = files.iter().find(|f| f["rel"] == "src/qc.py").unwrap();
    assert_eq!(qc["status"], "M");
    assert_eq!(qc["added"], 2);
    assert_eq!(qc["removed"], 1);
    let added = files.iter().find(|f| f["rel"] == "new.txt").unwrap();
    assert_eq!(added["status"], "A");

    // The commit's own change to one file: parent side vs the commit.
    let (code, diff) = request(
        &state,
        Method::GET,
        &format!(
            "/api/v1/git/diff?workspace_id={ws}&path={}&rev={sha}&mode=commit",
            urlencode(&repo.join("src/qc.py").to_string_lossy())
        ),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{diff}");
    assert_eq!(diff["a"], "a = 1\nb = 2\n");
    assert_eq!(diff["b"], "a = 1\nb = 3\nc = 4\n");
    // The working tree against a revision.
    std::fs::write(repo.join("src/qc.py"), "edited\n").unwrap();
    let (_, diff) = request(
        &state,
        Method::GET,
        &format!(
            "/api/v1/git/diff?workspace_id={ws}&path={}&rev=HEAD~1",
            urlencode(&repo.join("src/qc.py").to_string_lossy())
        ),
        None,
    )
    .await;
    assert_eq!(diff["a"], "a = 1\nb = 2\n");
    assert_eq!(diff["b"], "edited\n");
    assert_eq!(diff["b_label"], "working tree");
    // A revision that is not one is refused, not guessed.
    let (code, _) = request(
        &state,
        Method::GET,
        &format!(
            "/api/v1/git/diff?workspace_id={ws}&path={}&rev=HEAD:src/qc.py",
            urlencode(&repo.join("src/qc.py").to_string_lossy())
        ),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
}

/// "Changes on this branch": a worktree's commits since it left the main
/// branch plus its uncommitted work, whatever the main branch did since.
#[tokio::test]
async fn compare_shows_everything_since_the_branch_left_its_base() {
    let repo = init_temp_repo("hist-compare");
    let state = test_state();
    let ws = workspace(&state, &repo).await;
    let (_, created) = request(
        &state,
        Method::POST,
        "/api/v1/git/worktrees",
        Some(serde_json::json!({"workspace_id": ws, "branch": "feat/cmp"})),
    )
    .await;
    let wt = PathBuf::from(created["worktree"]["path"].as_str().unwrap());
    commit(&wt, "feature.py", "x = 1\n", "feat: the feature");
    std::fs::write(wt.join("wip.txt"), "draft\n").unwrap();
    // The main branch moves on meanwhile: not part of this branch's changes.
    commit(&repo, "main-only.txt", "m\n", "chore: main moves");

    let (code, body) = request(
        &state,
        Method::GET,
        &format!(
            "/api/v1/git/compare?workspace_id={ws}&repo={}",
            urlencode(&wt.to_string_lossy())
        ),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(body["base"], "main");
    assert_eq!(body["ahead"], 1);
    let rels: Vec<&str> = body["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["rel"].as_str().unwrap())
        .collect();
    assert!(rels.contains(&"feature.py"), "{rels:?}");
    assert!(
        rels.contains(&"wip.txt"),
        "uncommitted work counts: {rels:?}"
    );
    assert!(!rels.contains(&"main-only.txt"), "{rels:?}");

    // Each file opens against the merge base, in the branch's checkout.
    let mb = body["merge_base"].as_str().unwrap();
    let (code, diff) = request(
        &state,
        Method::GET,
        &format!(
            "/api/v1/git/diff?workspace_id={ws}&repo={}&path={}&rev={mb}",
            urlencode(&wt.to_string_lossy()),
            urlencode(&wt.join("feature.py").to_string_lossy())
        ),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{diff}");
    assert_eq!(diff["added"], true);
    assert_eq!(diff["b"], "x = 1\n");
}
