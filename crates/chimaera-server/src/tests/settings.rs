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
    assert_eq!(lock(&state.settings).scrollback_lines(), Some(200));
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
