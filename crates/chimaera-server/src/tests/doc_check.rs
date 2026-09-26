//! `doc_check` (the portable-dialect checker), its route, the MCP document
//! tools, and the opt-in agent-docs installs.

use super::support::*;
use crate::doc_check::{check_path, Issue, Severity};
use crate::{lock, AppState};

/// A document folder with a few real targets, canonicalized (the checker
/// reports canonical paths).
fn fixture(label: &str) -> PathBuf {
    let dir = std::fs::canonicalize(test_dir(label)).unwrap();
    std::fs::create_dir_all(dir.join("figs")).unwrap();
    std::fs::write(dir.join("figs/UMAP.png"), b"png").unwrap();
    std::fs::write(
        dir.join("methods.md"),
        "# Methods\n\n## Data sources\n\n<a id=\"custom-anchor\"></a>\n\n## Data sources\n",
    )
    .unwrap();
    std::fs::write(dir.join("filter.py"), "a\nb\nc\nd\ne\n").unwrap();
    dir
}

fn check(dir: &std::path::Path, doc: &str) -> Vec<Issue> {
    check_root(dir, doc, None)
}

fn check_root(dir: &std::path::Path, doc: &str, root: Option<&std::path::Path>) -> Vec<Issue> {
    let path = dir.join("doc.md");
    std::fs::write(&path, doc).unwrap();
    check_path(&path, root).unwrap().issues
}

fn codes(issues: &[Issue]) -> Vec<(usize, &'static str)> {
    issues.iter().map(|i| (i.line, i.code)).collect()
}

fn find<'a>(issues: &'a [Issue], code: &str) -> &'a Issue {
    issues
        .iter()
        .find(|i| i.code == code)
        .unwrap_or_else(|| panic!("no {code} in {issues:#?}"))
}

#[test]
fn clean_document_has_no_issues() {
    let dir = fixture("dc-clean");
    let doc = "\
---
title: QC report
tags: [qc]
summary: >
  wrapped
  text
---
# Report

See [methods](methods.md#data-sources), the [second](methods.md#data-sources-1),
the [anchor](methods.md#custom-anchor), [lines](filter.py#L2-L5), [a page](paper.pdf#page=3),
[results](#results), and a footnote.[^1]

![UMAP of all cells|400](figs/UMAP.png)

> [!NOTE]
> Useful.

## Results

Math $a < B$ and `<Code>` and `[[not a wikilink]]` stay quiet. [[1]](https://example.com)

```python
import os from 'x'
[[wiki]] <Chart /> ::: {% tag %}
```

[^1]: The footnote.
";
    std::fs::write(dir.join("paper.pdf"), b"%PDF").unwrap();
    let issues = check(&dir, doc);
    assert!(issues.is_empty(), "{issues:#?}");
}

#[test]
fn broken_links_and_embeds_are_errors_with_fixes() {
    let dir = fixture("dc-broken");
    let issues = check(
        &dir,
        "[gone](missing.md)\n\n![Case](figs/umap.png)\n\n![Spaces](my%20file.png)\n",
    );
    assert_eq!(
        codes(&issues),
        [(1, "broken-link"), (3, "broken-embed"), (5, "broken-embed")]
    );
    assert!(issues.iter().all(|i| i.severity == Severity::Error));
    assert!(issues[0]
        .message
        .contains(&dir.join("missing.md").display().to_string()));
    // A case-only mismatch names the real file.
    assert!(issues[1].fix.contains("figs/UMAP.png"), "{}", issues[1].fix);
    // `%20` decodes before the stat.
    assert!(
        issues[2].message.contains("my file.png"),
        "{}",
        issues[2].message
    );
}

#[test]
fn a_root_relative_mistake_gets_the_relative_fix() {
    let root = fixture("dc-root");
    let sub = root.join("docs/deep");
    std::fs::create_dir_all(&sub).unwrap();
    let path = sub.join("doc.md");
    std::fs::write(&path, "[m](methods.md)\n[r](/methods.md)\n").unwrap();
    let issues = check_path(&path, Some(&root)).unwrap().issues;
    // `methods.md` exists at the root: suggest `../../methods.md`. The
    // root-relative `/methods.md` resolves at the root and is fine.
    assert_eq!(codes(&issues), [(1, "broken-link")]);
    assert!(
        issues[0].fix.contains("../../methods.md"),
        "{}",
        issues[0].fix
    );
}

#[test]
fn fragments_are_checked_against_their_targets() {
    let dir = fixture("dc-frag");
    let doc = "\
# Top

[a](methods.md#Data-Sources)
[b](methods.md#nope)
[c](filter.py#L4-L9)
[d](#top)
[e](#missing)
[f](#L99)
[g](doc.md#top)
[h](filter.py#L3)
";
    let issues = check(&dir, doc);
    assert_eq!(
        codes(&issues),
        [
            (4, "missing-heading"),
            (5, "line-out-of-range"),
            (7, "missing-anchor"),
            (8, "line-out-of-range"),
        ]
    );
    // Case is lenient (`#Data-Sources`); a wrong slug is not.
    assert!(find(&issues, "missing-heading").message.contains("#nope"));
    assert!(find(&issues, "line-out-of-range")
        .message
        .contains("5 lines"));
}

#[test]
fn a_fragment_with_spaces_suggests_the_slug() {
    let dir = fixture("dc-slug");
    let issues = check(
        &dir,
        "## Results and Discussion\n\n[x](#Results%20and%20Discussion!)\n\
         [y](methods.md#Data%20sources)\n",
    );
    let issue = find(&issues, "missing-anchor");
    assert!(
        issue.fix.contains("#results-and-discussion"),
        "{}",
        issue.fix
    );
    let issue = find(&issues, "missing-heading");
    assert!(issue.fix.contains("`#data-sources`"), "{}", issue.fix);
}

#[test]
fn footnotes_must_pair_up() {
    let dir = fixture("dc-fn");
    let issues = check(
        &dir,
        "Text[^a] and[^missing] and `[^code]`.\n\n[^a]: ok\n[^orphan]: never used\n",
    );
    assert_eq!(
        codes(&issues),
        [(1, "dangling-footnote"), (4, "unused-footnote")]
    );
}

#[test]
fn absolute_local_paths_are_warnings_with_a_relative_fix() {
    let dir = fixture("dc-abs");
    let methods = dir.join("methods.md").display().to_string();
    let doc = format!(
        "[a]({methods})\n[b](file://{methods})\n[c](C:\\\\data\\\\x.csv)\n[d](~/notes.md)\n"
    );
    let issues = check(&dir, &doc);
    let abs: Vec<&Issue> = issues
        .iter()
        .filter(|i| i.code == "absolute-path")
        .collect();
    // /tmp/... is a local prefix (the fixture lives under the temp dir on
    // Linux) — and `~/notes.md` does not exist, so it is broken instead.
    assert!(abs.len() >= 3, "{issues:#?}");
    assert!(abs[0].fix.contains("`methods.md`"), "{}", abs[0].fix);
    assert!(abs.iter().all(|i| i.severity == Severity::Warning));
    assert!(issues.iter().any(|i| i.line == 4));
}

#[test]
fn images_need_alt_text_and_a_sane_size() {
    let dir = fixture("dc-img");
    let big = std::fs::File::create(dir.join("figs/huge.png")).unwrap();
    big.set_len(11 * 1024 * 1024).unwrap(); // sparse: no real bytes written
    let issues = check(
        &dir,
        "![](figs/UMAP.png)\n\n![|400](figs/UMAP.png)\n\n![Huge](figs/huge.png)\n",
    );
    assert_eq!(
        codes(&issues),
        [(1, "missing-alt"), (3, "missing-alt"), (5, "large-embed")]
    );
}

#[test]
fn foreign_dialects_are_flagged_outside_code() {
    let dir = fixture("dc-dialect");
    let doc = "\
See [[methods#Data sources|the data]] and ![[figs/UMAP.png|300]].

import Chart from './chart'
export const meta = {}

<Chart data={x} />

::: {.callout-note}
Pandoc div.
:::

```{note}
MyST directive.
```

{% callout %}Markdoc{% /callout %}

See {ref}`intro` for more.

<details><summary>HTML is fine</summary></details>
";
    let issues = check(&dir, doc);
    assert_eq!(
        codes(&issues),
        [
            (1, "wikilink"),
            (1, "wikilink"),
            (3, "mdx"),
            (4, "mdx"),
            (6, "mdx"),
            (8, "fenced-div"),
            (12, "myst-directive"),
            (16, "markdoc"),
            (18, "myst-directive"),
        ]
    );
    assert_eq!(
        issues[0].fix,
        "Write `[the data](methods.md#data-sources)`."
    );
    assert_eq!(issues[1].fix, "Write `![UMAP|300](figs/UMAP.png)`.");
    assert!(issues[5].fix.contains("> [!NOTE]"), "{}", issues[5].fix);
    assert!(issues[6].fix.contains("> [!NOTE]"), "{}", issues[6].fix);
}

#[test]
fn alerts_must_be_ones_github_renders() {
    let dir = fixture("dc-alert");
    let doc = "\
> [!NOTE]
> fine

> [!danger]
> Obsidian type

> [!TIP] My title
> text

> [!WARNING]-
> folded

> quote
> [!INFO] not the first line: plain text either way
";
    let issues = check(&dir, doc);
    assert_eq!(
        codes(&issues),
        [(4, "alert-type"), (7, "alert-title"), (10, "alert-type")]
    );
    assert_eq!(issues[0].fix, "Use `> [!CAUTION]`.");
}

#[test]
fn frontmatter_lines_must_be_yaml_shaped() {
    let dir = fixture("dc-fm");
    let doc = "---\ntitle: ok\nstatus = draft\njust words\n- item\n  nested: fine\n---\n# Doc\n";
    let issues = check(&dir, doc);
    assert_eq!(codes(&issues), [(3, "frontmatter"), (4, "frontmatter")]);
    assert_eq!(issues[0].fix, "Write it as YAML: `status: draft`.");
}

#[test]
fn infos_cover_duplicate_slugs_and_plain_http() {
    let dir = fixture("dc-info");
    let doc =
        "# Setup\n\n# Setup\n\n<http://example.com> and www.example.org and https://ok.example\n";
    let issues = check(&dir, doc);
    assert_eq!(codes(&issues), [(3, "duplicate-heading"), (5, "http-link")]);
    assert!(issues.iter().all(|i| i.severity == Severity::Info));
    assert!(find(&issues, "duplicate-heading")
        .message
        .contains("#setup-1"));
}

#[test]
fn line_numbers_survive_frontmatter_and_promoted_math() {
    let dir = fixture("dc-lines");
    let doc = "---\ntitle: x\n---\n$$ a\n+ b\n$$\n\n[x](gone.md)\n";
    let issues = check(&dir, doc);
    assert_eq!(codes(&issues), [(8, "broken-link")]);
}

#[test]
fn target_stats_are_bounded() {
    let dir = fixture("dc-bound");
    let doc: String = (0..260).map(|i| format!("[x](missing-{i}.md)\n")).collect();
    let report = {
        let path = dir.join("doc.md");
        std::fs::write(&path, &doc).unwrap();
        check_path(&path, None).unwrap()
    };
    let broken = report
        .issues
        .iter()
        .filter(|i| i.code == "broken-link")
        .count();
    assert_eq!(broken, 200, "one stat per target, capped");
    let unchecked = find(&report.issues, "unchecked");
    assert!(
        unchecked.message.starts_with("60 link"),
        "{}",
        unchecked.message
    );
    assert_eq!(unchecked.line, 201);
}

#[test]
fn issues_are_capped_keeping_the_most_severe() {
    let dir = fixture("dc-cap");
    // 600 missing-alt warnings on one real image, plus one broken link last.
    let mut doc: String = (0..600).map(|_| "![](figs/UMAP.png)\n\n").collect();
    doc.push_str("[x](gone.md)\n");
    let path = dir.join("doc.md");
    std::fs::write(&path, &doc).unwrap();
    let report = check_path(&path, None).unwrap();
    assert!(report.truncated);
    assert_eq!(report.issues.len(), 500);
    assert!(report.issues.iter().any(|i| i.code == "broken-link"));
}

#[test]
fn oversized_documents_are_refused() {
    let dir = fixture("dc-huge");
    let path = dir.join("big.md");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(5 * 1024 * 1024).unwrap();
    let err = check_path(&path, None).unwrap_err();
    assert!(format!("{err:#}").contains("too large"), "{err:#}");
}

// ---- the route ---------------------------------------------------------------

#[tokio::test]
async fn check_document_route_reports_issues() {
    let state = test_state();
    let dir = fixture("dc-route");
    let path = dir.join("doc.md");
    std::fs::write(&path, "[x](gone.md)\n").unwrap();
    let uri = format!(
        "/api/v1/fs/check_document?path={}",
        urlencode(&path.to_string_lossy())
    );
    let (status, body) = request(&state, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["truncated"], false);
    let issue = &body["issues"][0];
    assert_eq!(issue["line"], 1);
    assert_eq!(issue["severity"], "error");
    assert_eq!(issue["code"], "broken-link");
    assert!(issue["message"].is_string() && issue["fix"].is_string());

    let (status, body) = request(
        &state,
        Method::GET,
        "/api/v1/fs/check_document?path=relative.md",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

// ---- the MCP tools -----------------------------------------------------------

#[tokio::test]
async fn mcp_document_tools_guide_and_check() {
    let state = test_state();
    let id = inject_agent(&state, "dk");

    let (_, init) = mcp_post(
        &state,
        &id,
        "dk",
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
    )
    .await;
    let instructions = init["result"]["instructions"].as_str().unwrap();
    assert!(instructions.contains("document_guide"), "{instructions}");
    assert!(instructions.contains("check_document"), "{instructions}");

    let (is_error, guide) =
        mcp_tool_call(&state, &id, "dk", "document_guide", serde_json::json!({})).await;
    assert!(!is_error);
    assert_eq!(guide, crate::agent_docs::GUIDE);

    // A relative path resolves against the session's working directory.
    let cwd = state.sessions.get(&id).unwrap().cwd;
    std::fs::write(cwd.join("notes.md"), "# Notes\n\n[x](gone.md)\n").unwrap();
    let (is_error, text) = mcp_tool_call(
        &state,
        &id,
        "dk",
        "check_document",
        serde_json::json!({"path": "notes.md"}),
    )
    .await;
    assert!(!is_error, "{text}");
    assert!(text.contains("1 error"), "{text}");
    assert!(text.contains("line 3 error [broken-link]"), "{text}");
    assert!(text.contains("fix: "), "{text}");

    std::fs::write(cwd.join("clean.md"), "# Clean\n").unwrap();
    let (_, text) = mcp_tool_call(
        &state,
        &id,
        "dk",
        "check_document",
        serde_json::json!({"path": "clean.md"}),
    )
    .await;
    assert!(text.contains("no issues"), "{text}");

    let (is_error, text) = mcp_tool_call(
        &state,
        &id,
        "dk",
        "check_document",
        serde_json::json!({"path": "nope.md"}),
    )
    .await;
    assert!(is_error && text.contains("not found"), "{text}");

    state.sessions.kill(&id).ok();
}

/// After the cwd, a relative path falls back to the workspace root.
#[tokio::test]
async fn mcp_check_document_falls_back_to_the_workspace_root() {
    let state = test_state();
    let ws = make_workspace(&state, "dc-mcp-ws").await;
    let id = inject_agent(&state, "wk");
    lock(&state.session_workspaces).insert(id.clone(), ws.clone());
    let root = lock(&state.workspaces).get(&ws).unwrap().root;
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(root.join("docs/r.md"), "![](../figs/missing.png)\n").unwrap();
    let (is_error, text) = mcp_tool_call(
        &state,
        &id,
        "wk",
        "check_document",
        serde_json::json!({"path": "docs/r.md"}),
    )
    .await;
    assert!(!is_error, "{text}");
    assert!(text.starts_with("docs/r.md: 1 error, 1 warning"), "{text}");
    state.sessions.kill(&id).ok();
}

// ---- the agent-docs installs ------------------------------------------------

fn state_with_home(home: &std::path::Path) -> Arc<AppState> {
    let data = test_dir("dc-home-data");
    let mut state = AppState::new(
        "test-token".to_string(),
        "testhost".to_string(),
        4242,
        0,
        data.clone(),
        data.join("config"),
    );
    state.claude_settings_path = home.join(".claude").join("settings.json");
    Arc::new(state)
}

#[tokio::test]
async fn agent_docs_installs_are_idempotent() {
    let home = test_dir("dc-home");
    let state = state_with_home(&home);
    let ws = make_workspace(&state, "dc-install-ws").await;
    let root = lock(&state.workspaces).get(&ws).unwrap().root;

    let (status, body) = request(
        &state,
        Method::GET,
        &format!("/api/v1/agent-docs?workspace_id={ws}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let targets = body["targets"].as_array().unwrap();
    assert_eq!(targets.len(), 2);
    assert_eq!(targets[0]["target"], "agents_md");
    assert_eq!(targets[0]["state"], "no_file");
    assert!(targets[0]["text"]
        .as_str()
        .unwrap()
        .starts_with(crate::agent_docs::START));
    assert_eq!(targets[1]["target"], "claude_skill");
    assert_eq!(targets[1]["state"], "absent");

    // AGENTS.md: created, then a no-op, then kept around the user's text.
    let install = |target: &'static str| {
        let state = state.clone();
        let ws = ws.clone();
        async move {
            request(
                &state,
                Method::POST,
                "/api/v1/agent-docs/install",
                Some(serde_json::json!({"target": target, "workspace_id": ws})),
            )
            .await
        }
    };
    let (status, body) = install("agents_md").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (body["changed"].as_bool(), body["created"].as_bool()),
        (Some(true), Some(true))
    );
    let agents = root.join("AGENTS.md");
    let first = std::fs::read_to_string(&agents).unwrap();
    assert_eq!(first, format!("{}\n", crate::agent_docs::agents_block()));
    let (_, body) = install("agents_md").await;
    assert_eq!(body["changed"], false);
    std::fs::write(&agents, format!("# Mine\n\n{first}\n## After\n")).unwrap();
    let (_, body) = install("agents_md").await;
    assert_eq!(body["changed"], false, "a current block is left alone");
    let edited = first.replace("Every image", "Each image");
    std::fs::write(&agents, format!("# Mine\n\n{edited}\n## After\n")).unwrap();
    let (_, body) = install("agents_md").await;
    assert_eq!(body["changed"], true);
    assert_eq!(
        std::fs::read_to_string(&agents).unwrap(),
        format!("# Mine\n\n{first}\n## After\n")
    );
    // No temp files left behind.
    let leftovers: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty());

    // The Claude skill lands under the (fixture) home, once.
    let (status, body) = install("claude_skill").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["created"], true);
    let skill = home.join(".claude/skills/chimaera-docs/SKILL.md");
    assert_eq!(body["path"], skill.to_string_lossy().as_ref());
    assert_eq!(
        std::fs::read_to_string(&skill).unwrap(),
        crate::agent_docs::skill_text()
    );
    let (_, body) = install("claude_skill").await;
    assert_eq!(body["changed"], false);

    let (_, body) = request(
        &state,
        Method::GET,
        &format!("/api/v1/agent-docs?workspace_id={ws}"),
        None,
    )
    .await;
    assert_eq!(body["targets"][0]["state"], "installed");
    assert_eq!(body["targets"][1]["state"], "installed");

    // Unknown targets and a missing workspace are refused.
    let (status, _) = install("codex_skill").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = request(
        &state,
        Method::POST,
        "/api/v1/agent-docs/install",
        Some(serde_json::json!({"target": "agents_md"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
