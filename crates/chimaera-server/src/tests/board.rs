//! Board route tests: render → ticket → raw fetch, describe, edit
//! serialization + epochs, journal round-trip, exports, auth coverage.

use axum::http::{Method, StatusCode};

use super::support::{request, request_bytes, test_dir, test_state, urlencode};

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

/// Two independently movable objects — the lost-update race needs a second
/// victim.
const BOARD_TWO_OBJECTS: &str = r#"{
  "format": "chimaera.board",
  "formatVersion": 1,
  "title": "Race test",
  "canvas": { "size": [400, 300] },
  "pages": [
    {
      "id": "p1",
      "objects": [
        { "id": "a", "type": "text", "role": "heading", "at": [16, 16], "size": [96, 32],
          "text": ["a"] },
        { "id": "b", "type": "text", "role": "heading", "at": [16, 96], "size": [96, 32],
          "text": ["b"] }
      ]
    }
  ]
}
"#;

/// Two pages — the all-pages SVG export tickets a directory (zip download).
const BOARD_TWO_PAGES: &str = r#"{
  "format": "chimaera.board",
  "formatVersion": 1,
  "title": "Deck",
  "canvas": { "size": [400, 300] },
  "pages": [
    {
      "id": "p1",
      "objects": [
        { "id": "t1", "type": "text", "role": "heading", "at": [40, 40], "size": [320, 64],
          "text": ["One"] }
      ]
    },
    {
      "id": "p2",
      "objects": [
        { "id": "t2", "type": "text", "role": "heading", "at": [40, 40], "size": [320, 64],
          "text": ["Two"] }
      ]
    }
  ]
}
"#;

fn write_board(label: &str) -> std::path::PathBuf {
    write_board_src(label, BOARD)
}

fn write_board_src(label: &str, src: &str) -> std::path::PathBuf {
    let root = test_dir(label);
    let path = root.join("demo.board.json");
    std::fs::write(&path, src).unwrap();
    path
}

/// The path-derived journal key for a board fixture, as the daemon computes
/// it (canonical path first).
fn journal_path_of(path: &std::path::Path) -> std::path::PathBuf {
    let canon = path.canonicalize().unwrap();
    let ws = chimaera_board::workspace_root(&canon);
    chimaera_board::journal::journal_path(&ws, &canon)
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

/// The render response carries the theme picker's data: `themeSelection`
/// (`"auto"` = match the app, the zero-config default; a scheme id; or
/// `"pinned"`) and `schemes` (the override choices with the variant each
/// resolves to under THIS render's mode). The fixture board pins no theme, so
/// it matches the app by default.
#[tokio::test]
async fn board_render_lists_schemes_for_the_picker() {
    let state = test_state();
    let path = write_board("board-render-schemes");

    // Light mode, no theme: the default is "match app" — NOT an explicit
    // scheme pick — and schemes resolve to their light variants.
    let (status, json) = request(
        &state,
        Method::POST,
        "/api/v1/board/render",
        Some(serde_json::json!({"path": path.to_string_lossy(), "scale": 1.0, "mode": "light"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["themeSelection"], "auto");
    let schemes = json["schemes"].as_array().unwrap();
    let talk = schemes.iter().find(|s| s["id"] == "talk").unwrap();
    assert_eq!(talk["label"], "Talk");
    assert_eq!(talk["variant"], "talk-light");
    let figure = schemes.iter().find(|s| s["id"] == "figure").unwrap();
    assert_eq!(figure["variant"], "figure-light");

    // A scheme override (`figure`) in dark mode → the figure scheme's dark
    // variant, and the selection reports that explicit scheme.
    let (_, json) = request(
        &state,
        Method::POST,
        "/api/v1/board/render",
        Some(serde_json::json!({
            "path": path.to_string_lossy(), "scale": 1.0, "mode": "dark", "theme": "figure"
        })),
    )
    .await;
    assert_eq!(json["themeSelection"], "figure");
    let figure = json["schemes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "figure")
        .unwrap()
        .clone();
    assert_eq!(figure["variant"], "figure-dark");

    // A pinned concrete variant is a fixed ground — the picker shows "pinned".
    let (_, json) = request(
        &state,
        Method::POST,
        "/api/v1/board/render",
        Some(serde_json::json!({
            "path": path.to_string_lossy(), "scale": 1.0, "mode": "light", "theme": "talk-dark"
        })),
    )
    .await;
    assert_eq!(json["themeSelection"], "pinned", "{json}");
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

/// A diagram composite — its derived children must ride the render response
/// as `childFrames`, on the miss AND the cached-hit path (the sidecar carries
/// them like diagnostics).
const BOARD_DIAGRAM: &str = r#"{
  "format": "chimaera.board",
  "formatVersion": 1,
  "title": "Flow",
  "canvas": { "size": [400, 300] },
  "pages": [
    {
      "id": "p1",
      "objects": [
        { "id": "flow", "type": "diagram", "at": [24, 24], "size": [352, 252],
          "nodes": [ { "id": "a", "label": "Start" }, { "id": "b", "label": "End" } ],
          "edges": [ { "from": "a", "to": "b" } ] }
      ]
    }
  ]
}
"#;

#[tokio::test]
async fn board_render_carries_child_frames_on_miss_and_hit() {
    let state = test_state();
    let path = write_board_src("board-render-children", BOARD_DIAGRAM);
    let body = serde_json::json!({"path": path.to_string_lossy(), "scale": 1.0});

    let (status, first) = request(
        &state,
        Method::POST,
        "/api/v1/board/render",
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let kids = first["childFrames"]["flow"].as_array().unwrap();
    let ids: Vec<&str> = kids.iter().map(|k| k["id"].as_str().unwrap()).collect();
    // The exact child set follows the engine's expansion (edge underlays come
    // and go with routing work); the contract is: derived ids under the
    // composite, node shapes present, nodes AFTER edge children so a
    // backwards hit-test walk picks the node.
    assert!(ids.iter().all(|i| i.starts_with("flow/")), "{ids:?}");
    let pos = |id: &str| {
        ids.iter()
            .position(|i| *i == id)
            .unwrap_or_else(|| panic!("{id} in {ids:?}"))
    };
    let (a, b) = (pos("flow/a"), pos("flow/b"));
    if let Some(edge) = ids.iter().position(|i| i.starts_with("flow/edge")) {
        assert!(a > edge && b > edge, "nodes above edges (z-order): {ids:?}");
    }
    let frame = kids[a]["frame"].as_array().unwrap();
    assert_eq!(frame.len(), 4, "[x, y, w, h] in page points");
    assert!(frame[2].as_f64().unwrap() > 0.0, "a laid-out width");

    // The cached hit serves the same frames from the sidecar.
    let (_, second) = request(&state, Method::POST, "/api/v1/board/render", Some(body)).await;
    assert_eq!(first["childFrames"], second["childFrames"]);
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
        "/api/v1/board/journal",
        "/api/v1/board/export",
    ] {
        let (status, _, _) = request_bytes(&state, Method::POST, uri, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri} must be authed");
    }
    let (status, _, _) =
        request_bytes(&state, Method::GET, "/api/v1/board/journal?path=x", None).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "journal GET must be authed"
    );
}

#[tokio::test]
async fn board_edit_serializes_concurrent_gestures_per_path() {
    // Two read-modify-write cycles on one file, started together: without
    // the per-path shard lock one save clobbers the other and a gesture is
    // silently lost.
    let state = test_state();
    let path = write_board_src("board-edit-race", BOARD_TWO_OBJECTS);
    let body = |object: &str, at: [f64; 2]| serde_json::json!({"path": path.to_string_lossy(), "object": object, "at": at});

    let (a, b) = tokio::join!(
        request(
            &state,
            Method::POST,
            "/api/v1/board/edit",
            Some(body("a", [200.0, 16.0])),
        ),
        request(
            &state,
            Method::POST,
            "/api/v1/board/edit",
            Some(body("b", [200.0, 96.0])),
        ),
    );
    assert_eq!(a.0, StatusCode::OK, "{}", a.1);
    assert_eq!(b.0, StatusCode::OK, "{}", b.1);

    let (_, described) = request(
        &state,
        Method::POST,
        "/api/v1/board/describe",
        Some(serde_json::json!({"path": path.to_string_lossy()})),
    )
    .await;
    let text = described["text"].as_str().unwrap();
    assert!(text.contains("a text/heading at [200, 16]"), "{text}");
    assert!(text.contains("b text/heading at [200, 96]"), "{text}");

    // Both gestures journaled with distinct server-assigned seqs — the
    // append rides the same shard, so seq stamping cannot race either.
    let events = chimaera_board::journal::read_since(&journal_path_of(&path), 0).unwrap();
    let mut seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
    seqs.sort_unstable();
    assert_eq!(seqs, [1, 2], "{events:?}");
}

#[tokio::test]
async fn board_edit_bumps_the_board_epoch_and_defers_the_git_bump() {
    let state = test_state();
    let root = test_dir("board-epoch");
    let path = root.join("epoch.board.json");
    std::fs::write(&path, BOARD).unwrap();
    // Both epochs key on the registered workspace containing the board.
    let (status, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ws}");
    let ws_id = ws["id"].as_str().unwrap().to_string();
    let git_before = state
        .git
        .epochs_snapshot()
        .get(&ws_id)
        .copied()
        .unwrap_or(0);

    for at in [[120.0, 80.0], [200.0, 80.0]] {
        let (status, json) = request(
            &state,
            Method::POST,
            "/api/v1/board/edit",
            Some(serde_json::json!({"path": path.to_string_lossy(), "object": "t", "at": at})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
    }

    // The board epoch moves immediately, once per gesture — that is what the
    // pane invalidates on instead of its poll.
    assert_eq!(
        crate::lock(&state.board_epochs).get(&ws_id).copied(),
        Some(2),
        "one board-epoch bump per successful edit"
    );
    // The git epoch does NOT move per gesture: it settles ~1s after the last
    // edit, so a layout session costs one `git status` announcement.
    assert_eq!(
        state
            .git
            .epochs_snapshot()
            .get(&ws_id)
            .copied()
            .unwrap_or(0),
        git_before,
        "no per-gesture git bump"
    );
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let epoch = state
            .git
            .epochs_snapshot()
            .get(&ws_id)
            .copied()
            .unwrap_or(0);
        if epoch > git_before {
            assert_eq!(
                epoch,
                git_before + 1,
                "both gestures coalesced into one settle bump"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the git settle timer never fired"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn board_journal_post_and_get_round_trip() {
    let state = test_state();
    let path = write_board("board-journal-routes");
    let encoded = urlencode(&path.to_string_lossy());

    // POST appends one validated event; seq is assigned server-side.
    let (status, json) = request(
        &state,
        Method::POST,
        "/api/v1/board/journal",
        Some(serde_json::json!({
            "path": path.to_string_lossy(),
            "actor": "agent",
            "event": "object-added", "object": "note", "kind": "text", "page": "p1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["seq"], 1, "{json}");
    let (status, json) = request(
        &state,
        Method::POST,
        "/api/v1/board/journal",
        Some(serde_json::json!({
            "path": path.to_string_lossy(),
            "actor": "human",
            "event": "text-edited", "object": "note",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["seq"], 2, "{json}");

    // GET reads everything after `since`, oldest first, in the journal
    // file's own shape (seq-first, kebab-case op), plus latestSeq.
    let (status, json) = request(
        &state,
        Method::GET,
        &format!("/api/v1/board/journal?path={encoded}&since=0"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["latestSeq"], 2, "{json}");
    let events = json["events"].as_array().unwrap();
    assert_eq!(events.len(), 2, "{json}");
    assert_eq!(events[0]["seq"], 1);
    assert_eq!(events[0]["actor"], "agent");
    assert_eq!(events[0]["event"], "object-added");
    assert_eq!(events[0]["object"], "note");
    assert_eq!(events[1]["event"], "text-edited");

    // since=N means strictly after N.
    let (_, json) = request(
        &state,
        Method::GET,
        &format!("/api/v1/board/journal?path={encoded}&since=1"),
        None,
    )
    .await;
    let events = json["events"].as_array().unwrap();
    assert_eq!(events.len(), 1, "{json}");
    assert_eq!(events[0]["seq"], 2);

    // An op outside the journal vocabulary — or a missing actor — is
    // rejected at deserialization and never reaches the file.
    for bad in [
        serde_json::json!({
            "path": path.to_string_lossy(),
            "actor": "agent",
            "event": "from-the-future",
        }),
        serde_json::json!({
            "path": path.to_string_lossy(),
            "event": "brief-changed",
        }),
    ] {
        let (status, _) = request(&state, Method::POST, "/api/v1/board/journal", Some(bad)).await;
        assert!(status.is_client_error(), "{status}");
    }
    let events = chimaera_board::journal::read_since(&journal_path_of(&path), 0).unwrap();
    assert_eq!(events.len(), 2, "rejected events never landed: {events:?}");
}

#[tokio::test]
async fn board_export_pdf_mints_a_download_ticket() {
    let state = test_state();
    let path = write_board("board-export-pdf");
    let (status, json) = request(
        &state,
        Method::POST,
        "/api/v1/board/export",
        Some(serde_json::json!({"path": path.to_string_lossy(), "format": "pdf"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["filename"], "demo.pdf", "{json}");
    assert_eq!(json["pageCount"], 1, "{json}");
    assert!(json.get("exportPath").is_none(), "no fs paths: {json}");

    let ticket = json["ticket"].as_str().unwrap();
    let (status, _, body) =
        request_bytes(&state, Method::GET, &format!("/download/{ticket}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..5], b"%PDF-");
}

#[tokio::test]
async fn board_export_pptx_reports_object_fates() {
    let state = test_state();
    let path = write_board("board-export-pptx");
    let (status, json) = request(
        &state,
        Method::POST,
        "/api/v1/board/export",
        Some(serde_json::json!({"path": path.to_string_lossy(), "format": "pptx"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["filename"], "demo.pptx", "{json}");
    // The degradation contract rides the response, per object.
    let fates = json["objects"].as_array().unwrap();
    assert!(fates.iter().any(|f| f["id"] == "t"), "{json}");

    let ticket = json["ticket"].as_str().unwrap();
    let (status, _, body) =
        request_bytes(&state, Method::GET, &format!("/download/{ticket}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..4], b"PK\x03\x04", "a pptx is a zip");
}

#[tokio::test]
async fn board_export_svg_single_page_serves_markup() {
    let state = test_state();
    let path = write_board("board-export-svg");
    for (format, filename) in [("svg", "demo.svg"), ("svg-outlined", "demo-outlined.svg")] {
        let (status, json) = request(
            &state,
            Method::POST,
            "/api/v1/board/export",
            Some(serde_json::json!({"path": path.to_string_lossy(), "format": format})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["filename"], filename, "{json}");
        let ticket = json["ticket"].as_str().unwrap();
        let (status, _, body) =
            request_bytes(&state, Method::GET, &format!("/download/{ticket}"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            String::from_utf8_lossy(&body).contains("<svg"),
            "{format} serves svg markup"
        );
    }
}

#[tokio::test]
async fn board_export_svg_all_pages_downloads_as_zip() {
    let state = test_state();
    let path = write_board_src("board-export-svg-zip", BOARD_TWO_PAGES);
    let (status, json) = request(
        &state,
        Method::POST,
        "/api/v1/board/export",
        Some(serde_json::json!({"path": path.to_string_lossy(), "format": "svg"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["pageCount"], 2, "{json}");
    assert_eq!(json["filename"], "demo-svg", "{json}");

    // The ticket names the per-export directory; the download route streams
    // it as a zip.
    let ticket = json["ticket"].as_str().unwrap();
    let (status, headers, body) =
        request_bytes(&state, Method::GET, &format!("/download/{ticket}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get("content-type").unwrap().to_str().unwrap(),
        "application/zip"
    );
    assert_eq!(&body[..2], b"PK");
}

#[tokio::test]
async fn board_export_refuses_bad_format_and_page_misuse() {
    let state = test_state();
    let path = write_board("board-export-refusals");
    for (body, needle) in [
        (
            serde_json::json!({"path": path.to_string_lossy(), "format": "docx"}),
            "unknown format",
        ),
        (
            serde_json::json!({"path": path.to_string_lossy(), "format": "pdf", "page": 0}),
            "does not apply",
        ),
        (
            serde_json::json!({"path": path.to_string_lossy(), "format": "pptx", "page": 0}),
            "does not apply",
        ),
    ] {
        let (status, json) =
            request(&state, Method::POST, "/api/v1/board/export", Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
        assert!(json["error"].as_str().unwrap().contains(needle), "{json}");
    }

    // chartsNative off pptx is a 422 — "fix the parameters", distinct from
    // the generic 400s above, with the CLI-parity message.
    let (status, json) = request(
        &state,
        Method::POST,
        "/api/v1/board/export",
        Some(serde_json::json!({
            "path": path.to_string_lossy(), "format": "svg", "chartsNative": true
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{json}");
    assert!(
        json["error"]
            .as_str()
            .unwrap()
            .contains("applies to pptx only"),
        "{json}"
    );
}
