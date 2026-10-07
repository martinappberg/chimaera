//! No extension: admit everything, record nothing, run nothing. The one
//! thing it still honours is the durable fence an earlier composed daemon
//! may have left ([`fence`]).
use std::sync::Arc;

use super::*;
use crate::AppState;

pub struct Inert {
    fence: fence::Fence,
}
impl Inert {
    pub fn new(fence: fence::Fence) -> Self {
        Self { fence }
    }
}
impl WorkspacePolicy for Inert {
    fn composed(&self, _: &AppState) -> bool {
        false
    }
    fn active(&self, _: &AppState) -> bool {
        false
    }
    fn allows(&self, _: &AppState, workspace: &str, _: Need) -> bool {
        !self.fence.fenced(workspace)
    }
    fn reserve(
        &self,
        state: &AppState,
        workspace: &str,
        kind: LaunchKind,
    ) -> anyhow::Result<Option<Reservation>> {
        let need = match kind {
            LaunchKind::Agent => Need::Execute,
            LaunchKind::Shell => Need::Shell,
        };
        if self.allows(state, workspace, need) {
            Ok(None)
        } else {
            Err(Changed.into())
        }
    }
    fn admit_launch<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        workspace: &'a str,
        kind: LaunchKind,
    ) -> BoxFuture<'a, anyhow::Result<(Launch, Option<Reservation>)>> {
        Box::pin(async move {
            self.reserve(state, workspace, kind)?;
            Ok((Launch::inert(), None))
        })
    }
    fn hold_session<'a>(
        &'a self,
        _: &'a AppState,
        _: &str,
        _: &str,
        _: Option<&str>,
        _: bool,
    ) -> anyhow::Result<Hold<'a>> {
        Ok(Hold::none())
    }
    fn check_import(&self, _: &AppState, _: &str, _: Option<&str>) -> anyhow::Result<()> {
        Ok(())
    }
    fn capture(&self, state: &AppState, workspace: &str) -> anyhow::Result<Admission> {
        let admission = Admission::inert(workspace);
        admission.check(state)?;
        Ok(admission)
    }
    fn launch_context(&self, _: &AppState, _: &str) -> LaunchContext {
        LaunchContext::default()
    }
    fn launch_env<'a>(
        &'a self,
        _: &'a AppState,
        _: &'a str,
        _: &'a mut Vec<(String, String)>,
        _: &'a mut Vec<String>,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn updates_managed(&self, _: &AppState) -> bool {
        false
    }
    fn codex_notify_args<'a>(
        &'a self,
        _: &'a AppState,
        _: &'a str,
        _: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, Vec<String>> {
        Box::pin(async { Vec::new() })
    }
    fn session_retired(&self, _: &AppState, _: &str) {}
    fn session_pause(
        &self,
        _: &AppState,
        _: &str,
        _: Option<&crate::ledger::LedgerEntry>,
    ) -> Option<serde_json::Value> {
        None
    }
    fn owner(&self, _: &AppState, _: &str) -> Option<&'static str> {
        None
    }
    fn refusal(&self, _: &AppState, _: &str, watching: bool) -> serde_json::Value {
        if watching {
            serde_json::json!({"type":"error","code":"read_only","reason":"watching",
                "message":"You're watching. Take control to type."})
        } else {
            serde_json::json!({"type":"error","code":"read_only","reason":"elsewhere",
                "message":"This project is not available here right now. That was not sent."})
        }
    }
    fn acted(&self, _: &AppState, _: &str) {}
    fn decorate_sessions(
        &self,
        _: &AppState,
        _: &mut Vec<(u64, serde_json::Value)>,
    ) -> Vec<serde_json::Value> {
        Vec::new()
    }
    fn decorate_workspace(&self, _: &AppState, _: &str, _: &mut serde_json::Value) {}
    fn health(&self, _: &AppState, _: &mut serde_json::Value) {}
    fn scope_admission(&self, _: &AppState, _: &str, _: u64) -> anyhow::Result<Admission> {
        Err(Changed.into())
    }
    fn scope_check(&self, _: &AppState, _: &str, _: u64) -> anyhow::Result<()> {
        Err(Changed.into())
    }
    fn scope_renewing(&self, _: &AppState, _: &str, _: u64) -> bool {
        false
    }
    fn await_scope_renewal<'a>(
        &'a self,
        _: &'a AppState,
        _: &'a str,
        _: u64,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn capture_command(&self, _: &AppState, _: &str) -> anyhow::Result<Option<Admission>> {
        Ok(None)
    }
    fn run_reserved<'a>(
        &'a self,
        reservation: Reservation,
        operation: BoxFuture<'a, axum::response::Response>,
    ) -> BoxFuture<'a, axum::response::Response> {
        Box::pin(async move {
            let _reservation = reservation;
            operation.await
        })
    }
    fn tools(&self, _: &AppState, _: &str) -> Vec<serde_json::Value> {
        Vec::new()
    }
    fn call_tool<'a>(
        &'a self,
        _: &'a Arc<AppState>,
        _: &'a str,
        _: &'a str,
        _: &'a serde_json::Value,
    ) -> Option<BoxFuture<'a, serde_json::Value>> {
        None
    }
    fn auto_tools(&self, _: &AppState, _: &str) -> Vec<String> {
        Vec::new()
    }
    fn start_note<'a>(
        &'a self,
        _: &'a AppState,
        _: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, Option<StartNote>> {
        Box::pin(async { None })
    }
    fn note_told<'a>(&'a self, _: &'a AppState, _: &'a StartNote) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
    fn reads_folder_identity(&self, _: &AppState) -> bool {
        false
    }
    fn workspace_known(&self, _: &AppState, _: &str) {}
    fn workspace_opened<'a>(
        &'a self,
        _: &'a Arc<AppState>,
        _: &'a crate::workspaces::Workspace,
        _: Option<bool>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
    fn started(&self, _: &Arc<AppState>) -> anyhow::Result<()> {
        Ok(())
    }
    fn held_at_boot(&self, _: &AppState, _: &crate::ledger::LedgerEntry) {}
    fn restored(&self, _: &Arc<AppState>) {}
    fn may_resume(&self, _: &AppState, entry: &crate::ledger::LedgerEntry) -> bool {
        !self.fence.fenced(&entry.workspace_id)
    }
    fn resume_check(&self, _: &AppState, _: &crate::ledger::LedgerEntry) -> anyhow::Result<()> {
        Ok(())
    }
    fn stopping(&self, _: &AppState) {}
    fn shutdown<'a>(&'a self, _: &'a Arc<AppState>) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
    fn routed(&self, _: &AppState, _: &str) -> bool {
        false
    }
    fn outside_project(&self, _: &AppState, _: &str, paths: &[String]) -> Vec<String> {
        paths.to_vec()
    }
    fn project_feed(
        &self,
        _: &Arc<AppState>,
        _: &str,
        _: Vec<String>,
        _: Vec<String>,
    ) -> Option<Box<dyn ProjectFeed>> {
        None
    }
    fn proxy_socket<'a>(
        &'a self,
        _: &'a Arc<AppState>,
        _: &'a str,
        _: &'a str,
        _: &'a crate::ws::SocketOptions,
        _: serde_json::Value,
        _: &'a mut axum::extract::ws::WebSocket,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn workspace_resuming(&self, _: &AppState, _: &str) {}
    fn routed_decisions(&self, _: &AppState) -> Vec<(String, String)> {
        Vec::new()
    }
    fn routes(&self, _: &Arc<AppState>) -> axum::Router<Arc<AppState>> {
        axum::Router::new()
    }
    fn api_layers(
        &self,
        _: &Arc<AppState>,
        api: axum::Router<Arc<AppState>>,
    ) -> axum::Router<Arc<AppState>> {
        api
    }
    fn ticket_layers(
        &self,
        _: &Arc<AppState>,
        routes: axum::Router<Arc<AppState>>,
    ) -> axum::Router<Arc<AppState>> {
        routes
    }
    fn outer_layers(&self, app: axum::Router) -> axum::Router {
        app
    }
}
