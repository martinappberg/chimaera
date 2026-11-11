use super::support::*;
use crate::*;

#[tokio::test]
async fn environment_round_trip_and_validation() {
    let state = test_state();

    // Fresh daemon: empty map (scopes absent, not null husks).
    let (status, body) = request(&state, Method::GET, "/api/v1/environment", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({}));

    // PUT the whole map; entries are objects (future-proofed for profiles).
    let map = serde_json::json!({
        "host": {"text": "ml bcftools"},
        "workspaces": {"w-abc": {"text": "conda activate hello"}},
    });
    let (status, _) = request(
        &state,
        Method::PUT,
        "/api/v1/environment",
        Some(map.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = request(&state, Method::GET, "/api/v1/environment", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, map);

    // Empty-text entries are normalized away, not persisted as husks.
    let (status, _) = request(
        &state,
        Method::PUT,
        "/api/v1/environment",
        Some(serde_json::json!({
            "host": {"text": "  "},
            "workspaces": {"w-abc": {"text": ""}, "w-keep": {"text": "ml git"}},
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = request(&state, Method::GET, "/api/v1/environment", None).await;
    assert_eq!(
        body,
        serde_json::json!({"workspaces": {"w-keep": {"text": "ml git"}}})
    );

    // Malformed shapes are rejected.
    for bad in [
        serde_json::json!([1, 2]),
        serde_json::json!({"host": "bare string"}),
        serde_json::json!({"host": {"text": "a\u{0000}b"}}),
    ] {
        let (status, _) = request(&state, Method::PUT, "/api/v1/environment", Some(bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    // Oversized scope text → 413.
    let big = "x".repeat(33 * 1024);
    let (status, _) = request(
        &state,
        Method::PUT,
        "/api/v1/environment",
        Some(serde_json::json!({"host": {"text": big}})),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn environment_requires_auth() {
    let state = test_state();
    let app = app(state);
    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/environment")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn environment_hand_edit_on_disk_is_picked_up() {
    let data_dir = test_dir("env-disk");
    let state = test_state_with_data_dir(0, data_dir.clone());

    // Simulate `vim ~/.config/chimaera/env-profiles.json`.
    let path = data_dir.join("config").join("env-profiles.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, r#"{"host": {"text": "module load samtools"}}"#).unwrap();
    lock(&state.env_preludes).force_stale_for_tests();

    let (_, body) = request(&state, Method::GET, "/api/v1/environment", None).await;
    assert_eq!(body["host"]["text"], "module load samtools");

    // Corrupt on-disk content degrades to empty, never an error.
    std::fs::write(&path, "not json").unwrap();
    lock(&state.env_preludes).force_stale_for_tests();
    let (status, body) = request(&state, Method::GET, "/api/v1/environment", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({}));
}

async fn env_of(state: &Arc<AppState>) -> serde_json::Value {
    let (status, body) = request(state, Method::GET, "/api/v1/environment", None).await;
    assert_eq!(status, StatusCode::OK);
    body
}

async fn patch(state: &Arc<AppState>, body: serde_json::Value) -> StatusCode {
    request(state, Method::PATCH, "/api/v1/environment", Some(body))
        .await
        .0
}

fn env_path(data_dir: &std::path::Path) -> PathBuf {
    let path = data_dir.join("config").join("env-profiles.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    path
}

#[tokio::test]
async fn environment_patch_changes_only_the_scopes_it_names() {
    let state = test_state();
    let (status, _) = request(
        &state,
        Method::PUT,
        "/api/v1/environment",
        Some(serde_json::json!({
            "host": {"text": "ml bcftools"},
            "workspaces": {"w-1": {"text": "conda activate one"}, "w-2": {"text": "ml two"}},
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Present replaces; absent (host, w-2) is untouched.
    let status = patch(
        &state,
        serde_json::json!({"workspaces": {"w-1": {"text": "conda activate uno"}}}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        env_of(&state).await,
        serde_json::json!({
            "host": {"text": "ml bcftools"},
            "workspaces": {"w-1": {"text": "conda activate uno"}, "w-2": {"text": "ml two"}},
        })
    );

    // null deletes; an empty body changes nothing.
    assert_eq!(
        patch(&state, serde_json::json!({"host": null})).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        patch(&state, serde_json::json!({"workspaces": {"w-2": null}})).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        patch(&state, serde_json::json!({})).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        env_of(&state).await,
        serde_json::json!({"workspaces": {"w-1": {"text": "conda activate uno"}}})
    );

    // Empty text deletes too (no `{text: ""}` husks); a new scope is added.
    assert_eq!(
        patch(
            &state,
            serde_json::json!({"host": {"text": "ml git"}, "workspaces": {"w-1": {"text": " "}}})
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        env_of(&state).await,
        serde_json::json!({"host": {"text": "ml git"}})
    );
}

#[tokio::test]
async fn environment_patch_validation() {
    let state = test_state();
    assert_eq!(
        patch(&state, serde_json::json!({"host": {"text": "ml keep"}})).await,
        StatusCode::NO_CONTENT
    );
    for bad in [
        serde_json::json!([1, 2]),
        serde_json::json!({"hosts": {"text": "typo"}}),
        serde_json::json!({"host": "bare string"}),
        serde_json::json!({"workspaces": null}),
        serde_json::json!({"host": {"text": "a\u{0000}b"}}),
        serde_json::json!({"workspaces": {"w-1": {"text": "a\u{0000}b"}}}),
    ] {
        assert_eq!(
            patch(&state, bad.clone()).await,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
    let big = "x".repeat(33 * 1024);
    assert_eq!(
        patch(
            &state,
            serde_json::json!({"workspaces": {"w-1": {"text": big}}})
        )
        .await,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let huge = "x".repeat(300 * 1024);
    assert_eq!(
        patch(&state, serde_json::json!({"host": {"text": huge}})).await,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        env_of(&state).await,
        serde_json::json!({"host": {"text": "ml keep"}})
    );

    // Scopes that each fit can still add up past the whole-store cap a read
    // enforces: that patch is refused and the store stays as it was.
    let scope = "y".repeat(32_000);
    let mut refused = None;
    for i in 0..10 {
        let status = patch(
            &state,
            serde_json::json!({"workspaces": {format!("w-{i}"): {"text": scope}}}),
        )
        .await;
        if status != StatusCode::NO_CONTENT {
            refused = Some((i, status));
            break;
        }
    }
    let (at, status) = refused.expect("the store cap was never reached");
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    lock(&state.env_preludes).force_stale_for_tests();
    let body = env_of(&state).await;
    assert_eq!(body["workspaces"].as_object().unwrap().len(), at);
    assert_eq!(body["host"]["text"], "ml keep");
}

/// Two windows saving different scopes at the same time both keep their
/// edit: the read-modify-write runs one at a time under the store's gate.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn environment_interleaved_patches_converge() {
    let state = test_state();
    let mut tasks = Vec::new();
    for i in 0..16 {
        let state = state.clone();
        tasks.push(tokio::spawn(async move {
            let body = if i == 0 {
                serde_json::json!({"host": {"text": "ml host"}})
            } else {
                serde_json::json!({"workspaces": {format!("w-{i}"): {"text": format!("ml {i}")}}})
            };
            patch(&state, body).await
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap(), StatusCode::NO_CONTENT);
    }
    lock(&state.env_preludes).force_stale_for_tests();
    let body = env_of(&state).await;
    assert_eq!(body["host"]["text"], "ml host");
    let workspaces = body["workspaces"].as_object().unwrap();
    assert_eq!(workspaces.len(), 15);
    for i in 1..16 {
        assert_eq!(workspaces[&format!("w-{i}")]["text"], format!("ml {i}"));
    }
}

/// A PATCH re-reads the file first, so a hand-edit since the last read is
/// kept; a file it can't read is never overwritten.
#[tokio::test]
async fn environment_patch_keeps_hand_edits_and_never_overwrites_an_unreadable_file() {
    let data_dir = test_dir("env-patch-disk");
    let state = test_state_with_data_dir(0, data_dir.clone());
    let path = env_path(&data_dir);
    std::fs::write(&path, r#"{"host": {"text": "module load samtools"}}"#).unwrap();
    lock(&state.env_preludes).force_stale_for_tests();
    assert_eq!(
        patch(
            &state,
            serde_json::json!({"workspaces": {"w-1": {"text": "ml one"}}})
        )
        .await,
        StatusCode::NO_CONTENT
    );
    let on_disk: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(on_disk["host"]["text"], "module load samtools");
    assert_eq!(on_disk["workspaces"]["w-1"]["text"], "ml one");

    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    lock(&state.env_preludes).force_stale_for_tests();
    assert_eq!(
        patch(&state, serde_json::json!({"host": null})).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(path.is_dir());
    // GET keeps serving the last good read meanwhile.
    assert_eq!(env_of(&state).await["host"]["text"], "module load samtools");
}

#[tokio::test]
async fn environment_file_states_on_read() {
    let data_dir = test_dir("env-states");
    let path = env_path(&data_dir);
    std::fs::write(
        &path,
        format!(r#"{{"host": {{"text": "{}"}}}}"#, "x".repeat(300 * 1024)),
    )
    .unwrap();
    let state = test_state_with_data_dir(0, data_dir.clone());
    assert_eq!(env_of(&state).await, serde_json::json!({}));

    std::fs::write(&path, r#"{"host": {"text": "ml a"}}"#).unwrap();
    lock(&state.env_preludes).force_stale_for_tests();
    assert_eq!(env_of(&state).await["host"]["text"], "ml a");

    // Grows past the cap between the stat and the read: still refused.
    let grown = path.clone();
    let once = Arc::new(std::sync::atomic::AtomicBool::new(false));
    lock(&state.env_preludes).set_before_read_for_tests(Arc::new(move || {
        if !once.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let big = format!(r#"{{"host": {{"text": "{}"}}}}"#, "x".repeat(300 * 1024));
            std::fs::write(&grown, big).unwrap();
        }
    }));
    std::fs::write(&path, r#"{"host": {"text": "ml b"}}"#).unwrap();
    lock(&state.env_preludes).force_stale_for_tests();
    assert_eq!(env_of(&state).await, serde_json::json!({}));

    std::fs::remove_file(&path).unwrap();
    lock(&state.env_preludes).force_stale_for_tests();
    assert_eq!(env_of(&state).await, serde_json::json!({}));
}

/// With env-profiles.json's read parked on a hung disk, a single-threaded
/// runtime still answers health and the settings GET at once.
#[tokio::test(flavor = "current_thread")]
async fn environment_read_on_slow_storage_leaves_the_runtime_free() {
    let data_dir = test_dir("env-slow");
    let path = env_path(&data_dir);
    let state = test_state_with_data_dir(0, data_dir.clone());

    let stall = Stall::new();
    lock(&state.env_preludes).set_before_read_for_tests(stall.hook());
    std::fs::write(&path, r#"{"host": {"text": "ml slow"}}"#).unwrap();
    lock(&state.env_preludes).force_stale_for_tests();
    let parked = tokio::spawn({
        let state = state.clone();
        async move { request(&state, Method::GET, "/api/v1/environment", None).await }
    });
    stall.wait_entered().await;
    // A write queues behind the parked read (one disk job per store).
    let queued = tokio::spawn({
        let state = state.clone();
        async move {
            patch(
                &state,
                serde_json::json!({"workspaces": {"w": {"text": "ml w"}}}),
            )
            .await
        }
    });

    let started = std::time::Instant::now();
    let (status, _) = request(&state, Method::GET, "/api/v1/health", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request(&state, Method::GET, "/api/v1/settings", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "other routes waited on the stalled environment read"
    );
    assert!(!parked.is_finished());
    assert!(!queued.is_finished());

    stall.release();
    let (status, body) = parked.await.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["host"]["text"], "ml slow");
    assert_eq!(queued.await.unwrap(), StatusCode::NO_CONTENT);
    let body = env_of(&state).await;
    assert_eq!(body["host"]["text"], "ml slow");
    assert_eq!(body["workspaces"]["w"]["text"], "ml w");
}

/// Deleting a workspace prunes its prelude through the store's gate: it
/// waits behind a PATCH already in flight and runs after it, so that
/// patch's write can never put the deleted entry back.
#[tokio::test]
async fn environment_workspace_delete_never_resurrects_under_a_concurrent_patch() {
    let data_dir = test_dir("env-delete");
    let path = env_path(&data_dir);
    let state = test_state_with_data_dir(0, data_dir.clone());
    let (status, _) = request(
        &state,
        Method::PUT,
        "/api/v1/environment",
        Some(serde_json::json!({
            "host": {"text": "ml host"},
            "workspaces": {"w-gone": {"text": "ml gone"}, "w-keep": {"text": "ml keep"}},
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let stall = Stall::new();
    lock(&state.env_preludes).set_before_read_for_tests(stall.hook());
    lock(&state.env_preludes).force_stale_for_tests();
    let patching = tokio::spawn({
        let state = state.clone();
        async move { patch(&state, serde_json::json!({"host": {"text": "ml host 2"}})).await }
    });
    stall.wait_entered().await;
    let removing = tokio::spawn({
        let state = state.clone();
        async move { crate::environment::remove_workspace(&state, "w-gone").await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(!removing.is_finished(), "the prune skipped the queue");

    stall.release();
    assert_eq!(patching.await.unwrap(), StatusCode::NO_CONTENT);
    removing.await.unwrap();
    let on_disk: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        on_disk,
        serde_json::json!({
            "host": {"text": "ml host 2"},
            "workspaces": {"w-keep": {"text": "ml keep"}},
        })
    );
}

/// One shared poll pass folds a hand-edit of settings.json AND of
/// env-profiles.json into the caches with no route called; until then the
/// readers answer the cache, and a spawn's prelude carries the edit after.
#[tokio::test]
async fn hand_edits_of_both_stores_land_after_one_poll() {
    let data_dir = test_dir("env-poll");
    let env = env_path(&data_dir);
    let settings = data_dir.join("config").join("settings.json");
    let state = test_state_with_data_dir(0, data_dir.clone());
    let session = format!("env-poll-{}", std::process::id());

    std::fs::write(&settings, r#"{"git.path": "/opt/git/bin/git"}"#).unwrap();
    std::fs::write(
        &env,
        r#"{"workspaces": {"w-1": {"text": "ml hand-edited"}}}"#,
    )
    .unwrap();
    lock(&state.settings).force_stale_for_tests();
    lock(&state.env_preludes).force_stale_for_tests();

    // Before the poll: the caches, untouched by the edit.
    assert_eq!(lock(&state.settings).git_path(), None);
    assert!(
        crate::environment::materialize_prelude(&state, &session, "w-1", None, None).is_none(),
        "a spawn read env-profiles.json instead of the cache"
    );

    assert!(crate::settings::poll_external_edit(&state).await);
    assert_eq!(
        lock(&state.settings).git_path().as_deref(),
        Some("/opt/git/bin/git")
    );
    assert_eq!(
        lock(&state.env_preludes).data_cached().workspaces["w-1"].text,
        "ml hand-edited"
    );
    // Preludes are POSIX shell: a Windows spawn never gets one.
    #[cfg(unix)]
    {
        let prelude = crate::environment::materialize_prelude(&state, &session, "w-1", None, None)
            .expect("the polled workspace prelude");
        let text = std::fs::read_to_string(&prelude).unwrap();
        crate::environment::remove_prelude_file(&session);
        assert!(text.contains("ml hand-edited"), "{text}");
    }

    // Settled: the next pass sees both recorded mtimes and does nothing.
    assert!(!crate::settings::poll_external_edit(&state).await);

    // An environment-only edit is reported by the same pass.
    std::fs::write(&env, r#"{"host": {"text": "ml host"}}"#).unwrap();
    lock(&state.env_preludes).force_stale_for_tests();
    assert!(crate::environment::refresh(&state).await);
    assert!(lock(&state.env_preludes)
        .data_cached()
        .workspaces
        .is_empty());
}

/// With env-profiles.json's read parked on a hung disk, a spawn's prelude
/// composes from the cache at once and the settings getters answer.
#[tokio::test(flavor = "current_thread")]
async fn environment_prelude_answers_from_the_cache_while_the_read_is_stalled() {
    let data_dir = test_dir("env-prelude-stalled");
    let path = env_path(&data_dir);
    std::fs::write(&path, r#"{"host": {"text": "ml cached"}}"#).unwrap();
    let state = test_state_with_data_dir(0, data_dir.clone());

    let stall = Stall::new();
    lock(&state.env_preludes).set_before_read_for_tests(stall.hook());
    std::fs::write(&path, r#"{"host": {"text": "ml fresh"}}"#).unwrap();
    lock(&state.env_preludes).force_stale_for_tests();
    let polling = tokio::spawn({
        let state = state.clone();
        async move { crate::settings::poll_external_edit(&state).await }
    });
    stall.wait_entered().await;

    // Preludes are POSIX shell: a Windows spawn never gets one.
    #[cfg(unix)]
    {
        let session = format!("env-prelude-stalled-{}", std::process::id());
        let prelude = crate::environment::materialize_prelude(&state, &session, "w-1", None, None)
            .expect("the cached host prelude");
        let text = std::fs::read_to_string(&prelude).unwrap();
        crate::environment::remove_prelude_file(&session);
        assert!(text.contains("ml cached"), "{text}");
    }
    assert_eq!(lock(&state.settings).scrollback_lines(), None);
    let (status, _) = request(&state, Method::GET, "/api/v1/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!polling.is_finished(), "the stalled read let go early");

    stall.release();
    assert!(polling.await.unwrap());
    assert_eq!(
        lock(&state.env_preludes)
            .data_cached()
            .host
            .as_ref()
            .map(|e| e.text.as_str()),
        Some("ml fresh")
    );
}
