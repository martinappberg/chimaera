//! POST /sessions/{id}/upload — the streamed, size-capped, session-scoped
//! upload landing pad, and its prune-on-delete lifecycle.

use super::support::*;
use crate::*;

/// POST raw/streamed bytes to the upload route with the test bearer token.
async fn post_upload(
    state: &Arc<AppState>,
    uri: &str,
    body: Body,
) -> (StatusCode, serde_json::Value) {
    let res = app(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(uri)
                .header(header::AUTHORIZATION, "Bearer test-token")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            serde_json::Value::String(String::from_utf8_lossy(&bytes).into_owned())
        })
    };
    (status, json)
}

/// A body that arrives as many separate chunks — exercises the streaming
/// path rather than a single buffered frame.
fn chunked_body(chunk: Vec<u8>, count: usize) -> Body {
    let chunk = bytes::Bytes::from(chunk);
    let chunks: Vec<Result<bytes::Bytes, std::io::Error>> =
        (0..count).map(|_| Ok(chunk.clone())).collect();
    Body::from_stream(futures::stream::iter(chunks))
}

/// A session the upload route recognizes, without spawning a PTY.
fn plant_session(state: &Arc<AppState>) -> String {
    let id = agents::fresh_session_id();
    plant_agent_record(state, &id, "ws-x", agents::AgentKind::Claude, None, None);
    id
}

#[tokio::test]
async fn upload_requires_auth() {
    let state = test_state();
    let id = plant_session(&state);
    let res = app(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/api/v1/sessions/{id}/upload?name=a.txt"))
                .body(Body::from("hi"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn upload_rejects_unknown_sessions() {
    let state = test_state();
    let (status, body) = post_upload(
        &state,
        "/api/v1/sessions/nope/upload?name=a.txt",
        Body::from("hi"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(
        !state.uploads_root.join("nope").exists(),
        "an unknown id must never mint a directory"
    );
}

#[tokio::test]
async fn upload_streams_multi_chunk_bodies_into_the_session_dir() {
    let state = test_state();
    let id = plant_session(&state);
    // 3MB in 1MB chunks: bigger than axum's 2MB buffered-body default, so
    // this also proves the route's body-limit override.
    let chunk = vec![7u8; 1024 * 1024];
    let (status, body) = post_upload(
        &state,
        &format!("/api/v1/sessions/{id}/upload?name=screenshot.png"),
        chunked_body(chunk, 3),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["name"], "screenshot.png");
    assert_eq!(body["size"], 3 * 1024 * 1024);
    let path = PathBuf::from(body["path"].as_str().unwrap());
    assert_eq!(path, state.uploads_root.join(&id).join("screenshot.png"));
    let on_disk = std::fs::read(&path).unwrap();
    assert_eq!(on_disk.len(), 3 * 1024 * 1024);
    assert!(on_disk.iter().all(|b| *b == 7));
}

#[tokio::test]
async fn folder_upload_streams_files_past_the_old_32mb_limit() {
    let state = test_state();
    let dir = test_dir("folder-upload-large");
    let uri = format!(
        "/api/v1/fs/upload?dir={}&name=dataset.bin",
        dir.to_string_lossy()
    );
    let (status, body) = post_upload(&state, &uri, chunked_body(vec![9u8; 1024 * 1024], 33)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["size"], 33 * 1024 * 1024);
    assert_eq!(
        std::fs::metadata(dir.join("dataset.bin")).unwrap().len(),
        33 * 1024 * 1024
    );
}

#[tokio::test]
async fn upload_enforces_the_per_file_cap_and_leaves_nothing_behind() {
    let state = test_state();
    let id = plant_session(&state);
    // One chunk past the session-file cap, streamed so no oversized buffer exists
    // client-side either.
    let chunks = (upload::MAX_SESSION_UPLOAD_FILE_BYTES / (1024 * 1024)) as usize + 1;
    let (status, body) = post_upload(
        &state,
        &format!("/api/v1/sessions/{id}/upload?name=big.bin"),
        chunked_body(vec![0u8; 1024 * 1024], chunks),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    let dir = state.uploads_root.join(&id);
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .map(|it| it.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();
    assert!(
        leftovers.is_empty(),
        "an aborted upload must delete its tmp file: {leftovers:?}"
    );
}

#[tokio::test]
async fn upload_rejects_path_shaped_names() {
    let state = test_state();
    let id = plant_session(&state);
    for name in ["..", "a%2Fb", "%2e%2e%2fetc"] {
        // (the %-escapes decode to "/" and "../" in the query parser)
        let (status, body) = post_upload(
            &state,
            &format!("/api/v1/sessions/{id}/upload?name={name}"),
            Body::from("x"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: {body}");
    }
}

#[tokio::test]
async fn upload_dedupes_name_collisions_instead_of_clobbering() {
    let state = test_state();
    let id = plant_session(&state);
    let uri = format!("/api/v1/sessions/{id}/upload?name=a.txt");
    let (s1, b1) = post_upload(&state, &uri, Body::from("first")).await;
    let (s2, b2) = post_upload(&state, &uri, Body::from("second")).await;
    assert_eq!(s1, StatusCode::OK, "{b1}");
    assert_eq!(s2, StatusCode::OK, "{b2}");
    assert_eq!(b1["name"], "a.txt");
    let second = b2["name"].as_str().unwrap();
    assert_ne!(second, "a.txt", "a taken name must not be clobbered");
    assert!(
        second.ends_with("-a.txt"),
        "dedupe keeps the name visible: {second}"
    );
    let dir = state.uploads_root.join(&id);
    assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"first");
    assert_eq!(std::fs::read(dir.join(second)).unwrap(), b"second");
}

#[tokio::test]
async fn deleting_a_session_prunes_its_uploads() {
    let state = test_state();
    // A real PTY session: DELETE flows through sessions.kill, the arm that
    // prunes shell uploads.
    let id = inject_agent(&state, "key-upload");
    let (status, body) = post_upload(
        &state,
        &format!("/api/v1/sessions/{id}/upload?name=note.txt"),
        Body::from("keep me until the session dies"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let dir = state.uploads_root.join(&id);
    assert!(dir.join("note.txt").is_file());

    let (status, _) = request(
        &state,
        Method::DELETE,
        &format!("/api/v1/sessions/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // The prune is detached (spawn_blocking); poll it out.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while dir.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "uploads dir survived session deletion"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

/// One image block as the browser builds it (plus an optional client path).
fn image_block(data: &str, path: Option<&str>) -> chimaera_agent::model::ContentBlock {
    chimaera_agent::model::ContentBlock::Image {
        media_type: "image/png".into(),
        data: data.into(),
        path: path.map(str::to_string),
    }
}

fn block_paths(cmd: &chimaera_agent::model::AgentCommand) -> Vec<Option<String>> {
    let chimaera_agent::model::AgentCommand::Send { blocks } = cmd else {
        panic!("expected Send, got {cmd:?}");
    };
    blocks
        .iter()
        .filter_map(|b| match b {
            chimaera_agent::model::ContentBlock::Image { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn sent_chat_images_get_a_saved_copy_and_a_client_path_is_never_trusted() {
    let state = test_state();
    let id = plant_session(&state);
    let mut cmd = chimaera_agent::model::AgentCommand::Send {
        blocks: vec![
            chimaera_agent::model::ContentBlock::Text { text: "see".into() },
            // "ABC" — and a forged path the daemon must replace.
            image_block("QUJD", Some("/etc/passwd")),
            // Not base64: still reaches the agent, just not saved.
            image_block("not base64!", Some("/etc/hosts")),
        ],
    };
    let returned = upload::save_send_images(&state, &id, &mut cmd).await;
    let paths = block_paths(&cmd);
    assert_eq!(returned, vec![paths[0].clone().expect("a saved copy")]);
    let saved = PathBuf::from(paths[0].as_deref().expect("a saved copy"));
    assert_eq!(saved.parent().unwrap(), state.uploads_root.join(&id));
    let name = saved.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("image-") && name.ends_with(".png"),
        "{name}"
    );
    assert_eq!(std::fs::read(&saved).unwrap(), b"ABC");
    assert_eq!(
        paths[1], None,
        "an undecodable image drops the forged path too"
    );
    let leftovers: Vec<_> = std::fs::read_dir(state.uploads_root.join(&id))
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(
        leftovers.len(),
        1,
        "no tmp files left behind: {leftovers:?}"
    );
}

#[tokio::test]
async fn sent_chat_images_respect_the_session_file_cap() {
    let state = test_state();
    let id = plant_session(&state);
    let dir = state.uploads_root.join(&id);
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..upload::MAX_SESSION_UPLOAD_FILES {
        std::fs::write(dir.join(format!("drop-{i}.txt")), b"x").unwrap();
    }
    let mut cmd = chimaera_agent::model::AgentCommand::Send {
        blocks: vec![image_block("QUJD", None)],
    };
    upload::save_send_images(&state, &id, &mut cmd).await;
    assert_eq!(block_paths(&cmd), vec![None]);
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        upload::MAX_SESSION_UPLOAD_FILES
    );
}

#[tokio::test]
async fn a_refused_send_takes_its_saved_images_back() {
    let state = test_state();
    let id = plant_session(&state);
    let mut cmd = chimaera_agent::model::AgentCommand::Send {
        blocks: vec![image_block("QUJD", None), image_block("REVG", None)],
    };
    let saved = upload::save_send_images(&state, &id, &mut cmd).await;
    assert_eq!(saved.len(), 2);
    upload::discard_saved_images(saved.clone());
    for _ in 0..200 {
        if saved.iter().all(|p| !std::path::Path::new(p).exists()) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("saved copies still on disk: {saved:?}");
}
