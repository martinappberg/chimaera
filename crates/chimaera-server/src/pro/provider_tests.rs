use super::*;
use crate::{
    agents::AgentKind,
    cloud::providers::{ProviderState, ProviderStatus},
};

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
            finish_hydration_checked(&task_state, "w-project", 3, generation, async move {
                task_entered.notify_one();
                task_release.notified().await;
                vec![]
            })
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

/// A conversation the cloud machine keeps stopped because its agent is not
/// signed in there has no live session, yet it travels with the project:
/// handing the project back without it would lose the conversation.
#[tokio::test]
async fn a_conversation_waiting_for_its_sign_in_travels_with_the_project() {
    let root = std::env::temp_dir().join(format!(
        "chimaera-provider-roster-{}",
        chimaera_core::generate_token()
    ));
    let state = fixture(&root);
    crate::ledger::defer(&state, entry(AgentKind::Claude)).unwrap();
    let mut other = entry(AgentKind::Codex);
    other.workspace_id = "other-project".into();
    crate::ledger::defer(&state, other).unwrap();
    lock(&state.session_workspaces).insert("s-live".into(), "w-project".into());
    let mut ids = super::transfer_session_ids(&state, "w-project").unwrap();
    ids.sort();
    assert_eq!(ids, ["s-claude", "s-live"]);
    // A stopped session that is also listed live is carried once.
    lock(&state.session_workspaces).insert("s-claude".into(), "w-project".into());
    assert_eq!(
        super::transfer_session_ids(&state, "w-project")
            .unwrap()
            .len(),
        2
    );
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}
