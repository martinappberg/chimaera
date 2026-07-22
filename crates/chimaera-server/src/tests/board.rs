//! Board route tests: render → ticket → raw fetch, describe, auth coverage.

use axum::http::{Method, StatusCode};

use super::support::{request, request_bytes, test_dir, test_state};

const BOARD: &str = r#"{
  "format": "chimaera.board",
  "formatVersion": 1,
  "title": "Route test",
  "canvas": { "size": [400, 300] },
  "pages": [
    {
      "id": "p1",
      "objects": [
        { "id": "t", "type": "text", "role": "heading", "at": [40, 40], "size": [320, 64],
          "text": ["Hello from the daemon"] }
      ]
    }
  ]
}
"#;

fn write_board(label: &str) -> std::path::PathBuf {
    let root = test_dir(label);
    let path = root.join("demo.board.json");
    std::fs::write(&path, BOARD).unwrap();
    path
}

#[tokio::test]
async fn board_render_mints_a_ticket_that_serves_the_png() {
    let state = test_state();
    let path = write_board("board-render");

    let (status, json) = request(
        &state,
        Method::POST,
        "/api/v1/board/render",
        Some(serde_json::json!({"path": path.to_string_lossy(), "scale": 1.0})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["width"], 400);
    assert_eq!(json["height"], 300);
    assert_eq!(json["pageCount"], 1);
    assert_eq!(json["pages"][0], "p1");
    // The private filesystem path never reaches the wire.
    assert!(json.get("pngPath").is_none(), "{json}");

    let ticket = json["ticket"].as_str().unwrap();
    let uri = format!("/raw/{ticket}");
    let (status, headers, body) = request_bytes(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(
        headers.get("content-type").unwrap().to_str().unwrap(),
        "image/png"
    );
}

#[tokio::test]
async fn board_render_is_content_addressed_across_requests() {
    let state = test_state();
    let path = write_board("board-render-cache");
    let body = serde_json::json!({"path": path.to_string_lossy(), "scale": 1.0});

    let (_, first) = request(
        &state,
        Method::POST,
        "/api/v1/board/render",
        Some(body.clone()),
    )
    .await;
    let (_, second) = request(&state, Method::POST, "/api/v1/board/render", Some(body)).await;
    // Same board, same params → same cached render behind two fresh tickets,
    // and the hit reports real dimensions + diagnostics from the sidecar
    // rather than dropping them.
    assert_eq!(first["width"], second["width"]);
    assert_eq!(second["width"], 400, "a hit must not report 0×0");
    assert_eq!(
        first["diagnostics"], second["diagnostics"],
        "a hit must serve the same diagnostics the miss computed"
    );
    let renders = chimaera_board::board_dir(&chimaera_board::workspace_root(std::path::Path::new(
        path.to_str().unwrap(),
    )))
    .join("renders");
    let entries: Vec<String> = std::fs::read_dir(renders)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let pngs = entries.iter().filter(|n| n.ends_with(".png")).count();
    assert_eq!(pngs, 1, "one cached PNG, not a re-render: {entries:?}");
    assert!(
        entries.iter().any(|n| n.ends_with(".json")),
        "the diagnostics sidecar rides beside the PNG: {entries:?}"
    );
}

#[tokio::test]
async fn board_describe_returns_the_read_back() {
    let state = test_state();
    let path = write_board("board-describe");

    let (status, json) = request(
        &state,
        Method::POST,
        "/api/v1/board/describe",
        Some(serde_json::json!({"path": path.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    let text = json["text"].as_str().unwrap();
    assert!(text.contains("Route test"), "{text}");
    assert!(text.contains("t text/heading at [40, 40]"), "{text}");
}

#[tokio::test]
async fn board_routes_refuse_non_board_paths() {
    let state = test_state();
    let root = test_dir("board-not-a-board");
    let path = root.join("plain.json");
    std::fs::write(&path, "{}").unwrap();

    for uri in ["/api/v1/board/render", "/api/v1/board/describe"] {
        let (status, json) = request(
            &state,
            Method::POST,
            uri,
            Some(serde_json::json!({"path": path.to_string_lossy()})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert!(
            json["error"].as_str().unwrap().contains("not a board"),
            "{json}"
        );
    }
}

#[tokio::test]
async fn board_edit_moves_an_object_and_the_agent_reads_it_back() {
    // The core bet, as a route round-trip: human gesture → canonical file →
    // describe shows the new position.
    let state = test_state();
    let path = write_board("board-edit");

    let (status, json) = request(
        &state,
        Method::POST,
        "/api/v1/board/edit",
        Some(serde_json::json!({
            "path": path.to_string_lossy(),
            "object": "t",
            "at": [120.0, 80.0],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert!(json["mtime"].as_str().is_some(), "{json}");
    assert!(
        json.get("path").is_none(),
        "no fs paths on the wire: {json}"
    );

    let (_, described) = request(
        &state,
        Method::POST,
        "/api/v1/board/describe",
        Some(serde_json::json!({"path": path.to_string_lossy()})),
    )
    .await;
    let text = described["text"].as_str().unwrap();
    assert!(text.contains("t text/heading at [120, 80]"), "{text}");

    // The write is canonical: a re-save of the file moves no bytes.
    let on_disk = std::fs::read_to_string(&path).unwrap();
    let board = chimaera_board::parse(&on_disk).unwrap();
    assert_eq!(chimaera_board::to_string(&board).unwrap(), on_disk);
}

#[tokio::test]
async fn board_edit_appends_to_the_semantic_journal() {
    // Every human gesture that lands in the file also lands in the journal:
    // seq-first, actor human, from/to in the saved (post-normalize) points,
    // and the response carries the seq additively as journalSeq.
    let state = test_state();
    let path = write_board("board-edit-journal");

    let (status, json) = request(
        &state,
        Method::POST,
        "/api/v1/board/edit",
        Some(serde_json::json!({
            "path": path.to_string_lossy(),
            "object": "t",
            "at": [120.0, 80.0],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["journalSeq"], 1, "{json}");

    // A move + resize gesture appends two events; seq continues across
    // requests because the journal is reopened from disk each time.
    let (status, json) = request(
        &state,
        Method::POST,
        "/api/v1/board/edit",
        Some(serde_json::json!({
            "path": path.to_string_lossy(),
            "object": "t",
            "at": [200.0, 96.0],
            "size": [400.0, 160.0],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["journalSeq"], 3, "the last appended seq: {json}");

    // The journal file sits at the path-derived key beside the board's
    // workspace, and reads back the exact gesture history.
    let canon = path.canonicalize().unwrap();
    let ws = chimaera_board::workspace_root(&canon);
    let journal = chimaera_board::journal::journal_path(&ws, &canon);
    let events = chimaera_board::journal::read_since(&journal, 0).unwrap();
    let lines: Vec<String> = events.iter().map(|e| e.render()).collect();
    assert_eq!(
        lines,
        [
            "#1 human moved t [40, 40] → [120, 80]",
            "#2 human moved t [120, 80] → [200, 96]",
            "#3 human resized t [320, 64] → [400, 160]",
        ],
        "{lines:?}"
    );
    let raw = std::fs::read_to_string(&journal).unwrap();
    assert!(
        raw.lines()
            .next()
            .unwrap()
            .starts_with(r#"{"seq":1,"actor":"human","event":"move","object":"t""#),
        "seq-first, no timestamp: {raw}"
    );

    // The read half advertises the change history.
    let (_, described) = request(
        &state,
        Method::POST,
        "/api/v1/board/describe",
        Some(serde_json::json!({"path": path.to_string_lossy()})),
    )
    .await;
    let text = described["text"].as_str().unwrap();
    assert!(text.contains("journal: 3 events · latest seq 3"), "{text}");
}

#[tokio::test]
async fn board_edit_replaces_text_and_journals_text_edited() {
    // The text op: plain paragraphs replace the object's text, the agent
    // reads the new words back through describe, and the journal records the
    // gesture as actor human — content-free, because the board file has the
    // content and the journal never duplicates it.
    let state = test_state();
    let path = write_board("board-edit-text");

    let (status, json) = request(
        &state,
        Method::POST,
        "/api/v1/board/edit",
        Some(serde_json::json!({
            "path": path.to_string_lossy(),
            "object": "t",
            "text": ["Rewritten by the pane", "Second paragraph"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["journalSeq"], 1, "{json}");

    let (_, described) = request(
        &state,
        Method::POST,
        "/api/v1/board/describe",
        Some(serde_json::json!({"path": path.to_string_lossy()})),
    )
    .await;
    let text = described["text"].as_str().unwrap();
    assert!(text.contains("Rewritten by the pane"), "{text}");

    let canon = path.canonicalize().unwrap();
    let ws = chimaera_board::workspace_root(&canon);
    let journal = chimaera_board::journal::journal_path(&ws, &canon);
    let events = chimaera_board::journal::read_since(&journal, 0).unwrap();
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].render(), "#1 human edited text of t");
    let raw = std::fs::read_to_string(&journal).unwrap();
    assert!(
        raw.contains(r#""event":"text-edited""#),
        "kebab-case on the wire: {raw}"
    );
    assert!(
        !raw.contains("Rewritten"),
        "the journal carries no content: {raw}"
    );
}

#[tokio::test]
async fn board_edit_refuses_an_unknown_object_by_name() {
    let state = test_state();
    let path = write_board("board-edit-unknown");
    let (status, json) = request(
        &state,
        Method::POST,
        "/api/v1/board/edit",
        Some(serde_json::json!({
            "path": path.to_string_lossy(),
            "object": "ghost",
            "at": [0.0, 0.0],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json["error"].as_str().unwrap().contains("ghost"), "{json}");
}

#[tokio::test]
async fn board_endpoints_without_token_are_401() {
    let state = test_state();
    for uri in [
        "/api/v1/board/render",
        "/api/v1/board/describe",
        "/api/v1/board/edit",
    ] {
        let (status, _, _) = request_bytes(&state, Method::POST, uri, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri} must be authed");
    }
}
