use super::support::*;
use crate::*;

fn line(body: &str) -> String {
    format!(r#"{{"isSidechain":true,"agentId":"a1",{body}}}"#) + "\n"
}

/// A claude session whose hook reported `<dir>/native.jsonl`, with one
/// subagent transcript beside it. Returns the subagent file.
fn plant_subagent(state: &Arc<AppState>, sid: &str, ws: &str, dir: &std::path::Path) -> PathBuf {
    let transcript = dir.join("native.jsonl");
    plant_agent_record(
        state,
        sid,
        ws,
        agents::AgentKind::Claude,
        None,
        Some(transcript.to_str().unwrap()),
    );
    let subagents = dir.join("native").join("subagents");
    std::fs::create_dir_all(&subagents).unwrap();
    let file = subagents.join("agent-a1.jsonl");
    std::fs::write(
        &file,
        [
            line(r#""type":"user","message":{"role":"user","content":"read notes.txt"}"#),
            line(
                r#""type":"assistant","message":{"id":"m1","model":"claude-opus-5-5","content":[{"type":"tool_use","id":"tu_read","name":"Read","input":{"file_path":"/tmp/notes.txt"}}]}"#,
            ),
        ]
        .concat(),
    )
    .unwrap();
    file
}

#[tokio::test]
async fn subagent_transcript_is_read_incrementally() {
    let state = test_state();
    let ws = make_workspace(&state, "subagent-transcript").await;
    let dir = lock(&state.workspaces).get(&ws).unwrap().root;
    let sid = "s-sub";
    let file = plant_subagent(&state, sid, &ws, &dir.join("transcripts"));
    let url = format!("/api/v1/sessions/{sid}/subagents/a1/transcript");

    // First read of a working subagent: everything so far, the turn open.
    let (status, first) = request(&state, Method::GET, &format!("{url}?live=true"), None).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["agent"], "claude");
    assert_eq!(first["model"], "claude-opus-5-5");
    assert_eq!(first["from"], 0);
    let events = first["events"].as_array().unwrap();
    assert_eq!(events[0]["type"], "user_message");
    assert_eq!(events.last().unwrap()["type"], "tool_call");
    assert_eq!(events.last().unwrap()["status"], "in_progress");
    let (epoch, stamp, held) = (
        first["epoch"].as_str().unwrap(),
        first["stamp"].as_str().unwrap(),
        events.len(),
    );

    // Nothing appended: nothing to send.
    let again = format!("{url}?live=true&epoch={epoch}&after={held}&stamp={stamp}");
    let (status, same) = request(&state, Method::GET, &again, None).await;
    assert_eq!(status, StatusCode::OK, "{same}");
    assert_eq!(same["from"], held);
    assert!(same["events"].as_array().unwrap().is_empty());

    // The subagent moved on: only the new events travel.
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .unwrap();
    f.write_all(
        line(r#""type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu_read","content":"hello"}]}"#)
            .as_bytes(),
    )
    .unwrap();
    let (_, more) = request(&state, Method::GET, &again, None).await;
    assert_eq!(more["from"], held);
    let added = more["events"].as_array().unwrap();
    assert_eq!(added.len(), 1);
    assert_eq!(added[0]["type"], "tool_call_update");

    // Finished: the same file, plus the closing events.
    let done = format!("{url}?epoch={epoch}&after={}", held + 1);
    let (_, closed) = request(&state, Method::GET, &done, None).await;
    let tail = closed["events"].as_array().unwrap();
    assert_eq!(tail.last().unwrap()["type"], "turn_completed");

    // A reader from another window starts over.
    let (_, fresh) = request(
        &state,
        Method::GET,
        &format!("{url}?epoch=99&after=7"),
        None,
    )
    .await;
    assert_eq!(fresh["from"], 0);
    assert_eq!(fresh["events"][0]["type"], "user_message");
}

#[tokio::test]
async fn subagent_transcript_refuses_what_it_cannot_name() {
    let state = test_state();
    let ws = make_workspace(&state, "subagent-refusals").await;
    let dir = lock(&state.workspaces).get(&ws).unwrap().root;
    plant_subagent(&state, "s-sub2", &ws, &dir.join("transcripts"));

    // The id is put in a file name: nothing that could leave the folder.
    for bad in ["..%2F..%2Fnative", "a1.jsonl", "a%20b"] {
        let (status, _) = request(
            &state,
            Method::GET,
            &format!("/api/v1/sessions/s-sub2/subagents/{bad}/transcript"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (status, out) = request(
        &state,
        Method::GET,
        "/api/v1/sessions/s-sub2/subagents/nobody/transcript",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{out}");
    let (status, _) = request(
        &state,
        Method::GET,
        "/api/v1/sessions/s-ghost/subagents/a1/transcript",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
