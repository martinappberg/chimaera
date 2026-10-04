//! Passive account discovery tied to immutable acknowledged checkpoints.
use super::*;

#[derive(Serialize, Deserialize)]
pub struct Metadata {
    version: u32,
    name: String,
    visible: bool,
}

fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 512 && !name.chars().any(char::is_control)
}

pub(in crate::pro) fn metadata(name: &str, visible: bool) -> Option<Metadata> {
    valid_name(name).then(|| Metadata {
        version: 1,
        name: name.into(),
        visible,
    })
}

#[derive(Deserialize)]
struct Page {
    catalog_version: u32,
    projects: Vec<Row>,
    next_cursor: Option<String>,
}
#[derive(Deserialize)]
struct Row {
    workspace_id: String,
    name: String,
    epoch: u64,
    checkpoint_id: String,
}

/// Only an absent capability/legacy 404 falls back to worker discovery. A
/// negotiated empty catalog is authoritative, and errors retain the old cache.
pub(super) async fn list(
    state: &Arc<AppState>,
    config: &Configure,
) -> Result<Option<Vec<Project>>> {
    ensure!(
        config.role == Role::Device && !config.recovery && config.delegation.workspace.is_none(),
        "Project catalog requires device discovery authority"
    );
    let response = engine::account(config, "/v2/capabilities", "GET", None).await?;
    if response.status == 404 {
        return Ok(None);
    }
    let capabilities: serde_json::Value = response.json()?;
    if capabilities
        .get("project_catalog")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
    {
        return Ok(None);
    }
    let mut projects = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PROJECTS {
        let path = cursor.as_ref().map_or_else(
            || "/v2/projects".into(),
            |after| format!("/v2/projects?after={after}"),
        );
        let response = engine::account(config, &path, "GET", None).await?;
        if response.status == 404 && cursor.is_none() {
            return Ok(None);
        }
        let page: Page = response.json()?;
        ensure!(
            page.catalog_version == 1 && page.projects.len() <= MAX_PROJECTS,
            "Unsupported project catalog"
        );
        let mut previous = cursor.clone();
        for row in page.projects {
            ensure!(
                super::super::valid_id(&row.workspace_id)
                    && valid_name(&row.name)
                    && super::super::valid_id(&row.checkpoint_id)
                    && previous.as_ref().is_none_or(|id| row.workspace_id > *id),
                "Invalid project catalog row"
            );
            let _observed_epoch = row.epoch; // Presentation only; Take over reads authenticated Baton.
            previous = Some(row.workspace_id.clone());
            if projects.len() < MAX_PROJECTS {
                projects.push(Project {
                    local_root: local_root(state, &row.workspace_id),
                    destination_saved: account_matches(state, &row.workspace_id)
                        && lock(&state.pro.adoptions).contains_key(&row.workspace_id),
                    workspace_id: row.workspace_id,
                    name: row.name,
                    host_id: None,
                    host_alias: None,
                    available: true,
                    error: None,
                });
            }
        }
        let Some(next) = page.next_cursor else {
            return Ok(Some(projects));
        };
        // The cursor includes omitted legacy/private metadata rows, so it can
        // advance beyond the last visible row, including an empty page.
        ensure!(
            super::super::valid_id(&next)
                && previous.as_ref().is_none_or(|last| next >= *last)
                && cursor.as_ref().is_none_or(|old| next > *old),
            "Invalid project catalog cursor"
        );
        if projects.len() >= MAX_PROJECTS {
            return Ok(Some(projects));
        }
        cursor = Some(next);
    }
    anyhow::bail!("Project catalog exceeds pagination limit")
}

#[cfg(test)]
mod tests;
