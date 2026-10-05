//! The free contract: Pro's own state files never block a person Pro never
//! enrolled. Absent, unreadable or corrupt Pro state means ordinary work for
//! a never-enrolled project; a project Pro has a record of keeps its fence.
use super::support::*;
use crate::*;

/// A data dir holding `files` (relative path, bytes) before the daemon starts.
fn data_with(files: &[(&str, &[u8])]) -> PathBuf {
    let data = test_dir("free-contract-data");
    for (path, bytes) in files {
        let path = data.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    data
}

/// Registers a fresh folder and starts a plain shell in it.
async fn shell_starts(state: &Arc<AppState>) -> bool {
    let root = std::fs::canonicalize(test_dir("free-contract-project")).unwrap();
    let (status, workspace) = request(
        state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{workspace}");
    let id = workspace["id"].as_str().unwrap();
    let writable = crate::pro::may_execute(state, id);
    let (status, session) = request(
        state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": id})),
    )
    .await;
    if status == StatusCode::OK {
        let sid = session["id"].as_str().unwrap();
        request(
            state,
            Method::DELETE,
            &format!("/api/v1/sessions/{sid}"),
            None,
        )
        .await;
    }
    assert_eq!(writable, status == StatusCode::OK, "{status} {session}");
    writable
}

const GARBAGE: &[u8] = b"\x00 not json {";
/// `state.json` naming one enrolled project (not the one the tests open).
const ENROLLED_ELSEWHERE: &[u8] =
    br#"{"ownership":{"w-enrolledelsewhere":{"state":"local","epoch":1}},"preferences":{}}"#;

#[tokio::test]
async fn corrupt_pro_state_files_never_block_a_never_enrolled_project() {
    for file in [
        "pro/workspace-authority.json",
        "bundles/pending.json",
        "pro/copy-authority.json",
    ] {
        let state = test_state_with_data_dir(0, data_with(&[(file, GARBAGE)]));
        assert!(shell_starts(&state).await, "{file} blocked a free user");
    }
}

#[tokio::test]
async fn an_unreadable_pro_state_file_still_fences_once_pro_holds_projects() {
    // Pro has a record of a project here: an unreadable import-recovery
    // record could name anything, so it keeps fencing (the real protection).
    let state = test_state_with_data_dir(
        0,
        data_with(&[
            ("pro/state.json", ENROLLED_ELSEWHERE),
            ("bundles/pending.json", GARBAGE),
        ]),
    );
    assert!(!shell_starts(&state).await);
    // Pro's own record unreadable too: which projects it enrolled is
    // unknown, so the unreadable authority record keeps fencing every one.
    let state = test_state_with_data_dir(
        0,
        data_with(&[
            ("pro/state.json", GARBAGE),
            ("pro/workspace-authority.json", GARBAGE),
        ]),
    );
    assert!(!shell_starts(&state).await);
}

/// No roster poll, no input bookkeeping and no Pro-only row data on a daemon
/// without an active plan: typing never changes the shared sessions frame.
#[tokio::test]
async fn typing_on_a_free_daemon_changes_no_session_row() {
    let state = test_state();
    let root = std::fs::canonicalize(test_dir("free-contract-typing")).unwrap();
    let (_, workspace) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    let (status, session) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": workspace["id"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{session}");
    let sid = session["id"].as_str().unwrap().to_owned();
    let generation = state.changes.generation();
    crate::activity::record(&state, &sid);
    crate::activity::touch(&state);
    assert_eq!(
        state.changes.generation(),
        generation,
        "no broadcast per keystroke"
    );
    let rows = crate::session_view::sessions_json(&state);
    let row = rows.iter().find(|row| row["id"] == sid.as_str()).unwrap();
    assert_eq!(row["last_input_ms"], serde_json::Value::Null);
    assert!(crate::activity::last_change(&state).is_none());
    assert!(!state.session_proxy.polling(), "no roster timer");
    request(
        &state,
        Method::DELETE,
        &format!("/api/v1/sessions/{sid}"),
        None,
    )
    .await;
}
