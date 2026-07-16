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
