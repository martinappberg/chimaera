use super::support::*;
use crate::*;

/// Mint a ticket for `path` via the bearer-authed endpoint.
async fn mint_ticket(state: &Arc<AppState>, path: &std::path::Path) -> String {
    let (status, json) = request(
        state,
        Method::POST,
        "/api/v1/fs/ticket",
        Some(serde_json::json!({ "path": path.to_string_lossy() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    json["ticket"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn download_file_streams_attachment_with_unicode_name() {
    let state = test_state();
    let root = test_dir("dl-file");
    let file = root.join("å plot.png");
    std::fs::write(&file, b"not really a png").unwrap();

    let ticket = mint_ticket(&state, &file).await;
    // No Authorization header — the ticket IS the capability (an <a href>
    // navigation cannot send headers).
    let (status, headers, bytes) =
        request_bytes(&state, Method::GET, &format!("/download/{ticket}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&bytes[..], b"not really a png");
    assert_eq!(header_str(&headers, "content-length"), "16");
    assert_eq!(header_str(&headers, "content-type"), "image/png");
    let disposition = header_str(&headers, "content-disposition");
    // ASCII fallback plus the exact name in RFC 5987 form.
    assert!(
        disposition.starts_with("attachment; filename=\"_ plot.png\""),
        "{disposition}"
    );
    assert!(
        disposition.contains("filename*=UTF-8''%C3%A5%20plot.png"),
        "{disposition}"
    );

    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn fs_ticket_accepts_dirs_but_raw_stays_file_only() {
    let state = test_state();
    let root = test_dir("dl-rawguard");
    std::fs::write(root.join("f.txt"), "x").unwrap();

    let ticket = mint_ticket(&state, &root).await;
    let (status, _, _) = request_bytes(&state, Method::GET, &format!("/raw/{ticket}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) =
        request_bytes(&state, Method::GET, &format!("/download/{ticket}"), None).await;
    assert_eq!(status, StatusCode::OK);

    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn download_dir_zips_recursively_skipping_symlinks() {
    let state = test_state();
    let base = test_dir("dl-zip");
    let proj = base.join("proj");
    std::fs::create_dir_all(proj.join("sub")).unwrap();
    std::fs::create_dir_all(proj.join("empty")).unwrap();
    std::fs::write(proj.join("file.txt"), "round trip me").unwrap();
    std::fs::write(proj.join("sub/nested.bin"), vec![7u8; 4096]).unwrap();
    std::os::unix::fs::symlink(proj.join("file.txt"), proj.join("link")).unwrap();

    let ticket = mint_ticket(&state, &proj).await;
    let (status, headers, bytes) =
        request_bytes(&state, Method::GET, &format!("/download/{ticket}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header_str(&headers, "content-type"), "application/zip");
    assert!(
        header_str(&headers, "content-disposition").contains("filename=\"proj.zip\""),
        "{}",
        header_str(&headers, "content-disposition")
    );
    assert_eq!(&bytes[..4], b"PK\x03\x04");

    // Parse the archive back with async_zip's read side.
    let reader = async_zip::base::read::mem::ZipFileReader::new(bytes.to_vec())
        .await
        .expect("valid zip");
    let names: Vec<String> = reader
        .file()
        .entries()
        .iter()
        .map(|e| e.filename().as_str().unwrap().to_string())
        .collect();
    // Walk order is filesystem-dependent: assert membership, not order.
    for expected in [
        "proj/",
        "proj/file.txt",
        "proj/sub/",
        "proj/sub/nested.bin",
        "proj/empty/",
    ] {
        assert!(
            names.contains(&expected.to_string()),
            "{expected} missing from {names:?}"
        );
    }
    // The symlink never enters the archive — not followed, not stored.
    assert!(!names.iter().any(|n| n.contains("link")), "{names:?}");
    assert_eq!(names.len(), 5, "{names:?}");

    // Content survives the deflate round trip.
    let index = names.iter().position(|n| n == "proj/file.txt").unwrap();
    let mut entry = reader.reader_with_entry(index).await.unwrap();
    let mut content = Vec::new();
    entry.read_to_end_checked(&mut content).await.unwrap();
    assert_eq!(&content[..], b"round trip me");

    // Real mtimes ride into the archive — an extracted file must never read
    // as 1980 (the unstamped DOS epoch).
    let year = reader.file().entries()[index]
        .last_modification_date()
        .year();
    assert!(year >= 2026, "zip entry stamped {year}");

    std::fs::remove_dir_all(&base).ok();
}

#[tokio::test]
async fn download_expired_or_unknown_ticket_is_404() {
    let state = test_state();
    let root = test_dir("dl-expired");
    let file = root.join("f.txt");
    std::fs::write(&file, "x").unwrap();

    let ticket = mint_ticket(&state, &file).await;
    lock(&state.tickets).expire(&ticket);
    let (status, _, _) =
        request_bytes(&state, Method::GET, &format!("/download/{ticket}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = request_bytes(&state, Method::GET, "/download/t-nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    std::fs::remove_dir_all(&root).ok();
}
