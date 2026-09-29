//! Git G1 (docs/git-and-session-history-plan.md): sessions know their
//! repository and branch, anchors and the commits between them, refresh
//! after terminal commands, and the worktree polish (base, lock,
//! `.worktreeinclude`, the merged/unshared fences, a session's `cwd`).

use super::support::*;
use crate::{lock, AppState};

/// Run git hermetically in `dir` (never the developer's own config).
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

fn commit_file(dir: &std::path::Path, name: &str, body: &str, message: &str) -> String {
    std::fs::write(dir.join(name), body).unwrap();
    git_in(dir, &["add", name]);
    git_in(dir, &["commit", "-qm", message]);
    git_in(dir, &["rev-parse", "HEAD"])
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

/// Poll the session list until `check` holds for the session's row.
async fn wait_row(
    state: &Arc<AppState>,
    id: &str,
    what: &str,
    check: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let row = session_entry(state, id).await;
        if check(&row) {
            return row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what}: row stuck at git={} cwd_current={}",
            row["git"],
            row["cwd_current"]
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// The `cwd` every claude hook carries moves the session: its `git` field
/// names the checkout and branch it is in now, and `cwd_current` follows —
/// an agent that enters a linked worktree mid-session is shown there.
#[tokio::test]
async fn session_git_follows_the_agent_hook_cwd() {
    let repo = init_temp_repo("sg-follow");
    let linked = test_dir("sg-follow-wt").join("linked");
    git_in(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feat/y",
            &linked.to_string_lossy(),
        ],
    );
    let state = test_state();
    tokio::spawn(crate::git::track_sessions(state.clone()));
    let (ws, root) = add_workspace(&state, &repo).await;
    let id = inject_agent(&state, "k1");
    lock(&state.session_workspaces).insert(id.clone(), ws);

    // Outside a repository (its spawn folder): no git field.
    let row = wait_row(&state, &id, "tracked", |r| r.get("git").is_some()).await;
    assert!(row["git"].is_null(), "{row}");

    post_hook(
        &state,
        &id,
        "k1",
        serde_json::json!({"hook_event_name": "UserPromptSubmit", "prompt": "hi", "cwd": root}),
    )
    .await;
    let row = wait_row(&state, &id, "on main", |r| r["git"]["branch"] == "main").await;
    assert_eq!(row["git"]["worktree"], serde_json::json!(root));
    assert_eq!(row["git"]["repo"], serde_json::json!(root));
    assert_eq!(row["cwd_current"], serde_json::json!(root));

    let linked = std::fs::canonicalize(&linked).unwrap();
    post_hook(
        &state,
        &id,
        "k1",
        serde_json::json!({"hook_event_name": "PreToolUse", "cwd": linked.join("")}),
    )
    .await;
    let row = wait_row(&state, &id, "in the worktree", |r| {
        r["git"]["branch"] == "feat/y"
    })
    .await;
    assert_eq!(row["git"]["worktree"], serde_json::json!(linked));
    // Still the same repository: every worktree names the main checkout.
    assert_eq!(row["git"]["repo"], serde_json::json!(root));

    // A relative or empty cwd is ignored, never run in.
    post_hook(
        &state,
        &id,
        "k1",
        serde_json::json!({"hook_event_name": "PreToolUse", "cwd": "relative/dir"}),
    )
    .await;
    let row = session_entry(&state, &id).await;
    assert_eq!(row["git"]["branch"], "feat/y");
    state.sessions.kill(&id).ok();
}

/// Anchors: the session's start is where HEAD stood when it first landed in
/// the repository; commits made since show up as HEAD moving (newest
/// first), and a HEAD that stops descending on the same branch reads as
/// history rewritten. Nothing requires or prompts for commits.
#[tokio::test]
async fn session_git_route_lists_commits_made_during_the_session() {
    let repo = init_temp_repo("sg-anchor");
    let state = test_state();
    tokio::spawn(crate::git::track_sessions(state.clone()));
    let (ws, root) = add_workspace(&state, &repo).await;
    let id = inject_agent(&state, "k2");
    lock(&state.session_workspaces).insert(id.clone(), ws);
    post_hook(
        &state,
        &id,
        "k2",
        serde_json::json!({"hook_event_name": "SessionStart", "cwd": root}),
    )
    .await;
    wait_row(&state, &id, "on main", |r| r["git"]["branch"] == "main").await;
    let start_sha = git_in(&repo, &["rev-parse", "HEAD"]);

    let (status, body) = request(
        &state,
        Method::GET,
        &format!("/api/v1/sessions/{id}/git"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["start"]["head"], start_sha);
    assert_eq!(body["start"]["branch"], "main");
    assert_eq!(body["commits"], serde_json::json!([]));

    commit_file(&repo, "b.txt", "b\n", "add b");
    let tip = commit_file(&repo, "c.txt", "c\n", "add c");
    let (_, body) = request(
        &state,
        Method::GET,
        &format!("/api/v1/sessions/{id}/git"),
        None,
    )
    .await;
    assert_eq!(body["current"]["head"], tip);
    let commits = body["commits"].as_array().unwrap();
    assert_eq!(commits.len(), 2, "{body}");
    assert_eq!(commits[0]["subject"], "add c", "newest first");
    assert_eq!(body["rewritten"], false);

    // Rewrite under it: back to the start, then a different commit.
    git_in(&repo, &["reset", "-q", "--hard", &start_sha]);
    commit_file(&repo, "d.txt", "d\n", "other history");
    let (_, body) = request(
        &state,
        Method::GET,
        &format!("/api/v1/sessions/{id}/git"),
        None,
    )
    .await;
    assert_eq!(body["rewritten"], false, "start still descends: {body}");
    // Now rewrite the START itself away (amend the root's successor chain).
    git_in(&repo, &["reset", "-q", "--hard", &start_sha]);
    std::fs::write(repo.join("a.txt"), "amended\n").unwrap();
    git_in(&repo, &["commit", "-qa", "--amend", "-m", "amended root"]);
    let (_, body) = request(
        &state,
        Method::GET,
        &format!("/api/v1/sessions/{id}/git"),
        None,
    )
    .await;
    assert_eq!(body["rewritten"], true, "{body}");

    // An ended session keeps its anchors readable.
    state.sessions.kill(&id).ok();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let (status, body) = request(
            &state,
            Method::GET,
            &format!("/api/v1/sessions/{id}/git"),
            None,
        )
        .await;
        if status == StatusCode::OK && body["live"] == false && !body["current"].is_null() {
            assert_eq!(body["start"]["head"], start_sha);
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "end never recorded: {body}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    // An unknown session is a 404, not an empty answer.
    let (status, _) = request(&state, Method::GET, "/api/v1/sessions/nope/git", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A command finishing in a terminal refreshes git: the terminal's folder is
/// marked dirty, so the workspace's epoch moves without waiting for the
/// 12 s backstop (a `git commit` typed there shows at once).
#[tokio::test]
async fn a_finished_terminal_command_bumps_the_git_epoch() {
    let state = test_state();
    let id = spawn_integrated_bash(&state, "sg-term").await;
    let cwd = state.sessions.get(&id).unwrap().cwd;
    let (ws, _) = add_workspace(&state, &cwd).await;
    lock(&state.session_workspaces).insert(id.clone(), ws.clone());
    crate::naming::spawn_shell_watch(state.clone(), id.clone());
    let epoch = |state: &Arc<AppState>| state.git.epochs_snapshot().get(&ws).copied().unwrap_or(0);
    // Let the watcher take its first look before the command runs.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let before = epoch(&state);
    let att = state.sessions.attach(&id).expect("attach");
    att.input
        .send(bytes::Bytes::from("true\n"))
        .await
        .expect("type a command");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    while epoch(&state) <= before {
        assert!(
            tokio::time::Instant::now() < deadline,
            "a finished command never marked the folder dirty"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    state.sessions.kill(&id).ok();
}

/// "+ branch" with a base: the new branch starts where the base names, and
/// `.worktreeinclude` carries ignored files (only matching ones) across.
/// A base git can't resolve is refused before any worktree is made.
#[tokio::test]
async fn worktree_create_honors_base_and_worktreeinclude() {
    let repo = init_temp_repo("sg-include");
    let first = git_in(&repo, &["rev-parse", "HEAD"]);
    std::fs::write(repo.join(".gitignore"), ".env\nsecrets/\n*.log\n").unwrap();
    std::fs::write(repo.join(".worktreeinclude"), ".env\nsecrets/\n").unwrap();
    git_in(&repo, &["add", ".gitignore", ".worktreeinclude"]);
    git_in(&repo, &["commit", "-qm", "ignore + include"]);
    std::fs::write(repo.join(".env"), "TOKEN=x\n").unwrap();
    std::fs::create_dir_all(repo.join("secrets")).unwrap();
    std::fs::write(repo.join("secrets/key.pem"), "k\n").unwrap();
    std::fs::write(repo.join("build.log"), "noise\n").unwrap();
    let state = test_state();
    let (ws, _) = add_workspace(&state, &repo).await;

    let (status, err) = request(
        &state,
        Method::POST,
        "/api/v1/git/worktrees",
        Some(serde_json::json!({"workspace_id": ws, "branch": "feat/b", "base": "no-such-branch"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
    let (status, err) = request(
        &state,
        Method::POST,
        "/api/v1/git/worktrees",
        Some(serde_json::json!({"workspace_id": ws, "branch": "feat/b", "base": "--orphan"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");

    // Based on the FIRST commit (HEAD~1): the include file isn't there, but
    // the source checkout's is what counts.
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/git/worktrees",
        Some(serde_json::json!({"workspace_id": ws, "branch": "feat/b", "base": "HEAD~1"})),
    )
    .await;
    assert!(status.is_success(), "create failed: {body}");
    let wt = PathBuf::from(body["worktree"]["path"].as_str().unwrap());
    assert_eq!(git_in(&wt, &["rev-parse", "HEAD"]), first);
    assert_eq!(body["included"]["copied"], 2, "{body}");
    assert_eq!(
        std::fs::read_to_string(wt.join(".env")).unwrap(),
        "TOKEN=x\n"
    );
    assert!(wt.join("secrets/key.pem").exists());
    assert!(!wt.join("build.log").exists(), "ignored but not included");
}

/// Remove stays fenced, now also against commits that exist nowhere else;
/// a managed worktree whose branch the main checkout contains is `merged`.
#[tokio::test]
async fn worktree_remove_refuses_unshared_commits_and_lists_merged() {
    let repo = init_temp_repo("sg-merged");
    let state = test_state();
    let (ws, _) = add_workspace(&state, &repo).await;
    let mk = |branch: &'static str| {
        let state = state.clone();
        let ws = ws.clone();
        async move {
            let (status, body) = request(
                &state,
                Method::POST,
                "/api/v1/git/worktrees",
                Some(serde_json::json!({"workspace_id": ws, "branch": branch})),
            )
            .await;
            assert!(status.is_success(), "create failed: {body}");
            PathBuf::from(body["worktree"]["path"].as_str().unwrap())
        }
    };
    let fresh = mk("feat/fresh").await;
    let busy = mk("feat/busy").await;
    commit_file(&busy, "w.txt", "work\n", "unshared work");

    let (_, list) = request(
        &state,
        Method::GET,
        &format!("/api/v1/git/worktrees?workspace_id={ws}"),
        None,
    )
    .await;
    let row = |branch: &str| {
        list["worktrees"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["branch"] == branch)
            .cloned()
            .unwrap()
    };
    assert_eq!(row("feat/fresh")["merged"], true);
    assert_eq!(row("feat/busy")["merged"], false);
    assert!(
        row("main")["merged"].is_null(),
        "only managed rows are asked"
    );

    let (status, err) = request(
        &state,
        Method::DELETE,
        "/api/v1/git/worktrees",
        Some(serde_json::json!({"workspace_id": ws, "path": busy})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");
    assert!(err["error"]
        .as_str()
        .unwrap()
        .contains("neither pushed nor merged"));
    assert!(busy.exists());

    let (status, err) = request(
        &state,
        Method::DELETE,
        "/api/v1/git/worktrees",
        Some(serde_json::json!({"workspace_id": ws, "path": fresh})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{err}");
}

/// The read-only local branch list: most recently committed first, with
/// the current one marked.
#[tokio::test]
async fn branches_route_lists_local_branches() {
    let repo = init_temp_repo("sg-branches");
    git_in(&repo, &["branch", "old"]);
    git_in(&repo, &["checkout", "-q", "-b", "newer"]);
    commit_file(&repo, "n.txt", "n\n", "newer work");
    git_in(&repo, &["checkout", "-q", "main"]);
    let state = test_state();
    let (ws, _) = add_workspace(&state, &repo).await;
    let (status, body) = request(
        &state,
        Method::GET,
        &format!("/api/v1/git/branches?workspace_id={ws}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = body["branches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 3, "{body}");
    assert!(names.contains(&"old") && names.contains(&"main"));
    let main = body["branches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == "main")
        .unwrap();
    assert_eq!(main["current"], true);
    assert!(main["time"].as_i64().unwrap() > 0);
}

/// `CreateSession.cwd`: a folder inside one of the workspace's worktrees is
/// where the session starts; anything outside is refused.
#[tokio::test]
async fn create_session_accepts_a_cwd_inside_a_worktree() {
    let repo = init_temp_repo("sg-cwd");
    let state = test_state();
    let (ws, _) = add_workspace(&state, &repo).await;
    let (_, body) = request(
        &state,
        Method::POST,
        "/api/v1/git/worktrees",
        Some(serde_json::json!({"workspace_id": ws, "branch": "feat/c"})),
    )
    .await;
    let wt = PathBuf::from(body["worktree"]["path"].as_str().unwrap());

    let outside = test_dir("sg-cwd-outside");
    let (status, err) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": ws, "kind": "shell", "cwd": outside})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
    let (status, err) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": ws, "kind": "shell", "cwd": "relative"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");

    let (status, session) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": ws, "kind": "shell", "cwd": wt})),
    )
    .await;
    assert!(status.is_success(), "spawn failed: {session}");
    assert_eq!(session["cwd"], serde_json::json!(wt));
    assert_eq!(session["workspace_id"], ws, "it stays in this workspace");
    state.sessions.kill(session["id"].as_str().unwrap()).ok();
}

/// While an agent runs inside a managed worktree it is locked with a
/// chimaera reason naming the session; when the agent ends, the lock goes.
/// Another tool's lock is never touched.
#[tokio::test]
async fn managed_worktrees_are_locked_while_an_agent_runs_inside() {
    let repo = init_temp_repo("sg-lock");
    let state = test_state();
    tokio::spawn(crate::git::track_sessions(state.clone()));
    let (ws, _) = add_workspace(&state, &repo).await;
    let (_, body) = request(
        &state,
        Method::POST,
        "/api/v1/git/worktrees",
        Some(serde_json::json!({"workspace_id": ws, "branch": "feat/l"})),
    )
    .await;
    let wt = PathBuf::from(body["worktree"]["path"].as_str().unwrap());
    let lock_path = || {
        // git names the admin dir after the worktree's basename.
        let name = wt.file_name().unwrap().to_string_lossy().into_owned();
        repo.join(".git/worktrees").join(name).join("locked")
    };

    let id = inject_agent(&state, "k3");
    lock(&state.session_workspaces).insert(id.clone(), ws.clone());
    post_hook(
        &state,
        &id,
        "k3",
        serde_json::json!({"hook_event_name": "SessionStart", "cwd": wt}),
    )
    .await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Ok(reason) = std::fs::read_to_string(lock_path()) {
            assert_eq!(reason.trim(), format!("chimaera: {id}"));
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "never locked");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    state.sessions.kill(&id).ok();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    while lock_path().exists() {
        assert!(tokio::time::Instant::now() < deadline, "never unlocked");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // Another tool's lock: an agent arriving leaves it exactly as it was.
    git_in(
        &repo,
        &[
            "worktree",
            "lock",
            "--reason",
            "someone else",
            &wt.to_string_lossy(),
        ],
    );
    let id2 = inject_agent(&state, "k4");
    lock(&state.session_workspaces).insert(id2.clone(), ws.clone());
    post_hook(
        &state,
        &id2,
        "k4",
        serde_json::json!({"hook_event_name": "SessionStart", "cwd": wt}),
    )
    .await;
    wait_row(&state, &id2, "tracked in the worktree", |r| {
        r["git"]["branch"] == "feat/l"
    })
    .await;
    state.sessions.kill(&id2).ok();
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    assert_eq!(
        std::fs::read_to_string(lock_path()).unwrap().trim(),
        "someone else"
    );
}
