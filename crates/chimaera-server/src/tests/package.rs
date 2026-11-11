//! Big-file saves: `POST /api/v1/fs/package` (an Office package edit that
//! sends only the changed parts) and `PUT /api/v1/fs/file/stream` (a whole
//! file streamed to disk). Both answer like `PUT /fs/file`. The splice's own
//! byte-for-byte guarantees are unit-tested in `fs/zip_splice.rs`.

use std::io::{Cursor, Read, Write};

use super::support::*;
use crate::fs::sha256_hex;
use crate::{app, AppState};

/// Names in `dir` other than `keep` — a leftover temp shows up here.
fn strays(dir: &std::path::Path, keep: &[&str]) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| !keep.contains(&n.as_str()))
        .collect()
}

fn deck() -> Vec<u8> {
    use zip::write::SimpleFileOptions;
    use zip::CompressionMethod;
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    zip.start_file("[Content_Types].xml", stored).unwrap();
    zip.write_all(b"<Types/>").unwrap();
    zip.start_file("ppt/slides/slide1.xml", deflated).unwrap();
    zip.write_all(b"<p:sld>one</p:sld>").unwrap();
    zip.start_file("ppt/media/image1.png", stored).unwrap();
    zip.write_all(&[0x89, b'P', b'N', b'G', 9, 9, 9]).unwrap();
    zip.finish().unwrap().into_inner()
}

fn entry(bytes: &[u8], name: &str) -> Vec<u8> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut out = Vec::new();
    zip.by_name(name).unwrap().read_to_end(&mut out).unwrap();
    out
}

fn package_uri(path: &std::path::Path, extra: &str) -> String {
    format!(
        "/api/v1/fs/package?path={}{extra}",
        urlencode(&path.to_string_lossy())
    )
}

fn stream_uri(path: &std::path::Path, extra: &str) -> String {
    format!(
        "/api/v1/fs/file/stream?path={}{extra}",
        urlencode(&path.to_string_lossy())
    )
}

fn edit(name: &str, data: &[u8]) -> Vec<u8> {
    use base64::Engine as _;
    serde_json::json!({"entries": [{
        "name": name,
        "data": base64::engine::general_purpose::STANDARD.encode(data),
    }]})
    .to_string()
    .into_bytes()
}

async fn send(
    state: &Arc<AppState>,
    method: Method,
    uri: &str,
    body: Body,
    token: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let res = app(state.clone())
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, headers, json)
}

async fn post(
    state: &Arc<AppState>,
    uri: &str,
    body: Vec<u8>,
) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
    send(
        state,
        Method::POST,
        uri,
        Body::from(body),
        Some("test-token"),
    )
    .await
}

async fn put(
    state: &Arc<AppState>,
    uri: &str,
    body: Vec<u8>,
) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
    send(
        state,
        Method::PUT,
        uri,
        Body::from(body),
        Some("test-token"),
    )
    .await
}

/// The `X-Mtime` a whole-file read reports now.
async fn disk_mtime(state: &Arc<AppState>, path: &std::path::Path) -> String {
    let uri = format!(
        "/api/v1/fs/file?path={}",
        urlencode(&path.to_string_lossy())
    );
    let (_, headers, _) = request_bytes(state, Method::GET, &uri, Some("test-token")).await;
    header_str(&headers, "x-mtime").to_string()
}

/// A save replaces the one part, reports the new version, and chains.
#[tokio::test]
async fn fs_package_replaces_an_entry_and_chains() {
    let state = test_state();
    let root = test_dir("package");
    let path = root.join("deck.pptx");
    let original = deck();
    std::fs::write(&path, &original).unwrap();

    let expect = format!("&expect_hash={}", sha256_hex(&original));
    let (status, headers, _) = post(
        &state,
        &package_uri(&path, &expect),
        edit("ppt/slides/slide1.xml", b"<p:sld>edited</p:sld>"),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let saved = std::fs::read(&path).unwrap();
    assert_eq!(header_str(&headers, "x-content-hash"), sha256_hex(&saved));
    assert_eq!(
        header_str(&headers, "x-mtime"),
        disk_mtime(&state, &path).await
    );
    assert_eq!(
        entry(&saved, "ppt/slides/slide1.xml"),
        b"<p:sld>edited</p:sld>"
    );
    assert_eq!(entry(&saved, "[Content_Types].xml"), b"<Types/>");
    assert_eq!(
        entry(&saved, "ppt/media/image1.png"),
        entry(&original, "ppt/media/image1.png")
    );

    // The returned hash is the next save's precondition.
    let next = format!("&expect_hash={}", header_str(&headers, "x-content-hash"));
    let (status, _, _) = post(
        &state,
        &package_uri(&path, &next),
        edit("[Content_Types].xml", b"<Types></Types>"),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let saved = std::fs::read(&path).unwrap();
    assert_eq!(entry(&saved, "[Content_Types].xml"), b"<Types></Types>");
    assert_eq!(
        entry(&saved, "ppt/slides/slide1.xml"),
        b"<p:sld>edited</p:sld>"
    );
    assert!(strays(&root, &["deck.pptx"]).is_empty());
}

/// A stale save is refused with the disk's current version, and a missing
/// file is a conflict too.
#[tokio::test]
async fn fs_package_conflicts_when_the_disk_moved_on() {
    let state = test_state();
    let root = test_dir("package-409");
    let path = root.join("deck.pptx");
    let original = deck();
    std::fs::write(&path, &original).unwrap();

    let stale = format!("&expect_hash={}", sha256_hex(b"what the client opened"));
    let (status, headers, body) = post(
        &state,
        &package_uri(&path, &stale),
        edit("ppt/slides/slide1.xml", b"x"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body, serde_json::json!({"error": "file changed on disk"}));
    assert_eq!(
        header_str(&headers, "x-content-hash"),
        sha256_hex(&original)
    );
    assert_eq!(
        header_str(&headers, "x-mtime"),
        disk_mtime(&state, &path).await
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);

    let gone = root.join("gone.pptx");
    let (status, headers, _) = post(
        &state,
        &package_uri(&gone, &stale),
        edit("ppt/slides/slide1.xml", b"x"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(headers.get("x-content-hash").is_none());
    assert!(!gone.exists());
    assert!(strays(&root, &["deck.pptx"]).is_empty());
}

/// Requests and archives the splice will not take are a 400 with an error
/// the UI can show, and the file is left alone.
#[tokio::test]
async fn fs_package_refusals_are_400_with_a_reason() {
    let state = test_state();
    let root = test_dir("package-400");
    let path = root.join("deck.pptx");
    let original = deck();
    std::fs::write(&path, &original).unwrap();
    let expect = format!("&expect_hash={}", sha256_hex(&original));

    let (status, _, body) = post(
        &state,
        &package_uri(&path, ""),
        edit("ppt/slides/slide1.xml", b"x"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"].as_str().unwrap().contains("expect_hash"),
        "{body}"
    );

    let cases: [(Vec<u8>, &str); 5] = [
        (edit("ppt/slides/slide9.xml", b"x"), "no entry"),
        (edit("../escape.xml", b"x"), "invalid entry name"),
        (b"{\"entries\":".to_vec(), "invalid request body"),
        (
            br#"{"entries":[{"name":"ppt/slides/slide1.xml","data":"not base64!"}]}"#.to_vec(),
            "invalid base64",
        ),
        (
            serde_json::json!({"entries": (0..65)
                .map(|i| serde_json::json!({"name": format!("e{i}.xml"), "data": ""}))
                .collect::<Vec<_>>()})
            .to_string()
            .into_bytes(),
            "too many",
        ),
    ];
    for (body, why) in cases {
        let (status, _, out) = post(&state, &package_uri(&path, &expect), body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}");
        assert!(out["error"].as_str().unwrap().contains(why), "{why}: {out}");
    }

    // Not an archive at all.
    let text = root.join("notes.pptx");
    std::fs::write(&text, b"plain text, not a zip").unwrap();
    let expect_text = format!("&expect_hash={}", sha256_hex(b"plain text, not a zip"));
    let (status, _, out) = post(
        &state,
        &package_uri(&text, &expect_text),
        edit("ppt/slides/slide1.xml", b"x"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        out["error"].as_str().unwrap().contains("not a zip"),
        "{out}"
    );

    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(strays(&root, &["deck.pptx", "notes.pptx"]).is_empty());
}

/// Over the body cap, and a file over the package cap (sparse: no real
/// bytes), are both 413.
#[tokio::test]
async fn fs_package_caps_are_413() {
    let state = test_state();
    let root = test_dir("package-413");
    let path = root.join("deck.pptx");
    let original = deck();
    std::fs::write(&path, &original).unwrap();
    let expect = format!("&expect_hash={}", sha256_hex(&original));

    let body = vec![b' '; crate::fs::MAX_PACKAGE_BODY_BYTES + 1];
    let (status, _, _) = post(&state, &package_uri(&path, &expect), body).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

    let huge = root.join("huge.pptx");
    std::fs::File::create(&huge)
        .unwrap()
        .set_len(crate::fs::MAX_PACKAGE_BYTES + 1)
        .unwrap();
    let (status, _, out) = post(
        &state,
        &package_uri(&huge, &expect),
        edit("ppt/slides/slide1.xml", b"x"),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(
        out["error"].as_str().unwrap().contains("too large"),
        "{out}"
    );
    assert!(strays(&root, &["deck.pptx", "huge.pptx"]).is_empty());
}

/// Both routes sit behind the bearer, like `/fs/file`.
#[tokio::test]
async fn big_writes_need_the_bearer() {
    let state = test_state();
    let root = test_dir("package-auth");
    let path = root.join("deck.pptx");
    std::fs::write(&path, deck()).unwrap();
    let uri = package_uri(&path, "&expect_hash=00");
    let (status, _, _) = send(&state, Method::POST, &uri, Body::from("{}"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = send(
        &state,
        Method::PUT,
        &stream_uri(&path, ""),
        Body::from("x"),
        Some("wrong"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(std::fs::read(&path).unwrap(), deck());
}

/// The streamed write: create, chain on the hash, retry is a no-op, a
/// stale or missing-file precondition is a 409 that leaves no temp.
#[tokio::test]
async fn fs_file_stream_writes_chains_and_conflicts() {
    let state = test_state();
    let root = test_dir("stream");
    let path = root.join("deck.pptx");

    // A body in several chunks, as a real upload arrives.
    let chunks: Vec<Result<bytes::Bytes, std::io::Error>> = vec![
        Ok(bytes::Bytes::from_static(b"first ")),
        Ok(bytes::Bytes::from_static(b"version")),
    ];
    let body = Body::from_stream(futures::stream::iter(chunks));
    let (status, headers, _) = send(
        &state,
        Method::PUT,
        &stream_uri(&path, ""),
        body,
        Some("test-token"),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(std::fs::read(&path).unwrap(), b"first version");
    let h1 = header_str(&headers, "x-content-hash").to_string();
    assert_eq!(h1, sha256_hex(b"first version"));
    assert_eq!(
        header_str(&headers, "x-mtime"),
        disk_mtime(&state, &path).await
    );

    let expect = format!("&expect_hash={h1}");
    let (status, headers, _) = put(&state, &stream_uri(&path, &expect), b"second".to_vec()).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        header_str(&headers, "x-content-hash"),
        sha256_hex(b"second")
    );
    #[cfg(unix)]
    let ino = std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&path).unwrap());

    // A lost reply's retry: the disk already holds the body.
    let (status, _, _) = put(&state, &stream_uri(&path, &expect), b"second".to_vec()).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    #[cfg(unix)]
    assert_eq!(
        std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&path).unwrap()),
        ino
    );

    let (status, headers, body) = put(&state, &stream_uri(&path, &expect), b"third".to_vec()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body, serde_json::json!({"error": "file changed on disk"}));
    assert_eq!(
        header_str(&headers, "x-content-hash"),
        sha256_hex(b"second")
    );
    assert_eq!(
        header_str(&headers, "x-mtime"),
        disk_mtime(&state, &path).await
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"second");

    let gone = root.join("gone.pptx");
    let (status, _, _) = put(&state, &stream_uri(&gone, &expect), b"x".to_vec()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(!gone.exists());
    assert!(strays(&root, &["deck.pptx"]).is_empty());
}

/// A hard-linked target is rewritten in place by both routes, so every
/// name sees the save.
// Windows saves always rename (`fs::hard_linked`).
#[cfg(unix)]
#[tokio::test]
async fn big_writes_rewrite_hard_links_in_place() {
    use std::os::unix::fs::MetadataExt;
    let state = test_state();
    let root = test_dir("big-hardlink");
    let path = root.join("deck.pptx");
    let other = root.join("alias.pptx");
    let original = deck();
    std::fs::write(&path, &original).unwrap();
    std::fs::hard_link(&path, &other).unwrap();
    let ino = std::fs::metadata(&path).unwrap().ino();

    let expect = format!("&expect_hash={}", sha256_hex(&original));
    let (status, headers, _) = post(
        &state,
        &package_uri(&path, &expect),
        edit("ppt/slides/slide1.xml", b"<p:sld>linked</p:sld>"),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let saved = std::fs::read(&other).unwrap();
    assert_eq!(header_str(&headers, "x-content-hash"), sha256_hex(&saved));
    assert_eq!(
        entry(&saved, "ppt/slides/slide1.xml"),
        b"<p:sld>linked</p:sld>"
    );

    let expect = format!("&expect_hash={}", sha256_hex(&saved));
    let (status, _, _) = put(&state, &stream_uri(&path, &expect), b"short".to_vec()).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(std::fs::read(&other).unwrap(), b"short");
    let meta = std::fs::metadata(&path).unwrap();
    assert_eq!((meta.ino(), meta.nlink()), (ino, 2));
    assert!(strays(&root, &["deck.pptx", "alias.pptx"]).is_empty());
}

/// Past the cap the upload is cut off: 413, the old file untouched, no temp
/// left behind. Driven through the handler with a small cap — the real one
/// is 256 MiB.
#[tokio::test]
async fn fs_file_stream_over_the_cap_is_413_and_leaves_no_temp() {
    use axum::extract::{Query, State};
    use axum::response::IntoResponse;

    let state = test_state();
    let root = test_dir("stream-413");
    let path = root.join("deck.pptx");
    std::fs::write(&path, b"old").unwrap();

    let chunk = bytes::Bytes::from(vec![7u8; 1024]);
    let chunks = (0..8).map(move |_| Ok::<_, std::io::Error>(chunk.clone()));
    let body = Body::from_stream(futures::stream::iter(chunks));
    let query: crate::fs::BigWriteQuery =
        serde_json::from_value(serde_json::json!({"path": path.to_string_lossy()})).unwrap();
    let response = crate::fs::stream_file(State(state.clone()), Query(query), body, 4096)
        .await
        .into_response();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(std::fs::read(&path).unwrap(), b"old");
    assert!(strays(&root, &["deck.pptx"]).is_empty());
}
