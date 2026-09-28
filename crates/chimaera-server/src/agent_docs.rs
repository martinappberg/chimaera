//! Opt-in "teach agents the document dialect", for agents launched OUTSIDE
//! Chimaera (inside it, the MCP server's instructions and `document_guide`
//! already reach every session). Never automatic: each install is a user's
//! click in Settings behind a dialog that shows the exact text.
//!
//! - `agents_md`: a delimited block in the workspace's `AGENTS.md`, written
//!   or replaced in place (idempotent; the file is created when missing).
//! - `claude_skill`: `~/.claude/skills/chimaera-docs/SKILL.md`, the guide
//!   with skill frontmatter.
//!
//! Writes are atomic (temp sibling, fsync, rename, fsync dir), bounded (an
//! existing file over [`MAX_EXISTING_BYTES`] is refused, never truncated),
//! and run off the reactor under the shared filesystem limiter.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::AppState;

pub(crate) const START: &str = "<!-- chimaera:docs:start -->";
pub(crate) const END: &str = "<!-- chimaera:docs:end -->";

/// The full guide: `document_guide`'s answer and the skill's body.
pub(crate) const GUIDE: &str = include_str!("doc_guide.md");

/// Largest existing `AGENTS.md` / `SKILL.md` read back for an upsert.
const MAX_EXISTING_BYTES: u64 = 1024 * 1024;

/// What goes between the markers in `AGENTS.md`: the dialect in brief, for
/// any agent that reads the file (Codex, and Claude via `@AGENTS.md`).
const AGENTS_SECTION: &str = "\
## Writing documents

Markdown documents here are read on GitHub, in Obsidian and in the Chimaera
workbench. Write portable markdown:

- CommonMark + GitHub Flavored Markdown: tables, task lists, footnotes,
  strikethrough, fenced code with a language.
- Alerts: `> [!NOTE]`, `TIP`, `IMPORTANT`, `WARNING` or `CAUTION`, the marker
  alone on the quote's first line.
- Math: `$…$` inline; `$$…$$` or a `math` fence for display. Diagrams: a
  `mermaid` fence.
- Optional YAML frontmatter: `title`, `summary`, `status`, `audience`,
  `updated`, `tags`.
- Links and embeds use paths relative to the document's folder, `%20` for
  spaces: `[methods](methods.md#results)`, `![UMAP of all cells](figs/umap.png)`.
  Never absolute local paths (`/home/…`, `/scratch/…`).
- Every image gets meaningful alt text.
- No wikilinks (`[[note]]`), MDX/JSX, Markdoc tags, Pandoc/Quarto `:::` blocks
  or MyST directives: GitHub shows them as literal text.
- In replies, cite files as `path:line`, `path#L10-L20` or a relative link.

In a Chimaera session, the `document_guide` tool has the full rules; run
`check_document` on a document before handing it over.
";

const SKILL_FRONTMATTER: &str = "\
---
name: chimaera-docs
description: \"Write portable markdown documents (reports, READMEs, notes) that render on GitHub, in Obsidian and in Chimaera: GFM, alerts, math, mermaid, frontmatter, relative links and embeds with fragments. Use when writing or editing a markdown document someone else will read.\"
---

";

const SKILL_TRAILER: &str = "
## Outside Chimaera

The `document_guide` and `check_document` tools come from Chimaera's MCP
server. Without them, check by hand before handing a document over: every
relative link and embed resolves from the document's folder, every `#heading`
fragment matches a heading's GitHub slug, every image has alt text, and no
absolute local path remains.
";

/// The exact block written into `AGENTS.md` (markers included).
pub(crate) fn agents_block() -> String {
    format!("{START}\n{AGENTS_SECTION}{END}")
}

/// The exact `SKILL.md` written for Claude Code.
pub(crate) fn skill_text() -> String {
    format!("{SKILL_FRONTMATTER}{GUIDE}{SKILL_TRAILER}")
}

/// `existing` with the block written in: replaced between the markers when
/// present, appended after a blank line otherwise. A lone marker (a
/// hand-edit gone wrong) is refused rather than guessed at.
pub(crate) fn upsert_block(existing: Option<&str>, block: &str) -> anyhow::Result<String> {
    let Some(text) = existing else {
        return Ok(format!("{block}\n"));
    };
    match (text.find(START), text.find(END)) {
        (Some(start), Some(end)) if end > start => Ok(format!(
            "{}{block}{}",
            &text[..start],
            &text[end + END.len()..]
        )),
        (None, None) => {
            let mut out = text.to_string();
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            if !out.trim().is_empty() {
                out.push('\n');
            }
            out.push_str(block);
            out.push('\n');
            Ok(out)
        }
        _ => anyhow::bail!(
            "AGENTS.md has an unmatched {START} / {END} marker; fix it by hand, then try again"
        ),
    }
}

/// `installed` (the block or skill is current), `outdated` (present but
/// different), `absent` (the file has no block / there is no skill),
/// `no_file` (no AGENTS.md yet), or `broken` (an unmatched marker).
fn agents_md_state(existing: Option<&str>) -> &'static str {
    let Some(text) = existing else {
        return "no_file";
    };
    match (text.find(START), text.find(END)) {
        (None, None) => "absent",
        (Some(start), Some(end)) if end > start => {
            if text[start..end + END.len()] == agents_block() {
                "installed"
            } else {
                "outdated"
            }
        }
        _ => "broken",
    }
}

fn skill_state(existing: Option<&str>) -> &'static str {
    match existing {
        None => "absent",
        Some(text) if text == skill_text() => "installed",
        Some(_) => "outdated",
    }
}

/// Read a text file for an upsert: None when missing; refused when too
/// large or not UTF-8 (rewriting either could corrupt it).
fn read_existing(path: &Path) -> anyhow::Result<Option<String>> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(
                anyhow::Error::new(err).context(format!("{}: failed to stat", path.display()))
            )
        }
    };
    if !meta.is_file() {
        anyhow::bail!("{} is not a regular file", path.display());
    }
    if meta.len() > MAX_EXISTING_BYTES {
        anyhow::bail!(
            "{} is larger than {MAX_EXISTING_BYTES} bytes; edit it by hand",
            path.display()
        );
    }
    let bytes =
        std::fs::read(path).with_context(|| format!("{}: failed to read", path.display()))?;
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| anyhow::anyhow!("{} is not UTF-8 text; edit it by hand", path.display()))
}

/// Replace `path`'s contents atomically, keeping an existing file's mode and
/// writing through a symlink to its target. The temp file is removed on
/// every error path.
fn write_atomic(path: &Path, contents: &str) -> anyhow::Result<()> {
    write_atomic_if(path, contents, || Ok(true)).map(|_| ())
}

/// [`write_atomic`], renaming only if `unchanged()` still holds once the temp
/// file is written — the last moment before the rename, so a concurrent
/// edit made while we merged is not overwritten. `Ok(false)` (the temp
/// removed, nothing written) when it no longer holds.
fn write_atomic_if(
    path: &Path,
    contents: &str,
    unchanged: impl FnOnce() -> anyhow::Result<bool>,
) -> anyhow::Result<bool> {
    let target = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::canonicalize(path)
            .with_context(|| format!("{} is a dangling symlink", path.display()))?,
        _ => path.to_path_buf(),
    };
    let parent = target
        .parent()
        .with_context(|| format!("{} has no parent directory", target.display()))?;
    let name = target
        .file_name()
        .with_context(|| format!("{} has no file name", target.display()))?
        .to_string_lossy()
        .into_owned();
    let mode = std::fs::metadata(&target)
        .map(|m| m.permissions().mode() & 0o7777)
        .unwrap_or(0o644);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let tmp = parent.join(format!(
        ".{name}.chimaera-{}-{nanos}.tmp",
        std::process::id()
    ));
    let result = (|| -> anyhow::Result<bool> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)
            .with_context(|| format!("failed to create {}", tmp.display()))?;
        // The open's mode passed through the umask; restore the exact bits.
        let _ = file.set_permissions(std::fs::Permissions::from_mode(mode));
        file.write_all(contents.as_bytes())
            .and_then(|()| file.sync_all())
            .with_context(|| format!("failed to write {}", tmp.display()))?;
        if !unchanged()? {
            return Ok(false);
        }
        std::fs::rename(&tmp, &target)
            .with_context(|| format!("failed to rename into {}", target.display()))?;
        Ok(true)
    })();
    if !matches!(result, Ok(true)) {
        let _ = std::fs::remove_file(&tmp);
        return result;
    }
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(true)
}

/// Write the block into `AGENTS.md` at `path`. Returns (changed, created).
pub(crate) fn install_agents_md(path: &Path) -> anyhow::Result<(bool, bool)> {
    install_agents_md_with(path, || {})
}

/// [`install_agents_md`], with `before_check` run between writing the temp
/// file and re-reading `path` (tests use it to edit the file mid-install).
///
/// The merge is a read-modify-write of a file an agent or the user may be
/// editing: right before the rename the file is read again, and a change
/// since the merge's read means one fresh merge over the new text; a second
/// change refuses with an error rather than overwrite either edit.
fn install_agents_md_with(
    path: &Path,
    mut before_check: impl FnMut(),
) -> anyhow::Result<(bool, bool)> {
    for _attempt in 0..2 {
        let existing = read_existing(path)?;
        let next = upsert_block(existing.as_deref(), &agents_block())?;
        if existing.as_deref() == Some(next.as_str()) {
            return Ok((false, false));
        }
        let written = write_atomic_if(path, &next, || {
            before_check();
            Ok(read_existing(path)? == existing)
        })?;
        if written {
            return Ok((true, existing.is_none()));
        }
    }
    anyhow::bail!(
        "{} kept changing while the section was being written; nothing was written — \
         try again once it is idle",
        path.display()
    )
}

/// Write the skill at `path` (its folder created). Returns (changed, created).
pub(crate) fn install_skill(path: &Path) -> anyhow::Result<(bool, bool)> {
    let existing = read_existing(path)?;
    let text = skill_text();
    if existing.as_deref() == Some(text.as_str()) {
        return Ok((false, false));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
    }
    write_atomic(path, &text)?;
    Ok((true, existing.is_none()))
}

/// Where Claude Code reads user skills: beside the settings file the daemon
/// already respects (`~/.claude/settings.json`), so a test's fixture home
/// moves both.
fn claude_skill_path(state: &AppState) -> PathBuf {
    state
        .claude_settings_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
        .join("skills")
        .join("chimaera-docs")
        .join("SKILL.md")
}

fn workspace_agents_md(state: &AppState, workspace_id: Option<&str>) -> Option<PathBuf> {
    let id = workspace_id?;
    crate::lock(&state.workspaces)
        .get(id)
        .map(|w| w.root.join("AGENTS.md"))
}

#[derive(Deserialize)]
pub(crate) struct StatusQuery {
    #[serde(default)]
    workspace_id: Option<String>,
}

/// GET /api/v1/agent-docs?workspace_id= — each install target: its path,
/// its state and the exact text an install writes (the confirm dialog shows
/// it). `agents_md` is listed only for a known workspace.
pub(crate) async fn status(
    State(state): State<Arc<AppState>>,
    Query(query): Query<StatusQuery>,
) -> Response {
    let agents_md = workspace_agents_md(&state, query.workspace_id.as_deref());
    let skill = claude_skill_path(&state);
    let result = crate::doc_check::run_blocking(move || {
        let mut targets = Vec::new();
        if let Some(path) = agents_md {
            let state = match read_existing(&path) {
                Ok(existing) => agents_md_state(existing.as_deref()),
                Err(_) => "unreadable",
            };
            targets.push(json!({
                "target": "agents_md",
                "path": path,
                "state": state,
                "text": agents_block(),
            }));
        }
        let state = match read_existing(&skill) {
            Ok(existing) => skill_state(existing.as_deref()),
            Err(_) => "unreadable",
        };
        targets.push(json!({
            "target": "claude_skill",
            "path": skill,
            "state": state,
            "text": skill_text(),
        }));
        Ok(json!({ "targets": targets }))
    })
    .await;
    respond(result)
}

#[derive(Deserialize)]
pub(crate) struct InstallRequest {
    target: String,
    #[serde(default)]
    workspace_id: Option<String>,
}

/// POST /api/v1/agent-docs/install {target, workspace_id?} —
/// `{target, path, changed, created}`. Idempotent: a current install is a
/// success that writes nothing (`changed: false`).
pub(crate) async fn install(
    State(state): State<Arc<AppState>>,
    Json(body): Json<InstallRequest>,
) -> Response {
    let (path, is_skill) = match body.target.as_str() {
        "agents_md" => match workspace_agents_md(&state, body.workspace_id.as_deref()) {
            Some(path) => (path, false),
            None => {
                return respond(Err(anyhow::anyhow!(
                    "agents_md needs the workspace_id of a known workspace"
                )))
            }
        },
        "claude_skill" => (claude_skill_path(&state), true),
        other => return respond(Err(anyhow::anyhow!("unknown install target {other:?}"))),
    };
    let target = body.target.clone();
    let written = path.clone();
    let result = crate::doc_check::run_blocking(move || {
        let (changed, created) = if is_skill {
            install_skill(&path)?
        } else {
            install_agents_md(&path)?
        };
        tracing::info!(target = %target, path = %path.display(), changed, "agent docs installed");
        Ok(json!({"target": target, "path": path, "changed": changed, "created": created}))
    })
    .await;
    // Like a save: the git panel and any open preview refresh now, not at
    // the next backstop poll.
    if result.as_ref().is_ok_and(|body| body["changed"] == true) {
        crate::git::mark_path_dirty(&state, &written.to_string_lossy()).await;
    }
    respond(result)
}

fn respond(result: anyhow::Result<Value>) -> Response {
    match result {
        Ok(body) => Json(body).into_response(),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("{err:#}")})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_creates_appends_replaces_and_is_idempotent() {
        let block = agents_block();
        let created = upsert_block(None, &block).unwrap();
        assert_eq!(created, format!("{block}\n"));
        assert_eq!(upsert_block(Some(&created), &block).unwrap(), created);

        let appended = upsert_block(Some("# Repo\n\nRules."), &block).unwrap();
        assert_eq!(appended, format!("# Repo\n\nRules.\n\n{block}\n"));
        assert_eq!(upsert_block(Some(&appended), &block).unwrap(), appended);

        // An old block is replaced in place; text around it survives.
        let old = format!("# Repo\n\n{START}\nold text\n{END}\n\n## After\n");
        let replaced = upsert_block(Some(&old), &block).unwrap();
        assert_eq!(replaced, format!("# Repo\n\n{block}\n\n## After\n"));
        assert_eq!(agents_md_state(Some(&old)), "outdated");
        assert_eq!(agents_md_state(Some(&replaced)), "installed");
        assert_eq!(agents_md_state(Some("# Repo\n")), "absent");
        assert_eq!(agents_md_state(None), "no_file");
    }

    #[test]
    fn upsert_refuses_an_unmatched_marker() {
        let lone = format!("# Repo\n{START}\nhalf a block\n");
        assert!(upsert_block(Some(&lone), &agents_block()).is_err());
        assert_eq!(agents_md_state(Some(&lone)), "broken");
        let reversed = format!("{END}\n{START}\n");
        assert!(upsert_block(Some(&reversed), &agents_block()).is_err());
    }

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-agent-docs-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect()
    }

    /// An edit that lands while the block is being merged in is kept: the
    /// install re-reads right before the rename and merges again.
    #[test]
    fn agents_md_install_keeps_an_edit_made_mid_install() {
        let dir = scratch("race-once");
        let path = dir.join("AGENTS.md");
        std::fs::write(
            &path, "# Repo
",
        )
        .unwrap();
        let mut edits = 0;
        let (changed, created) = install_agents_md_with(&path, || {
            if edits == 0 {
                std::fs::write(
                    &path,
                    "# Repo

A rule an agent just added.
",
                )
                .unwrap();
            }
            edits += 1;
        })
        .unwrap();
        assert_eq!((changed, created), (true, false));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!(
                "# Repo

A rule an agent just added.

{}
",
                agents_block()
            )
        );
        assert!(leftovers(&dir).is_empty());
    }

    /// A file that keeps changing is refused, never overwritten.
    #[test]
    fn agents_md_install_refuses_a_file_that_keeps_changing() {
        let dir = scratch("race-always");
        let path = dir.join("AGENTS.md");
        std::fs::write(
            &path, "# Repo
",
        )
        .unwrap();
        let mut n = 0;
        let err = install_agents_md_with(&path, || {
            n += 1;
            std::fs::write(
                &path,
                format!(
                    "# Repo

edit {n}
"
                ),
            )
            .unwrap();
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("kept changing"), "{err:#}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# Repo

edit 2
"
        );
        assert!(leftovers(&dir).is_empty());
    }

    #[test]
    fn skill_has_frontmatter_and_the_guide() {
        let text = skill_text();
        assert!(text.starts_with("---\nname: chimaera-docs\ndescription: \""));
        let fm_end = text[4..].find("\n---\n").unwrap();
        assert!(
            text[4..4 + fm_end].lines().count() == 2,
            "name + description only"
        );
        assert!(text.contains(GUIDE));
        assert_eq!(skill_state(Some(&text)), "installed");
        assert_eq!(skill_state(Some("old")), "outdated");
        assert_eq!(skill_state(None), "absent");
    }
}
