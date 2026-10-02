#![cfg(feature = "fixtures")]

use axum::{extract::State, http::HeaderMap, routing::post, Json, Router};
use chimaera_link::{
    fake, AlreadySubscribed, BillingInterval, BillingPortalTarget, BillingSession, Client,
    DesktopBillingCallback, Plan,
};
use serde_json::{json, Value};
use tokio::{net::TcpListener, sync::mpsc};

fn callback() -> DesktopBillingCallback {
    DesktopBillingCallback {
        redirect_uri: "http://127.0.0.1:49152/billing/callback".into(),
        state: chimaera_link::Pkce::new().state,
    }
}

#[tokio::test]
async fn checkout_conflict_is_typed_without_exposing_service_diagnostics() {
    use axum::http::StatusCode;
    for status in [StatusCode::CONFLICT, StatusCode::SERVICE_UNAVAILABLE] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new().route(
            "/v1/billing/checkout",
            post(move || async move { (status, "private provider diagnostic") }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = Client::new(&endpoint, Some(fake::FakeKeeper::tokens())).unwrap();
        let error = client
            .billing_checkout_with_callback(Plan::Pro, BillingInterval::Month, &callback())
            .await
            .err()
            .unwrap();
        assert_eq!(
            error.is::<AlreadySubscribed>(),
            status == StatusCode::CONFLICT
        );
        assert!(!error.to_string().contains("private provider diagnostic"));
        server.abort();
    }
}

#[test]
fn callback_rejects_deceptive_origins_paths_and_nonces() {
    let mut value = callback();
    assert!(value.validate().is_ok());
    for uri in [
        "http://localhost:49152/billing/callback",
        "http://127.1:49152/billing/callback",
        "http://2130706433:49152/billing/callback",
        "http://[::1]:49152/billing/callback",
        "http://127.0.0.1.evil.test:49152/billing/callback",
        "https://127.0.0.1:49152/billing/callback",
        "http://user@127.0.0.1:49152/billing/callback",
        "http://127.0.0.1:49152/billing/callback?state=attacker",
        "http://127.0.0.1:49152/billing/callback#fragment",
        "http://127.0.0.1:49152/billing/other/../callback",
        "http://127.0.0.1:49152/billing/%63allback",
        "http://127.0.0.1:49152/billing/callback/",
        "http://127.0.0.1:0/billing/callback",
        "http://127.0.0.1:443/billing/callback",
        "http://127.0.0.1:049152/billing/callback",
        "http://127.0.0.1:65536/billing/callback",
    ] {
        value.redirect_uri = uri.into();
        assert!(value.validate().is_err(), "accepted {uri}");
    }
    value.redirect_uri = callback().redirect_uri;
    for nonce in [
        "".into(),
        "a".repeat(42),
        "a".repeat(44),
        "!".repeat(43),
        format!("{}=", "a".repeat(42)),
        format!("{}B", "A".repeat(42)),
    ] {
        value.state = nonce;
        assert!(value.validate().is_err());
    }
}

async fn record(
    State(sender): State<mpsc::Sender<Value>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<BillingSession> {
    assert_eq!(
        headers["authorization"],
        format!("Bearer {}", fake::STATIC_TOKEN)
    );
    sender.send(body).await.unwrap();
    Json(BillingSession {
        url: "https://checkout.stripe.com/c/pay/fixture".into(),
    })
}

#[tokio::test]
async fn callback_requests_are_authenticated_additive_and_validate_before_sending() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (sender, mut requests) = mpsc::channel(4);
    let router = Router::new()
        .route("/v1/billing/checkout", post(record))
        .route("/v1/billing/portal", post(record))
        .with_state(sender);
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = Client::new(&endpoint, Some(fake::FakeKeeper::tokens())).unwrap();
    let callback = callback();
    client
        .billing_checkout_with_callback(Plan::Max, BillingInterval::Year, &callback)
        .await
        .unwrap();
    let checkout = requests.recv().await.unwrap();
    assert_eq!(
        checkout,
        json!({"plan":"max","interval":"year","return_to":"desktop",
        "desktop_callback":{"redirect_uri":callback.redirect_uri,"state":callback.state}})
    );
    client
        .billing_portal_with_callback(&callback)
        .await
        .unwrap();
    let portal = requests.recv().await.unwrap();
    assert_eq!(
        portal,
        json!({"return_to":"desktop", "desktop_callback":checkout["desktop_callback"]})
    );
    client
        .billing_checkout(Plan::Pro, BillingInterval::Month)
        .await
        .unwrap();
    assert_eq!(
        requests.recv().await.unwrap(),
        json!({"plan":"pro","interval":"month","return_to":"desktop"})
    );
    client.billing_portal().await.unwrap();
    assert_eq!(
        requests.recv().await.unwrap(),
        json!({"return_to":"desktop"})
    );
    let target = BillingPortalTarget {
        plan: Plan::Max,
        interval: BillingInterval::Year,
    };
    client
        .billing_portal_review_with_callback(&target, &callback)
        .await
        .unwrap();
    assert_eq!(
        requests.recv().await.unwrap(),
        json!({
            "return_to":"desktop", "desktop_callback":portal["desktop_callback"],
            "target":{"plan":"max","interval":"year"}
        })
    );
    assert!(client
        .billing_portal_review_with_callback(
            &BillingPortalTarget {
                plan: Plan::None,
                interval: BillingInterval::Month
            },
            &callback
        )
        .await
        .is_err());
    assert!(serde_json::from_value::<BillingPortalTarget>(
        json!({"plan":"max","interval":"month","price_id":"arbitrary"})
    )
    .is_err());
    let invalid = DesktopBillingCallback {
        redirect_uri: "https://evil.test/callback".into(),
        ..callback
    };
    assert!(client
        .billing_checkout_with_callback(Plan::Pro, BillingInterval::Month, &invalid)
        .await
        .is_err());
    assert!(client.billing_portal_with_callback(&invalid).await.is_err());
    assert!(requests.try_recv().is_err());
    server.abort();
}
