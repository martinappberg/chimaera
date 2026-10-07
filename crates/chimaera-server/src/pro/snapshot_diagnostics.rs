//! Snapshot diagnostic fields never emit or retain user-controlled error text.
use anyhow::Error;

pub(super) fn category(error: &Error) -> &'static str {
    if let Some(error) = error.downcast_ref::<std::io::Error>() {
        return match error.kind() {
            std::io::ErrorKind::PermissionDenied => "permission_denied",
            std::io::ErrorKind::NotFound => "not_found",
            std::io::ErrorKind::TimedOut => "timeout",
            _ => "io",
        };
    }
    if error.is::<super::release::UpgradeRequired>() {
        return "service_rejected";
    }
    match error.to_string().as_str() {
        "mirror Git operation failed" => "git",
        "workspace release failed" | "mirror policy update failed" => "service_rejected",
        "session handoff timed out" => "session_timeout",
        "session archive exceeds mirror file limit"
        | "workspace and conversations exceed mirror storage quota" => "quota",
        // Why one conversation could not be saved into a copy (the fixed
        // texts of `bundle::export_for_mirror`), so a "left out" line says
        // which (review R4 follow-up: these all read "other").
        "unknown session" => "session_unknown",
        "session lifecycle operation already in progress" => "session_busy",
        "bundle operation limit" => "session_saves_busy",
        "native conversation is not ready to export" => "conversation_not_started",
        "invalid native conversation ID" => "conversation_id_invalid",
        "Claude transcript is unavailable" | "Codex rollout is unavailable" => "transcript_missing",
        "this agent does not support portable session archives" => "agent_unsupported",
        "session input did not pause before transfer deadline"
        | "agent did not stop before bundle deadline" => "session_timeout",
        "suspended session limit reached" => "session_limit",
        "Session is not in the original transfer roster" => "session_not_in_roster",
        "unknown workspace" => "workspace_unknown",
        text if text.starts_with("Session import needs recovery.") => "import_pending",
        text if text
            .strip_prefix("service request returned HTTP ")
            .is_some_and(|status| {
                status.len() == 3 && status.bytes().all(|byte| byte.is_ascii_digit())
            }) =>
        {
            "service_rejected"
        }
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        lock,
        pro::{
            engine,
            protocol::{Configure, Delegation, Role},
            Ownership,
        },
        AppState,
    };
    use axum::{http::StatusCode, routing::post, Router};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[test]
    fn secrets_in_unknown_or_io_errors_never_become_diagnostic_fields() {
        let sensitive = "https://synthetic:private-token@example.invalid/prompt";
        assert_eq!(category(&anyhow::anyhow!(sensitive)), "other");
        let io = Error::new(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            sensitive,
        ))
        .context(sensitive);
        assert_eq!(category(&io), "permission_denied");
        assert_eq!(
            category(&anyhow::anyhow!("service request returned HTTP 403")),
            "service_rejected"
        );
        // A conversation left out of a copy says why.
        assert_eq!(
            category(&anyhow::anyhow!("Claude transcript is unavailable")),
            "transcript_missing"
        );
        assert_eq!(
            category(
                &anyhow::anyhow!("inner").context("native conversation is not ready to export")
            ),
            "conversation_not_started"
        );
        assert_eq!(
            category(&anyhow::anyhow!(
                "session lifecycle operation already in progress"
            )),
            "session_busy"
        );
        assert_eq!(
            category(&anyhow::anyhow!(
                "service request returned HTTP 403 private-token"
            )),
            "other"
        );
    }

    #[tokio::test]
    async fn rejected_credentials_identify_phase_without_retry_or_ownership_change() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-snapshot-diagnostic-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let state = Arc::new(AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        ));
        lock(&state.workspaces)
            .import_exact(crate::workspaces::Workspace {
                id: "w-diagnostic".into(),
                root: root.clone(),
                name: "Fixture".into(),
                last_opened_at: 1,
                mastermind: None,
                plugins_on: vec![],
                cloud_internal: false,
                hidden: false,
            })
            .unwrap();
        lock(&state.pro().ownership).insert("w-diagnostic".into(), Ownership::Local { epoch: 3 });
        let requests = Arc::new(AtomicUsize::new(0));
        let observed = requests.clone();
        let app = Router::new().route(
            "/v1/mirror/credentials",
            post(move || {
                let observed = observed.clone();
                async move {
                    observed.fetch_add(1, Ordering::SeqCst);
                    (StatusCode::FORBIDDEN, "synthetic-secret-response")
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let config = Configure {
            recovery: false,
            execution: None,
            account_id: None,
            role: Role::Worker,
            endpoint: format!("http://{address}"),
            keeper_url: String::new(),
            delegation: Delegation {
                access_token: "synthetic".into(),
                expires_at: "2099-01-01T00:00:00Z".into(),
                scope: vec!["mirror".into()],
                device_id: "worker-fixture".into(),
                workspace: None,
            },
            hours_exhausted: false,
            alias: None,
        };
        let mut phase = "ownership";
        let error = engine::snapshot_inner(&state, &config, "w-diagnostic", true, None, &mut phase)
            .await
            .unwrap_err();
        assert_eq!(phase, "credentials");
        assert_eq!(category(&error), "service_rejected");
        assert_eq!(error.to_string(), "service request returned HTTP 403");
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert!(matches!(
            lock(&state.pro().ownership).get("w-diagnostic"),
            Some(Ownership::Local { epoch: 3 })
        ));
        assert!(lock(&state.pro().status).get("w-diagnostic").is_none());
        server.abort();
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
}
