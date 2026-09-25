//! Save safety (`GET/PUT /api/v1/fs/file`): content hashes on whole-file
//! reads, the `expect_hash` precondition with its idempotent retry, and the
//! hardened write path (mode, owner, hard links, symlinks, temp hygiene).

use std::os::unix::fs::{MetadataExt, PermissionsExt};

use super::support::*;
use crate::fs::sha256_hex;

fn file_uri(path: &std::path::Path, extra: &str) -> String {
    format!("/api/v1/fs/file?path={}{extra}", path.to_string_lossy())
}

/// Names in `dir` other than `keep` — a leftover temp shows up here.
fn strays(dir: &std::path::Path, keep: &[&str]) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| !keep.contains(&n.as_str()))
        .collect()
}

#[test]
fn sha256_hex_is_lowercase_sha256() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

/// `X-Content-Hash` rides only a body that IS the whole raw file.
#[tokio::test]
async fn fs_file_hash_only_for_the_whole_raw_file() {
    let state = test_state();
    let root = test_dir("hash-read");
    let path = root.join("notes.txt");
    std::fs::write(&path, "0123456789").unwrap();

    let (status, headers, body) = request_bytes(
        &state,
        Method::GET,
        &file_uri(&path, ""),
        Some("test-token"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"0123456789");
    assert_eq!(
        header_str(&headers, "x-content-hash"),
        sha256_hex(b"0123456789")
    );
    // A limit that exactly covers the file is still the whole file.
    let (_, headers, _) = request_bytes(
        &state,
        Method::GET,
        &file_uri(&path, "&limit=10"),
        Some("test-token"),
    )
    .await;
    assert_eq!(
        header_str(&headers, "x-content-hash"),
        sha256_hex(b"0123456789")
    );

    // A head slice, a tail slice: no hash.
    for extra in ["&limit=4", "&offset=3"] {
        let (status, headers, _) = request_bytes(
            &state,
            Method::GET,
            &file_uri(&path, extra),
            Some("test-token"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(headers.get("x-content-hash").is_none(), "{extra}");
    }

    // An empty file hashes as empty.
    let empty = root.join("empty.txt");
    std::fs::write(&empty, "").unwrap();
    let (_, headers, _) = request_bytes(
        &state,
        Method::GET,
        &file_uri(&empty, ""),
        Some("test-token"),
    )
    .await;
    assert_eq!(header_str(&headers, "x-content-hash"), sha256_hex(b""));
    assert_eq!(header_str(&headers, "x-file-size"), "0");

    // A gzip body is decompressed, not the raw file: no hash.
    let gz = root.join("t.txt.gz");
    std::fs::write(&gz, gzip_bytes(b"inner", None)).unwrap();
    let (status, headers, body) =
        request_bytes(&state, Method::GET, &file_uri(&gz, ""), Some("test-token")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"inner");
    assert!(headers.get("x-content-hash").is_none());
}

/// The `expect_hash` protocol end to end: a save chains on the returned
/// hash, a retry after a lost reply is a no-op success, a foreign write is a
/// 409 that describes the disk, and `expect_hash` wins over `expect_mtime`.
#[tokio::test]
async fn fs_put_file_expect_hash_chain_retry_and_conflict() {
    let state = test_state();
    let root = test_dir("hash-put");
    let path = root.join("doc.md");

    let (status, headers, _) = put_raw(&state, &file_uri(&path, ""), b"v1".to_vec()).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let h1 = header_str(&headers, "x-content-hash").to_string();
    assert_eq!(h1, sha256_hex(b"v1"));
    assert!(headers.get("x-mtime").is_some());

    let (_, headers, _) = request_bytes(
        &state,
        Method::GET,
        &file_uri(&path, ""),
        Some("test-token"),
    )
    .await;
    assert_eq!(header_str(&headers, "x-content-hash"), h1);

    // Chained save (the hash is accepted in any case).
    let expect = format!("&expect_hash={}", h1.to_ascii_uppercase());
    let (status, headers, _) = put_raw(&state, &file_uri(&path, &expect), b"v2".to_vec()).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(header_str(&headers, "x-content-hash"), sha256_hex(b"v2"));
    let mtime2 = header_str(&headers, "x-mtime").to_string();
    let ino2 = std::fs::metadata(&path).unwrap().ino();
    assert_eq!(std::fs::read(&path).unwrap(), b"v2");

    // The reply was lost; the client retries the same PUT. The disk already
    // holds exactly these bytes: success, and nothing is rewritten.
    let (status, headers, _) = put_raw(&state, &file_uri(&path, &expect), b"v2".to_vec()).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(header_str(&headers, "x-content-hash"), sha256_hex(b"v2"));
    assert_eq!(header_str(&headers, "x-mtime"), mtime2);
    assert_eq!(std::fs::metadata(&path).unwrap().ino(), ino2);

    // Someone else writes; our stale save is refused with the disk's state.
    std::fs::write(&path, "external").unwrap();
    let stale = format!("&expect_hash={}", sha256_hex(b"v2"));
    let (status, headers, body) = put_raw(&state, &file_uri(&path, &stale), b"v3".to_vec()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let err: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(err, serde_json::json!({"error": "file changed on disk"}));
    assert_eq!(
        header_str(&headers, "x-content-hash"),
        sha256_hex(b"external")
    );
    let disk_mtime = header_str(&headers, "x-mtime").to_string();
    let (_, headers, _) = request_bytes(
        &state,
        Method::GET,
        &file_uri(&path, ""),
        Some("test-token"),
    )
    .await;
    assert_eq!(header_str(&headers, "x-mtime"), disk_mtime);
    assert_eq!(std::fs::read(&path).unwrap(), b"external");

    // expect_hash wins over a (bogus) expect_mtime.
    let both = format!(
        "&expect_mtime=12345&expect_hash={}",
        sha256_hex(b"external")
    );
    let (status, _, _) = put_raw(&state, &file_uri(&path, &both), b"v4".to_vec()).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(std::fs::read(&path).unwrap(), b"v4");

    // A missing file under expect_hash is a conflict, and stays missing.
    let gone = root.join("gone.md");
    let (status, headers, _) = put_raw(&state, &file_uri(&gone, &stale), b"x".to_vec()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(headers.get("x-content-hash").is_none());
    assert!(!gone.exists());

    assert!(strays(&root, &["doc.md"]).is_empty());
}

/// The temp is born with the target's mode and gets its exact bits (the
/// umask cannot strip a shared file's group write), and the owner and group
/// are carried over where the daemon may (root here re-owns to a stranger).
#[tokio::test]
async fn fs_put_file_keeps_mode_and_owner() {
    let state = test_state();
    let root = test_dir("put-mode");
    let path = root.join("shared.sh");
    std::fs::write(&path, "old").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o775)).unwrap();
    let foreign = std::os::unix::fs::chown(&path, Some(4242), Some(4343)).is_ok();

    let (status, _, _) = put_raw(&state, &file_uri(&path, ""), b"new".to_vec()).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let meta = std::fs::metadata(&path).unwrap();
    assert_eq!(meta.permissions().mode() & 0o7777, 0o775);
    if foreign {
        // Only a privileged test run can give a file away to check this.
        assert_eq!((meta.uid(), meta.gid()), (4242, 4343));
    }
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
    assert!(strays(&root, &["shared.sh"]).is_empty());
}

/// A hard-linked file is rewritten in place: every name sees the new bytes
/// and the link count survives (a rename would have split the links).
#[tokio::test]
async fn fs_put_file_rewrites_hard_links_in_place() {
    let state = test_state();
    let root = test_dir("put-hardlink");
    let path = root.join("a.txt");
    let other = root.join("b.txt");
    std::fs::write(&path, "a much longer original body").unwrap();
    std::fs::hard_link(&path, &other).unwrap();
    let ino = std::fs::metadata(&path).unwrap().ino();

    let expect = format!(
        "&expect_hash={}",
        sha256_hex(b"a much longer original body")
    );
    let (status, headers, _) = put_raw(&state, &file_uri(&path, &expect), b"short".to_vec()).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(header_str(&headers, "x-content-hash"), sha256_hex(b"short"));
    assert_eq!(std::fs::read(&other).unwrap(), b"short");
    let meta = std::fs::metadata(&path).unwrap();
    assert_eq!((meta.ino(), meta.nlink()), (ino, 2));

    // The same precondition guards the in-place path.
    let (status, _, _) = put_raw(&state, &file_uri(&path, &expect), b"late".to_vec()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(std::fs::read(&other).unwrap(), b"short");
    assert!(strays(&root, &["a.txt", "b.txt"]).is_empty());
}

/// A live symlink is written through (the link survives, its target
/// changes); a dangling one is refused rather than replaced by a file.
#[tokio::test]
async fn fs_put_file_writes_through_live_symlinks_and_refuses_dangling_ones() {
    let state = test_state();
    let root = test_dir("put-symlink");
    let target = root.join("target.txt");
    let link = root.join("link.txt");
    std::fs::write(&target, "old").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let (status, _, _) = put_raw(&state, &file_uri(&link, ""), b"new".to_vec()).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(std::fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(std::fs::read(&target).unwrap(), b"new");

    let dangling = root.join("dangling.txt");
    let missing = root.join("missing.txt");
    std::os::unix::fs::symlink(&missing, &dangling).unwrap();
    let (status, _, body) = put_raw(&state, &file_uri(&dangling, ""), b"x".to_vec()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let err: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(err["error"].as_str().unwrap().contains("symlink"), "{err}");
    assert!(std::fs::symlink_metadata(&dangling)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(!missing.exists());
    assert!(strays(&root, &["target.txt", "link.txt", "dangling.txt"]).is_empty());
}

/// A 250-byte name still saves: the temp's name is shortened to fit NAME_MAX.
#[tokio::test]
async fn fs_put_file_saves_names_near_the_length_limit() {
    let state = test_state();
    let root = test_dir("put-longname");
    let name = format!("{}.md", "é".repeat(123)); // 246 + 3 = 249 bytes
    assert!(name.len() > 240 && name.len() <= 255);
    let path = root.join(&name);

    for body in [&b"first"[..], b"second"] {
        let (status, _, _) = put_raw(&state, &file_uri(&path, ""), body.to_vec()).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(std::fs::read(&path).unwrap(), body);
    }
    assert!(strays(&root, &[name.as_str()]).is_empty());
}

/// A special file is not something to save over.
#[tokio::test]
async fn fs_put_file_refuses_non_regular_files() {
    let state = test_state();
    let (status, _, body) = put_raw(&state, "/api/v1/fs/file?path=/dev/null", b"x".to_vec()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let err: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        err["error"]
            .as_str()
            .unwrap()
            .contains("not a regular file"),
        "{err}"
    );
}
