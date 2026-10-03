use super::*;
use crate::{
    agents::AgentKind,
    cloud::providers::{ProviderState, ProviderStatus},
};
use std::os::unix::fs::PermissionsExt;

fn fixture(root: &Path) -> Arc<AppState> {
    std::fs::create_dir_all(root).unwrap();
    let state = Arc::new(AppState::new(
        "fixture".into(),
        "fixture".into(),
        4242,
        0,
        root.into(),
        root.join("config"),
    ));
    lock(&state.workspaces)
        .import_exact(crate::workspaces::Workspace {
            id: "w-project".into(),
            root: root.into(),
            name: "Fixture".into(),
            last_opened_at: super::super::now(),
            mastermind: None,
            plugins_on: vec![],
            cloud_internal: false,
            hidden: false,
        })
        .unwrap();
    lock(&state.pro.ownership).insert("w-project".into(), Ownership::Hydrating { epoch: 3 });
    state
}
fn entry(kind: AgentKind) -> crate::ledger::LedgerEntry {
    crate::ledger::LedgerEntry {
        id: format!("s-{}", kind.as_str()),
        suspended: true,
        manual_resume_reason: None,
        handoff: None,
        workspace_id: "w-project".into(),
        cwd: PathBuf::from("/missing-fixture-root"),
        pinned_name: None,
        cols: 80,
        rows: 24,
        theme: "dark".into(),
        created_at: 1,
        agent: Some(crate::ledger::LedgerAgent {
            kind,
            resume: None,
            transcript: None,
            native_cwd: None,
            title: "Fixture".into(),
            ui: chimaera_agent::model::SessionUi::Term,
            model: None,
            carryover: None,
        }),
    }
}
fn status(id: &str, state: ProviderState) -> ProviderStatus {
    ProviderStatus {
        disconnect_supported: true,
        id: id.into(),
        label: id.into(),
        category: "agent".into(),
        installed: Some(true),
        state,
        reason: None,
        checked_at: Some(1),
        methods: vec![],
    }
}

#[tokio::test]
async fn actual_deferred_agents_require_their_own_provider_and_unknown_fails_closed() {
    let root = std::env::temp_dir().join(format!(
        "chimaera-provider-kinds-{}",
        chimaera_core::generate_token()
    ));
    let state = fixture(&root);
    for kind in [AgentKind::Claude, AgentKind::Codex, AgentKind::Gemini] {
        crate::ledger::defer(&state, entry(kind)).unwrap();
    }
    let mut other = entry(AgentKind::Antigravity);
    other.workspace_id = "other-project".into();
    crate::ledger::defer(&state, other).unwrap();
    let ids = super::super::provider_gate::required(&state, "w-project");
    assert_eq!(ids, ["claude", "codex", "gemini"]);
    let blocked = super::super::provider_gate::blocked(
        &ids,
        &[
            status("claude", ProviderState::SignedIn),
            status("github", ProviderState::SignedIn),
        ],
    );
    assert_eq!(
        blocked.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(),
        ["codex", "gemini"]
    );
    assert_eq!(blocked[1].reason.as_deref(), Some("unsupported_provider"));
    assert!(super::super::provider_gate::blocked(
        &["claude".into(), "codex".into()],
        &[
            status("claude", ProviderState::SignedIn),
            status("codex", ProviderState::SignedIn)
        ]
    )
    .is_empty());
    assert!(
        super::super::provider_gate::check(&state, "w-project", true)
            .await
            .is_empty(),
        "unconfigured local projects must not probe cloud sign-in"
    );
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn provider_checks_cannot_grant_after_cancellation_account_or_owner_change() {
    let root = std::env::temp_dir().join(format!(
        "chimaera-provider-races-{}",
        chimaera_core::generate_token()
    ));
    let state = fixture(&root);
    for case in 0..3 {
        let generation = state.pro.generation.load(Ordering::Acquire);
        lock(&state.pro.ownership).insert("w-project".into(), Ownership::Hydrating { epoch: 3 });
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let task_state = state.clone();
        let task_entered = entered.clone();
        let task_release = release.clone();
        let task = tokio::spawn(async move {
            finish_hydration_checked(
                &task_state,
                "w-project",
                3,
                generation,
                async { Ok(()) },
                async move {
                    task_entered.notify_one();
                    task_release.notified().await;
                    vec![]
                },
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        assert!(!super::super::may_write(&state, "w-project"));
        match case {
            0 => {
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            }
            1 => {
                state.pro.generation.fetch_add(1, Ordering::AcqRel);
                release.notify_one();
                assert!(task.await.unwrap().is_err());
            }
            _ => {
                lock(&state.pro.ownership).insert(
                    "w-project".into(),
                    Ownership::Remote {
                        epoch: 4,
                        holder: "other-device".into(),
                    },
                );
                release.notify_one();
                assert!(task.await.unwrap().is_err());
            }
        }
        assert!(!super::super::may_write(&state, "w-project"));
        assert_eq!(super::super::owned_epoch(&state, "w-project"), None);
    }
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn missing_provider_keeps_staged_files_and_explicit_retry_resumes_real_pty() {
    const CHILD: &str = "CHIMAERA_PROVIDER_FENCE_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-provider-resume-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(root.join("home")).unwrap();
        let output=std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","pro::engine::provider_tests::missing_provider_keeps_staged_files_and_explicit_retry_resumes_real_pty","--nocapture"])
            .env(CHILD,"1").env("CHIMAERA_HOME",&root).env("HOME",root.join("home")).env("SHELL","/bin/sh").output().unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let root=PathBuf::from(std::env::var("CHIMAERA_HOME").unwrap());
        let state=fixture(&root.join("project"));
        let script=root.join("claude-fixture");
        let proof=root.join("signed-in");let launched=root.join("launched");
        std::fs::write(&script,format!("#!/bin/sh\nif [ \"$1\" = auth ]; then if [ -f '{}' ]; then printf '{{\"loggedIn\":true}}\\n'; else printf '{{\"loggedIn\":false}}\\n'; fi; exit 0; fi\nprintf launched > '{}'\nexec /bin/sleep 20\n",proof.display(),launched.display())).unwrap();
        std::fs::set_permissions(&script,std::fs::Permissions::from_mode(0o700)).unwrap();
        lock(&state.agent_bins).insert(AgentKind::Claude,crate::launcher::AgentDetection{path:Ok(script),version:Some("2.1.204".into()),managed:false,explicit:true,mtime:None});
        let paths=Arc::new(std::sync::Mutex::new(Vec::new()));let observed=paths.clone();
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin=format!("http://{}",listener.local_addr().unwrap());
        let app=axum::Router::new().fallback(axum::routing::any(move |uri:axum::http::Uri| {let observed=observed.clone();async move {lock(&observed).push(uri.path().to_owned());axum::Json(json!({"workspace_id":"w-project","holder_id":"worker-fixture","epoch":3,"expires_at":"2099-01-01T00:00:00Z","server_now":"2026-01-01T00:00:00Z","requires_fork":false}))}}));
        let server=tokio::spawn(async move{axum::serve(listener,app).await.unwrap()});
        *lock(&state.pro.runtime)=Some(Configure{recovery:false,
execution:None,account_id:None,role:Role::Worker,endpoint:origin,keeper_url:String::new(),hours_exhausted:false,delegation:super::super::protocol::Delegation{workspace:None,access_token:"fixture".into(),expires_at:String::new(),scope:vec!["baton".into(),"mirror".into()],device_id:"worker-fixture".into()}});
        crate::ledger::defer(&state,entry(AgentKind::Claude)).unwrap();
        // A terminal in the same project needs no provider at all.
        let mut terminal=entry(AgentKind::Claude);terminal.id="s-terminal".into();terminal.agent=None;terminal.cwd=root.join("project");
        crate::ledger::defer(&state,terminal).unwrap();
        let source=root.join("project/preserved.txt");std::fs::write(&source,"installed source").unwrap();
        // One provider not signed in holds back only its own session: the
        // project runs here and every other session continues.
        finish_hydration(&state,"w-project",3,0,async{Ok(())}).await.unwrap();
        assert!(super::super::may_write(&state,"w-project"));
        assert_eq!(super::super::owned_epoch(&state,"w-project"),Some(3));
        assert!(!launched.exists());
        assert!(state.sessions.get("s-terminal").is_some_and(|s|s.alive));
        assert!(lock(&state.deferred_sessions).contains_key("s-claude"));
        let blocks=super::super::cloud_provider_blocks(&state);
        assert_eq!(blocks[0]["blocked_providers"][0]["id"],"claude");
        assert_eq!(blocks[0]["expected_epoch"],3);
        let row=crate::session_view::sessions_json(&state).into_iter().find(|row|row["id"]=="s-claude").unwrap();
        assert_eq!(row["blocked_provider"],"claude");
        let restored = AppState::new("fixture".into(), "fixture".into(), 4242, 0, root.join("project"), root.join("project/config"));
        assert_eq!(lock(&restored.pro.status).get("w-project").unwrap().blocked_providers[0].id,"claude");
        drop(restored);
        std::fs::write(&source,"preserved through retry").unwrap();
        let request=||serde_json::from_value(json!({"workspace_id":"w-project","expected_epoch":3})).unwrap();
        let failed=super::super::routes::hydrate(axum::extract::State(state.clone()),axum::Json(request())).await;
        assert_eq!(failed.status(),axum::http::StatusCode::CONFLICT);
        let failure:serde_json::Value=serde_json::from_slice(&axum::body::to_bytes(failed.into_body(),4096).await.unwrap()).unwrap();
        assert_eq!(failure["error"],"cloud_provider_not_ready");
        assert_eq!(failure["blocked_providers"][0]["id"],"claude");
        std::fs::write(&proof,"fixture sign-in proof").unwrap();
        let ready=super::super::routes::hydrate(axum::extract::State(state.clone()),axum::Json(request())).await;
        assert_eq!(ready.status(),axum::http::StatusCode::NO_CONTENT);
        assert_eq!(super::super::owned_epoch(&state,"w-project"),Some(3));
        assert!(lock(&state.deferred_sessions).is_empty());
        assert_eq!(state.sessions.list().len(),2);
        tokio::time::timeout(Duration::from_secs(3),async{while !launched.exists(){tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
        assert_eq!(std::fs::read_to_string(&source).unwrap(),"preserved through retry");
        assert!(lock(&paths).iter().all(|path|path=="/v1/baton/w-project"||path=="/v1/baton/w-project/renew"));
        assert!(super::super::cloud_provider_blocks(&state).is_empty());
        state.sessions.kill_all();server.abort();let _=server.await;
    });
}
