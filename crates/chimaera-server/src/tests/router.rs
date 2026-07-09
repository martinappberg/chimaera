use super::support::*;
use crate::*;

#[tokio::test]
async fn health_without_token_is_401() {
    let res = app(test_state())
        .oneshot(
            Request::builder()
                .uri("/api/v1/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json, serde_json::json!({"error": "unauthorized"}));
}

#[tokio::test]
async fn health_with_token_is_200() {
    let res = app(test_state())
        .oneshot(
            Request::builder()
                .uri("/api/v1/health")
                .header(header::AUTHORIZATION, "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["name"], "chimaera");
    assert_eq!(json["version"], chimaera_core::VERSION);
    assert_eq!(json["hostname"], "testhost");
    assert_eq!(json["pid"], 4242);
    assert!(json["uptime_secs"].is_u64());
}

#[tokio::test]
async fn root_serves_html_without_auth() {
    let res = app(test_state())
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let content_type = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string();
    assert!(content_type.starts_with("text/html"));
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert!(!body.is_empty());
}

#[tokio::test]
async fn spa_fallback_serves_index() {
    let res = app(test_state())
        .oneshot(
            Request::builder()
                .uri("/some/client/route")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let content_type = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string();
    assert!(content_type.starts_with("text/html"));
}

#[tokio::test]
async fn unknown_api_path_is_404() {
    let res = app(test_state())
        .oneshot(
            Request::builder()
                .uri("/api/v1/nope")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
