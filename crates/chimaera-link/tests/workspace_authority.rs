use chimaera_link::{Delegation, WorkspaceBinding, WorkspaceConfigureAck};
use serde_json::json;
use std::path::Path;

#[test]
fn optional_binding_is_legacy_compatible_and_credentials_stay_redacted() {
    let legacy = json!({"access_token":"synthetic-private-token","expires_at":"2099-01-01T00:00:00Z","scope":["baton","mirror"],"device_id":"worker-a"});
    let grant: Delegation = serde_json::from_value(legacy.clone()).unwrap();
    assert!(grant.workspace.is_none());
    assert_eq!(serde_json::to_value(&grant).unwrap(), legacy);
    let mut bound = legacy;
    bound["workspace"] = json!({"workspace_id":"w-a","revision":9});
    let grant: Delegation = serde_json::from_value(bound.clone()).unwrap();
    assert_eq!(serde_json::to_value(&grant).unwrap(), bound);
    assert!(!format!("{grant:?}").contains("synthetic-private-token"));
    bound["workspace"]["unknown_scope"] = true.into();
    assert!(serde_json::from_value::<Delegation>(bound).is_err());
}

#[test]
fn acknowledgment_requires_exact_version_binding_and_root() {
    let binding = WorkspaceBinding {
        workspace_id: "w-a".into(),
        revision: 9,
    };
    let root = Path::new("/projects/a");
    let body = json!({"workspace_authority":1,"workspace":binding,"workspace_root":root});
    assert!(WorkspaceConfigureAck::decode(
        200,
        &serde_json::to_vec(&body).unwrap(),
        &binding,
        root
    )
    .is_ok());
    for status in [204, 404, 401, 500] {
        assert!(WorkspaceConfigureAck::decode(
            status,
            &serde_json::to_vec(&body).unwrap(),
            &binding,
            root
        )
        .is_err());
    }
    for bad in [
        json!({}),
        json!({"workspace_authority":2,"workspace":binding,"workspace_root":root}),
        json!({"workspace_authority":1,"workspace":{"workspace_id":"w-b","revision":9},"workspace_root":root}),
        json!({"workspace_authority":1,"workspace":{"workspace_id":"w-a","revision":10},"workspace_root":root}),
        json!({"workspace_authority":1,"workspace":binding,"workspace_root":"/projects/b"}),
    ] {
        assert!(WorkspaceConfigureAck::decode(
            200,
            &serde_json::to_vec(&bad).unwrap(),
            &binding,
            root
        )
        .is_err());
    }
    assert!(WorkspaceConfigureAck::decode(
        200,
        b"<html>old daemon SPA fallback</html>",
        &binding,
        root
    )
    .is_err());
    assert!(
        WorkspaceConfigureAck::decode(200, &vec![b' '; 16 * 1024 + 1], &binding, root).is_err()
    );
}

#[cfg(feature = "fixtures")]
#[tokio::test]
async fn old_daemon_response_never_confirms_scoped_configuration() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    use axum::{http::StatusCode, routing::post, Router};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    // The old daemon knows only its legacy path. No client fallback may give
    // it the bound credential it would silently interpret as account-wide.
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/api/v1/pro/configure",
                post(move || {
                    let count = count.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        StatusCode::NO_CONTENT
                    }
                }),
            ),
        )
        .await
        .unwrap();
    });
    let response=reqwest::Client::new().post(format!("{endpoint}/api/v1/pro/configure/workspace")).json(&json!({"delegation":{"access_token":"synthetic-private-token","workspace":{"workspace_id":"w-a","revision":9}}})).send().await.unwrap();
    let status = response.status().as_u16();
    let body = response.bytes().await.unwrap();
    assert_eq!(status, 404);
    assert!(WorkspaceConfigureAck::decode(
        status,
        &body,
        &WorkspaceBinding {
            workspace_id: "w-a".into(),
            revision: 9
        },
        Path::new("/projects/a")
    )
    .is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    server.abort();
    let _ = server.await;
}
