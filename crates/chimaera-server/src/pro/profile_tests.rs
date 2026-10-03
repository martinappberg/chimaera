use super::*;
use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use tower::ServiceExt;

struct Fixture {
    root: std::path::PathBuf,
    state: Arc<AppState>,
    workspace: String,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "chimaera-profile-cas-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(root.join("project")).unwrap();
        let state = Arc::new(AppState::new(
            "profile-test".into(),
            "test-host".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        ));
        state.stopping.store(true, Ordering::Release);
        let workspace = lock(&state.workspaces)
            .add(root.join("project"))
            .unwrap()
            .id;
        Self {
            root,
            state,
            workspace,
        }
    }
    async fn request(&self, body: Option<serde_json::Value>, revisions: &[&str]) -> Response {
        let mut request = Request::builder()
            .method(if body.is_some() { "PUT" } else { "GET" })
            .uri(format!(
                "/api/v1/pro/profile?workspace_id={}",
                self.workspace
            ))
            .header(header::AUTHORIZATION, "Bearer profile-test")
            .header(header::CONTENT_TYPE, "application/json");
        for revision in revisions {
            request = request.header(header::IF_MATCH, *revision);
        }
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            crate::app(self.state.clone()).oneshot(
                request
                    .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
                    .unwrap(),
            ),
        )
        .await
        .unwrap()
        .unwrap()
    }
    async fn read(&self) -> (String, serde_json::Value) {
        let response = self.request(None, &[]).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let revision = response.headers()[header::ETAG]
            .to_str()
            .unwrap()
            .to_owned();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (revision, serde_json::from_slice(&body).unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn stale_confirmation_preserves_new_proposal_and_guidance() {
    let fixture = Fixture::new();
    let original = json!({"pending_setup_command":"npm ci","deferred":["xcodebuild test"]});
    assert_eq!(
        fixture.request(Some(original), &[]).await.status(),
        StatusCode::NO_CONTENT
    );
    let (old_revision, mut confirmation) = fixture.read().await;
    let mut updated = confirmation.clone();
    updated["pending_setup_command"] = json!("npm install");
    updated["deferred"] = json!(["xcodebuild test", "xcrun simctl list"]);
    assert_eq!(
        fixture
            .request(Some(updated.clone()), &[&old_revision])
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    confirmation["pending_setup_command"] = serde_json::Value::Null;
    confirmation["setup_command"] = json!("npm ci");
    let refused = fixture.request(Some(confirmation), &[&old_revision]).await;
    assert_eq!(refused.status(), StatusCode::PRECONDITION_FAILED);
    let (new_revision, stored) = fixture.read().await;
    assert_ne!(old_revision, new_revision);
    assert_eq!(stored, updated);
    // The new revision and the newly shown proposal can be confirmed normally.
    let mut confirmed = stored;
    confirmed["pending_setup_command"] = serde_json::Value::Null;
    confirmed["setup_command"] = json!("npm install");
    assert_eq!(
        fixture
            .request(Some(confirmed), &[&new_revision])
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(fixture.read().await.1["setup_command"], "npm install");
}

#[tokio::test]
async fn profile_revision_binds_account_generation_and_validates_conditions() {
    let fixture = Fixture::new();
    let (revision, profile) = fixture.read().await;
    fixture.state.pro.generation.fetch_add(1, Ordering::AcqRel);
    assert_eq!(
        fixture
            .request(Some(profile.clone()), &[&revision])
            .await
            .status(),
        StatusCode::PRECONDITION_FAILED
    );
    for invalid in ["*", "garbage", "W/\"revision\"", "\"nonhex\""] {
        assert_eq!(
            fixture
                .request(Some(profile.clone()), &[invalid])
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        fixture
            .request(Some(profile.clone()), &[&revision, &revision])
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let job = fixture.state.pro.jobs.lock().await;
    assert_eq!(
        fixture.request(Some(profile.clone()), &[]).await.status(),
        StatusCode::CONFLICT
    );
    drop(job);
    // The header is additive: old settings clients still have a valid PUT.
    assert_eq!(
        fixture.request(Some(profile), &[]).await.status(),
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn failed_profile_persistence_restores_the_previous_profile() {
    let fixture = Fixture::new();
    let (revision, before) = fixture.read().await;
    // A regular file at the state-directory path deterministically refuses
    // durable storage without depending on permission behavior under root.
    std::fs::create_dir_all(fixture.state.pro.root.parent().unwrap()).unwrap();
    std::fs::write(&fixture.state.pro.root, b"not a directory").unwrap();
    let failure = fixture
        .request(Some(json!({"setup_command":"npm ci"})), &[&revision])
        .await;
    assert!(!failure.status().is_success());
    let (after_revision, after) = fixture.read().await;
    assert_eq!(after, before);
    assert_eq!(after_revision, revision);
}

#[tokio::test]
async fn cancelled_agent_profile_save_keeps_admission_until_durable() {
    let fixture = Fixture::new();
    fixture.state.pro.configured.store(true, Ordering::Release);
    let disk = fixture.state.pro.persistence.lock().await;
    let updated: super::super::CloudProfile =
        serde_json::from_value(json!({"setup_command":"npm ci"})).unwrap();
    let owner = fixture.state.clone();
    let workspace = fixture.workspace.clone();
    let next = updated.clone();
    let caller = tokio::spawn(async move {
        super::super::save_workspace_profile(&owner, &workspace, 0, &Default::default(), next).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if lock(&fixture.state.pro.preferences)
                .get(&fixture.workspace)
                .is_some_and(|entry| entry.profile == updated)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    caller.abort();
    let _ = caller.await;
    assert!(fixture.state.pro.configuration.try_lock().is_err());
    assert!(fixture.state.pro.jobs.try_lock().is_err());
    drop(disk);
    let _finished = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        fixture.state.pro.configuration.lock(),
    )
    .await
    .unwrap();
    let bytes = std::fs::read(fixture.state.pro.root.join("state.json")).unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        saved["preferences"][&fixture.workspace]["profile"]["setup_command"],
        "npm ci"
    );
}

#[tokio::test]
async fn failed_agent_profile_save_preserves_new_execution_evidence_and_guidance() {
    let fixture = Fixture::new();
    fixture.state.pro.configured.store(true, Ordering::Release);
    std::fs::write(&fixture.state.pro.root, b"not a directory").unwrap();
    let disk = fixture.state.pro.persistence.lock().await;
    let owner = fixture.state.clone();
    let workspace = fixture.workspace.clone();
    let caller = tokio::spawn(async move {
        super::super::save_workspace_profile(
            &owner,
            &workspace,
            0,
            &Default::default(),
            serde_json::from_value(json!({"setup_command":"npm ci"})).unwrap(),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if lock(&fixture.state.pro.preferences)
                .get(&fixture.workspace)
                .is_some_and(|entry| entry.profile.setup_command.is_some())
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    {
        let mut preferences = lock(&fixture.state.pro.preferences);
        let entry = preferences.get_mut(&fixture.workspace).unwrap();
        entry.execution_uncertain = true;
        entry.profile.deferred.push("xcodebuild test".into());
    }
    drop(disk);
    assert!(caller.await.unwrap().is_err());
    let preferences = lock(&fixture.state.pro.preferences);
    let entry = &preferences[&fixture.workspace];
    assert!(entry.execution_uncertain);
    assert_eq!(entry.profile.deferred, ["xcodebuild test"]);
}
