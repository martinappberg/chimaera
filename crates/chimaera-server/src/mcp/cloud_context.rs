//! Where an agent runs and what is here, for projects that move between the
//! user's computer and their cloud machine. The host gathers facts it already
//! holds (this machine, the project, the last move into it, kept-both pairs,
//! cached sign-ins); the optional Runtime words them. Without the Runtime, or
//! for a project that is not synced, nothing here runs, writes or is offered.
//!
//! The note rides the carriers the cluster-job context already proved: Claude
//! hook `additionalContext` (`agents::ingest`, chat and terminal) and the Codex
//! chat developer note (`chat::spawn_chat_session`, `thread/inject_items`).
//! Every agent process gets it once at its start, so each move (which always
//! starts a new process) replaces what the conversation was told before. The
//! read-only `where_am_i` tool answers the same facts at any time.
use crate::{pro::CloudProfile, AppState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{path::PathBuf, sync::Arc};

const PROFILE_CAP: usize = 32 * 1024;
/// The note rides every agent start; a longer one is withheld, not cut.
const NOTE_CAP: usize = 4 * 1024;
/// Left-out paths kept from a move (the sender's list holds up to 4,096).
const LEFT_OUT_KEPT: usize = 200;
const ARRIVAL_CAP: u64 = 64 * 1024;
pub(crate) const LOOKUP: &str = "where_am_i";
pub(super) const NAMES: &[&str] = &[LOOKUP, "update_cloud_profile"];

/// The last move into this machine, kept beside the project's other transfer
/// state (`<data>/pro/<workspace>/arrival.json`), never in the project folder.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Arrival {
    pub at_ms: u64,
    /// The machine it came from (`std::env::consts` names), when the sender
    /// recorded them.
    #[serde(default)]
    pub from_os: Option<String>,
    #[serde(default)]
    pub from_arch: Option<String>,
    /// Project-relative paths the sender left out by rule or size (the first
    /// `LEFT_OUT_KEPT`), and how many there were.
    #[serde(default)]
    pub left_out: Vec<String>,
    #[serde(default)]
    pub left_out_total: usize,
}

/// One file a return kept in both versions: the incoming version is at
/// `file`, the user's earlier one at `copy` (project-relative).
#[derive(Clone, Debug, Serialize)]
pub struct KeptPair {
    pub file: String,
    pub copy: String,
}

/// What an agent in a synced project is told. Only this machine's own
/// observations and what the last move recorded: no account, host, service or
/// plan identifiers.
#[derive(Clone, Debug, Serialize)]
pub struct Facts {
    /// This machine is the user's cloud machine (else their own computer).
    pub cloud: bool,
    pub os: &'static str,
    pub arch: &'static str,
    /// The same absolute path on both machines.
    pub project_folder: String,
    pub home_folder: Option<String>,
    pub arrival: Option<Arrival>,
    /// Untracked folders that never travel and are rebuilt where work runs.
    pub rebuilt_folders: &'static [&'static str],
    pub kept_both: Vec<KeptPair>,
    /// Environment variable NAMES the last move's configuration export left out.
    pub environment_not_copied: Vec<String>,
    /// Cached sign-in observations on this machine, by agent name (the cloud
    /// machine only; nothing is assumed on the user's computer).
    pub signed_in: Vec<String>,
    pub not_signed_in: Vec<String>,
    pub setup_command: Option<String>,
    pub proposed_setup_waiting: bool,
}

pub struct GuidanceSetup {
    pub setup_command: Option<String>,
    pub pending_setup_command: Option<String>,
    pub note: &'static str,
}

fn short_name(value: Option<&str>) -> Option<String> {
    value
        .filter(|v| {
            !v.is_empty()
                && v.len() <= 32
                && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
        .map(str::to_owned)
}

fn arrival_path(state: &AppState, workspace: &str) -> Option<PathBuf> {
    (!workspace.is_empty()
        && workspace.len() <= 128
        && workspace
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
    .then(|| {
        crate::pro::storage(state)
            .join(workspace)
            .join("arrival.json")
    })
}

/// Records a move into this machine before its agents resume. Best effort: a
/// note that cannot be saved never fails the move.
pub(crate) async fn record_arrival(
    state: &AppState,
    workspace: &str,
    left_out: Option<&[PathBuf]>,
    [os, arch]: [Option<&str>; 2],
) {
    if state.daemon_extension.is_none() {
        return;
    }
    let Some(path) = arrival_path(state, workspace) else {
        return;
    };
    let left_out = left_out.unwrap_or_default();
    let arrival = Arrival {
        at_ms: crate::session_view::now_ms(),
        from_os: short_name(os),
        from_arch: short_name(arch),
        left_out: left_out
            .iter()
            .take(LEFT_OUT_KEPT)
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
        left_out_total: left_out.len(),
    };
    let written = tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(path.parent().unwrap_or(&path))?;
        crate::persist::atomic_write_json_durable(&path, serde_json::to_vec(&arrival)?)
    })
    .await;
    if !matches!(written, Ok(Ok(()))) {
        tracing::warn!("could not record what a move left behind for its agents");
    }
}

pub(crate) async fn read_arrival(state: &AppState, workspace: &str) -> Option<Arrival> {
    let path = arrival_path(state, workspace)?;
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let (file, meta) = crate::fs::open_regular(&path).ok()?;
        if meta.len() > ARRIVAL_CAP {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(ARRIVAL_CAP).read_to_end(&mut bytes).ok()?;
        serde_json::from_slice(&bytes).ok()
    })
    .await
    .ok()
    .flatten()
}

/// The facts for a synced project on a daemon with the Runtime, else None.
async fn facts(state: &AppState, workspace: &str) -> Option<(Facts, CloudProfile)> {
    state.daemon_extension.as_ref()?;
    let (profile, kept) = crate::pro::synced(state, workspace)?;
    let root = crate::lock(&state.workspaces).get(workspace)?.root;
    let cloud = crate::pro::is_worker(state);
    let arrival = read_arrival(state, workspace).await;
    let (mut signed_in, mut not_signed_in) = (Vec::new(), Vec::new());
    if cloud {
        use crate::daemon_extension::providers::ProviderState;
        for status in crate::cloud::providers::cached_observations(state)
            .into_iter()
            .take(16)
        {
            match status.state {
                ProviderState::SignedIn => signed_in.push(status.label),
                ProviderState::NeedsSignIn | ProviderState::Missing => {
                    not_signed_in.push(status.label)
                }
                ProviderState::Unknown | ProviderState::Unavailable => {}
            }
        }
    }
    let text = |path: &std::path::Path| path.to_string_lossy().into_owned();
    let facts = Facts {
        cloud,
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        project_folder: text(&root),
        home_folder: state
            .claude_settings_path
            .parent()
            .and_then(std::path::Path::parent)
            .map(text),
        arrival,
        rebuilt_folders: &crate::pro::policy::REBUILT_DIRS,
        kept_both: kept
            .iter()
            .take(32)
            .map(|(copy, file)| KeptPair {
                file: text(file),
                copy: text(copy),
            })
            .collect(),
        environment_not_copied: profile
            .missing_environment
            .iter()
            .filter(|name| environment_name(name))
            .take(32)
            .cloned()
            .collect(),
        signed_in,
        not_signed_in,
        setup_command: profile.setup_command.clone(),
        proposed_setup_waiting: profile.pending_setup_command.is_some(),
    };
    Some((facts, profile))
}

/// The note for agents in this project, worded by the Runtime; None when the
/// Runtime is absent, the project is not synced or the wording is empty.
pub(crate) async fn note(state: &AppState, workspace: &str) -> Option<String> {
    let runtime = state.daemon_extension.as_ref()?;
    let (facts, _) = facts(state, workspace).await?;
    let text = runtime.placement_note(&facts)?;
    let text = text.trim();
    (!text.is_empty() && text.len() <= NOTE_CAP).then(|| text.to_owned())
}

/// The note for one agent session's project. One Option check on a daemon
/// without the Runtime.
pub(crate) async fn note_for_session(state: &AppState, session: &str) -> Option<String> {
    state.daemon_extension.as_ref()?;
    let workspace = crate::lock(&state.session_workspaces)
        .get(session)
        .cloned()?;
    note(state, &workspace).await
}

/// Whether this project's agents get the note and the lookup tool.
pub(crate) fn available_in(state: &AppState, workspace: &str) -> bool {
    state.daemon_extension.is_some() && crate::pro::synced(state, workspace).is_some()
}

pub(super) fn available(state: &AppState, session: &str) -> bool {
    state.daemon_extension.is_some()
        && super::workspace_of(state, session).is_some_and(|w| available_in(state, &w.id))
}

pub(super) fn definitions(state: &AppState) -> Vec<Value> {
    state
        .daemon_extension
        .as_ref()
        .map_or_else(Vec::new, |runtime| runtime.guidance_definitions())
}

fn revision(profile: &CloudProfile, generation: u64) -> String {
    Sha256::digest(serde_json::to_vec(&(generation, profile)).unwrap_or_default())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// An agent may propose (never set) the command a cloud machine runs before
/// work continues there. Everything else in the profile is written by moves
/// and by the user.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    expected_revision: String,
    setup_command: Option<String>,
}
fn environment_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphabetic() || (index > 0 && byte.is_ascii_digit())
        })
}
fn update(args: &Value) -> anyhow::Result<Update> {
    anyhow::ensure!(
        serde_json::to_vec(args)?.len() <= PROFILE_CAP,
        "Cloud profile exceeds 32 KiB"
    );
    let request: Update = serde_json::from_value(args.clone())?;
    anyhow::ensure!(
        request.expected_revision.len() == 64
            && request
                .expected_revision
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit()),
        "Call where_am_i first and pass its profile_revision"
    );
    Ok(request)
}

pub(super) async fn call(state: &Arc<AppState>, session: &str, name: &str, args: &Value) -> Value {
    let Some(runtime) = state.daemon_extension.as_ref() else {
        return super::tool_error("This project does not move between machines".into());
    };
    let result = async {
        let generation = crate::pro::profile_generation(state);
        let workspace = super::workspace_of(state, session)
            .ok_or_else(|| anyhow::anyhow!("This session has no project"))?;
        let (facts, profile) = facts(state, &workspace.id)
            .await
            .ok_or_else(|| anyhow::anyhow!("This project does not move between machines"))?;
        profile.validate()?;
        if name == LOOKUP {
            anyhow::ensure!(
                args.as_object().is_none_or(|object| object.is_empty()),
                "where_am_i takes no arguments"
            );
            let note = runtime.placement_note(&facts).unwrap_or_default();
            return Ok(json!({
                "note": note,
                "facts": facts,
                "profile_revision": revision(&profile, generation),
            })
            .to_string());
        }
        let request = update(args)?;
        anyhow::ensure!(
            request.expected_revision == revision(&profile, generation),
            "The profile changed; call where_am_i again before saving"
        );
        let proposal = runtime
            .guidance_setup(&profile, request.setup_command.as_deref())
            .ok_or_else(|| anyhow::anyhow!("This project does not move between machines"))?;
        let awaiting_confirmation = proposal.pending_setup_command.is_some();
        let next = CloudProfile {
            setup_command: proposal.setup_command,
            pending_setup_command: proposal.pending_setup_command,
            missing_environment: profile.missing_environment.clone(),
        };
        next.validate()?;
        crate::pro::save_workspace_profile(
            state,
            &workspace.id,
            generation,
            &profile,
            next.clone(),
        )
        .await?;
        Ok::<_, anyhow::Error>(
            json!({
                "saved": true,
                "profile_revision": revision(&next, generation),
                "executed": false,
                "awaiting_confirmation": awaiting_confirmation,
                "note": proposal.note,
            })
            .to_string(),
        )
    }
    .await;
    match result {
        Ok(text) => super::tool_text(text),
        // Never return parse diagnostics containing a submitted field/value.
        Err(error) => super::tool_error(if error.is::<serde_json::Error>() {
            "Invalid cloud profile fields".into()
        } else {
            error.to_string()
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_writes_keep_normal_agent_permissions() {
        assert!(!super::super::ALWAYS_ALLOWED_TOOLS.contains(&"update_cloud_profile"));
        assert!(!super::super::MASTERMIND_READ_TOOLS.contains(&"update_cloud_profile"));
    }
    #[test]
    fn profile_proposals_cannot_smuggle_scope_or_unbounded_data() {
        let profile = CloudProfile {
            setup_command: Some("npm ci".into()),
            ..Default::default()
        };
        let args = json!({"expected_revision":revision(&profile, 0),"setup_command":"npm ci"});
        assert!(update(&args).is_ok());
        assert_ne!(revision(&profile, 0), revision(&profile, 1));
        let mut foreign = args.clone();
        foreign["workspace_id"] = json!("other");
        assert!(update(&foreign).is_err());
        // Agents no longer write device-only lists or environment names.
        let mut old = args.clone();
        old["missing_environment"] = json!(["API_TOKEN"]);
        assert!(update(&old).is_err());
        assert!(update(
            &json!({"expected_revision":"0".repeat(64),"setup_command":"x".repeat(40_000)})
        )
        .is_err());
    }
    #[test]
    fn arrival_names_are_short_identifiers_only() {
        assert_eq!(short_name(Some("macos")).as_deref(), Some("macos"));
        assert_eq!(short_name(Some("x86_64")).as_deref(), Some("x86_64"));
        assert_eq!(short_name(Some("mac os")), None);
        assert_eq!(short_name(Some(&"a".repeat(33))), None);
        assert_eq!(short_name(None), None);
    }
    #[tokio::test]
    async fn absent_runtime_offers_no_tools_note_or_record_and_keeps_reserved_names() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-mcp-absent-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let state = Arc::new(AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            root.join("data"),
            root.join("home/.claude"),
        ));
        assert!(definitions(&state).is_empty());
        assert!(!available(&state, "missing-session"));
        assert!(note(&state, "missing-workspace").await.is_none());
        assert!(note_for_session(&state, "missing-session").await.is_none());
        record_arrival(
            &state,
            "w-1",
            Some(&[PathBuf::from("data.csv")]),
            [Some("macos"), None],
        )
        .await;
        assert!(!crate::pro::storage(&state).join("w-1").exists());
        for name in NAMES {
            let refusal = call(&state, "missing-session", name, &json!({})).await;
            assert_eq!(refusal["isError"], true);
            assert!(super::super::is_core_tool(name));
        }
        assert!(!state.cloud_providers.initialized());
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
}
