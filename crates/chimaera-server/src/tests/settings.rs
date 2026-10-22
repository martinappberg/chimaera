use super::support::*;
use crate::*;

#[tokio::test]
async fn settings_round_trip_and_validation() {
    let state = test_state();

    // Fresh daemon: empty settings object.
    let (status, body) = request(&state, Method::GET, "/api/v1/settings", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({"settings": {}}));

    // PUT stores the sparse map verbatim (unknown keys preserved).
    let map = serde_json::json!({
        "terminal.fontSize": 15,
        "appearance.theme": "dark",
        "future.unknownKey": [1, 2, 3],
    });
    let (status, _) = request(&state, Method::PUT, "/api/v1/settings", Some(map.clone())).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = request(&state, Method::GET, "/api/v1/settings", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["settings"], map);

    // Non-object bodies are rejected.
    let (status, _) = request(
        &state,
        Method::PUT,
        "/api/v1/settings",
        Some(serde_json::json!([1, 2])),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Daemon-consumed keys parse with clamping; garbage yields None.
    let (_, _) = request(
        &state,
        Method::PUT,
        "/api/v1/settings",
        Some(serde_json::json!({
            "daemon.scrollbackLines": 50,
            "quickOpen.ignoreDirs": ["node_modules", "", "a/b", ".git"],
        })),
    )
    .await;
    assert_eq!(lock(&state.settings).scrollback_lines(), Some(1_000));
    assert_eq!(
        lock(&state.settings).quickopen_ignore_dirs(),
        Some(vec!["node_modules".to_string(), ".git".to_string()]),
    );
    let (_, _) = request(
        &state,
        Method::PUT,
        "/api/v1/settings",
        Some(serde_json::json!({"daemon.scrollbackLines": "lots"})),
    )
    .await;
    assert_eq!(lock(&state.settings).scrollback_lines(), None);

    let (_, _) = request(
        &state,
        Method::PUT,
        "/api/v1/settings",
        Some(serde_json::json!({"daemon.scrollbackLines": 9_000_000})),
    )
    .await;
    assert_eq!(
        lock(&state.settings).scrollback_lines(),
        Some(200_000),
        "hand-edited settings must honor the UI's memory ceiling"
    );
}

#[tokio::test]
async fn codex_steering_setting_preserves_native_defaults_and_rejects_unknown_overrides() {
    let state = test_state();
    assert_eq!(lock(&state.settings).codex_instant_interrupt(), None);
    for (value, expected) in [
        (serde_json::json!("immediate"), Some(true)),
        (serde_json::json!("next_step"), Some(false)),
        (serde_json::json!("agent"), None),
        (serde_json::json!("future_mode"), None),
        (serde_json::json!(true), None),
    ] {
        let (status, _) = request(
            &state,
            Method::PUT,
            "/api/v1/settings",
            Some(serde_json::json!({"chat.codexSteering": value})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(lock(&state.settings).codex_instant_interrupt(), expected);
    }
}

#[tokio::test]
async fn new_chat_model_setting_opts_out_only_for_agent_defaults() {
    let state = test_state();
    assert!(lock(&state.settings).remember_chat_model());
    for (value, expected) in [
        (serde_json::json!("agent"), false),
        (serde_json::json!("remember"), true),
        (serde_json::json!("future_mode"), true),
        (serde_json::json!(false), true),
        (serde_json::Value::Null, true),
    ] {
        let (status, _) = request(
            &state,
            Method::PUT,
            "/api/v1/settings",
            Some(serde_json::json!({"chat.newSessionModel": value})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(lock(&state.settings).remember_chat_model(), expected);
    }
}

#[tokio::test]
async fn settings_hand_edit_on_disk_is_picked_up() {
    let data_dir = test_dir("settings-disk");
    let state = test_state_with_data_dir(0, data_dir.clone());

    // Simulate `vim ~/.config/chimaera/settings.json`.
    let path = data_dir.join("config").join("settings.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, r#"{"terminal.fontSize": 16}"#).unwrap();
    // Filesystem mtime granularity can swallow same-instant rewrites in
    // this synthetic test; force the stat cache stale. Real hand-edits
    // happen seconds apart and are caught by the mtime check alone.
    lock(&state.settings).force_stale_for_tests();

    let (status, body) = request(&state, Method::GET, "/api/v1/settings", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["settings"]["terminal.fontSize"], 16);
}

/// The events bus reads only the CACHED settings generation/map (no reactor
/// stat) — external edits reach it through the watcher's off-reactor poll,
/// which reloads and reports a change so the caller can broadcast.
#[tokio::test]
async fn settings_watcher_poll_surfaces_hand_edits_off_reactor() {
    let data_dir = test_dir("settings-watch");
    let state = test_state_with_data_dir(0, data_dir.clone());

    let before = lock(&state.settings).generation_cached();

    // Simulate `vim ~/.config/chimaera/settings.json`.
    let path = data_dir.join("config").join("settings.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, r#"{"terminal.fontSize": 17}"#).unwrap();
    // Same mtime-granularity dodge as the hand-edit test above.
    lock(&state.settings).force_stale_for_tests();

    // One watcher pass: detects the edit, reloads, reports the change.
    assert!(crate::settings::poll_external_edit(&state).await);
    {
        let store = lock(&state.settings);
        assert!(store.generation_cached() > before);
        assert_eq!(
            store.map_cached().get("terminal.fontSize"),
            Some(&serde_json::json!(17))
        );
    }

    // Settled: the next pass sees the recorded mtime and does nothing.
    assert!(!crate::settings::poll_external_edit(&state).await);
}

fn settings_path(data_dir: &std::path::Path) -> PathBuf {
    let path = data_dir.join("config").join("settings.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    path
}

async fn settings_of(state: &Arc<AppState>) -> serde_json::Value {
    let (status, body) = request(state, Method::GET, "/api/v1/settings", None).await;
    assert_eq!(status, StatusCode::OK);
    body["settings"].clone()
}

/// Oversized, corrupt, non-object and missing files read as an empty map
/// (the documented never-brick rule); an UNREADABLE file keeps the last
/// map, so a filesystem hiccup never flashes every window to defaults.
#[tokio::test]
async fn settings_file_states_on_read() {
    let data_dir = test_dir("settings-states");
    let path = settings_path(&data_dir);
    // Oversized at boot: ignored, never an error.
    std::fs::write(&path, format!(r#"{{"big": "{}"}}"#, "x".repeat(300 * 1024))).unwrap();
    let state = test_state_with_data_dir(0, data_dir.clone());
    assert_eq!(settings_of(&state).await, serde_json::json!({}));

    let reread = |contents: &str| {
        std::fs::write(&path, contents).unwrap();
        lock(&state.settings).force_stale_for_tests();
    };
    reread(r#"{"terminal.fontSize": 14}"#);
    assert_eq!(settings_of(&state).await["terminal.fontSize"], 14);
    reread("{ not json");
    assert_eq!(settings_of(&state).await, serde_json::json!({}));
    reread("[1, 2]");
    assert_eq!(settings_of(&state).await, serde_json::json!({}));
    reread(r#"{"terminal.fontSize": 15}"#);
    assert_eq!(settings_of(&state).await["terminal.fontSize"], 15);

    // A directory where the file should be: unreadable, the map stays.
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    lock(&state.settings).force_stale_for_tests();
    assert_eq!(settings_of(&state).await["terminal.fontSize"], 15);
    std::fs::remove_dir(&path).unwrap();

    // Missing: the user cleared it.
    lock(&state.settings).force_stale_for_tests();
    assert_eq!(settings_of(&state).await, serde_json::json!({}));
}

/// The cap is enforced by the read, not by the stat before it: a file that
/// grows past it between the two is still refused.
#[tokio::test]
async fn settings_file_growing_past_the_cap_mid_read_is_refused() {
    let data_dir = test_dir("settings-grow");
    let path = settings_path(&data_dir);
    std::fs::write(&path, r#"{"terminal.fontSize": 14}"#).unwrap();
    let state = test_state_with_data_dir(0, data_dir.clone());
    assert_eq!(settings_of(&state).await["terminal.fontSize"], 14);

    let grown = path.clone();
    let once = Arc::new(std::sync::atomic::AtomicBool::new(false));
    lock(&state.settings).set_before_read_for_tests(Arc::new(move || {
        if !once.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let big = format!(r#"{{"big": "{}"}}"#, "x".repeat(300 * 1024));
            std::fs::write(&grown, big).unwrap();
        }
    }));
    std::fs::write(&path, r#"{"terminal.fontSize": 16}"#).unwrap();
    lock(&state.settings).force_stale_for_tests();
    assert_eq!(settings_of(&state).await, serde_json::json!({}));
}

/// A PUT whose pretty-printed file would exceed the cap a read enforces is
/// refused (413) and changes nothing — writing it would empty the store on
/// the next read.
#[tokio::test]
async fn settings_put_refuses_a_file_the_next_read_would_ignore() {
    let state = test_state();
    let small = serde_json::json!({"terminal.fontSize": 13});
    let (status, _) = request(&state, Method::PUT, "/api/v1/settings", Some(small.clone())).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // ~200 KB compact, ~280 KB once pretty-printed.
    let wide: serde_json::Map<String, serde_json::Value> = (0..20_000)
        .map(|i| (format!("k{i:05}"), serde_json::json!(0)))
        .collect();
    let wide = serde_json::Value::Object(wide);
    assert!(wide.to_string().len() < 256 * 1024);
    let (status, _) = request(&state, Method::PUT, "/api/v1/settings", Some(wide)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    lock(&state.settings).force_stale_for_tests();
    assert_eq!(settings_of(&state).await, small);
}

/// The disk work runs on the blocking pool: with settings.json's read
/// parked on a hung disk, a single-threaded runtime still answers health
/// and the other store's GET at once, and the parked GET completes after.
#[tokio::test(flavor = "current_thread")]
async fn settings_read_on_slow_storage_leaves_the_runtime_free() {
    let data_dir = test_dir("settings-slow");
    let path = settings_path(&data_dir);
    let state = test_state_with_data_dir(0, data_dir.clone());

    let stall = Stall::new();
    lock(&state.settings).set_before_read_for_tests(stall.hook());
    std::fs::write(&path, r#"{"terminal.fontSize": 18}"#).unwrap();
    lock(&state.settings).force_stale_for_tests();
    let parked = tokio::spawn({
        let state = state.clone();
        async move { request(&state, Method::GET, "/api/v1/settings", None).await }
    });
    stall.wait_entered().await;

    let (status, _) = request(&state, Method::GET, "/api/v1/health", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request(&state, Method::GET, "/api/v1/environment", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!parked.is_finished(), "the stalled read let go early");

    stall.release();
    let (status, body) = parked.await.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["settings"]["terminal.fontSize"], 18);
}

fn plant_claude_miss(state: &Arc<AppState>) {
    lock(&state.agent_misses).insert(
        crate::agents::AgentKind::Claude,
        ("not found".to_string().into(), std::time::Instant::now()),
    );
}

fn claude_miss_cleared(state: &Arc<AppState>) -> bool {
    !lock(&state.agent_misses).contains_key(&crate::agents::AgentKind::Claude)
}

/// A PUT whose handler is dropped mid-job (a superseded save aborted on
/// page hide) still runs everything after its write: the detection-cache
/// drop for a moved agent path and the `/ws/events` wake.
#[tokio::test]
async fn settings_put_side_effects_survive_a_dropped_handler() {
    let data_dir = test_dir("settings-dropped");
    let path = settings_path(&data_dir);
    let state = test_state_with_data_dir(0, data_dir.clone());
    plant_claude_miss(&state);

    let stall = Stall::new();
    lock(&state.settings).set_before_read_for_tests(stall.hook());
    std::fs::write(&path, r#"{"terminal.fontSize": 12}"#).unwrap();
    lock(&state.settings).force_stale_for_tests();

    let wakes_before = state.changes.generation();
    let dropped = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        request(
            &state,
            Method::PUT,
            "/api/v1/settings",
            Some(serde_json::json!({"agents.claude.path": "/opt/tools/claude"})),
        ),
    )
    .await;
    assert!(dropped.is_err(), "the PUT should still be parked");
    stall.wait_entered().await;
    stall.release();

    // The parked job finishes on the blocking pool with nobody awaiting it.
    for _ in 0..500 {
        if claude_miss_cleared(&state) && state.changes.generation() > wakes_before {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        state.changes.generation() > wakes_before,
        "the dropped PUT never woke /ws/events"
    );
    assert!(claude_miss_cleared(&state));
    assert_eq!(
        lock(&state.settings).map_cached()["agents.claude.path"],
        "/opt/tools/claude"
    );
}

/// The getters read only the cache: one called between a PUT's rename and
/// its install still answers the old value (no re-read of the new file), and
/// the PUT's diff sees the moved agent path.
#[tokio::test]
async fn settings_getters_read_the_cache_during_a_put() {
    let state = test_state();
    plant_claude_miss(&state);
    let weak = Arc::downgrade(&state);
    let seen = Arc::new(std::sync::Mutex::new(None));
    let seen_in_hook = seen.clone();
    lock(&state.settings).set_after_write_for_tests(Arc::new(move || {
        if let Some(state) = weak.upgrade() {
            let path = lock(&state.settings).agent_path(crate::agents::AgentKind::Claude);
            *seen_in_hook.lock().unwrap() = Some(path);
        }
    }));
    let (status, _) = request(
        &state,
        Method::PUT,
        "/api/v1/settings",
        Some(serde_json::json!({"agents.claude.path": "/opt/tools/claude"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        *seen.lock().unwrap(),
        Some(None),
        "a getter mid-write must answer the cache, not the new file"
    );
    assert!(
        claude_miss_cleared(&state),
        "the moved agent path went unnoticed"
    );
    assert_eq!(
        lock(&state.settings)
            .agent_path(crate::agents::AgentKind::Claude)
            .as_deref(),
        Some("/opt/tools/claude")
    );
}

/// With settings.json's read parked on a hung disk, the daemon-side getters
/// and a spawn's prelude answer from the caches at once — never a stat under
/// the store mutex — and an unrelated route answers too. The on-disk edit
/// reaches the getters only once the refresh lands.
#[tokio::test(flavor = "current_thread")]
async fn settings_getters_answer_from_the_cache_while_the_read_is_stalled() {
    let data_dir = test_dir("settings-getters-stalled");
    let path = settings_path(&data_dir);
    std::fs::write(
        &path,
        r#"{"daemon.scrollbackLines": 5000, "git.path": "/opt/git-old/bin/git"}"#,
    )
    .unwrap();
    std::fs::write(
        data_dir.join("config").join("env-profiles.json"),
        r#"{"host": {"text": "ml cached-host"}}"#,
    )
    .unwrap();
    let state = test_state_with_data_dir(0, data_dir.clone());

    let stall = Stall::new();
    lock(&state.settings).set_before_read_for_tests(stall.hook());
    std::fs::write(
        &path,
        r#"{"daemon.scrollbackLines": 9000, "git.path": "/opt/git-new/bin/git"}"#,
    )
    .unwrap();
    lock(&state.settings).force_stale_for_tests();
    let parked = tokio::spawn({
        let state = state.clone();
        async move { request(&state, Method::GET, "/api/v1/settings", None).await }
    });
    stall.wait_entered().await;

    assert_eq!(lock(&state.settings).scrollback_lines(), Some(5_000));
    assert_eq!(
        lock(&state.settings).git_path().as_deref(),
        Some("/opt/git-old/bin/git")
    );
    // Preludes are POSIX shell: a Windows spawn never gets one.
    #[cfg(unix)]
    {
        let session = format!("settings-stalled-{}", std::process::id());
        let prelude = crate::environment::materialize_prelude(&state, &session, "w-1", None, None)
            .expect("the cached host prelude");
        let text = std::fs::read_to_string(&prelude).unwrap();
        crate::environment::remove_prelude_file(&session);
        assert!(text.contains("ml cached-host"), "{text}");
    }
    let (status, _) = request(&state, Method::GET, "/api/v1/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!parked.is_finished(), "the stalled read let go early");

    stall.release();
    let (status, body) = parked.await.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["settings"]["daemon.scrollbackLines"], 9000);
    assert_eq!(lock(&state.settings).scrollback_lines(), Some(9_000));
    assert_eq!(
        lock(&state.settings).git_path().as_deref(),
        Some("/opt/git-new/bin/git")
    );
}

/// A new window's first `/ws/events` settings frame refreshes first, but a
/// hung settings read can't hold it: past one poll period the frame goes out
/// from the cache.
#[tokio::test(flavor = "current_thread")]
async fn first_events_settings_frame_comes_from_the_cache_behind_a_stalled_read() {
    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let data_dir = test_dir("settings-events-stalled");
    let path = settings_path(&data_dir);
    std::fs::write(&path, r#"{"terminal.fontSize": 11}"#).unwrap();
    let state = test_state_with_data_dir(0, data_dir.clone());
    let stall = Stall::new();
    lock(&state.settings).set_before_read_for_tests(stall.hook());
    std::fs::write(&path, r#"{"terminal.fontSize": 19}"#).unwrap();
    lock(&state.settings).force_stale_for_tests();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws/events"))
        .await
        .unwrap();
    socket
        .send(WsMessage::text(
            serde_json::json!({"type": "auth", "token": "test-token"}).to_string(),
        ))
        .await
        .unwrap();
    stall.wait_entered().await;

    // The Stall lets go on its own at 15 s; the frame must come well before.
    let frame = tokio::time::timeout(
        crate::settings::SETTINGS_POLL + std::time::Duration::from_secs(5),
        next_ws_frame(&mut socket),
    )
    .await
    .expect("no settings frame behind the stalled read");
    let settings = match frame {
        WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
        other => panic!("expected a settings text frame, got {other:?}"),
    };
    assert_eq!(settings["type"], "settings");
    assert_eq!(settings["settings"]["terminal.fontSize"], 11);
    stall.release();
}
