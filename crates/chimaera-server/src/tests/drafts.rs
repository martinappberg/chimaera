//! The draft mirror (`/api/v1/fs/drafts`, `/api/v1/fs/draft`).

use std::os::unix::fs::PermissionsExt;

use super::support::*;
use crate::AppState;

fn draft_file(state: &Arc<AppState>, path: &str) -> PathBuf {
    let digest = crate::fs::sha256_hex(path.as_bytes());
    state.drafts_root.join(format!("{}.json", &digest[..32]))
}

fn meta_file(state: &Arc<AppState>, path: &str) -> PathBuf {
    let digest = crate::fs::sha256_hex(path.as_bytes());
    state
        .drafts_root
        .join(format!("{}.meta.json", &digest[..32]))
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
    // Both files go together.
    assert!(!draft_file(&state, "/w/a.md").exists());
    assert!(!meta_file(&state, "/w/a.md").exists());
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
    for file in [draft_file(&state, "/w/b.md"), meta_file(&state, "/w/b.md")] {
        let file_mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(file_mode & 0o777, 0o600, "{}", file.display());
    }
}

/// The writer's own clock (the PUT's `updated_ms`) is stored beside the
/// daemon's arrival time and answered as `client_updated_ms`, so a client
/// can compare it with its own copy's time. A draft without one (an older
/// client) or with a malformed one answers null and is still stored.
#[tokio::test]
async fn drafts_keep_the_writers_clock_beside_the_daemons() {
    let state = test_state();
    let put = |path: &'static str, updated: serde_json::Value| {
        let state = state.clone();
        async move {
            request(
                &state,
                Method::PUT,
                "/api/v1/fs/drafts",
                Some(serde_json::json!({
                    "path": path, "base_hash": "h", "text": "t", "updated_ms": updated
                })),
            )
            .await
            .0
        }
    };
    // A client clock far behind the daemon's.
    assert_eq!(
        put("/w/c.md", serde_json::json!(1_000)).await,
        StatusCode::NO_CONTENT
    );
    for (path, bad) in [
        ("/w/str.md", serde_json::json!("soon")),
        ("/w/neg.md", serde_json::json!(-5)),
    ] {
        assert_eq!(put(path, bad).await, StatusCode::NO_CONTENT, "{path}");
    }
    let (status, _) = put_draft(&state, "/w/old.md", None, "t").await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, body) = request(&state, Method::GET, &draft_uri("/w/c.md"), None).await;
    assert_eq!(body["client_updated_ms"], 1_000);
    assert!(body["updated_ms"].as_u64().unwrap() > 1_000);
    for path in ["/w/str.md", "/w/neg.md", "/w/old.md"] {
        let (_, body) = request(&state, Method::GET, &draft_uri(path), None).await;
        assert_eq!(body["client_updated_ms"], serde_json::Value::Null, "{path}");
        assert_eq!(body["text"], "t");
    }
    let (_, body) = request(&state, Method::GET, "/api/v1/fs/drafts", None).await;
    let listed: std::collections::HashMap<&str, &serde_json::Value> = body["drafts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| (d["path"].as_str().unwrap(), &d["client_updated_ms"]))
        .collect();
    assert_eq!(listed["/w/c.md"], &serde_json::json!(1_000));
    assert_eq!(listed["/w/old.md"], &serde_json::Value::Null);

    // A sidecar written before the field existed still lists.
    std::fs::write(
        meta_file(&state, "/w/old.md"),
        br#"{"path":"/w/old.md","base_hash":null,"updated_ms":5,"bytes":1}"#,
    )
    .unwrap();
    let (_, body) = request(&state, Method::GET, "/api/v1/fs/drafts", None).await;
    assert!(body["drafts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["path"] == "/w/old.md" && d["client_updated_ms"].is_null()));
}

/// Two windows on one file share its draft key: a `writer`-scoped DELETE
/// removes only the draft that window wrote, never another window's (or an
/// older client's, which names no writer); a plain DELETE still removes any.
#[tokio::test]
async fn drafts_delete_by_writer_spares_another_windows_draft() {
    let state = test_state();
    let put = |path: &'static str, writer: Option<&'static str>| {
        let state = state.clone();
        async move {
            request(
                &state,
                Method::PUT,
                "/api/v1/fs/drafts",
                Some(serde_json::json!({"path": path, "text": "t", "writer": writer})),
            )
            .await
        }
    };
    let delete = |path: &'static str, writer: &'static str| {
        let state = state.clone();
        async move {
            request(
                &state,
                Method::DELETE,
                &format!("/api/v1/fs/draft?path={path}&writer={writer}"),
                None,
            )
            .await
            .0
        }
    };
    assert_eq!(
        put("/w/p.md", Some("win-b")).await.0,
        StatusCode::NO_CONTENT
    );
    let (_, body) = request(&state, Method::GET, &draft_uri("/w/p.md"), None).await;
    assert_eq!(body["writer"], "win-b");

    // Window A saved or discarded: window B's draft stays.
    assert_eq!(delete("/w/p.md", "win-a").await, StatusCode::NO_CONTENT);
    assert!(draft_file(&state, "/w/p.md").exists());
    assert!(meta_file(&state, "/w/p.md").exists());
    // Judged from the draft file when the sidecar is gone.
    std::fs::remove_file(meta_file(&state, "/w/p.md")).unwrap();
    assert_eq!(delete("/w/p.md", "win-a").await, StatusCode::NO_CONTENT);
    assert!(draft_file(&state, "/w/p.md").exists());
    assert_eq!(delete("/w/p.md", "win-b").await, StatusCode::NO_CONTENT);
    assert!(!draft_file(&state, "/w/p.md").exists());

    // An older client's draft names no writer: only a plain DELETE takes it.
    assert_eq!(put("/w/old.md", None).await.0, StatusCode::NO_CONTENT);
    assert_eq!(delete("/w/old.md", "win-a").await, StatusCode::NO_CONTENT);
    assert!(draft_file(&state, "/w/old.md").exists());
    let (status, _) = request(&state, Method::DELETE, &draft_uri("/w/old.md"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!draft_file(&state, "/w/old.md").exists());
    assert!(!meta_file(&state, "/w/old.md").exists());

    let (status, _) = request(
        &state,
        Method::PUT,
        "/api/v1/fs/drafts",
        Some(serde_json::json!({"path": "/w/x.md", "text": "t", "writer": "w".repeat(129)})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// The listing reads only the small sidecars — never the (up to 1 MiB) draft
/// files — and skips a draft whose sidecar is missing or corrupt.
#[tokio::test]
async fn drafts_list_reads_only_the_sidecars() {
    let state = test_state();
    for path in ["/w/one.md", "/w/two.md", "/w/three.md", "/w/four.md"] {
        let (status, _) = put_draft(&state, path, Some("h"), "text").await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    // A draft file the listing would choke on if it parsed it: still listed.
    std::fs::write(draft_file(&state, "/w/one.md"), b"{not json").unwrap();
    // No sidecar, or a corrupt one: skipped, though the draft itself reads.
    std::fs::remove_file(meta_file(&state, "/w/two.md")).unwrap();
    std::fs::write(meta_file(&state, "/w/three.md"), b"{not json").unwrap();
    // A sidecar naming another path than its file name: skipped.
    std::fs::copy(
        meta_file(&state, "/w/one.md"),
        meta_file(&state, "/w/four.md"),
    )
    .unwrap();

    let (status, body) = request(&state, Method::GET, "/api/v1/fs/drafts", None).await;
    assert_eq!(status, StatusCode::OK);
    let paths: Vec<&str> = body["drafts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["/w/one.md"], "{body}");
    assert_eq!(body["drafts"][0]["bytes"], 4);
    assert_eq!(body["drafts"][0]["base_hash"], "h");
    let (status, body) = request(&state, Method::GET, &draft_uri("/w/two.md"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["text"], "text");
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
    // The evicted draft took its sidecar with it: 64 pairs, nothing else.
    assert!(!draft_file(&state, "/w/7.md").exists());
    assert!(!meta_file(&state, "/w/7.md").exists());
    assert_eq!(std::fs::read_dir(&state.drafts_root).unwrap().count(), 128);
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
    // Draft files and sidecars count together.
    let total: u64 = std::fs::read_dir(&state.drafts_root)
        .unwrap()
        .map(|e| e.unwrap().metadata().unwrap().len())
        .sum();
    assert!(total <= 16 * 1024 * 1024, "{total}");
    assert!(!draft_file(&state, "/w/0.md").exists());
    assert!(!meta_file(&state, "/w/0.md").exists());
    assert!(draft_file(&state, "/w/16.md").exists());
    assert!(meta_file(&state, "/w/16.md").exists());
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

/// Writes under the caps never scan the directory: one scan (the first
/// write) learns what is there, and the eviction tests above show a write
/// past a cap still scans and evicts.
#[tokio::test]
async fn drafts_scan_for_eviction_only_when_a_cap_may_be_exceeded() {
    let state = test_state();
    for i in 0..20 {
        let (status, _) = put_draft(&state, &format!("/w/{}.md", i % 5), None, "x").await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    assert_eq!(crate::drafts::eviction_scans(&state.drafts_root).await, 1);
    // A crash's stale temp is swept by the next scan, not by every write.
    let temp = state.drafts_root.join(".stale.json.0123456789abcdef.tmp");
    std::fs::write(&temp, b"x").unwrap();
    age_file(&temp, 3600);
    for _ in 0..70 {
        let (status, _) = put_draft(&state, "/w/0.md", None, "x").await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    assert_eq!(crate::drafts::eviction_scans(&state.drafts_root).await, 2);
    assert!(!temp.exists());
}

/// A pagehide burst of draft PUTs rides the drafts' own limiter: it lands
/// even while every shared filesystem permit is taken, and never takes one.
#[tokio::test]
async fn drafts_never_take_the_shared_filesystem_permits() {
    let state = test_state();
    let held = crate::fs::FILESYSTEM_WORK.acquire_many(8).await.unwrap();
    let burst: Vec<_> = (0..12)
        .map(|i| {
            let state = state.clone();
            tokio::spawn(
                async move { put_draft(&state, &format!("/w/b{i}.md"), None, "x").await.0 },
            )
        })
        .collect();
    let landed = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        let mut statuses = Vec::new();
        for put in burst {
            statuses.push(put.await.unwrap());
        }
        statuses
    })
    .await
    .expect("draft PUTs must not wait on the shared filesystem limiter");
    drop(held);
    assert!(landed.iter().all(|s| *s == StatusCode::NO_CONTENT));
    let (_, body) = request(&state, Method::GET, "/api/v1/fs/drafts", None).await;
    assert_eq!(body["drafts"].as_array().unwrap().len(), 12);
}
