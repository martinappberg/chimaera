//! The chunked, resumable upload routes (`upload::chunked`): what a slow or
//! interrupted link may and may not do to the file that lands.

use serde_json::json;
use sha2::{Digest, Sha256};

use super::support::*;
use crate::*;

/// Deterministic bytes with no repeats a slicing bug could hide behind.
fn pattern(size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(size);
    let mut x: u32 = 0x9e37_79b9;
    for _ in 0..size {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        out.push((x >> 24) as u8);
    }
    out
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The running digest a client keeps: each piece's SHA-256 folded in.
fn chain_of(pieces: &[&[u8]]) -> String {
    let mut chain = [0u8; 32];
    for piece in pieces {
        let mut hasher = Sha256::new();
        hasher.update(chain);
        hasher.update(sha256(piece));
        chain = hasher.finalize().into();
    }
    hex(&chain)
}

fn request(method: Method, uri: impl AsRef<str>) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(uri.as_ref())
        .header(header::AUTHORIZATION, "Bearer test-token")
}

async fn call(state: &Arc<AppState>, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let res = app(state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

async fn begin(state: &Arc<AppState>, dest: serde_json::Value) -> (StatusCode, serde_json::Value) {
    call(
        state,
        request(Method::POST, "/api/v1/uploads")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(dest.to_string()))
            .unwrap(),
    )
    .await
}

async fn begin_in_dir(
    state: &Arc<AppState>,
    dir: &std::path::Path,
    name: &str,
    size: usize,
) -> String {
    let (status, body) = begin(state, json!({"dir": dir, "name": name, "size": size})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["id"].as_str().unwrap().to_string()
}

/// PUT one piece with its checksum, as the web UI sends it.
async fn put(
    state: &Arc<AppState>,
    id: &str,
    offset: usize,
    piece: &[u8],
) -> (StatusCode, serde_json::Value) {
    put_body(
        state,
        id,
        offset,
        Some(&hex(&sha256(piece))),
        Body::from(piece.to_vec()),
    )
    .await
}

async fn put_body(
    state: &Arc<AppState>,
    id: &str,
    offset: usize,
    checksum: Option<&str>,
    body: Body,
) -> (StatusCode, serde_json::Value) {
    let mut req = request(Method::PUT, format!("/api/v1/uploads/{id}?offset={offset}"));
    if let Some(checksum) = checksum {
        req = req.header("x-chunk-sha256", checksum);
    }
    call(state, req.body(body).unwrap()).await
}

async fn offset_of(state: &Arc<AppState>, id: &str) -> (StatusCode, serde_json::Value) {
    call(
        state,
        request(Method::GET, format!("/api/v1/uploads/{id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

async fn finish(
    state: &Arc<AppState>,
    id: &str,
    chain: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let body = match chain {
        Some(chain) => json!({"chain": chain}),
        None => json!({}),
    };
    call(
        state,
        request(Method::POST, format!("/api/v1/uploads/{id}/finish"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

/// A piece whose connection drops after `sent` arrived.
fn cut_off_after(sent: &[u8]) -> Body {
    let parts: Vec<Result<bytes::Bytes, std::io::Error>> = vec![
        Ok(bytes::Bytes::copy_from_slice(sent)),
        Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "connection reset",
        )),
    ];
    Body::from_stream(futures::stream::iter(parts))
}

/// A piece whose bytes stop arriving after `sent`, on a connection that
/// stays open.
fn stalls_after(sent: &[u8]) -> Body {
    use futures::StreamExt as _;
    let first: Result<bytes::Bytes, std::io::Error> = Ok(bytes::Bytes::copy_from_slice(sent));
    Body::from_stream(futures::stream::iter([first]).chain(futures::stream::pending()))
}

/// The hidden temp files in `dir` (an upload in progress has exactly one).
fn temps(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            name.starts_with('.') && name.ends_with(".tmp")
        })
        .collect();
    out.sort();
    out
}

fn names(dir: &std::path::Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

fn open_records(state: &Arc<AppState>) -> Vec<String> {
    let dir = state.uploads_root.with_file_name("uploads-open");
    if dir.exists() {
        names(&dir)
    } else {
        Vec::new()
    }
}

const MB: usize = 1024 * 1024;

/// The whole point: a piece that breaks off mid-stream, a replayed piece and
/// a resume leave a file whose checksum equals the source's — and nothing
/// else in the folder.
#[tokio::test]
async fn an_interrupted_upload_resumes_and_the_landed_file_matches_its_source() {
    let state = test_state();
    let dest = test_dir("chunked-dest");
    let source = pattern(5 * MB + 123);
    let (a, rest) = source.split_at(MB);
    let (b, c) = rest.split_at(2 * MB);
    let id = begin_in_dir(&state, &dest, "data set.bin", source.len()).await;

    let (status, body) = put(&state, &id, 0, a).await;
    assert_eq!(
        (status, body["offset"].as_u64()),
        (StatusCode::OK, Some(MB as u64)),
        "{body}"
    );

    // The link drops 700 KB into the second piece.
    let (status, body) = put_body(
        &state,
        &id,
        MB,
        Some(&hex(&sha256(b))),
        cut_off_after(&b[..700 * 1024]),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (_, body) = offset_of(&state, &id).await;
    assert_eq!(
        body["offset"].as_u64(),
        Some(MB as u64),
        "nothing of the broken piece counts"
    );
    let tmp = temps(&dest);
    assert_eq!(tmp.len(), 1, "{tmp:?}");
    assert_eq!(
        std::fs::metadata(&tmp[0]).unwrap().len(),
        MB as u64,
        "the broken piece's bytes are cut back off the temp file"
    );
    assert_eq!(
        names(&dest).len(),
        1,
        "nothing is visible under the real name yet"
    );

    // A client that never saw the first answer sends that piece again.
    let (status, body) = put(&state, &id, 0, a).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["offset"].as_u64(),
        Some(MB as u64),
        "the answer says where to go on"
    );

    // Finishing early is refused and costs nothing.
    let (status, body) = finish(&state, &id, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["offset"].as_u64(), Some(MB as u64));

    let (status, body) = put(&state, &id, MB, b).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = put(&state, &id, 3 * MB, c).await;
    assert_eq!(
        (status, body["offset"].as_u64()),
        (StatusCode::OK, Some(source.len() as u64))
    );

    let chain = chain_of(&[a, b, c]);
    let (status, body) = finish(&state, &id, Some(&chain)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["name"], "data set.bin",
        "a folder upload keeps the user's name"
    );
    assert_eq!(body["size"].as_u64(), Some(source.len() as u64));
    let landed = std::fs::read(body["path"].as_str().unwrap()).unwrap();
    assert_eq!(
        hex(&sha256(&landed)),
        hex(&sha256(&source)),
        "checksum of the landed file"
    );
    assert_eq!(
        names(&dest),
        vec!["data set.bin"],
        "no temp file is left beside it"
    );
    assert!(
        open_records(&state).is_empty(),
        "{:?}",
        open_records(&state)
    );

    // The answer to `finish` was lost: asking again gives the same one.
    let (status, again) = finish(&state, &id, Some(&chain)).await;
    assert_eq!((status, &again), (StatusCode::OK, &body));
}

/// A connection that stays open but stops delivering must not hold the
/// upload: the piece is given up, cut back, and the next try goes through.
#[tokio::test]
async fn a_piece_that_stops_arriving_is_given_up_and_can_be_sent_again() {
    let state = test_state();
    state
        .chunked_uploads
        .set_chunk_idle(std::time::Duration::from_millis(150));
    let dest = test_dir("chunked-stall");
    let source = pattern(600 * 1024);
    let id = begin_in_dir(&state, &dest, "slow.bin", source.len()).await;

    let (status, body) = put_body(&state, &id, 0, None, stalls_after(&source[..200 * 1024])).await;
    assert_eq!(status, StatusCode::REQUEST_TIMEOUT, "{body}");
    let (_, body) = offset_of(&state, &id).await;
    assert_eq!(body["offset"].as_u64(), Some(0));
    assert_eq!(std::fs::metadata(&temps(&dest)[0]).unwrap().len(), 0);

    let (status, body) = put(&state, &id, 0, &source).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = finish(&state, &id, Some(&chain_of(&[&source]))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(std::fs::read(dest.join("slow.bin")).unwrap(), source);
}

/// The client gave up on a piece and sends it again while the daemon is
/// still waiting on the old connection: the new request wins at once.
#[tokio::test]
async fn a_resent_piece_takes_over_from_the_one_still_waiting() {
    let state = test_state();
    let dest = test_dir("chunked-takeover");
    let source = pattern(400 * 1024);
    let id = begin_in_dir(&state, &dest, "take.bin", source.len()).await;

    let stalled = {
        let (state, id, head) = (state.clone(), id.clone(), source[..100 * 1024].to_vec());
        tokio::spawn(async move { put_body(&state, &id, 0, None, stalls_after(&head)).await })
    };
    // Let the stalled piece take the seat and write its head.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let (status, body) = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        put(&state, &id, 0, &source),
    )
    .await
    .expect("the resent piece must not wait for the stalled one's 60 s");
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = stalled.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let (status, body) = finish(&state, &id, Some(&chain_of(&[&source]))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(std::fs::read(dest.join("take.bin")).unwrap(), source);
}

#[tokio::test]
async fn a_piece_that_arrived_changed_is_refused_and_can_be_sent_again() {
    let state = test_state();
    let dest = test_dir("chunked-changed");
    let source = pattern(300 * 1024);
    let id = begin_in_dir(&state, &dest, "c.bin", source.len()).await;

    let mut changed = source.clone();
    changed[1234] ^= 0x40;
    let (status, body) = put_body(
        &state,
        &id,
        0,
        Some(&hex(&sha256(&source))),
        Body::from(changed),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (_, body) = offset_of(&state, &id).await;
    assert_eq!(body["offset"].as_u64(), Some(0));
    assert_eq!(std::fs::metadata(&temps(&dest)[0]).unwrap().len(), 0);

    let (status, _) = put(&state, &id, 0, &source).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = finish(&state, &id, Some(&chain_of(&[&source]))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(std::fs::read(dest.join("c.bin")).unwrap(), source);
}

/// The end-of-upload check: a file whose pieces are not the ones the client
/// sent is never published, and leaves nothing behind.
#[tokio::test]
async fn finish_does_not_publish_a_file_that_fails_verification() {
    let state = test_state();
    let dest = test_dir("chunked-verify");
    let source = pattern(200 * 1024);
    let id = begin_in_dir(&state, &dest, "v.bin", source.len()).await;
    // Sent without a per-piece checksum, so only the end check can object.
    let (status, _) = put_body(&state, &id, 0, None, Body::from(source.clone())).await;
    assert_eq!(status, StatusCode::OK);

    let other = pattern(200 * 1024 - 1);
    let (status, body) = finish(&state, &id, Some(&chain_of(&[&other]))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("did not verify"),
        "{body}"
    );
    assert!(names(&dest).is_empty(), "{:?}", names(&dest));
    assert!(open_records(&state).is_empty());
    let (status, _) = offset_of(&state, &id).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(state.chunked_uploads.open_count(), 0);
}

#[tokio::test]
async fn cancel_removes_the_partial_file() {
    let state = test_state();
    let dest = test_dir("chunked-cancel");
    let source = pattern(100 * 1024);
    let id = begin_in_dir(&state, &dest, "x.bin", source.len() * 2).await;
    let (status, _) = put(&state, &id, 0, &source).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(temps(&dest).len(), 1);

    let (status, _) = call(
        &state,
        request(Method::DELETE, format!("/api/v1/uploads/{id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(names(&dest).is_empty(), "{:?}", names(&dest));
    assert!(open_records(&state).is_empty());
    let (status, body) = put(&state, &id, source.len(), &source).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

/// Too big is said at `begin`, before a byte crosses the link — and the
/// one-request routes say it from the declared length instead of reading
/// the whole body first.
#[tokio::test]
async fn a_file_past_the_cap_is_refused_before_any_of_it_is_read() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let state = test_state();
    let session = agents::fresh_session_id();
    plant_agent_record(
        &state,
        &session,
        "ws-x",
        agents::AgentKind::Claude,
        None,
        None,
    );
    let dest = test_dir("chunked-cap");

    let over = upload::MAX_SESSION_UPLOAD_FILE_BYTES + 1;
    let (status, body) = begin(
        &state,
        json!({"session": session, "name": "big.zip", "size": over}),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(
        body["error"],
        "big.zip is 64 MB; a file dropped on a session can be at most 64 MB"
    );
    let (status, body) = begin(
        &state,
        json!({"dir": dest, "name": "huge.bin", "size": upload::chunked::MAX_UPLOAD_BYTES + 1}),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(state.chunked_uploads.open_count(), 0);
    assert!(names(&dest).is_empty());

    // A body that records whether anyone read it.
    let read = Arc::new(AtomicBool::new(false));
    let unread_body = |read: Arc<AtomicBool>| {
        Body::from_stream(futures::stream::poll_fn(move |_| {
            read.store(true, Ordering::SeqCst);
            std::task::Poll::Ready(Some(Ok::<_, std::io::Error>(bytes::Bytes::from_static(
                b"x",
            ))))
        }))
    };
    for (uri, declared) in [
        (
            format!("/api/v1/sessions/{session}/upload?name=big.zip"),
            over,
        ),
        (
            format!("/api/v1/fs/upload?dir={}&name=big.zip", dest.display()),
            upload::MAX_DIR_UPLOAD_BYTES + 1,
        ),
    ] {
        let (status, body) = call(
            &state,
            request(Method::POST, &uri)
                .header(header::CONTENT_LENGTH, declared)
                .body(unread_body(read.clone()))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{uri}: {body}");
        assert!(
            !read.load(Ordering::SeqCst),
            "{uri}: refused only after reading the body"
        );
    }
    assert!(names(&dest).is_empty());
}

#[tokio::test]
async fn a_session_upload_lands_mention_safe_within_the_session_caps() {
    let state = test_state();
    let session = agents::fresh_session_id();
    plant_agent_record(
        &state,
        &session,
        "ws-x",
        agents::AgentKind::Claude,
        None,
        None,
    );
    let source = pattern(150 * 1024);

    let (status, body) = begin(
        &state,
        json!({"session": session, "name": "Screenshot 2026-09-26 at 12.30.png", "size": source.len()}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let id = body["id"].as_str().unwrap().to_string();
    let (status, _) = put(&state, &id, 0, &source).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = finish(&state, &id, Some(&chain_of(&[&source]))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["name"], "Screenshot-2026-09-26-at-12.30.png");
    let path = PathBuf::from(body["path"].as_str().unwrap());
    assert!(
        path.starts_with(state.uploads_root.join(&session)),
        "{}",
        path.display()
    );
    assert_eq!(std::fs::read(&path).unwrap(), source);

    // What is left of the session's room is checked up front too.
    let room = upload::MAX_SESSION_UPLOAD_BYTES - source.len() as u64;
    let (status, body) = begin(
        &state,
        json!({"session": session, "name": "more.bin", "size": upload::MAX_SESSION_UPLOAD_FILE_BYTES}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}: {room} bytes of room");
    let (status, body) = begin(&state, json!({"session": "nope", "name": "a", "size": 1})).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(!state.uploads_root.join("nope").exists());
}

/// Only uploads still in progress count against the limit: a run of small
/// drops (each finished, its answer kept a while for a retried `finish`)
/// must not lock the next one out.
#[tokio::test]
async fn finished_uploads_do_not_use_up_the_open_upload_limit() {
    let state = test_state();
    let dest = test_dir("chunked-many");
    let source = pattern(1024);
    for n in 0..80 {
        let id = begin_in_dir(&state, &dest, &format!("f{n}.bin"), source.len()).await;
        let (status, _) = put(&state, &id, 0, &source).await;
        assert_eq!(status, StatusCode::OK);
        let (status, body) = finish(&state, &id, Some(&chain_of(&[&source]))).await;
        assert_eq!(status, StatusCode::OK, "upload {n}: {body}");
    }
    assert_eq!(names(&dest).len(), 80);
    assert!(
        state.chunked_uploads.open_count() <= 64,
        "kept answers stay bounded"
    );

    // Uploads left open do reach it, and the refusal says so.
    let mut open = Vec::new();
    let refused = loop {
        let (status, body) =
            begin(&state, json!({"dir": dest, "name": "open.bin", "size": 10})).await;
        if status != StatusCode::OK {
            break (status, body);
        }
        open.push(body["id"].as_str().unwrap().to_string());
        assert!(open.len() <= 32, "more than the limit were opened");
    };
    assert_eq!(open.len(), 32);
    assert_eq!(refused.0, StatusCode::TOO_MANY_REQUESTS, "{}", refused.1);
}

#[tokio::test]
async fn chunked_upload_routes_require_auth_and_one_destination() {
    let state = test_state();
    let dest = test_dir("chunked-auth");
    let res = app(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/uploads")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"dir": dest, "name": "a", "size": 1}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    for bad in [
        json!({"name": "a", "size": 1}),
        json!({"dir": dest, "session": "s", "name": "a", "size": 1}),
        json!({"dir": dest, "name": "../a", "size": 1}),
        json!({"dir": dest.join("missing"), "name": "a", "size": 1}),
    ] {
        let (status, body) = begin(&state, bad.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }
    let (status, _) = offset_of(&state, "no-such-upload").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(names(&dest).is_empty());
}

/// A daemon that dies mid-upload (a crash, a job at its time limit) leaves a
/// hidden temp file in the user's folder; the next daemon removes it from
/// the record the first one kept — and only ever a file of that shape.
#[tokio::test]
async fn the_next_daemon_removes_what_an_interrupted_upload_left() {
    let data = test_dir("chunked-restart-data");
    let dest = test_dir("chunked-restart-dest");
    let first = test_state_with_data_dir(0, data.clone());
    let source = pattern(64 * 1024);
    let id = begin_in_dir(&first, &dest, "big.bin", source.len() * 4).await;
    let (status, _) = put(&first, &id, 0, &source).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(temps(&dest).len(), 1);
    assert_eq!(open_records(&first).len(), 1);
    drop(first);

    // A damaged record must never point the sweep at a real file.
    let keep = dest.join("important.txt");
    std::fs::write(&keep, "keep me").unwrap();
    let records = data.join("uploads-open");
    std::fs::write(
        records.join("deadbeef.json"),
        json!({"tmp": keep}).to_string(),
    )
    .unwrap();

    let second = test_state_with_data_dir(0, data);
    upload::chunked::spawn_boot_sweep(second.clone());
    for _ in 0..200 {
        if open_records(&second).is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        open_records(&second).is_empty(),
        "{:?}",
        open_records(&second)
    );
    assert_eq!(
        names(&dest),
        vec!["important.txt"],
        "the temp file went, the user's file stayed"
    );
}
