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

/// A missing hashed asset chunk must 404 — never fall back to index.html (a
/// stale browser would otherwise get HTML for `/assets/index-*.js` and break
/// silently instead of being told to hard-reload).
#[tokio::test]
async fn missing_asset_chunk_is_404_not_index_html() {
    let res = app(test_state())
        .oneshot(
            Request::builder()
                .uri("/assets/does-not-exist-deadbeef.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::NOT_FOUND,
        "a missing /assets/* chunk must 404, not serve index.html"
    );
}

/// A client-side SPA route (extension-less, not under /assets) still falls
/// back to index.html so the app boots and routes on the client.
#[tokio::test]
async fn spa_route_falls_back_to_index_html() {
    let res = app(test_state())
        .oneshot(
            Request::builder()
                .uri("/workspace/some-client-route")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let ct = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.contains("text/html"),
        "SPA route should serve index.html, got {ct}"
    );
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
    assert!(json.get("daemon_extension").is_none(), "{json}");
    assert!(json.get("daemon_assembly").is_none());
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
    assert_eq!(
        res.headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store"),
        "a daemon handoff must not leave reloads on a cached entry bundle"
    );
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains(&format!(
        "<meta name=\"chimaera-build\" content=\"{}\"",
        chimaera_core::BUILD_ID
    )));
    assert!(!html.contains("__CHIMAERA_BUILD_ID__"));
}

#[tokio::test]
async fn hashed_assets_are_immutable() {
    let root = app(test_state())
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body = root.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(body.to_vec()).unwrap();
    let start = html.find("/assets/").expect("built index has an asset");
    let end = html[start..]
        .find('"')
        .map(|offset| start + offset)
        .expect("asset URL ends at an attribute quote");
    let asset = &html[start..end];

    let res = app(test_state())
        .oneshot(Request::builder().uri(asset).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "missing built asset {asset}");
    assert_eq!(
        res.headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("public, max-age=31536000, immutable")
    );
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

#[tokio::test]
async fn health_assembly_metadata_is_authenticated_optional_and_stateless() {
    struct DefaultRuntime;
    impl crate::daemon_extension::Runtime for DefaultRuntime {
        fn coordinate(
            &self,
            _owner: crate::daemon_extension::CoordinatorOwner,
        ) -> crate::daemon_extension::RuntimeFuture {
            Box::pin(async {})
        }
    }
    struct SelectedRuntime;
    impl crate::daemon_extension::Runtime for SelectedRuntime {
        fn coordinate(
            &self,
            _owner: crate::daemon_extension::CoordinatorOwner,
        ) -> crate::daemon_extension::RuntimeFuture {
            Box::pin(async {})
        }
        fn assembly_identity(&self) -> Option<&'static str> {
            Some("2.0.0@fixture")
        }
    }
    for (runtime, expected) in [
        (
            std::sync::Arc::new(DefaultRuntime)
                as std::sync::Arc<dyn crate::daemon_extension::Runtime>,
            None,
        ),
        (
            std::sync::Arc::new(SelectedRuntime)
                as std::sync::Arc<dyn crate::daemon_extension::Runtime>,
            Some("2.0.0@fixture"),
        ),
    ] {
        let state = test_state();
        state.pro().set_runtime(runtime);
        let router = app(state);
        let unauthorized = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["daemon_extension"], true);
        assert_eq!(
            value.get("daemon_assembly").and_then(|v| v.as_str()),
            expected
        );
        if expected.is_none() {
            assert!(value.get("daemon_assembly").is_none());
        }
        assert_eq!(value["build"], chimaera_core::BUILD_ID);
    }
}
