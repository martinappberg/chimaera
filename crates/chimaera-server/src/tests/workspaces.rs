use super::support::*;
use crate::*;

#[tokio::test]
async fn workspaces_with_token_is_empty_list() {
    let res = app(test_state())
        .oneshot(
            Request::builder()
                .uri("/api/v1/workspaces")
                .header(header::AUTHORIZATION, "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json, serde_json::json!([]));
}

#[tokio::test]
async fn workspaces_post_get_round_trip() {
    let state = test_state();
    let root = test_dir("ws-root");
    let root_str = root.to_string_lossy().into_owned();

    // POST registers the directory.
    let (status, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root_str})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = ws["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("w-") && id.len() == 10, "bad id {id}");
    assert!(id[2..].chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')));
    assert_eq!(
        ws["name"].as_str().unwrap(),
        root.file_name().unwrap().to_str().unwrap()
    );
    assert_eq!(
        ws["root"].as_str().unwrap(),
        std::fs::canonicalize(&root).unwrap().to_str().unwrap()
    );

    // POST again with the same root is idempotent.
    let (status, again) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root_str})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["id"], ws["id"]);

    // GET lists it.
    let (status, list) = request(&state, Method::GET, "/api/v1/workspaces", None).await;
    assert_eq!(status, StatusCode::OK);
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], ws["id"]);

    // Nonexistent root is a 400 with an error body.
    let (status, err) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": "/definitely/not/a/dir"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(err["error"].is_string());
}

#[tokio::test]
async fn workspaces_open_and_delete() {
    let state = test_state();
    let root = test_dir("ws-open-del");

    let (status, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = ws["id"].as_str().unwrap().to_string();
    let stamped = ws["last_opened_at"].as_u64().unwrap();
    assert!(stamped > 0, "registration stamps last_opened_at");

    // Touch returns the workspace with a fresh (>=) stamp.
    let (status, touched) = request(
        &state,
        Method::POST,
        &format!("/api/v1/workspaces/{id}/open"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(touched["id"], ws["id"]);
    assert!(touched["last_opened_at"].as_u64().unwrap() >= stamped);

    // Unknown ids 404 on both endpoints.
    let (status, _) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces/w-00000000/open",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = request(
        &state,
        Method::DELETE,
        "/api/v1/workspaces/w-00000000",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // DELETE unregisters (files untouched) and the list empties.
    let (status, _) = request(
        &state,
        Method::DELETE,
        &format!("/api/v1/workspaces/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(root.is_dir(), "delete never touches the directory");
    let (status, list) = request(&state, Method::GET, "/api/v1/workspaces", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 0);
}
