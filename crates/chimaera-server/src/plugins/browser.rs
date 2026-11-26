//! A browser a plugin supplies (`provides.browser`) for the agent browser's
//! Chromium engine (`agent_browser::chromium`), on a host with no Chrome or
//! Chromium of its own.
//!
//! - **Opt-in twice, nothing implicit.** The plugin names one of its
//!   `[[programs]]` that one of its `[[tools]]` installs; the tool downloads
//!   only on the user's click (`toolchain`: pinned URL, sha256 checked as it
//!   streams, size cap). This module never downloads.
//! - **Where it is active.** Like everything a plugin adds, its browser serves
//!   only sessions in workspaces where the plugin is active (switched on,
//!   passing its gates, not held or faulted).
//! - **The user's own browser wins.** The engine asks here only after
//!   `browser.path`, PATH and the macOS app bundles found nothing.
//! - Generic: nothing here names a plugin; the words come from its manifest.

use std::path::PathBuf;

use super::Manifest;
use crate::AppState;

/// What the plugins on this host offer the Chromium engine.
#[derive(Debug)]
pub(crate) enum Offer {
    /// An installed, executable browser of a plugin active in the workspace.
    Ready(PathBuf),
    /// A plugin provides one, but not usable here yet: model-facing words
    /// naming the user's next step.
    NotReady(String),
    /// No installed plugin provides a browser.
    Nothing,
}

/// The browser a plugin active in `ws` provides, or why none is usable.
pub(crate) async fn offer(state: &AppState, ws: Option<&str>) -> Offer {
    let providers: Vec<_> = super::catalog(state)
        .iter()
        .filter(|m| m.provides.browser.is_some())
        .cloned()
        .collect();
    if providers.is_empty() {
        return Offer::Nothing;
    }
    let active = match ws {
        Some(ws) => super::active(state, ws).await,
        None => Vec::new(),
    };
    let is_active = |m: &Manifest| active.iter().any(|a| a.id == m.id);
    // An active provider's state is the more useful answer: try those first.
    let mut ordered = providers;
    ordered.sort_by_key(|m| !is_active(m));
    let mut why: Option<String> = None;
    for m in &ordered {
        let Some(program) = m.provides.browser.as_deref() else {
            continue;
        };
        // `validate` guarantees the tool exists.
        let Some(tool) = m
            .tools
            .iter()
            .find(|t| t.programs.iter().any(|p| p == program))
        else {
            continue;
        };
        if !is_active(m) {
            if why.is_none() {
                why = Some(not_active(state, m, ws.is_some()));
            }
            continue;
        }
        let Some(artifact) = tool.artifact() else {
            why.get_or_insert_with(|| {
                format!(
                    "The {} plugin is switched on here, but it has no {} build for this \
                     computer ({} {}).",
                    m.name,
                    tool.name,
                    std::env::consts::OS,
                    std::env::consts::ARCH
                )
            });
            continue;
        };
        let Some(bin) = super::toolchain::installed_bin(state, &m.id, &tool.id).await else {
            why.get_or_insert_with(|| {
                format!(
                    "The {name} plugin is switched on in this workspace, but its {tool} isn't \
                     downloaded on this host yet. Ask the user to click Install under Tools on \
                     the {name} card in Extensions → Plugins (about {mb} MB from {host}, \
                     checked against its pinned sha256), then call browser_navigate again.",
                    name = m.name,
                    tool = tool.name,
                    mb = artifact.size.div_ceil(1_000_000),
                    host = super::platform::https_host(&artifact.url).unwrap_or("its publisher"),
                )
            });
            continue;
        };
        let exe = bin.join(program);
        let found = tokio::task::spawn_blocking(move || {
            crate::launcher::is_executable(&exe).then_some(exe)
        })
        .await
        .ok()
        .flatten();
        if let Some(exe) = found {
            return Offer::Ready(exe);
        }
        why.get_or_insert_with(|| {
            format!(
                "The {name} plugin's {tool} is installed on this host, but {program} isn't \
                 there in a runnable form. Ask the user to Remove it and click Install again \
                 under Tools on the {name} card in Extensions → Plugins.",
                name = m.name,
                tool = tool.name,
            )
        });
    }
    why.map(Offer::NotReady).unwrap_or(Offer::Nothing)
}

/// Why an installed provider that isn't active in this workspace can't
/// serve it.
fn not_active(state: &AppState, m: &Manifest, has_workspace: bool) -> String {
    if m.origin.fault.is_some() || super::trust::hold(state, m).is_some() {
        return format!(
            "The {} plugin provides a browser, but it can't run on this host now (its card in \
             Extensions → Plugins says why).",
            m.name
        );
    }
    if !has_workspace {
        return format!(
            "The {} plugin provides a browser only to sessions in a workspace where it is \
             switched on, and this session has no workspace.",
            m.name
        );
    }
    format!(
        "The {name} plugin is installed on this host but not switched on in this workspace: ask \
         the user to switch it on in Extensions → Plugins (and, if its browser isn't \
         downloaded yet, to click Install under its Tools), then call browser_navigate again.",
        name = m.name
    )
}
