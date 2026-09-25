//! The draft mirror (`/api/v1/fs/drafts`, `/api/v1/fs/draft`).

use std::os::unix::fs::PermissionsExt;

use super::support::*;
use crate::AppState;

fn draft_file(state: &Arc<AppState>, path: &str) -> PathBuf {
    let digest = crate::fs::sha256_hex(path.as_bytes());
    state.drafts_root.join(format!("{}.json", &digest[..32]))
}

async fn put_draft(
    state: &Arc<AppState>,
    path: &str,
    base_hash: Option<&str>,
    text: &str,
) -> (StatusCode, serde_json::Value) {
    request(
        state,
        Method::PUT,
        "/api/v1/fs/drafts",
        Some(serde_json::json!({"path": path, "base_hash": base_hash, "text": text})),
    )
    .await
}

fn draft_uri(path: &str) -> String {
    format!("/api/v1/fs/draft?path={path}")
}

#[tokio::test]
async fn drafts_round_trip_list_newest_first_and_delete_idempotently() {
    let state = test_state();

    let (status, body) = request(&state, Method::GET, "/api/v1/fs/drafts", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, serde_json::json!({"drafts": []}));

    let (status, _) = put_draft(&state, "/w/a.md", Some("abc"), "alpha ✓").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = put_draft(&state, "/w/b.md", None, "beta").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // Guarantee distinct stamps (ms resolution) before the update.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let (status, _) = put_draft(&state, "/w/a.md", Some("def"), "alpha 2").await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = request(&state, Method::GET, "/api/v1/fs/drafts", None).await;
    assert_eq!(status, StatusCode::OK);
    let drafts = body["drafts"].as_array().unwrap();
    assert_eq!(drafts.len(), 2, "{body}");
    assert_eq!(drafts[0]["path"], "/w/a.md", "newest first: {body}");
    assert_eq!(drafts[0]["base_hash"], "def");
    assert_eq!(drafts[0]["bytes"], 7);
    assert!(drafts[0].get("text").is_none(), "{body}");
    assert!(drafts[0]["updated_ms"].as_u64().unwrap() >= drafts[1]["updated_ms"].as_u64().unwrap());
    assert_eq!(drafts[1]["path"], "/w/b.md");
    assert_eq!(drafts[1]["base_hash"], serde_json::Value::Null);

    let (status, body) = request(&state, Method::GET, &draft_uri("/w/a.md"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["path"], "/w/a.md");
    assert_eq!(body["base_hash"], "def");
    assert_eq!(body["text"], "alpha 2");
    assert!(body["updated_ms"].as_u64().unwrap() > 0);

    for _ in 0..2 {
        let (status, _) = request(&state, Method::DELETE, &draft_uri("/w/a.md"), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    let (status, body) = request(&state, Method::GET, &draft_uri("/w/a.md"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (_, body) = request(&state, Method::GET, "/api/v1/fs/drafts", None).await;
    assert_eq!(body["drafts"].as_array().unwrap().len(), 1);

    // Unsaved text is private on a shared host.
    let dir_mode = std::fs::metadata(&state.drafts_root)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(dir_mode & 0o777, 0o700);
    let file_mode = std::fs::metadata(draft_file(&state, "/w/b.md"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(file_mode & 0o777, 0o600);
}

#[tokio::test]
async fn drafts_reject_oversize_text_and_bad_paths() {
    let state = test_state();
    // Exactly 1 MiB is accepted, even with JSON escaping doubling the body.
    let (status, body) = put_draft(&state, "/w/big.md", None, &"\n".repeat(1024 * 1024)).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (status, body) = put_draft(&state, "/w/big.md", None, &"x".repeat(1024 * 1024 + 1)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert!(body["error"].as_str().unwrap().contains("too large"));
    // The refused update left the earlier draft alone.
    let (_, body) = request(&state, Method::GET, &draft_uri("/w/big.md"), None).await;
    assert_eq!(body["text"].as_str().unwrap().len(), 1024 * 1024);

    let (status, _) = put_draft(&state, "", None, "x").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = put_draft(&state, &"p".repeat(4097), None, "x").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = put_draft(&state, "/w/x.md", Some(&"h".repeat(129)), "x").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Past 64 drafts the least recently updated one goes.
#[tokio::test]
async fn drafts_evict_least_recently_updated_past_the_count_cap() {
    let state = test_state();
    for i in 0..64 {
        let (status, _) = put_draft(&state, &format!("/w/{i}.md"), None, "x").await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    // Make /w/7.md unambiguously the stalest (mtime is the recency key).
    age_file(&draft_file(&state, "/w/7.md"), 3600);
    let (status, _) = put_draft(&state, "/w/new.md", None, "x").await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, body) = request(&state, Method::GET, "/api/v1/fs/drafts", None).await;
    let drafts = body["drafts"].as_array().unwrap();
    assert_eq!(drafts.len(), 64);
    assert!(drafts.iter().all(|d| d["path"] != "/w/7.md"), "{body}");
    assert!(drafts.iter().any(|d| d["path"] == "/w/new.md"));
}

/// Past 16 MiB in total the oldest drafts go, never the one just written.
#[tokio::test]
async fn drafts_evict_past_the_byte_cap() {
    let state = test_state();
    let text = "y".repeat(1024 * 1024 - 256);
    for i in 0..17 {
        let (status, _) = put_draft(&state, &format!("/w/{i}.md"), None, &text).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        age_file(&draft_file(&state, &format!("/w/{i}.md")), 1000 - i);
    }
    let total: u64 = std::fs::read_dir(&state.drafts_root)
        .unwrap()
        .map(|e| e.unwrap().metadata().unwrap().len())
        .sum();
    assert!(total <= 16 * 1024 * 1024, "{total}");
    assert!(!draft_file(&state, "/w/0.md").exists());
    assert!(draft_file(&state, "/w/16.md").exists());
}

/// A corrupt draft file is skipped by the listing and reads as no draft.
#[tokio::test]
async fn drafts_skip_corrupt_files() {
    let state = test_state();
    let (status, _) = put_draft(&state, "/w/ok.md", None, "fine").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    std::fs::write(draft_file(&state, "/w/bad.md"), b"{not json").unwrap();
    std::fs::write(state.drafts_root.join("stray.txt"), b"ignored").unwrap();

    let (status, body) = request(&state, Method::GET, "/api/v1/fs/drafts", None).await;
    assert_eq!(status, StatusCode::OK);
    let drafts = body["drafts"].as_array().unwrap();
    assert_eq!(drafts.len(), 1, "{body}");
    assert_eq!(drafts[0]["path"], "/w/ok.md");
    let (status, _) = request(&state, Method::GET, &draft_uri("/w/bad.md"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn drafts_require_the_bearer_token() {
    let state = test_state();
    for (method, uri) in [
        (Method::GET, "/api/v1/fs/drafts"),
        (Method::PUT, "/api/v1/fs/drafts"),
        (Method::GET, "/api/v1/fs/draft?path=/x"),
        (Method::DELETE, "/api/v1/fs/draft?path=/x"),
    ] {
        let (status, _, _) = request_bytes(&state, method.clone(), uri, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
}
