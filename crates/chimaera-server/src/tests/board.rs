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
    // Same board, same params → same cached file behind two fresh tickets.
    assert_eq!(first["width"], second["width"]);
    let renders = chimaera_board::board_dir(&chimaera_board::workspace_root(std::path::Path::new(
        path.to_str().unwrap(),
    )))
    .join("renders");
    let count = std::fs::read_dir(renders).unwrap().count();
    assert_eq!(
        count, 1,
        "a re-render of unchanged bytes must hit the cache"
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
async fn board_endpoints_without_token_are_401() {
    let state = test_state();
    for uri in ["/api/v1/board/render", "/api/v1/board/describe"] {
        let (status, _, _) = request_bytes(&state, Method::POST, uri, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri} must be authed");
    }
}
