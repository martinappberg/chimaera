//! Managed agent runtimes + theming shims (DESIGN.md "Managed agent
//! runtimes" / "Agent theming + shell shims").
//!
//! Chimaera installs and updates the agent CLIs itself — credentials stay
//! entirely the user's (every CLI keeps auth in the user's HOME, and managed
//! binaries run as the user with their HOME + login-shell env). The daemon
//! composes a CURATED install script per agent (official artifact sources
//! only, checksums verified where the source publishes them, never sudo)
//! and spawns it as an ordinary kind-"shell" session, so the installer
//! streams into a visible pane: no silent execution.
//!
//! Layout: `~/.chimaera/agents/<agent>/<version>/bin/<agent>` with an atomic
//! per-agent symlink swap into `~/.chimaera/agents/bin/` (symlink to a fresh
//! name, then `mv -f` = rename(2) over the old link). Running sessions keep
//! their binary across updates; new spawns resolve the new link.
//!
//! Artifact sources, each verified against the real endpoint on 2026-07-06:
//! - claude: `https://downloads.claude.ai/claude-code-releases` — the exact
//!   base URL the official `https://claude.ai/install.sh` uses (`latest` →
//!   version, `manifest.json` → per-platform sha256, `$version/$platform/
//!   claude` → self-contained binary). The installer script itself cannot
//!   target a custom prefix (it ends in `claude install`, which owns
//!   `~/.local`), so the curated script replicates its download + checksum
//!   steps and installs under the chimaera prefix instead.
//! - codex: GitHub release artifacts of `openai/codex` — the
//!   `codex-package-<triple>.tar.gz` variant, because the release publishes
//!   `codex-package_SHA256SUMS` covering it (the bare `codex-<triple>.tar.gz`
//!   has no published checksum). Verified against the live release at
//!   rust-v0.142.5 (2026-07-07): the package tarball's sha256 matches its
//!   SUMS line, its layout is `bin/codex` (plus bundled rg/zsh resources,
//!   unused), and that `bin/codex` is byte-identical to the bare tarball's
//!   binary.
//! - agy: the manifest endpoint pinned inside the official
//!   `https://antigravity.google/cli/install.sh` (version/url/sha512;
//!   the tarball's one member is named `antigravity`). The official
//!   installer's final `agy install` step ("Configure environment paths and
//!   shell settings") is deliberately NOT run: user dotfiles stay the
//!   user's. The installer's own `--dir` flag can't help here because the
//!   script still hands off to `agy install` afterwards.
//! - gemini: no curated install in phase 1 — gemini-cli genuinely needs a
//!   node runtime (DESIGN: phase 2). POST install returns an honest 400.
//!
//! Shims: `~/.chimaera/shims/{claude,codex,gemini,agy}` — tiny sh wrappers
//! prepended to a chimaera session's PATH via spawn env only. Agent spawns
//! are guaranteed to resolve them (the login-shell wrap re-prepends the dir
//! via `$CHIMAERA_SHIMS` after profile init); plain kind-"shell" panes are
//! BEST-EFFORT: rc-file PATH prepends commonly demote the shim dir
//! (measured on this machine — a typed `claude` resolved ~/.local/bin,
//! bypassing the shim) until the queued ZDOTDIR-based shell integration
//! lands and reclaims the front slot. Each shim resolves the real binary
//! (the user's own install first, managed fallback) and injects the
//! scheme-matched theme, gated on
//! `CHIMAERA_SESSION` so a copied script can never theme a terminal outside
//! chimaera. Theme levers per CLI, verified against the real binaries on
//! this machine (2026-07-06):
//! - claude 2.1.202: `"theme"` in a `--settings` file (differential PTY
//!   boot: light renders 38;5;241 secondary text, dark 38;5;246 — the flag
//!   file wins over the user's settings.json layer). Injection is skipped
//!   when the user's own `~/.claude/settings.json` sets a theme (respect an
//!   explicit user choice; only fill the gap).
//! - codex 0.142.5: `-c tui.theme=<kebab-name>` (the config schema at tag
//!   rust-v0.142.5 documents `tui.theme` as "overrides automatic light/dark
//!   theme detection"). The injected names are the ones codex itself picks
//!   per scheme (light → catppuccin-latte, dark → catppuccin-mocha), pinned
//!   so a mis-answered OSC-11 background query cannot flip them. Skipped
//!   when `~/.codex/config.toml` sets a theme.
//! - gemini 0.49.0: no theme CLI flag (`--help`) and no theme env var (the
//!   bundle resolves `ui.theme` from its own settings files only). Its
//!   settings.json is the user's — never edited — so gemini spawns
//!   un-themed; its ANSI-16 UI follows the pane palette.
//! - agy 1.0.16: no theme flag (`--help`) or env var (only internal
//!   protobuf editor-theme types). Un-themed, same reasoning.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::agents::AgentKind;
use crate::launcher::managed_fallback;
use crate::AppState;

/// The managed bin dir: one symlink per installed agent, swapped atomically
/// on install/update. This is also detection's fallback when the login
/// shell misses.
pub(crate) fn managed_bin_dir(managed_root: &Path) -> PathBuf {
    managed_root.join("bin")
}

/// Whether a resolved binary lives under the managed prefix (the
/// `"managed": true` flag on GET /api/v1/agents rows).
pub(crate) fn is_managed(path: &Path, managed_root: &Path) -> bool {
    path.starts_with(managed_root)
}

// --- curated install scripts -------------------------------------------------

/// Official artifact base URLs (see module docs for verification notes).
const CLAUDE_DOWNLOAD_BASE: &str = "https://downloads.claude.ai/claude-code-releases";
const CODEX_RELEASE_BASE: &str = "https://github.com/openai/codex/releases/latest/download";
/// Pinned inside https://antigravity.google/cli/install.sh (fetched
/// 2026-07-06); serves `manifests/<platform>.json` with version/url/sha512.
const AGY_MANIFEST_BASE: &str =
    "https://antigravity-cli-auto-updater-974169037036.us-central1.run.app";

/// Single-quote `s` for sh (`'` becomes `'\''`): every path interpolated
/// into a generated script or shim lands inside one of these — a quote in
/// the prefix must not terminate the quoting context.
fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Shared script prelude: strict mode, platform detection, and the helpers
/// every per-agent script uses. `root` is embedded through `sq` (it is the
/// daemon's own `~/.chimaera/agents`; no user input reaches it, but a home
/// directory may still carry a quote).
fn script_prelude(root: &Path) -> String {
    format!(
        r#"set -euo pipefail
root={root}
case "$(uname -s)" in Darwin) os=darwin ;; Linux) os=linux ;; *) echo "chimaera: unsupported OS $(uname -s)" >&2; exit 1 ;; esac
case "$(uname -m)" in x86_64|amd64) arch=x86_64 ;; arm64|aarch64) arch=aarch64 ;; *) echo "chimaera: unsupported architecture $(uname -m)" >&2; exit 1 ;; esac
musl=no
if [ "$os" = linux ]; then
  if [ -f /lib/libc.musl-x86_64.so.1 ] || [ -f /lib/libc.musl-aarch64.so.1 ] || ldd /bin/ls 2>&1 | grep -q musl; then musl=yes; fi
fi
sha256() {{ if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }}
sha512() {{ if command -v sha512sum >/dev/null 2>&1; then sha512sum "$1"; else shasum -a 512 "$1"; fi | cut -d' ' -f1; }}
# Atomic activation: symlink under a fresh name, then rename(2) over the old
# link — readers see the old or the new target, never a missing one.
swap() {{ # swap <agent> <version>
  mkdir -p "$root/bin"
  ln -sfn "../$1/$2/bin/$1" "$root/bin/.$1.new"
  mv -f "$root/bin/.$1.new" "$root/bin/$1"
}}
"#,
        root = sq(&root.display().to_string()),
    )
}

/// The curated install/update script for one agent, or `None` when no
/// phase-1 managed install exists (gemini needs a node runtime). The daemon
/// composes this itself — never from the client — and every script ends by
/// printing the installed version through the swapped symlink.
pub(crate) fn install_script(kind: AgentKind, managed_root: &Path) -> Option<String> {
    let prelude = script_prelude(managed_root);
    let body = match kind {
        AgentKind::Claude => format!(
            r#"base='{base}'
echo "chimaera: installing Claude Code from $base into $root/claude"
case "$arch" in x86_64) carch=x64 ;; *) carch=arm64 ;; esac
platform="$os-$carch"
[ "$musl" = yes ] && platform="$os-$carch-musl"
version="$(curl -fsSL "$base/latest")"
# The version builds filesystem paths and echo lines: anything outside
# [0-9.A-Za-z-] (traversal, control bytes) is refused, unechoed.
case "$version" in
  *[!0-9.A-Za-z-]*|'') echo "chimaera: unexpected version string from $base/latest" >&2; exit 1 ;;
  [0-9]*.[0-9]*) ;;
  *) echo "chimaera: unexpected version '$version' from $base/latest" >&2; exit 1 ;;
esac
manifest="$(curl -fsSL "$base/$version/manifest.json")"
if [[ "$manifest" =~ \"$platform\"[^}}]*\"checksum\"[[:space:]]*:[[:space:]]*\"([a-f0-9]{{64}})\" ]]; then
  checksum="${{BASH_REMATCH[1]}}"
else
  echo "chimaera: no checksum for $platform in the release manifest" >&2; exit 1
fi
tmp="$(mktemp -d "${{TMPDIR:-/tmp}}/chimaera-claude.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
echo "chimaera: fetching claude $version ($platform)"
curl -fsSL -o "$tmp/claude" "$base/$version/$platform/claude"
actual="$(sha256 "$tmp/claude")"
if [ "$actual" != "$checksum" ]; then
  echo "chimaera: sha256 mismatch (expected $checksum, got $actual)" >&2; exit 1
fi
chmod +x "$tmp/claude"
dest="$root/claude/$version/bin"
mkdir -p "$dest"
mv -f "$tmp/claude" "$dest/claude"
swap claude "$version"
echo "chimaera: installed claude -> $root/bin/claude"
"$root/bin/claude" --version
"#,
            base = CLAUDE_DOWNLOAD_BASE,
        ),
        // The codex-package-<triple>.tar.gz variant, not the bare
        // codex-<triple>.tar.gz: only the package variant is covered by the
        // release's published codex-package_SHA256SUMS, and its bin/codex is
        // byte-identical to the bare binary (verified at rust-v0.142.5).
        AgentKind::Codex => format!(
            r#"base='{base}'
case "$os" in darwin) triple="$arch-apple-darwin" ;; *) triple="$arch-unknown-linux-musl" ;; esac
echo "chimaera: installing Codex from $base into $root/codex"
tmp="$(mktemp -d "${{TMPDIR:-/tmp}}/chimaera-codex.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
curl -fsSL -o "$tmp/codex.tar.gz" "$base/codex-package-$triple.tar.gz"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/codex-package_SHA256SUMS"
checksum="$(sed -n "s/^\([0-9a-f]\{{64\}}\)[[:space:]][[:space:]]*codex-package-$triple\.tar\.gz$/\1/p" "$tmp/SHA256SUMS")"
if [ -z "$checksum" ]; then
  echo "chimaera: no checksum for codex-package-$triple.tar.gz in codex-package_SHA256SUMS" >&2; exit 1
fi
actual="$(sha256 "$tmp/codex.tar.gz")"
if [ "$actual" != "$checksum" ]; then
  echo "chimaera: sha256 mismatch (expected $checksum, got $actual)" >&2; exit 1
fi
tar -xzf "$tmp/codex.tar.gz" -C "$tmp" bin/codex
chmod +x "$tmp/bin/codex"
version="$("$tmp/bin/codex" --version | sed -n 's/^codex-cli //p')"
if [ -z "$version" ]; then echo "chimaera: could not read the codex version" >&2; exit 1; fi
# The version builds filesystem paths and echo lines: anything outside
# [0-9.A-Za-z-] (traversal, control bytes) is refused, unechoed.
case "$version" in
  *[!0-9.A-Za-z-]*|'') echo "chimaera: unexpected codex version string" >&2; exit 1 ;;
esac
dest="$root/codex/$version/bin"
mkdir -p "$dest"
mv -f "$tmp/bin/codex" "$dest/codex"
swap codex "$version"
echo "chimaera: installed codex -> $root/bin/codex"
"$root/bin/codex" --version
"#,
            base = CODEX_RELEASE_BASE,
        ),
        AgentKind::Antigravity => format!(
            r#"base='{base}'
echo "chimaera: installing the Antigravity CLI from $base into $root/agy"
case "$arch" in x86_64) garch=amd64 ;; *) garch=arm64 ;; esac
platform="${{os}}_${{garch}}"
[ "$musl" = yes ] && platform="${{platform}}_musl"
manifest="$(curl -fsSL "$base/manifests/$platform.json")"
jsonkey() {{ printf '%s\n' "$manifest" | sed -n 's/.*"'"$1"'"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1; }}
version="$(jsonkey version)"
url="$(jsonkey url)"
sha="$(jsonkey sha512)"
if [ -z "$version" ] || [ -z "$url" ] || [ -z "$sha" ]; then
  echo "chimaera: could not parse the release manifest" >&2; exit 1
fi
# The version builds filesystem paths and echo lines: anything outside
# [0-9.A-Za-z-] (traversal, control bytes) is refused, unechoed.
case "$version" in
  *[!0-9.A-Za-z-]*|'') echo "chimaera: unexpected version string in the release manifest" >&2; exit 1 ;;
esac
tmp="$(mktemp -d "${{TMPDIR:-/tmp}}/chimaera-agy.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
echo "chimaera: fetching agy $version"
curl -fsSL -o "$tmp/agy.pkg" "$url"
actual="$(sha512 "$tmp/agy.pkg")"
if [ "$actual" != "$sha" ]; then
  echo "chimaera: sha512 mismatch (expected $sha, got $actual)" >&2; exit 1
fi
case "$url" in
  *.tar.gz*) tar -xzf "$tmp/agy.pkg" -C "$tmp" antigravity; bin="$tmp/antigravity" ;;
  *) bin="$tmp/agy.pkg" ;;
esac
chmod +x "$bin"
dest="$root/agy/$version/bin"
mkdir -p "$dest"
mv -f "$bin" "$dest/agy"
if [ "$os" = darwin ]; then xattr -d com.apple.quarantine "$dest/agy" 2>/dev/null || true; fi
swap agy "$version"
echo "chimaera: installed agy -> $root/bin/agy"
echo "chimaera: (the official installer would now run 'agy install' to edit your shell config; chimaera skips that on purpose)"
"$root/bin/agy" --version
"#,
            base = AGY_MANIFEST_BASE,
        ),
        // gemini-cli genuinely needs a node runtime (DESIGN: phase 2); no
        // official standalone artifact exists to curate.
        AgentKind::Gemini => return None,
    };
    Some(format!("{prelude}{body}"))
}

// --- POST /api/v1/agents/{id}/install ----------------------------------------

#[derive(Deserialize)]
pub(crate) struct InstallBody {
    workspace_id: String,
}

/// How long an install reservation stays authoritative without a visible
/// session: `start_install` inserts it before `SessionManager::spawn`
/// registers the session, so inside this window "no live session" proves
/// nothing — a same-agent POST must 409, not overwrite.
pub(crate) const INSTALL_RESERVATION_GRACE: std::time::Duration =
    std::time::Duration::from_secs(10);

/// POST /api/v1/agents/{id}/install — spawn the curated install/update
/// command as an ordinary kind-"shell" session in the given workspace, so
/// installer output streams into a normal pane. 200 `{"session_id"}`;
/// 404 unknown agent id or workspace; 409 while an install session for the
/// same agent is still running; 400 for agents with no curated install
/// (gemini, phase 2).
pub(crate) async fn install_agent(
    State(state): State<Arc<AppState>>,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<InstallBody>,
) -> Response {
    let Some(kind) = AgentKind::parse(&id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown agent {id:?}")})),
        )
            .into_response();
    };
    let Some(workspace) = crate::lock(&state.workspaces).get(&body.workspace_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown workspace {}", body.workspace_id)})),
        )
            .into_response();
    };
    let Some(script) = install_script(kind, &state.managed_root) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!(
                "no managed install for {}: gemini-cli needs a node runtime (phase 2); \
                 use `{}` yourself instead",
                kind.product_name(),
                crate::launcher::install_command(kind),
            )})),
        )
            .into_response();
    };
    match start_install(&state, kind, &workspace, script) {
        Ok(session_id) => Json(json!({"session_id": session_id})).into_response(),
        Err(response) => *response,
    }
}

/// Spawn one install session (kind "shell", pinned name, standard session
/// env) and its exit watcher. `Err` carries the ready-made 409/500
/// response (boxed: a `Response` is bulky next to the id). Factored from
/// the handler so tests can drive the session mechanics with a stub script
/// instead of the real (network-bound) curated one.
pub(crate) fn start_install(
    state: &Arc<AppState>,
    kind: AgentKind,
    workspace: &crate::workspaces::Workspace,
    script: String,
) -> Result<String, Box<Response>> {
    let session_id = crate::agents::fresh_session_id();
    {
        // One install session per agent: replace only stale entries. Stale
        // means no live session AND older than the reservation grace — the
        // session is registered only after spawn, so a fresh reservation
        // with no visible session is a spawn in flight, not a leftover.
        let mut installs = crate::lock(&state.installs);
        if let Some((existing, reserved)) = installs.get(&kind) {
            if state.sessions.get(existing).is_some()
                || reserved.elapsed() < INSTALL_RESERVATION_GRACE
            {
                return Err(Box::new(
                    (
                        StatusCode::CONFLICT,
                        Json(json!({
                            "error": format!(
                                "an install session for {} is already running",
                                kind.product_name()
                            ),
                            "session_id": existing,
                        })),
                    )
                        .into_response(),
                ));
            }
        }
        installs.insert(kind, (session_id.clone(), std::time::Instant::now()));
    }

    let opts = chimaera_pty::SpawnOpts {
        cwd: workspace.root.clone(),
        // Pinned name: the pane reads as what it is on every surface.
        name: Some(format!("install {}", kind.as_str())),
        cols: 80,
        rows: 24,
        command: Some(vec!["/bin/bash".to_string(), "-c".to_string(), script]),
        id: Some(session_id.clone()),
        env: crate::api::session_env(state, &session_id, "dark"),
        env_remove: crate::api::launcher_context_env(),
        scrollback: crate::lock(&state.settings).scrollback_lines(),
    };
    match state.sessions.spawn(opts) {
        Ok(info) => {
            crate::lock(&state.session_workspaces).insert(info.id.clone(), workspace.id.clone());
            spawn_install_watch(state.clone(), kind, info.id.clone());
            state.changes.notify_waiters();
            Ok(info.id)
        }
        Err(err) => {
            // Clear only this call's own reservation: a concurrent caller
            // may have legitimately reclaimed the slot already.
            let mut installs = crate::lock(&state.installs);
            if installs.get(&kind).map(|(sid, _)| sid.as_str()) == Some(session_id.as_str()) {
                installs.remove(&kind);
            }
            drop(installs);
            tracing::error!(%err, "failed to spawn install session");
            Err(Box::new(
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": err.to_string()})),
                )
                    .into_response(),
            ))
        }
    }
}

/// Watch an install session; when it ends, re-detect that agent (bypassing
/// the daemon-lifetime cache) and regenerate the shims so the next spawn —
/// and the open popover, via the change notification — sees the new binary.
fn spawn_install_watch(state: Arc<AppState>, kind: AgentKind, session_id: String) {
    tokio::spawn(async move {
        while state.sessions.get(&session_id).is_some() {
            tokio::time::sleep(crate::agents::poll_interval()).await;
        }
        let detection = crate::launcher::detect(&state, kind, true).await;
        tracing::info!(
            agent = kind.as_str(),
            installed = detection.path.is_ok(),
            "re-detected agent after install session ended"
        );
        // A managed install just landed — a shim for it should now exist.
        regenerate_shims(&state);
        // Clear only this watcher's own session: a stale-slot reclaim may
        // have installed a newer reservation under the same agent.
        let mut installs = crate::lock(&state.installs);
        if installs.get(&kind).map(|(sid, _)| sid.as_str()) == Some(session_id.as_str()) {
            installs.remove(&kind);
        }
        drop(installs);
        state.changes.notify_waiters();
    });
}

// --- DELETE /api/v1/agents/{id}/install --------------------------------------

/// Remove a directory tree, treating "already gone" as success.
fn remove_dir_if_exists(path: &Path) -> anyhow::Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("failed to remove {}", path.display())),
    }
}

/// DELETE /api/v1/agents/{id}/install — uninstall a chimaera-MANAGED agent: the
/// active symlink plus its version tree under `~/.chimaera/agents`. Only ever
/// touches chimaera's own prefix — the user's own install (and its auth in
/// `$HOME`) is never touched. Running sessions keep their already-exec'd binary
/// (the inode survives the unlink). 404 unknown id; 409 while an install for the
/// same agent is in flight; 200 `{"removed": bool}` otherwise (`false` = nothing
/// chimaera-managed to remove).
pub(crate) async fn uninstall_agent(
    State(state): State<Arc<AppState>>,
    UrlPath(id): UrlPath<String>,
) -> Response {
    let Some(kind) = AgentKind::parse(&id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown agent {id:?}")})),
        )
            .into_response();
    };
    // Never yank files from under a running install of the same agent.
    {
        let installs = crate::lock(&state.installs);
        if let Some((existing, reserved)) = installs.get(&kind) {
            if state.sessions.get(existing).is_some()
                || reserved.elapsed() < INSTALL_RESERVATION_GRACE
            {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({"error": format!(
                        "an install session for {} is running", kind.product_name()
                    )})),
                )
                    .into_response();
            }
        }
    }
    let managed_bin = managed_bin_dir(&state.managed_root);
    if managed_fallback(kind.as_str(), &managed_bin).is_none() {
        // Nothing of ours to remove (a user's own install is not ours to touch).
        return Json(json!({"removed": false})).into_response();
    }
    // Drop the active symlink first (so nothing resolves a half-deleted tree),
    // then the version tree.
    let link = managed_bin.join(kind.as_str());
    let tree = state.managed_root.join(kind.as_str());
    if let Err(err) = remove_if_exists(&link).and_then(|()| remove_dir_if_exists(&tree)) {
        tracing::error!(%err, agent = kind.as_str(), "failed to uninstall managed agent");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response();
    }
    // The shim for it now disappears (unless an explicit path keeps it), and the
    // detection cache must forget the managed binary.
    regenerate_shims(&state);
    state.changes.notify_waiters();
    Json(json!({"removed": true})).into_response()
}

// --- theming shims ------------------------------------------------------------

/// Codex's own per-scheme defaults (tui/src/render/highlight.rs at
/// rust-v0.142.5), pinned via `-c tui.theme=` so the scheme cannot be
/// mis-detected inside a web pane.
pub(crate) fn codex_theme_name(theme: &str) -> &'static str {
    if theme == "light" {
        "catppuccin-latte"
    } else {
        "catppuccin-mocha"
    }
}

/// Whether the user's own claude settings file sets a theme — if so,
/// chimaera respects it and skips injection (fill the gap, never fight an
/// explicit choice).
pub(crate) fn claude_user_theme_set(settings_path: &Path) -> bool {
    let Ok(contents) = std::fs::read_to_string(settings_path) else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&contents)
        .ok()
        .is_some_and(|v| v.get("theme").is_some_and(|t| !t.is_null()))
}

/// Whether the user's codex config sets a theme — any of the TOML
/// spellings codex 0.142.5 accepts: `theme = ...` under `[tui]`, the
/// dotted-key `tui.theme = ...`, or the inline-table `tui = { theme = ...
/// }`. Line-based on purpose: no toml dependency for a yes/no gate.
pub(crate) fn codex_user_theme_set(config_path: &Path) -> bool {
    let Ok(contents) = std::fs::read_to_string(config_path) else {
        return false;
    };
    contents.lines().any(|line| {
        let line = line.trim_start();
        if line.starts_with("theme") && line[5..].trim_start().starts_with('=') {
            return true;
        }
        if let Some(rest) = line.strip_prefix("tui.theme") {
            if rest.trim_start().starts_with('=') {
                return true;
            }
        }
        // Inline table: coarse on purpose (a `tui = { ... }` line naming
        // `theme` anywhere) — over-matching skips injection, never fights.
        if let Some(rest) = line.strip_prefix("tui") {
            let rest = rest.trim_start();
            if let Some(value) = rest.strip_prefix('=') {
                let value = value.trim_start();
                if value.starts_with('{') && value.contains("theme") {
                    return true;
                }
            }
        }
        false
    })
}

/// The explicit `agents.<kind>.path` overrides from settings, as a map.
pub(crate) fn explicit_agent_paths(state: &AppState) -> HashMap<AgentKind, String> {
    let mut settings = crate::lock(&state.settings);
    let mut map = HashMap::new();
    for kind in AgentKind::ALL {
        if let Some(path) = settings.agent_path(kind) {
            map.insert(kind, path);
        }
    }
    map
}

/// Regenerate the shims from current settings + managed installs, and drop the
/// agent detection cache so the next spawn re-resolves. Called at daemon start
/// and after anything that changes resolution: an install, an uninstall, or a
/// settings edit to `agents.*.path`.
pub(crate) fn regenerate_shims(state: &AppState) {
    let explicit = explicit_agent_paths(state);
    if let Err(err) = write_shims(
        &state.shims_dir,
        &managed_bin_dir(&state.managed_root),
        &explicit,
    ) {
        tracing::warn!(%err, "failed to regenerate shims");
    }
    // The next detection must see the new world (a removed managed binary, a
    // new explicit path), so clear the daemon-lifetime cache.
    crate::lock(&state.agent_bins).clear();
}

/// Generate `~/.chimaera/shims/*`: called at daemon start and after every
/// install/uninstall/settings change (via [`regenerate_shims`]). The shims are
/// daemon-owned, but live sessions exec them, so every file goes in via tmp +
/// rename — never truncated in place.
///
/// A shim is written for a kind ONLY when chimaera has something to contribute:
/// a managed install of it exists, or the user pinned an explicit
/// `agents.<kind>.path`. Otherwise the shim is REMOVED (or never created), so
/// typing `<bin>` resolves through the user's own PATH exactly like a plain
/// terminal — chimaera never shadows an install it doesn't own and then fails.
pub(crate) fn write_shims(
    shim_dir: &Path,
    managed_bin: &Path,
    explicit: &HashMap<AgentKind, String>,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(shim_dir)
        .with_context(|| format!("failed to create {}", shim_dir.display()))?;
    // The minimal theme settings files the claude shim points --settings at
    // (typed spawns; launcher spawns merge the theme into the hook settings).
    for theme in ["light", "dark"] {
        let path = shim_dir.join(format!("claude-theme-{theme}.json"));
        write_atomic(
            &path,
            format!("{{\"theme\":\"{theme}\"}}\n").as_bytes(),
            0o644,
        )?;
    }
    for kind in AgentKind::ALL {
        let path = shim_dir.join(kind.as_str());
        let explicit_path = explicit.get(&kind).map(String::as_str);
        let managed = managed_fallback(kind.as_str(), managed_bin).is_some();
        if explicit_path.is_some() || managed {
            write_atomic(
                &path,
                shim_script(kind, shim_dir, managed_bin, explicit_path).as_bytes(),
                0o755,
            )?;
        } else {
            // A prior managed install now uninstalled, or a cleared explicit
            // path: drop the stale shim so it stops shadowing the user's own.
            remove_if_exists(&path)?;
        }
    }
    Ok(())
}

/// Remove a file, treating "already gone" as success (idempotent shim cleanup).
fn remove_if_exists(path: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("failed to remove {}", path.display())),
    }
}

/// Put a file in place via `<path>.tmp` + rename(2): the targets are live
/// files (0755 shims mid-exec, theme JSONs mid-read), and an in-place
/// truncate hands a racing reader a partial file — measured as real exec
/// failures. Permissions land on the tmp, so the target never transitions
/// through a wrong mode.
fn write_atomic(path: &Path, contents: &[u8], mode: u32) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, contents).with_context(|| format!("failed to write {}", tmp.display()))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("failed to chmod {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("failed to move {} into place", tmp.display()))?;
    Ok(())
}

/// One shim: resolve the real binary (an explicit `agents.<kind>.path` wins,
/// then the user's own install — with the shim dir stripped from PATH so the
/// shim never finds itself — then a chimaera-managed install), then exec with
/// the scheme-matched theme when inside a chimaera session.
fn shim_script(
    kind: AgentKind,
    shim_dir: &Path,
    managed_bin: &Path,
    explicit: Option<&str>,
) -> String {
    let bin = kind.as_str();
    let resolve = format!(
        r#"#!/bin/sh
# generated by chimaera — do not edit (rewritten at daemon start and after installs)
# Resolves the real {bin} (an explicit path you set, else your own install, else
# a chimaera-managed one) and themes it to match the chimaera pane. Applies ONLY
# inside chimaera sessions (CHIMAERA_SESSION); elsewhere this dir is not on PATH.
shims={shims}
real=''
# An explicit agents.{bin}.path setting wins outright when it is runnable.
explicit={explicit}
if [ -n "$explicit" ] && [ -x "$explicit" ]; then
  real="$explicit"
else
  clean=''
  old_ifs="${{IFS-}}"; IFS=:
  for d in $PATH; do
    [ "$d" = "$shims" ] && continue
    clean="${{clean:+$clean:}}$d"
  done
  IFS="$old_ifs"
  # An empty $clean (PATH held only the shim dir) never reaches command -v:
  # some shells search the cwd through an empty PATH.
  if [ -n "$clean" ]; then
    real="$(PATH="$clean" command -v {bin} 2>/dev/null || true)"
  fi
fi
"#,
        shims = sq(&shim_dir.display().to_string()),
        explicit = explicit.map(sq).unwrap_or_else(|| "''".to_string()),
    );
    // The Antigravity IDE ships an `agy` symlink to its own app launcher
    // (opens the GUI, exits 0) — same guard as server-side detection.
    let agy_guard = r#"if [ -n "$real" ]; then
  target="$(readlink -f "$real" 2>/dev/null || readlink "$real" 2>/dev/null || printf '%s' "$real")"
  case "$target" in
    *Antigravity.app*|*/antigravity) real='' ;; # the IDE's launcher, not the CLI
  esac
fi
"#;
    let fallback = format!(
        r#"[ -n "$real" ] || real={managed}
if [ ! -x "$real" ]; then
  echo "chimaera: {bin} is not installed." >&2
  echo "  add your own {bin} to your PATH, or install one from the agent launcher (+ new agent)." >&2
  exit 127
fi
"#,
        managed = sq(&format!("{}/{bin}", managed_bin.display())),
    );
    let exec = match kind {
        // Theme via a minimal --settings file; the user's own flags come
        // after ours, so an explicit `--settings` they typed still wins.
        // Skipped when their ~/.claude/settings.json sets a theme. The
        // settings paths expand from the sq-quoted $shims above.
        AgentKind::Claude => r#"if [ -n "${CHIMAERA_SESSION:-}" ] && ! grep -qs '"theme"' "$HOME/.claude/settings.json"; then
  case "${CHIMAERA_THEME:-dark}" in
    light) exec "$real" --settings "$shims/claude-theme-light.json" "$@" ;;
    *)     exec "$real" --settings "$shims/claude-theme-dark.json" "$@" ;;
  esac
fi
exec "$real" "$@"
"#
        .to_string(),
        // -c CLI overrides outrank config.toml, so skip when the user's own
        // config already picks a theme — under [tui] or as the dotted
        // tui.theme spelling (the inline-table form is caught server-side).
        AgentKind::Codex => r#"if [ -n "${CHIMAERA_SESSION:-}" ] \
  && ! grep -Eqs '^[[:space:]]*(tui\.)?theme[[:space:]]*=' "${CODEX_HOME:-$HOME/.codex}/config.toml"; then
  case "${CHIMAERA_THEME:-dark}" in
    light) exec "$real" -c tui.theme=catppuccin-latte "$@" ;;
    *)     exec "$real" -c tui.theme=catppuccin-mocha "$@" ;;
  esac
fi
exec "$real" "$@"
"#
        .to_string(),
        // No theme mechanism (verified: no flag in --help, no env; the
        // settings.json theme is the user's own file). ANSI-16 UIs follow
        // the pane palette.
        AgentKind::Gemini | AgentKind::Antigravity => "exec \"$real\" \"$@\"\n".to_string(),
    };
    match kind {
        AgentKind::Antigravity => format!("{resolve}{agy_guard}{fallback}{exec}"),
        _ => format!("{resolve}{fallback}{exec}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-runtimes-{label}-{}-{}",
            std::process::id(),
            &chimaera_core::generate_token()[..8]
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Write an executable stub at `path` (parent dirs must exist).
    fn write_exec(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A managed bin dir holding an executable stub for every agent, so
    /// `write_shims` writes a shim for each (a shim only exists when chimaera
    /// manages the binary or an explicit path is set).
    fn managed_with_all(dir: &Path) -> PathBuf {
        let managed = dir.join("agents").join("bin");
        std::fs::create_dir_all(&managed).unwrap();
        for kind in AgentKind::ALL {
            write_exec(&managed.join(kind.as_str()), "#!/bin/sh\n");
        }
        managed
    }

    #[test]
    fn install_scripts_are_curated_and_safe() {
        let root = PathBuf::from("/home/u/.chimaera/agents");
        for kind in [AgentKind::Claude, AgentKind::Codex, AgentKind::Antigravity] {
            let script = install_script(kind, &root).expect("curated install");
            // Strict mode, official HTTPS sources only, never sudo.
            assert!(script.starts_with("set -euo pipefail"), "{kind:?}");
            assert!(!script.contains("sudo"), "{kind:?}");
            assert!(!script.contains("http://"), "{kind:?}");
            // Installs land under <root>/<agent>/<version>/bin and activate
            // via the atomic symlink swap into <root>/bin.
            assert!(script.contains("root='/home/u/.chimaera/agents'"));
            assert!(script.contains("mv -f \"$root/bin/.$1.new\" \"$root/bin/$1\""));
            let agent = kind.as_str();
            assert!(
                script.contains(&format!("swap {agent} \"$version\"")),
                "{kind:?}"
            );
            // Ends by printing the installed version through the new link.
            assert!(
                script
                    .trim_end()
                    .ends_with(&format!("\"$root/bin/{agent}\" --version")),
                "{kind:?}"
            );
            // Every script whitelists the version charset before the value
            // reaches a path or an echo (traversal, terminal escapes).
            assert!(
                script.contains("*[!0-9.A-Za-z-]*|'')"),
                "{kind:?} gates the version charset"
            );
            // Downloads land in a mktemp dir removed on any exit, so a
            // mid-transfer death leaves no partials under the prefix.
            assert!(
                script.contains(r#"trap 'rm -rf "$tmp"' EXIT"#),
                "{kind:?} cleans up its temp dir"
            );
        }

        let claude = install_script(AgentKind::Claude, &root).unwrap();
        assert!(claude.contains(CLAUDE_DOWNLOAD_BASE));
        assert!(
            claude.contains("sha256"),
            "claude verifies the manifest checksum"
        );
        let codex = install_script(AgentKind::Codex, &root).unwrap();
        assert!(codex.contains("https://github.com/openai/codex/releases/latest/download"));
        // The package tarball variant: the one the release publishes
        // checksums for; its sha256 is verified against its SUMS line.
        assert!(codex.contains("codex-package-$triple.tar.gz"));
        assert!(codex.contains("codex-package_SHA256SUMS"));
        assert!(
            codex.contains(r#"if [ "$actual" != "$checksum" ]"#),
            "codex verifies the tarball against the published checksum"
        );
        assert!(codex.contains("tar -xzf \"$tmp/codex.tar.gz\" -C \"$tmp\" bin/codex"));
        let agy = install_script(AgentKind::Antigravity, &root).unwrap();
        assert!(agy.contains(AGY_MANIFEST_BASE));
        assert!(agy.contains("sha512"), "agy verifies the manifest checksum");
        assert!(
            agy.contains("chimaera skips that on purpose"),
            "the skipped `agy install` dotfile handoff is disclosed in the pane"
        );

        // gemini: honestly no curated install (needs a node runtime).
        assert!(install_script(AgentKind::Gemini, &root).is_none());
    }

    /// The version charset gate, executed as real sh against the hostile
    /// shapes it exists for: path traversal out of the managed root,
    /// terminal escape bytes, newlines, whitespace.
    #[test]
    fn version_whitelist_rejects_hostile_strings() {
        let gate = r#"case "$1" in *[!0-9.A-Za-z-]*|'') exit 1 ;; esac; exit 0"#;
        let ok = |v: &str| {
            std::process::Command::new("/bin/sh")
                .args(["-c", gate, "sh", v])
                .status()
                .unwrap()
                .success()
        };
        assert!(ok("2.1.202"));
        assert!(ok("0.142.5"));
        assert!(ok("1.0.16-beta"));
        assert!(!ok(""));
        assert!(!ok("2.1/../../../x"));
        assert!(!ok("2.1\u{1b}[31m"));
        assert!(!ok("2.1\nrm -rf /"));
        assert!(!ok("2.1 202"));
        assert!(!ok("$(id)"));
    }

    /// A quote in the install prefix cannot terminate the scripts' quoting
    /// contexts: every interpolated path goes through sq.
    #[test]
    fn quoted_prefix_survives_script_interpolation() {
        let root = PathBuf::from("/home/o'brien/.chimaera/agents");
        for kind in [AgentKind::Claude, AgentKind::Codex, AgentKind::Antigravity] {
            let script = install_script(kind, &root).expect("curated install");
            assert!(
                script.contains(r#"root='/home/o'\''brien/.chimaera/agents'"#),
                "{kind:?}"
            );
        }
        assert_eq!(sq("plain"), "'plain'");
        assert_eq!(sq("o'brien"), r#"'o'\''brien'"#);
    }

    #[test]
    fn shims_resolve_user_first_then_managed_and_gate_on_session() {
        let dir = test_dir("shims");
        let shim_dir = dir.join("shims");
        // Every agent managed, so a shim is written for each.
        let managed = managed_with_all(&dir);
        write_shims(&shim_dir, &managed, &HashMap::new()).unwrap();

        for kind in AgentKind::ALL {
            let path = shim_dir.join(kind.as_str());
            let script = std::fs::read_to_string(&path).unwrap();
            assert!(script.starts_with("#!/bin/sh"), "{kind:?}");
            // The shim strips its own dir before resolving, so it can never
            // exec itself; managed is the fallback, user PATH wins.
            assert!(script.contains(&format!("shims='{}'", shim_dir.display())));
            assert!(script.contains(&format!("command -v {} 2>/dev/null", kind.as_str())));
            assert!(script.contains(&format!("real='{}/{}'", managed.display(), kind.as_str())));
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755, "{kind:?}");
        }

        // Theme injection exists exactly where a mechanism was verified,
        // and is always gated on CHIMAERA_SESSION.
        let claude = std::fs::read_to_string(shim_dir.join("claude")).unwrap();
        assert!(claude.contains("CHIMAERA_SESSION"));
        assert!(claude.contains("claude-theme-light.json"));
        assert!(claude.contains("claude-theme-dark.json"));
        assert!(claude.contains(r#"grep -qs '"theme"' "$HOME/.claude/settings.json""#));
        let codex = std::fs::read_to_string(shim_dir.join("codex")).unwrap();
        assert!(codex.contains("CHIMAERA_SESSION"));
        assert!(codex.contains("tui.theme=catppuccin-latte"));
        assert!(codex.contains("tui.theme=catppuccin-mocha"));
        // The user-theme grep covers both the [tui] `theme =` and the
        // dotted `tui.theme =` spellings.
        assert!(codex.contains(r#"(tui\.)?theme[[:space:]]*="#), "{codex}");
        // gemini/agy: no verified theme lever — plain exec, no injection.
        let gemini = std::fs::read_to_string(shim_dir.join("gemini")).unwrap();
        assert!(!gemini.contains("CHIMAERA_THEME"));
        let agy = std::fs::read_to_string(shim_dir.join("agy")).unwrap();
        assert!(!agy.contains("CHIMAERA_THEME"));
        // The IDE-launcher trap guard rides in the agy shim.
        assert!(agy.contains("Antigravity.app"));

        // The theme settings files the claude shim points at.
        let light: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(shim_dir.join("claude-theme-light.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(light, json!({"theme": "light"}));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The generated shims are real sh programs: run one against a fake
    /// user-installed binary and assert resolution order, theme injection,
    /// argument passthrough, and the outside-chimaera gate.
    #[test]
    fn claude_shim_execs_real_binary_with_scheme_theme() {
        use std::os::unix::fs::PermissionsExt;

        let dir = test_dir("shim-exec");
        let shim_dir = dir.join("shims");
        // A managed claude makes the shim exist; the user's own (on PATH) still
        // wins the resolution below.
        let managed = dir.join("agents").join("bin");
        std::fs::create_dir_all(&managed).unwrap();
        write_exec(&managed.join("claude"), "#!/bin/sh\necho managed-claude\n");
        write_shims(&shim_dir, &managed, &HashMap::new()).unwrap();

        // A fake "user install" of claude that prints its argv.
        let user_bin = dir.join("user-bin");
        std::fs::create_dir_all(&user_bin).unwrap();
        let fake = user_bin.join("claude");
        std::fs::write(&fake, "#!/bin/sh\necho \"fake-claude $*\"\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        // An empty HOME so no real user settings can suppress injection.
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();

        let run = |envs: &[(&str, &str)]| -> String {
            let mut cmd = std::process::Command::new(shim_dir.join("claude"));
            cmd.arg("--resume").arg("abc");
            cmd.env_clear();
            cmd.env(
                "PATH",
                format!(
                    "{}:{}:/usr/bin:/bin",
                    shim_dir.display(),
                    user_bin.display()
                ),
            );
            cmd.env("HOME", &home);
            for (k, v) in envs {
                cmd.env(k, v);
            }
            let out = cmd.output().expect("shim ran");
            assert!(out.status.success(), "{out:?}");
            String::from_utf8_lossy(&out.stdout).into_owned()
        };

        // Inside a chimaera session: theme injected per scheme, user args kept.
        let out = run(&[("CHIMAERA_SESSION", "s-1"), ("CHIMAERA_THEME", "light")]);
        assert!(out.starts_with("fake-claude --settings "), "{out}");
        assert!(out.contains("claude-theme-light.json"), "{out}");
        assert!(out.contains("--resume abc"), "{out}");
        // Theme defaults to dark when unset.
        let out = run(&[("CHIMAERA_SESSION", "s-1")]);
        assert!(out.contains("claude-theme-dark.json"), "{out}");
        // Outside chimaera (no CHIMAERA_SESSION): exec untouched.
        let out = run(&[]);
        assert_eq!(out.trim(), "fake-claude --resume abc");
        // An explicit theme in the user's own settings.json suppresses
        // injection (fill the gap, never fight a choice).
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(home.join(".claude/settings.json"), r#"{"theme":"dark"}"#).unwrap();
        let out = run(&[("CHIMAERA_SESSION", "s-1"), ("CHIMAERA_THEME", "light")]);
        assert_eq!(out.trim(), "fake-claude --resume abc");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// With a managed install present and no user install on PATH, the shim
    /// resolves the managed binary under ~/.chimaera/agents/bin and themes it.
    #[test]
    fn shim_execs_managed_binary_when_user_has_none() {
        let dir = test_dir("shim-managed");
        let shim_dir = dir.join("shims");
        let managed = dir.join("agents").join("bin");
        std::fs::create_dir_all(&managed).unwrap();
        // A managed codex → the shim exists and, with no user codex on PATH,
        // falls to the managed one.
        write_exec(
            &managed.join("codex"),
            "#!/bin/sh\necho \"managed-codex $*\"\n",
        );
        write_shims(&shim_dir, &managed, &HashMap::new()).unwrap();
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();

        let run = || -> std::process::Output {
            let mut cmd = std::process::Command::new(shim_dir.join("codex"));
            cmd.env_clear();
            // No user codex anywhere on this PATH.
            cmd.env("PATH", format!("{}:/usr/bin:/bin", shim_dir.display()));
            cmd.env("HOME", &home);
            cmd.env("CHIMAERA_SESSION", "s-1");
            cmd.env("CHIMAERA_THEME", "light");
            cmd.output().expect("shim ran")
        };

        let out = run();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "managed-codex -c tui.theme=catppuccin-latte"
        );

        // A dotted-key theme in the user's own config suppresses injection
        // just like the [tui] spelling.
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(home.join(".codex/config.toml"), "tui.theme = \"zenburn\"\n").unwrap();
        let out = run();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "managed-codex");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Never shadow the user's own binary: a shim is written ONLY when chimaera
    /// manages the binary or an explicit path is set, and a stale shim is
    /// removed the moment neither holds (an uninstall).
    #[test]
    fn shim_written_only_when_managed_or_explicit() {
        let dir = test_dir("shim-active");
        let shim_dir = dir.join("shims");
        let managed = dir.join("agents").join("bin");
        std::fs::create_dir_all(&managed).unwrap();

        // Nothing managed, no explicit path → no shim, so `codex` resolves
        // through the user's own PATH instead of being shadowed.
        write_shims(&shim_dir, &managed, &HashMap::new()).unwrap();
        assert!(!shim_dir.join("codex").exists());

        // A managed codex lands → a shim appears.
        write_exec(&managed.join("codex"), "#!/bin/sh\n");
        write_shims(&shim_dir, &managed, &HashMap::new()).unwrap();
        assert!(shim_dir.join("codex").exists());

        // It is uninstalled → the stale shim is removed.
        std::fs::remove_file(managed.join("codex")).unwrap();
        write_shims(&shim_dir, &managed, &HashMap::new()).unwrap();
        assert!(!shim_dir.join("codex").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// An explicit `agents.<kind>.path` wins outright — over both a user install
    /// on PATH and a chimaera-managed one — and is still themed.
    #[test]
    fn shim_prefers_explicit_path_over_user_and_managed() {
        let dir = test_dir("shim-explicit");
        let shim_dir = dir.join("shims");
        let managed = dir.join("agents").join("bin");
        std::fs::create_dir_all(&managed).unwrap();
        write_exec(&managed.join("codex"), "#!/bin/sh\necho managed-codex\n");
        let user_bin = dir.join("user-bin");
        std::fs::create_dir_all(&user_bin).unwrap();
        write_exec(&user_bin.join("codex"), "#!/bin/sh\necho user-codex\n");
        let explicit_bin = dir.join("elsewhere-codex");
        write_exec(&explicit_bin, "#!/bin/sh\necho \"explicit-codex $*\"\n");

        let mut map = HashMap::new();
        map.insert(AgentKind::Codex, explicit_bin.display().to_string());
        write_shims(&shim_dir, &managed, &map).unwrap();

        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let mut cmd = std::process::Command::new(shim_dir.join("codex"));
        cmd.env_clear();
        cmd.env(
            "PATH",
            format!(
                "{}:{}:/usr/bin:/bin",
                shim_dir.display(),
                user_bin.display()
            ),
        );
        cmd.env("HOME", &home);
        cmd.env("CHIMAERA_SESSION", "s-1");
        cmd.env("CHIMAERA_THEME", "light");
        let out = cmd.output().expect("shim ran");
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "explicit-codex -c tui.theme=catppuccin-latte"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// PATH holding only the shim dir strips to an empty $clean — the shim
    /// must not resolve through an empty PATH (some shells search the cwd)
    /// and falls straight to the managed binary.
    #[test]
    fn shim_skips_user_resolution_when_path_strips_to_empty() {
        let dir = test_dir("shim-empty-path");
        let shim_dir = dir.join("shims");
        let managed = dir.join("agents").join("bin");
        std::fs::create_dir_all(&managed).unwrap();
        // Managed codex present so the shim is written.
        write_exec(
            &managed.join("codex"),
            "#!/bin/sh\necho \"managed-codex $*\"\n",
        );
        write_shims(&shim_dir, &managed, &HashMap::new()).unwrap();
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();

        // A booby-trapped ./codex in the cwd: an empty PATH must not run it.
        let cwd = dir.join("cwd");
        std::fs::create_dir_all(&cwd).unwrap();
        write_exec(&cwd.join("codex"), "#!/bin/sh\necho cwd-codex-ran\n");

        let mut cmd = std::process::Command::new(shim_dir.join("codex"));
        cmd.env_clear();
        cmd.current_dir(&cwd);
        cmd.env("PATH", shim_dir.display().to_string());
        cmd.env("HOME", &home);
        let out = cmd.output().expect("shim ran");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(!stdout.contains("cwd-codex-ran"), "{stdout}");
        assert_eq!(stdout.trim(), "managed-codex");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A quote in the shim/managed prefix survives the generated shim's own
    /// quoting (sq): resolution and fallback still work.
    #[test]
    fn shim_survives_quoted_prefix() {
        let base = test_dir("shim-quoted");
        let dir = base.join("o'brien");
        let shim_dir = dir.join("shims");
        let managed = dir.join("agents").join("bin");
        std::fs::create_dir_all(&managed).unwrap();
        // Managed codex present so the shim is written.
        write_exec(
            &managed.join("codex"),
            "#!/bin/sh\necho \"managed-codex $*\"\n",
        );
        write_shims(&shim_dir, &managed, &HashMap::new()).unwrap();
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();

        let mut cmd = std::process::Command::new(shim_dir.join("codex"));
        cmd.env_clear();
        cmd.env("PATH", format!("{}:/usr/bin:/bin", shim_dir.display()));
        cmd.env("HOME", &home);
        let out = cmd.output().expect("shim ran");
        assert!(out.status.success(), "{out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "managed-codex");

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn user_theme_gates_read_real_files() {
        let dir = test_dir("theme-gates");

        // claude: a "theme" key in settings.json means hands off.
        let settings = dir.join("settings.json");
        assert!(!claude_user_theme_set(&settings)); // absent file
        std::fs::write(&settings, r#"{"model": "opus"}"#).unwrap();
        assert!(!claude_user_theme_set(&settings));
        std::fs::write(&settings, r#"{"theme": "dark", "tui": "fullscreen"}"#).unwrap();
        assert!(claude_user_theme_set(&settings));
        std::fs::write(&settings, r#"{"theme": null}"#).unwrap();
        assert!(!claude_user_theme_set(&settings)); // explicit null = unset
        std::fs::write(&settings, "not json").unwrap();
        assert!(!claude_user_theme_set(&settings));

        // codex: a theme set in config.toml — in any of the TOML spellings
        // codex accepts — means hands off.
        let config = dir.join("config.toml");
        assert!(!codex_user_theme_set(&config)); // absent file
        std::fs::write(&config, "model = \"gpt-5.5\"\n[tui]\nanimations = true\n").unwrap();
        assert!(!codex_user_theme_set(&config));
        std::fs::write(&config, "[tui]\ntheme = \"zenburn\"\n").unwrap();
        assert!(codex_user_theme_set(&config));
        std::fs::write(&config, "[tui]\n  theme=\"nord\"\n").unwrap();
        assert!(codex_user_theme_set(&config));
        // The dotted-key spelling (valid TOML, accepted by codex 0.142.5)...
        std::fs::write(&config, "tui.theme = \"zenburn\"\n").unwrap();
        assert!(codex_user_theme_set(&config));
        std::fs::write(&config, "  tui.theme=\"nord\"\n").unwrap();
        assert!(codex_user_theme_set(&config));
        // ...and the inline-table spelling.
        std::fs::write(&config, "tui = { theme = \"zenburn\" }\n").unwrap();
        assert!(codex_user_theme_set(&config));
        std::fs::write(&config, "tui = { animations = true }\n").unwrap();
        assert!(!codex_user_theme_set(&config));
        // `theme` as a substring of another key is not a theme.
        std::fs::write(&config, "themes_dir = \"x\"\n").unwrap();
        assert!(!codex_user_theme_set(&config));
        std::fs::write(&config, "tui.themes_dir = \"x\"\n").unwrap();
        assert!(!codex_user_theme_set(&config));

        assert_eq!(codex_theme_name("light"), "catppuccin-latte");
        assert_eq!(codex_theme_name("dark"), "catppuccin-mocha");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn managed_paths() {
        let root = PathBuf::from("/home/u/.chimaera/agents");
        assert_eq!(
            managed_bin_dir(&root),
            PathBuf::from("/home/u/.chimaera/agents/bin")
        );
        assert!(is_managed(
            Path::new("/home/u/.chimaera/agents/bin/codex"),
            &root
        ));
        assert!(!is_managed(Path::new("/usr/local/bin/codex"), &root));
        assert!(!is_managed(Path::new("/home/u/.local/bin/claude"), &root));
    }
}
