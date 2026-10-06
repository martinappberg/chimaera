//! The daemon's own release check, against a local fake releases API.

use serde_json::json;

use super::plugin_updates::FakeReleases;
use super::support::*;

/// A published `version` as GitHub's `releases/latest` answers it.
async fn releases_with(version: &str) -> FakeReleases {
    let releases = FakeReleases::start().await;
    releases.put(
        "/latest",
        json!({
            "tag_name": format!("v{version}"),
            "html_url": format!("https://example.invalid/releases/tag/v{version}"),
            "published_at": "2026-09-30T00:00:00Z",
        })
        .to_string()
        .into_bytes(),
    );
    releases
}

#[tokio::test]
async fn the_clouds_daemon_never_checks_for_its_own_updates() {
    let releases = releases_with("99.0.0").await;
    let state = test_state();
    crate::update::set_api_for_tests(&state, &format!("{}/latest", releases.base()));

    // Any other daemon asks when asked (the control: the fake is reachable).
    let (status, body) = request(&state, Method::GET, "/api/v1/update?refresh=true", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(releases.hits(), 1);
    assert_eq!(body["managed"], false);
    assert_eq!(body["latest"]["version"], "99.0.0");

    // The account's cloud: the service updates it, so it asks nobody, and
    // says so instead of passing on what it knew before.
    crate::pro::worker_execution_fixture(&state);
    let (status, body) = request(&state, Method::GET, "/api/v1/update?refresh=true", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "managed");
    assert_eq!(body["managed"], true);
    assert_eq!(body["available"], false);
    assert!(body["latest"].is_null() && body["checked_at"].is_null() && body["error"].is_null());
    // Neither the explicit check above nor the periodic one reached the feed.
    crate::update::check_now(&state).await;
    let (_, cached) = request(&state, Method::GET, "/api/v1/update", None).await;
    assert_eq!(cached["state"], "managed");
    assert_eq!(
        releases.hits(),
        1,
        "the cloud's daemon made a release request"
    );
}

/// The official app's daemon (composed with the extension) updates with the
/// app: with or without a plan it neither asks the public feed nor reports
/// a failed check, and says so (`with_app`). A daemon without the extension
/// reports exactly what it always did.
#[tokio::test]
async fn the_official_apps_daemon_updates_with_the_app_without_a_warning() {
    let releases = releases_with("99.0.0").await;
    let state = test_state_with_extension();
    crate::update::set_api_for_tests(&state, &format!("{}/latest", releases.base()));
    let (status, body) = request(&state, Method::GET, "/api/v1/update?refresh=true", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["with_app"], true);
    assert_eq!(body["state"], "unchecked");
    assert_eq!(body["managed"], false);
    assert_eq!(body["available"], false);
    assert!(
        body["error"].is_null() && body["latest"].is_null(),
        "{body}"
    );
    assert_eq!(
        releases.hits(),
        0,
        "the official app's daemon asked the feed"
    );

    let free = test_state();
    let (_, body) = request(&free, Method::GET, "/api/v1/update", None).await;
    assert!(body.get("with_app").is_none(), "{body}");
}
