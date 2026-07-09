use super::support::*;
use crate::*;

#[tokio::test]
async fn view_state_put_get_round_trip_and_persists() {
    let data_dir = test_dir("view-state");
    let state = test_state_with_data_dir(0, data_dir.clone());

    let blob = serde_json::json!({
        "layout": {"type": "pane", "tabs": [{"surface": "terminal", "session": "s-1"}]},
        "focusMode": false,
        "zoom": serde_json::Value::Null,
    });
    let (status, body) = request(
        &state,
        Method::PUT,
        "/api/v1/view-state/win-abc_123",
        Some(blob.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(body, serde_json::Value::Null);

    let (status, body) = request(&state, Method::GET, "/api/v1/view-state/win-abc_123", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({"state": blob}));

    // A second PUT overwrites.
    let (status, _) = request(
        &state,
        Method::PUT,
        "/api/v1/view-state/win-abc_123",
        Some(serde_json::json!({"v": 2})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = request(&state, Method::GET, "/api/v1/view-state/win-abc_123", None).await;
    assert_eq!(body, serde_json::json!({"state": {"v": 2}}));

    // Other keys are independent.
    let (status, _) = request(
        &state,
        Method::PUT,
        "/api/v1/view-state/win-other",
        Some(serde_json::json!(true)),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = request(&state, Method::GET, "/api/v1/view-state/win-abc_123", None).await;
    assert_eq!(body, serde_json::json!({"state": {"v": 2}}));

    // Survives a daemon restart (fresh state over the same data dir).
    let reloaded = test_state_with_data_dir(0, data_dir);
    let (status, body) = request(
        &reloaded,
        Method::GET,
        "/api/v1/view-state/win-abc_123",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({"state": {"v": 2}}));
}

#[tokio::test]
async fn view_state_unknown_key_is_404() {
    let (status, body) = request(
        &test_state(),
        Method::GET,
        "/api/v1/view-state/win-nope",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, serde_json::json!({"error": "not found"}));
}

#[tokio::test]
async fn view_state_bad_key_is_400() {
    let state = test_state();
    let too_long = "a".repeat(65);
    // "sp%20ace" percent-decodes to a key with a space.
    for key in ["bad.key", "sp%20ace", too_long.as_str()] {
        let uri = format!("/api/v1/view-state/{key}");
        let (status, err) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "GET {key}");
        assert!(err["error"].is_string());
        let (status, err) = request(&state, Method::PUT, &uri, Some(serde_json::json!({}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "PUT {key}");
        assert!(err["error"].is_string());
    }
    // A 64-char key is still fine.
    let max_key = "k".repeat(64);
    let uri = format!("/api/v1/view-state/{max_key}");
    let (status, _) = request(&state, Method::PUT, &uri, Some(serde_json::json!(1))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn view_state_rejects_non_json_body() {
    let res = app(test_state())
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri("/api/v1/view-state/win-raw")
                .header(header::AUTHORIZATION, "Bearer test-token")
                .body(Body::from("not json at all"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn view_state_oversize_is_413_and_not_stored() {
    let state = test_state();

    // A body of exactly 64KB is accepted: {"blob":"x..."} is 11 bytes of
    // scaffolding around the payload string.
    let fitting = serde_json::json!({"blob": "x".repeat(64 * 1024 - 11)});
    assert_eq!(fitting.to_string().len(), 64 * 1024);
    let (status, _) = request(
        &state,
        Method::PUT,
        "/api/v1/view-state/win-fits",
        Some(fitting),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // One byte over is a 413 and nothing is stored.
    let oversize = serde_json::json!({"blob": "x".repeat(64 * 1024 - 10)});
    assert_eq!(oversize.to_string().len(), 64 * 1024 + 1);
    let (status, err) = request(
        &state,
        Method::PUT,
        "/api/v1/view-state/win-big",
        Some(oversize),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(err["error"].is_string());
    let (status, _) = request(&state, Method::GET, "/api/v1/view-state/win-big", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn view_state_without_token_is_401() {
    for method in [Method::GET, Method::PUT] {
        let res = app(test_state())
            .oneshot(
                Request::builder()
                    .method(method.clone())
                    .uri("/api/v1/view-state/win-abc")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{method}");
    }
}
