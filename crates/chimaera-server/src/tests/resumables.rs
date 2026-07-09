use super::support::*;
use crate::*;

#[tokio::test]
async fn claude_resumables_titles_order_and_noise() {
    let store = test_dir("claude-store");
    let state = test_state_with_claude_store(store.clone());
    let root = test_dir("resume-root");
    let (_, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    let workspace_id = ws["id"].as_str().unwrap().to_string();
    // The store dir is keyed by the *canonical* workspace root, encoded
    // with claude's every-non-alphanumeric-to-dash rule.
    let project_dir = store.join(launcher::encode_cwd(std::path::Path::new(
        ws["root"].as_str().unwrap(),
    )));
    std::fs::create_dir_all(&project_dir).unwrap();

    // Oldest: prompt-only -> truncated first prompt is the title.
    write_transcript(
        &project_dir,
        "aaaa",
        concat!(
            r#"{"type":"user","message":{"role":"user","content":"please refactor the entire qc pipeline so the reports land in results/qc and nothing downstream breaks"}}"#,
            "\n",
        ),
        300,
    );
    // Middle: ai-title outranks the prompt; user+assistant lines count.
    write_transcript(
        &project_dir,
        "bbbb",
        concat!(
            r#"{"type":"user","message":{"role":"user","content":"fix the STAR index"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"on it"}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#,
            "\n",
            r#"{"type":"ai-title","aiTitle":"Fix the STAR index","sessionId":"bbbb"}"#,
            "\n",
        ),
        200,
    );
    // Newest: a rename (custom-title) wins over everything.
    write_transcript(
        &project_dir,
        "cccc",
        concat!(
            r#"{"type":"user","message":{"role":"user","content":"align the fastqs"}}"#,
            "\n",
            r#"{"type":"ai-title","aiTitle":"Align fastqs","sessionId":"cccc"}"#,
            "\n",
            r#"{"type":"custom-title","customTitle":"Pinned by hand","sessionId":"cccc"}"#,
            "\n",
        ),
        100,
    );
    // Noise: a titleless boot transcript (skipped), a non-jsonl file
    // and a subdirectory (ignored).
    write_transcript(
        &project_dir,
        "dddd",
        "{\"type\":\"mode\",\"mode\":\"normal\"}\n",
        50,
    );
    std::fs::write(project_dir.join("notes.txt"), "not a transcript").unwrap();
    std::fs::create_dir_all(project_dir.join("memory")).unwrap();

    let uri = format!("/api/v1/agents/claude/sessions?workspace_id={workspace_id}");
    let (status, list) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    let list = list.as_array().unwrap();
    let ids: Vec<&str> = list.iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["cccc", "bbbb", "aaaa"], "newest first, noise skipped");

    assert_eq!(list[0]["title"], "Pinned by hand");
    assert_eq!(list[0]["approx_messages"], 1);
    assert_eq!(list[1]["title"], "Fix the STAR index");
    assert_eq!(list[1]["approx_messages"], 3);
    let truncated = list[2]["title"].as_str().unwrap();
    assert!(
        truncated.starts_with("please refactor the entire qc pipeline"),
        "{truncated}"
    );
    assert!(truncated.ends_with('…'), "{truncated}");
    assert!(truncated.chars().count() <= 61, "{truncated}");
    assert_eq!(list[2]["approx_messages"], 1);

    // mtimes are unix seconds matching the backdated fixtures.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    for (entry, secs_ago) in [(&list[0], 100u64), (&list[1], 200), (&list[2], 300)] {
        let mtime = entry["mtime"].as_u64().unwrap();
        let expect = now - secs_ago;
        assert!(
            mtime.abs_diff(expect) < 30,
            "mtime {mtime} not within 30s of {expect}"
        );
    }

    // A workspace never used with claude: empty list, not an error.
    let bare_root = test_dir("resume-bare");
    let (_, bare) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": bare_root.to_string_lossy()})),
    )
    .await;
    let uri = format!(
        "/api/v1/agents/claude/sessions?workspace_id={}",
        bare["id"].as_str().unwrap()
    );
    let (status, list) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list, serde_json::json!([]));

    // Unknown workspace: 404.
    let (status, _) = request(
        &state,
        Method::GET,
        "/api/v1/agents/claude/sessions?workspace_id=w-00000000",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn claude_resumables_cap_at_twenty_newest() {
    let store = test_dir("claude-store-cap");
    let state = test_state_with_claude_store(store.clone());
    let root = test_dir("resume-cap-root");
    let (_, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    let project_dir = store.join(launcher::encode_cwd(std::path::Path::new(
        ws["root"].as_str().unwrap(),
    )));
    std::fs::create_dir_all(&project_dir).unwrap();

    for i in 0..25u64 {
        write_transcript(
                &project_dir,
                &format!("f{i:02}"),
                &format!(
                    "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"prompt {i}\"}}}}\n"
                ),
                (i + 1) * 60, // f00 newest .. f24 oldest
            );
    }

    let uri = format!(
        "/api/v1/agents/claude/sessions?workspace_id={}",
        ws["id"].as_str().unwrap()
    );
    let (status, list) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 20, "capped at 20");
    let ids: Vec<&str> = list.iter().map(|s| s["id"].as_str().unwrap()).collect();
    let expect: Vec<String> = (0..20).map(|i| format!("f{i:02}")).collect();
    assert_eq!(ids, expect, "the 20 newest, newest first");
}
