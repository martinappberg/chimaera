//! Git G1 (docs/design/git-and-session-history-plan.md): sessions know their
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

    // A hook fired inside a subagent carries the subagent's folder (its own
    // worktree when isolated): the session has not moved.
    for event in ["SubagentStart", "PreToolUse", "SubagentStop"] {
        post_hook(
            &state,
            &id,
            "k1",
            serde_json::json!({
                "hook_event_name": event,
                "cwd": root,
                "agent_id": "a55d52603606673c2",
                "agent_type": "general-purpose",
            }),
        )
        .await;
    }
    let row = session_entry(&state, &id).await;
    assert_eq!(row["git"]["branch"], "feat/y");
    assert_eq!(row["cwd_current"], serde_json::json!(linked.join("")));

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

    // A commit that arrives with an old commit time — what a `git pull`
    // brings in from other people — moves HEAD but is not the session's.
    std::fs::write(repo.join("pulled.txt"), "theirs\n").unwrap();
    git_in(&repo, &["add", "pulled.txt"]);
    let pulled = std::process::Command::new("git")
        .current_dir(&repo)
        .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")
        .args([
            "-c",
            "user.name=Someone Else",
            "-c",
            "user.email=else@example.org",
            "commit",
            "-q",
            "-m",
            "their older work",
        ])
        .status()
        .unwrap();
    assert!(pulled.success());
    let (_, body) = request(
        &state,
        Method::GET,
        &format!("/api/v1/sessions/{id}/git"),
        None,
    )
    .await;
    let subjects: Vec<&str> = body["commits"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["subject"].as_str())
        .collect();
    assert_eq!(subjects, vec!["add c", "add b"], "{body}");

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
    assert_eq!(body["included"]["names"].as_array().unwrap().len(), 2);
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
    // A brand-new branch has nothing of its own yet: not "merged".
    assert_eq!(row("feat/fresh")["merged"], false);
    assert_eq!(row("feat/busy")["merged"], false);
    assert_eq!(
        row("feat/busy")["ahead_of_main"],
        1,
        "one commit main lacks"
    );
    assert_eq!(row("feat/fresh")["ahead_of_main"], 0);
    assert!(row("main")["ahead_of_main"].is_null());
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

    // Once main holds the branch's own commits, it reads "merged".
    git_in(&repo, &["merge", "-q", "--ff-only", "feat/busy"]);
    let (_, list) = request(
        &state,
        Method::GET,
        &format!("/api/v1/git/worktrees?workspace_id={ws}"),
        None,
    )
    .await;
    let busy_row = list["worktrees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["branch"] == "feat/busy")
        .cloned()
        .unwrap();
    assert_eq!(busy_row["merged"], true, "{list}");
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
/// A stand-in agent binary that ignores its argv and stays alive for a
/// while, so its session stays listed while the tracker resolves it.
fn lingering_agent(label: &str) -> PathBuf {
    let dir = test_dir(label);
    let bin = dir.join("fake-agent");
    std::fs::write(&bin, "#!/bin/sh\nsleep 30\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// The session's `managed_worktree` once the tracker has resolved it.
async fn managed_worktree_eventually(state: &Arc<AppState>, id: &str) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let entry = session_entry(state, id).await;
        if !entry["managed_worktree"].is_null() {
            return entry["managed_worktree"].clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "managed_worktree never resolved: {entry}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// `worktree: true` starts a new agent in a managed worktree of its own on a
/// fresh `chimaera/<agent>-<hex>` branch, and the row says so; the
/// per-workspace choice round-trips through PATCH. Refused for shells, with
/// `cwd`/`resume`, and outside a repository.
#[tokio::test]
async fn a_new_agent_can_start_in_its_own_managed_worktree() {
    let repo = init_temp_repo("sg-own-wt");
    let state = test_state();
    tokio::spawn(crate::git::track_sessions(state.clone()));
    preset_agent(
        &state,
        crate::agents::AgentKind::Claude,
        Ok(lingering_agent("sg-own-wt-bin")),
        None,
    );
    let (ws, _) = add_workspace(&state, &repo).await;

    let (status, row) = request(
        &state,
        Method::PATCH,
        &format!("/api/v1/workspaces/{ws}"),
        Some(serde_json::json!({"new_in_worktree": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{row}");
    assert_eq!(row["new_in_worktree"], true);
    assert!(lock(&state.workspaces).get(&ws).unwrap().new_in_worktree);

    for bad in [
        serde_json::json!({"workspace_id": ws, "kind": "shell", "worktree": true}),
        serde_json::json!({"workspace_id": ws, "kind": "agent", "worktree": true, "resume": "x"}),
        serde_json::json!({"workspace_id": ws, "kind": "agent", "worktree": true, "cwd": repo}),
    ] {
        let (status, err) = request(&state, Method::POST, "/api/v1/sessions", Some(bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
    }

    let (status, session) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": ws, "kind": "agent", "worktree": true})),
    )
    .await;
    assert!(status.is_success(), "spawn failed: {session}");
    let id = session["id"].as_str().unwrap().to_string();
    let cwd = PathBuf::from(session["cwd"].as_str().unwrap());
    let managed = std::fs::canonicalize(&state.worktrees_root).unwrap();
    assert!(cwd.starts_with(&managed), "{cwd:?} not under {managed:?}");
    let branch = git_in(&cwd, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert!(branch.starts_with("chimaera/claude-"), "{branch}");
    assert!(cwd.ends_with(&branch), "named after its branch: {cwd:?}");
    assert_eq!(session["workspace_id"], ws, "it stays in this workspace");
    assert_eq!(
        managed_worktree_eventually(&state, &id).await,
        serde_json::json!(cwd)
    );
    state.sessions.kill(&id).ok();

    // Not a repository: refused, nothing created.
    let plain = test_dir("sg-own-wt-plain");
    let (plain_ws, _) = add_workspace(&state, &plain).await;
    let (status, err) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": plain_ws, "kind": "agent", "worktree": true})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
}
/// A spawn that fails after its worktree was made (here: a model id the
/// argv check refuses) leaves neither the worktree nor its branch behind.
#[tokio::test]
async fn a_failed_spawn_discards_its_new_worktree_and_branch() {
    let repo = init_temp_repo("sg-own-wt-fail");
    let state = test_state();
    preset_agent(
        &state,
        crate::agents::AgentKind::Claude,
        Ok(PathBuf::from("/bin/cat")),
        None,
    );
    let (ws, _) = add_workspace(&state, &repo).await;
    for ui in ["term", "chat"] {
        let (status, err) = request(
            &state,
            Method::POST,
            "/api/v1/sessions",
            Some(serde_json::json!({
                "workspace_id": ws, "kind": "agent", "worktree": true,
                "ui": ui, "model": "--not-a-model",
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{ui}: {err}");
        let list = git_in(&repo, &["worktree", "list", "--porcelain"]);
        assert_eq!(
            list.lines().filter(|l| l.starts_with("worktree ")).count(),
            1,
            "{ui}: only the main checkout remains: {list}"
        );
        let branches = git_in(&repo, &["branch", "--list", "chimaera/*"]);
        assert!(branches.is_empty(), "{ui}: no branch left: {branches}");
    }
}

/// A session started in a SUBFOLDER of a managed worktree reports the
/// worktree's top as `managed_worktree` (what the Remove offer targets),
/// never the subfolder. And a managed worktree opened as its own workspace
/// stops being listed once its folder is gone.
#[tokio::test]
async fn managed_worktree_is_the_worktree_top_and_dead_registrations_go() {
    let repo = init_temp_repo("sg-own-wt-sub");
    let state = test_state();
    tokio::spawn(crate::git::track_sessions(state.clone()));
    preset_agent(
        &state,
        crate::agents::AgentKind::Claude,
        Ok(lingering_agent("sg-own-wt-sub-bin")),
        None,
    );
    let (ws, _) = add_workspace(&state, &repo).await;
    let (_, body) = request(
        &state,
        Method::POST,
        "/api/v1/git/worktrees",
        Some(serde_json::json!({"workspace_id": ws, "branch": "feat/sub"})),
    )
    .await;
    let wt = PathBuf::from(body["worktree"]["path"].as_str().unwrap());
    let sub = wt.join("deep").join("er");
    std::fs::create_dir_all(&sub).unwrap();
    let (status, session) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": ws, "kind": "agent", "cwd": sub})),
    )
    .await;
    assert!(status.is_success(), "spawn failed: {session}");
    let id = session["id"].as_str().unwrap().to_string();
    assert_eq!(
        managed_worktree_eventually(&state, &id).await,
        serde_json::json!(wt),
        "the worktree's top, not the subfolder it started in"
    );
    state.sessions.kill(&id).ok();

    // Opened as its own window, then removed outside chimaera.
    let (_, opened) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": wt})),
    )
    .await;
    let opened_id = opened["id"].as_str().unwrap().to_string();
    // Forcefully, as an outside tool might: past the lock the just-killed
    // session may still hold until the tracker's next pass.
    git_in(
        &repo,
        &["worktree", "remove", "-f", "-f", wt.to_str().unwrap()],
    );
    let (_, listed) = request(&state, Method::GET, "/api/v1/workspaces", None).await;
    assert!(
        !listed
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["id"] == opened_id.as_str()),
        "a gone managed worktree is unregistered: {listed}"
    );
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["id"] == ws.as_str()),
        "the repository's own workspace stays"
    );
}

/// A `git` that runs `before` ahead of every `worktree add` and `after`
/// behind it (the real git's status is dropped when `after` exits), and is
/// the real git otherwise — set as `git.path`.
fn git_around_add(state: &Arc<AppState>, label: &str, before: &str, after: &str) {
    use std::os::unix::fs::PermissionsExt;
    let real = std::process::Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("git must be installed");
    let real = String::from_utf8_lossy(&real.stdout).trim().to_string();
    let bin = test_dir(label).join("git");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\ncase \" $* \" in\n*\" worktree add \"*) {before}; '{real}' \"$@\"; {after} ;;\n*) exec '{real}' \"$@\" ;;\nesac\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut map = serde_json::Map::new();
    map.insert("git.path".into(), bin.to_string_lossy().into());
    lock(&state.settings).put(map).unwrap();
}

/// A fake `claude` chat binary that writes the folder it started in to
/// `marker`, then answers the handshake and stays.
fn claude_noting_cwd(label: &str, marker: &std::path::Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = test_dir(label).join("claude");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\npwd -P > '{}'\n\
             printf '%s\\n' '{{\"type\":\"control_response\",\"response\":{{\"subtype\":\"success\",\"request_id\":\"init\",\"response\":{{\"commands\":[]}}}}}}'\n\
             cat >/dev/null\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A new chat in its own worktree opens before its checkout (which grows
/// with the repository), refuses a view switch while it runs, and its agent
/// starts inside the finished worktree, at the path the chat reported. The
/// session tracker, which first saw that folder before it was a repository,
/// still finds the worktree once the checkout lands.
#[tokio::test]
async fn a_chat_in_its_own_worktree_opens_before_its_checkout() {
    let repo = init_temp_repo("sg-wt-early");
    let state = test_state();
    tokio::spawn(crate::git::track_sessions(state.clone()));
    let marker = test_dir("sg-wt-early-marker").join("cwd");
    preset_agent(
        &state,
        crate::agents::AgentKind::Claude,
        Ok(claude_noting_cwd("sg-wt-early-bin", &marker)),
        None,
    );
    git_around_add(&state, "sg-wt-early-git", "sleep 1.5", "exit $?");
    let (ws, _) = add_workspace(&state, &repo).await;

    let (status, session) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({
            "workspace_id": ws, "kind": "agent", "worktree": true, "ui": "chat",
        })),
    )
    .await;
    assert!(status.is_success(), "spawn failed: {session}");
    let id = session["id"].as_str().unwrap().to_string();
    let cwd = PathBuf::from(session["cwd"].as_str().unwrap());
    assert!(
        !cwd.join(".git").exists(),
        "answered before the checkout ran: {cwd:?}"
    );
    assert!(lock(&state.worktree_setups).contains_key(&id));
    let (status, err) = request(
        &state,
        Method::POST,
        &format!("/api/v1/sessions/{id}/view"),
        Some(serde_json::json!({"ui": "term"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    while !marker.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the agent never started"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let ran_in = std::fs::read_to_string(&marker).unwrap();
    assert_eq!(
        std::path::Path::new(ran_in.trim()),
        cwd,
        "the reported path is real"
    );
    assert!(!lock(&state.worktree_setups).contains_key(&id));
    let branch = git_in(&cwd, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert!(branch.starts_with("chimaera/claude-"), "{branch}");
    assert_eq!(
        managed_worktree_eventually(&state, &id).await,
        serde_json::json!(cwd)
    );
    state.chat.kill(&id);
}

/// A checkout git refuses after its chat opened ends the chat's startup with
/// git's words and leaves no worktree, folder or branch behind.
#[tokio::test]
async fn a_failed_checkout_ends_the_chat_and_leaves_nothing() {
    let repo = init_temp_repo("sg-wt-cut");
    let state = test_state();
    preset_agent(
        &state,
        crate::agents::AgentKind::Claude,
        Ok(write_fake_claude("sg-wt-cut-bin")),
        None,
    );
    // git refuses the checkout; the branch made for it must go again.
    git_around_add(
        &state,
        "sg-wt-cut-git",
        "echo 'fatal: cut short' >&2; exit 128",
        ":",
    );
    let (ws, _) = add_workspace(&state, &repo).await;

    let (status, session) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({
            "workspace_id": ws, "kind": "agent", "worktree": true, "ui": "chat",
        })),
    )
    .await;
    assert!(status.is_success(), "spawn failed: {session}");
    let id = session["id"].as_str().unwrap().to_string();
    let cwd = PathBuf::from(session["cwd"].as_str().unwrap());

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let said = loop {
        let replay = state
            .chat
            .attach(&id, 0)
            .map(|a| a.replay)
            .unwrap_or_default();
        let said = replay.iter().find_map(|e| match &e.ev {
            chimaera_agent::model::AgentEvent::Error { message, .. } => Some(message.clone()),
            _ => None,
        });
        if let Some(said) = said {
            break said;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no startup error: {replay:#?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert!(
        said.starts_with("Couldn't set up this chat's worktree") && said.contains("cut short"),
        "{said}"
    );
    let list = git_in(&repo, &["worktree", "list", "--porcelain"]);
    assert_eq!(
        list.lines().filter(|l| l.starts_with("worktree ")).count(),
        1,
        "only the main checkout remains: {list}"
    );
    let branches = git_in(&repo, &["branch", "--list", "chimaera/*"]);
    assert!(branches.is_empty(), "no branch left: {branches}");
    assert!(!cwd.exists(), "no folder left: {cwd:?}");
    assert!(lock(&state.worktree_setups).is_empty());
}

/// A checkout killed at its deadline (3 s in tests) leaves nothing either:
/// git can't clean up after a kill, so its registration, folder and branch
/// are removed here.
#[tokio::test]
async fn a_checkout_cut_off_at_its_deadline_leaves_nothing() {
    let repo = init_temp_repo("sg-wt-slow");
    let state = test_state();
    preset_agent(
        &state,
        crate::agents::AgentKind::Claude,
        Ok(lingering_agent("sg-wt-slow-bin")),
        None,
    );
    // The real checkout completes, then hangs past the deadline.
    git_around_add(&state, "sg-wt-slow-git", ":", "sleep 10");
    let (ws, _) = add_workspace(&state, &repo).await;
    let (status, err) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": ws, "kind": "agent", "worktree": true})),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{err}");
    assert!(
        err["error"].as_str().unwrap().contains("timed out"),
        "{err}"
    );
    let list = git_in(&repo, &["worktree", "list", "--porcelain"]);
    assert_eq!(
        list.lines().filter(|l| l.starts_with("worktree ")).count(),
        1,
        "only the main checkout remains: {list}"
    );
    let branches = git_in(&repo, &["branch", "--list", "chimaera/*"]);
    assert!(branches.is_empty(), "no branch left: {branches}");
    let managed = std::fs::canonicalize(&state.worktrees_root).unwrap();
    let leftovers: Vec<_> = walk_leaves(&managed);
    assert!(leftovers.is_empty(), "no folder left: {leftovers:?}");
}

/// A chat closed while its worktree is still being checked out never used
/// it, so the worktree and its branch go once the checkout ends.
#[tokio::test]
async fn a_chat_closed_during_its_checkout_leaves_no_worktree() {
    let repo = init_temp_repo("sg-wt-closed");
    let state = test_state();
    preset_agent(
        &state,
        crate::agents::AgentKind::Claude,
        Ok(write_fake_claude("sg-wt-closed-bin")),
        None,
    );
    git_around_add(&state, "sg-wt-closed-git", "sleep 2", "exit $?");
    let (ws, _) = add_workspace(&state, &repo).await;
    let (status, session) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({
            "workspace_id": ws, "kind": "agent", "worktree": true, "ui": "chat",
        })),
    )
    .await;
    assert!(status.is_success(), "spawn failed: {session}");
    let id = session["id"].as_str().unwrap().to_string();
    let (status, body) = request(
        &state,
        Method::DELETE,
        &format!("/api/v1/sessions/{id}"),
        None,
    )
    .await;
    assert!(status.is_success(), "close failed: {body}");

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let list = git_in(&repo, &["worktree", "list", "--porcelain"]);
        let branches = git_in(&repo, &["branch", "--list", "chimaera/*"]);
        let worktrees = list.lines().filter(|l| l.starts_with("worktree ")).count();
        if worktrees == 1 && branches.is_empty() && lock(&state.worktree_setups).is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the unused worktree stayed: {list} {branches}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// Every leaf (a file, or a folder with nothing in it but folders) under
/// `root`, skipping the empty `<repo>/chimaera/` parents worktree planning
/// makes.
fn walk_leaves(root: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}
