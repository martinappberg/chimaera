//! Session-bound cloud guidance. Profile text is untrusted project data;
//! saving it never grants a new tool permission or runs a command here.
use crate::{pro::CloudProfile, AppState};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;

const PROFILE_CAP: usize = 32 * 1024;

/// Pure current observations; no session/workspace/account mutation authority.
pub struct GuidanceContext<'a> {
    pub worker: bool,
    pub profile: &'a CloudProfile,
    pub root: Option<&'a str>,
    pub hours_exhausted: Option<bool>,
    pub providers: &'a [crate::daemon_extension::providers::ProviderStatus],
    pub required_provider_ids: &'a [String],
}
pub struct GuidanceSetup {
    pub setup_command: Option<String>,
    pub pending_setup_command: Option<String>,
    pub note: &'static str,
}
pub(super) const NAMES: &[&str] = &["read_cloud_profile", "update_cloud_profile"];

pub(super) fn available(state: &AppState, session: &str) -> bool {
    state.daemon_extension.is_some()
        && super::workspace_of(state, session)
            .and_then(|workspace| crate::pro::workspace_profile(state, &workspace.id))
            .is_some()
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    expected_revision: String,
    profile: Profile,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    setup_command: Option<String>,
    laptop_only: Vec<String>,
    deferred: Vec<String>,
    missing_environment: Vec<String>,
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
        "Read the current profile before updating it"
    );
    anyhow::ensure!(
        request
            .profile
            .missing_environment
            .iter()
            .all(|name| environment_name(name)),
        "Store environment variable names only, never values"
    );
    Ok(request)
}

pub(super) async fn call(state: &Arc<AppState>, session: &str, name: &str, args: &Value) -> Value {
    let Some(runtime) = state.daemon_extension.as_ref() else {
        return super::tool_error("Cloud profiles are unavailable for this project".into());
    };
    let result = async {
        let generation = crate::pro::profile_generation(state);
        let workspace = super::workspace_of(state, session)
            .ok_or_else(|| anyhow::anyhow!("This session has no project"))?;
        let profile = crate::pro::workspace_profile(state, &workspace.id)
            .ok_or_else(|| anyhow::anyhow!("Cloud profiles are unavailable for this project"))?;
        profile.validate()?;
        if name == "read_cloud_profile" {
            anyhow::ensure!(args.as_object().is_some_and(|object| object.is_empty()), "This tool only reads the current session's project");
            anyhow::ensure!(serde_json::to_vec(&profile)?.len() <= PROFILE_CAP, "This profile is too large; reduce it in project settings");
            return Ok(json!({"revision":revision(&profile, generation),"profile":profile,"context":arrival(state, &workspace.id).await}).to_string());
        }
        let request = update(args)?;
        anyhow::ensure!(request.expected_revision == revision(&profile, generation), "The profile changed; read it again before saving");
        let proposal = runtime.guidance_setup(&profile,request.profile.setup_command.as_deref())
            .ok_or_else(||anyhow::anyhow!("Cloud profiles are unavailable for this project"))?;
        let awaiting_confirmation = proposal.pending_setup_command.is_some();
        let next = CloudProfile {
            setup_command:proposal.setup_command,
            pending_setup_command:proposal.pending_setup_command,
            laptop_only: request.profile.laptop_only,
            deferred: request.profile.deferred,
            missing_environment: request.profile.missing_environment,
        };
        next.validate()?;
        crate::pro::save_workspace_profile(state, &workspace.id, generation, &profile, next.clone()).await?;
        let note = proposal.note;
        Ok::<_, anyhow::Error>(json!({"saved":true,"revision":revision(&next, generation),"executed":false,"awaiting_confirmation":awaiting_confirmation,"note":note}).to_string())
    }.await;
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

pub(crate) async fn arrival(state: &AppState, workspace: &str) -> String {
    let Some(runtime) = state.daemon_extension.as_ref() else {
        return String::new();
    };
    let generation = crate::pro::profile_generation(state);
    let Some(profile) = crate::pro::workspace_profile(state, workspace) else {
        return String::new();
    };
    // On the user's own computer an agent needs no brief about where it runs,
    // unless its project came back from the cloud during this daemon's life
    // (then earlier cloud assumptions must be replaced).
    let worker = crate::pro::is_worker(state);
    if !worker && !crate::pro::returned_here(state, workspace) {
        return String::new();
    }
    let root = crate::lock(&state.workspaces)
        .get(workspace)
        .and_then(|workspace| {
            workspace
                .root
                .to_str()
                .filter(|root| root.len() <= 4096)
                .map(str::to_owned)
        });
    let providers = if worker {
        crate::cloud::providers::cached_observations(state)
    } else {
        Vec::new()
    };
    let required: Vec<_> = crate::pro::workspace_provider_blocks(state, workspace)
        .as_array()
        .into_iter()
        .flatten()
        .take(16)
        .filter_map(|entry| entry["id"].as_str().map(str::to_owned))
        .collect();
    let text = runtime.guidance_arrival(GuidanceContext {
        worker,
        profile: &profile,
        root: root.as_deref(),
        hours_exhausted: crate::pro::cloud_hours_exhausted(state),
        providers: &providers,
        required_provider_ids: &required,
    });
    if generation != crate::pro::profile_generation(state) {
        return String::new();
    }
    text
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
    fn profiles_cannot_smuggle_scope_secrets_or_unbounded_prompt_data() {
        let profile = CloudProfile {
            setup_command: Some("npm ci".into()),
            missing_environment: vec!["API_TOKEN".into()],
            ..Default::default()
        };
        let args = json!({"expected_revision":revision(&profile, 0),"profile":profile});
        assert!(update(&args).is_ok());
        assert_ne!(revision(&profile, 0), revision(&profile, 1));
        let mut foreign = args.clone();
        foreign["workspace_id"] = json!("other");
        assert!(update(&foreign).is_err());
        let mut secret = args.clone();
        secret["profile"]["missing_environment"] = json!(["TOKEN=value"]);
        assert!(update(&secret).is_err());
        let huge = CloudProfile {
            laptop_only: vec!["x".repeat(2048); 64],
            ..Default::default()
        };
        assert!(update(&json!({"expected_revision":revision(&huge, 0),"profile":huge})).is_err());
    }
    #[tokio::test]
    async fn absent_runtime_offers_no_paid_tools_or_brief_and_keeps_reserved_names() {
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
        assert!(arrival(&state, "missing-workspace").await.is_empty());
        let refusal = call(
            &state,
            "missing-session",
            "update_cloud_profile",
            &json!({}),
        )
        .await;
        assert_eq!(refusal["isError"], true);
        assert_eq!(
            refusal["content"][0]["text"],
            "Cloud profiles are unavailable for this project"
        );
        assert!(crate::pro::cloud_hours_exhausted(&state).is_none());
        assert!(!state.cloud_providers.initialized());
        for name in NAMES {
            assert!(super::super::is_core_tool(name));
        }
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
}
