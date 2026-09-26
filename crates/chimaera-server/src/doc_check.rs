//! `check_document`: the portable-dialect checker behind the MCP tool of the
//! same name, `GET /api/v1/fs/check_document`, and the reading view's issues
//! chip — one checker, so an agent and the user see the same findings.
//!
//! The document is parsed the way the reading view parses it (the same `$$`
//! promotion, frontmatter rule and comrak extensions as
//! `fs::markdown_to_html`), then:
//!
//! - the AST yields links, embeds, headings, footnote and raw-HTML anchors,
//!   code blocks (masked from the line scans) and MyST directive fences;
//! - line scans outside code find what comrak reads as plain text but other
//!   dialects meant as syntax: wikilinks, MDX/JSX, `:::` fences, Markdoc
//!   tags, unsupported alert types, footnote labels, malformed frontmatter.
//!
//! Every relative target is stat-ed against the document's folder; heading
//! (`#slug`) and line (`#L10-L20`) fragments are checked against the target.
//! All of it is bounded — the daemon shares HPC login nodes and the targets
//! may sit on a stalling NFS mount: see the constants below. Callers run it
//! under `fs::FILESYSTEM_WORK` on a blocking thread ([`run_blocking`]).

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context;
use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use comrak::nodes::NodeValue;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Largest document checked (the reading view's own markdown ceiling).
const MAX_DOC_BYTES: u64 = 4 * 1024 * 1024;
/// Issues reported per check; past it the least severe are dropped first.
const MAX_ISSUES: usize = 500;
/// Issues collected before the cut (bounds memory on a pathological file).
const MAX_RAW_ISSUES: usize = 4 * MAX_ISSUES;
/// Distinct link/embed targets stat-ed per check.
const MAX_TARGET_STATS: usize = 200;
/// Target files read per check (heading anchors of `.md` targets, line counts
/// for `#L` fragments), each at most [`MAX_TARGET_READ_BYTES`].
const MAX_TARGET_READS: usize = 20;
const MAX_TARGET_READ_BYTES: u64 = 2 * 1024 * 1024;
/// Directory listings read to suggest a fix for a broken link (case
/// mismatches), each capped at [`MAX_SUGGEST_ENTRIES`] names.
const MAX_SUGGEST_SCANS: usize = 20;
const MAX_SUGGEST_ENTRIES: usize = 1000;
/// Embeds larger than this are slow to load remotely and heavy on GitHub.
const LARGE_EMBED_BYTES: u64 = 10 * 1024 * 1024;
/// Wall-clock budget for the target stats and reads of one check: targets
/// not reached in time are reported as unchecked, never waited on.
const CHECK_BUDGET: Duration = Duration::from_secs(10);

/// The alert types GitHub renders.
const ALERT_TYPES: [&str; 5] = ["NOTE", "TIP", "IMPORTANT", "WARNING", "CAUTION"];

/// Absolute local-path prefixes that never belong in a shared document.
const LOCAL_PREFIXES: [&str; 4] = ["/home/", "/Users/", "/scratch/", "/tmp/"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
    }
}

/// One finding. `line` is 1-based in the source file; `code` is a stable
/// identifier; `fix` says what to write instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Issue {
    pub(crate) line: usize,
    pub(crate) severity: Severity,
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) fix: String,
}

#[derive(Debug, Default)]
pub(crate) struct Report {
    pub(crate) issues: Vec<Issue>,
    /// More issues were found than [`MAX_ISSUES`].
    pub(crate) truncated: bool,
    /// Link and embed targets on local paths that were resolved.
    pub(crate) checked_targets: usize,
}

// ---- entry points -----------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct CheckQuery {
    path: String,
    /// Workspace root: where a root-relative `/docs/x.md` link resolves.
    #[serde(default)]
    root: Option<String>,
}

/// GET /api/v1/fs/check_document?path=&root= — `{issues, truncated}`.
pub(crate) async fn check_document(Query(query): Query<CheckQuery>) -> Response {
    let result = run_blocking(move || {
        let path = expand_tilde(&query.path);
        if !path.is_absolute() {
            anyhow::bail!("path {:?} is not absolute", query.path);
        }
        let root = query.root.as_deref().map(expand_tilde);
        check_path(&path, root.as_deref())
    })
    .await;
    match result {
        Ok(report) => {
            Json(json!({"issues": report.issues, "truncated": report.truncated})).into_response()
        }
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("{err:#}")})),
        )
            .into_response(),
    }
}

/// The MCP tool: resolve `raw` against the session's cwd, then its workspace
/// root, check it, and render a report for a model to act on. `Err` is
/// model-facing text.
pub(crate) async fn agent_report(
    raw: String,
    cwd: Option<PathBuf>,
    root: Option<PathBuf>,
) -> Result<String, String> {
    run_blocking(move || {
        let path = resolve_for_agent(&raw, cwd.as_deref(), root.as_deref())?;
        let report = check_path(&path, root.as_deref())?;
        Ok(render_report(&path, root.as_deref(), &report))
    })
    .await
    .map_err(|err| format!("{err:#}"))
}

/// Run checker work on a blocking thread under the shared filesystem limiter.
pub(crate) async fn run_blocking<T, F>(work: F) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
{
    let permit = crate::fs::FILESYSTEM_WORK
        .acquire()
        .await
        .expect("filesystem work semaphore is never closed");
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .map_err(|join| anyhow::anyhow!("document check failed: {join}"))?
}

fn expand_tilde(raw: &str) -> PathBuf {
    let home = || std::env::var_os("HOME").filter(|h| !h.is_empty());
    if raw == "~" {
        if let Some(home) = home() {
            return PathBuf::from(home);
        }
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = home() {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(raw)
}

/// An agent's `path`: absolute (or `~`) as given; otherwise the first of
/// `cwd/path`, `root/path` that exists.
fn resolve_for_agent(
    raw: &str,
    cwd: Option<&Path>,
    root: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        anyhow::bail!("missing required argument: path");
    }
    let given = expand_tilde(raw);
    if given.is_absolute() {
        return Ok(given);
    }
    let mut bases: Vec<&Path> = cwd.into_iter().collect();
    if let Some(root) = root.filter(|r| !bases.contains(r)) {
        bases.push(root);
    }
    for base in &bases {
        let candidate = base.join(&given);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    if bases.is_empty() {
        anyhow::bail!(
            "{raw} is relative and this session has no working directory; pass an absolute path"
        );
    }
    let tried: Vec<String> = bases
        .iter()
        .map(|b| b.join(&given).display().to_string())
        .collect();
    anyhow::bail!("{raw} not found (tried {})", tried.join(", "))
}

/// Check the markdown file at `path`.
pub(crate) fn check_path(path: &Path, root: Option<&Path>) -> anyhow::Result<Report> {
    let doc = std::fs::canonicalize(path).with_context(|| path.display().to_string())?;
    let meta =
        std::fs::metadata(&doc).with_context(|| format!("{}: failed to stat", doc.display()))?;
    if !meta.is_file() {
        anyhow::bail!("{} is not a file", doc.display());
    }
    if meta.len() > MAX_DOC_BYTES {
        anyhow::bail!(
            "{} is too large to check ({} bytes, limit {MAX_DOC_BYTES})",
            doc.display(),
            meta.len()
        );
    }
    // Bounded again at the read: the file may have grown (or been swapped)
    // since the stat.
    let bytes = read_regular(&doc, MAX_DOC_BYTES)
        .with_context(|| format!("{}: failed to read", doc.display()))?
        .with_context(|| {
            format!(
                "{} is too large to check (over the {MAX_DOC_BYTES}-byte limit)",
                doc.display()
            )
        })?;
    let text = String::from_utf8_lossy(&bytes);
    let root = root.map(|r| std::fs::canonicalize(r).unwrap_or_else(|_| r.to_path_buf()));
    Ok(check_text(&text, &doc, root.as_deref()))
}

/// The report as text for a model: a summary line, then one block per issue.
pub(crate) fn render_report(path: &Path, root: Option<&Path>, report: &Report) -> String {
    let shown = root
        .and_then(|r| path.strip_prefix(r).ok())
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(path)
        .display()
        .to_string();
    let count = |sev: Severity| report.issues.iter().filter(|i| i.severity == sev).count();
    let (errors, warnings, infos) = (
        count(Severity::Error),
        count(Severity::Warning),
        count(Severity::Info),
    );
    let checked = report.checked_targets;
    if report.issues.is_empty() {
        return format!(
            "{shown}: no issues ({checked} local link/embed target{} checked). \
             It is portable markdown.",
            if checked == 1 { "" } else { "s" }
        );
    }
    let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
    let mut out = format!(
        "{shown}: {}, {}, {} ({checked} local link/embed targets checked).\n",
        plural(errors, "error"),
        plural(warnings, "warning"),
        plural(infos, "note"),
    );
    if report.truncated {
        out.push_str(&format!(
            "Only the first {MAX_ISSUES} issues are listed; fix these and run again.\n"
        ));
    }
    for issue in &report.issues {
        out.push_str(&format!(
            "\nline {} {} [{}]: {}\n  fix: {}\n",
            issue.line,
            issue.severity.as_str(),
            issue.code,
            issue.message,
            issue.fix
        ));
    }
    out
}

// ---- the checker ------------------------------------------------------------

/// comrak's extensions exactly as `fs::markdown_to_html` sets them (render-
/// only options aside), so the AST here is the one the reading view renders.
fn parse_options(frontmatter: bool) -> comrak::Options<'static> {
    let mut options = comrak::Options::default();
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.footnotes = true;
    options.extension.math_dollars = true;
    options.extension.alerts = true;
    options.extension.header_id_prefix = Some(String::new());
    if frontmatter {
        options.extension.front_matter_delimiter = Some("---".to_owned());
    }
    options
}

#[derive(Clone, Copy)]
struct Meta {
    dir: bool,
    /// A regular file: the only kind a check ever opens (a device such as
    /// `/dev/zero` never ends; a FIFO blocks its open).
    file: bool,
    len: u64,
}

enum Stat {
    Found(Meta),
    Missing,
    Unchecked,
}

/// What a target read yields: its line count, and for a markdown target the
/// anchors a `#fragment` may name.
struct Facts {
    lines: usize,
    anchors: Option<HashSet<String>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TargetKind {
    Link,
    Embed,
}

struct Target {
    kind: TargetKind,
    url: String,
    line: usize,
    /// The link text, or the image's alt text.
    text: String,
}

/// How a local target was written.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Spelling {
    Relative,
    /// `/docs/x.md`: GitHub (and the reading view) resolve it at the root.
    RootRelative,
    /// A local absolute path (`/home/…`, `~/…`, `file://…`, `C:\…`).
    Absolute,
}

struct Checker<'a> {
    doc: &'a Path,
    doc_dir: PathBuf,
    root: Option<&'a Path>,
    issues: Vec<Issue>,
    truncated: bool,
    stats: HashMap<PathBuf, Option<Meta>>,
    reads: HashMap<PathBuf, Option<Facts>>,
    suggest_scans: usize,
    unchecked: usize,
    first_unchecked_line: usize,
    checked_targets: usize,
    deadline: Instant,
    /// (line, from byte, to byte) of code spans that wrap across lines.
    wrapped_spans: Vec<(usize, usize, usize)>,
    /// This document's own facts (line count and anchors).
    own_lines: usize,
    own_anchors: HashSet<String>,
}

/// Check `text` as the markdown file `doc` (absolute, canonical).
pub(crate) fn check_text(text: &str, doc: &Path, root: Option<&Path>) -> Report {
    let doc_dir = doc.parent().map(Path::to_path_buf).unwrap_or_default();
    let src: Vec<&str> = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let mut checker = Checker {
        doc,
        doc_dir,
        root,
        issues: Vec::new(),
        truncated: false,
        stats: HashMap::new(),
        reads: HashMap::new(),
        suggest_scans: 0,
        unchecked: 0,
        first_unchecked_line: 0,
        checked_targets: 0,
        deadline: Instant::now() + CHECK_BUDGET,
        own_lines: text.lines().count(),
        own_anchors: HashSet::new(),
        wrapped_spans: Vec::new(),
    };

    let input = crate::fs::markdown_parse_input(text);
    let arena = comrak::Arena::new();
    let ast = comrak::parse_document(
        &arena,
        &input.text,
        &parse_options(input.frontmatter_lines.is_some()),
    );

    // Lines the line scans skip: frontmatter, code blocks (promoted `$$`
    // blocks included), and HTML comments. Index 0 is unused.
    let mut masked = vec![false; src.len() + 2];
    if let Some(n) = input.frontmatter_lines {
        for m in masked.iter_mut().take(n + 1).skip(1) {
            *m = true;
        }
    }
    let mut targets = Vec::new();
    let mut seen_slugs: HashSet<String> = HashSet::new();
    let mut anchorizer = comrak::Anchorizer::new();
    for node in ast.descendants() {
        let data = node.data();
        let line = input.source_line(data.sourcepos.start.line.max(1));
        match &data.value {
            NodeValue::CodeBlock(block) => {
                let end = input.source_line(data.sourcepos.end.line.max(1));
                for m in masked.iter_mut().take(end + 1).skip(line) {
                    *m = true;
                }
                let info = block.info.trim();
                if block.fenced && info.starts_with('{') {
                    let name = info
                        .trim_start_matches('{')
                        .split('}')
                        .next()
                        .unwrap_or("")
                        .trim();
                    checker.push(
                        line,
                        Severity::Warning,
                        "myst-directive",
                        format!(
                            "MyST directive fence ```{info}``` shows as a code block on GitHub."
                        ),
                        myst_fix(name),
                    );
                }
            }
            NodeValue::HtmlBlock(block) => {
                collect_html_ids(&block.literal, &mut checker.own_anchors);
                if block.literal.trim_start().starts_with("<!--") {
                    let end = input.source_line(data.sourcepos.end.line.max(1));
                    for m in masked.iter_mut().take(end + 1).skip(line) {
                        *m = true;
                    }
                }
            }
            NodeValue::HtmlInline(html) => collect_html_ids(html, &mut checker.own_anchors),
            // A code span that wraps onto the next line: the per-line
            // backtick pairing cannot see it, so blank it from the AST.
            NodeValue::Code(_) | NodeValue::Math(_)
                if data.sourcepos.end.line > data.sourcepos.start.line =>
            {
                let end = input.source_line(data.sourcepos.end.line.max(1));
                if end > line && end - line < 64 {
                    let spans = &mut checker.wrapped_spans;
                    spans.push((
                        line,
                        data.sourcepos.start.column.saturating_sub(1),
                        usize::MAX,
                    ));
                    for mid in line + 1..end {
                        spans.push((mid, 0, usize::MAX));
                    }
                    spans.push((end, 0, data.sourcepos.end.column));
                }
            }
            NodeValue::Heading(_) => {
                let title = node.collect_text();
                let base = comrak::Anchorizer::new().anchorize(&title);
                let id = anchorizer.anchorize(&title);
                if !base.is_empty() && !seen_slugs.insert(base.clone()) {
                    checker.push(
                        line,
                        Severity::Info,
                        "duplicate-heading",
                        format!(
                            "Heading \"{}\" repeats an earlier heading's anchor: #{base} \
                             reaches the first one; this one is #{id}.",
                            title.trim()
                        ),
                        "Make the heading unique if anything links to it.".to_string(),
                    );
                }
                checker.own_anchors.insert(id);
            }
            NodeValue::FootnoteDefinition(def) => {
                checker
                    .own_anchors
                    .insert(format!("fn-{}", def.name.to_lowercase()));
            }
            NodeValue::FootnoteReference(r) => {
                checker
                    .own_anchors
                    .insert(format!("fnref-{}", r.name.to_lowercase()));
            }
            NodeValue::Link(link) => targets.push(Target {
                kind: TargetKind::Link,
                url: link.url.clone(),
                line,
                text: node.collect_text(),
            }),
            NodeValue::Image(link) => targets.push(Target {
                kind: TargetKind::Embed,
                url: link.url.clone(),
                line,
                text: node.collect_text(),
            }),
            _ => {}
        }
    }

    if let Some(n) = input.frontmatter_lines {
        checker.check_frontmatter(&src, n);
    }
    checker.scan_lines(&src, &masked);
    for target in &targets {
        checker.check_target(target);
    }
    if checker.unchecked > 0 {
        let n = checker.unchecked;
        checker.push(
            checker.first_unchecked_line,
            Severity::Info,
            "unchecked",
            format!(
                "{n} link or embed target{} not checked: a check stats at most \
                 {MAX_TARGET_STATS} targets and reads at most {MAX_TARGET_READS} \
                 files of up to {} MB within {} s.",
                if n == 1 { " was" } else { "s were" },
                MAX_TARGET_READ_BYTES / (1024 * 1024),
                CHECK_BUDGET.as_secs()
            ),
            "Split a very large document, or check the remaining links by hand.".to_string(),
        );
    }
    checker.finish()
}

impl Checker<'_> {
    fn push(
        &mut self,
        line: usize,
        severity: Severity,
        code: &'static str,
        message: String,
        fix: String,
    ) {
        if self.issues.len() >= MAX_RAW_ISSUES {
            self.truncated = true;
            return;
        }
        self.issues.push(Issue {
            line: line.max(1),
            severity,
            code,
            message,
            fix,
        });
    }

    fn finish(mut self) -> Report {
        if self.issues.len() > MAX_ISSUES {
            // Keep the most severe first, then restore reading order.
            self.issues.sort_by_key(|i| (i.severity, i.line));
            self.issues.truncate(MAX_ISSUES);
            self.truncated = true;
        }
        self.issues.sort_by_key(|i| (i.line, i.severity));
        Report {
            issues: self.issues,
            truncated: self.truncated,
            checked_targets: self.checked_targets,
        }
    }

    fn note_unchecked(&mut self, line: usize) {
        if self.unchecked == 0 {
            self.first_unchecked_line = line;
        }
        self.unchecked += 1;
    }

    fn stat(&mut self, path: &Path) -> Stat {
        if let Some(meta) = self.stats.get(path) {
            return meta.map_or(Stat::Missing, Stat::Found);
        }
        if self.stats.len() >= MAX_TARGET_STATS || Instant::now() >= self.deadline {
            return Stat::Unchecked;
        }
        let meta = match std::fs::metadata(path) {
            Ok(m) => Some(Meta {
                dir: m.is_dir(),
                file: m.is_file(),
                len: m.len(),
            }),
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                None
            }
            // Permission denied, a stalled mount's EIO: not the document's
            // fault, and not a finding.
            Err(_) => return Stat::Unchecked,
        };
        self.stats.insert(path.to_path_buf(), meta);
        meta.map_or(Stat::Missing, Stat::Found)
    }

    /// Read a target's facts (cached; bounded by count, size and budget).
    fn facts(&mut self, path: &Path, meta: Meta) -> Option<&Facts> {
        if !self.reads.contains_key(path) {
            if self.reads.len() >= MAX_TARGET_READS
                || !meta.file
                || meta.len > MAX_TARGET_READ_BYTES
                || Instant::now() >= self.deadline
            {
                return None;
            }
            // Over the cap (the file grew since the stat): too large to
            // check, like one that was already over it.
            let bytes = read_regular(path, MAX_TARGET_READ_BYTES).ok().flatten();
            let facts = bytes.map(|bytes| {
                let text = String::from_utf8_lossy(&bytes);
                let anchors = is_markdown(path).then(|| markdown_anchors(&text));
                Facts {
                    lines: text.lines().count(),
                    anchors,
                }
            });
            self.reads.insert(path.to_path_buf(), facts);
        }
        self.reads.get(path).and_then(Option::as_ref)
    }

    // ---- frontmatter --------------------------------------------------------

    /// Lines 2..n-1 of an accepted block must be YAML-shaped at column 0:
    /// `key: value`, `key:`, a `- item`, a comment, or blank. Indented lines
    /// (nested maps, list items, wrapped scalars) are left to YAML.
    fn check_frontmatter(&mut self, src: &[&str], block_lines: usize) {
        for (i, line) in src
            .iter()
            .enumerate()
            .take(block_lines.saturating_sub(1))
            .skip(1)
        {
            if line.trim().is_empty()
                || line.starts_with([' ', '\t'])
                || line.starts_with('#')
                || line.starts_with("- ")
                || *line == "-"
                || yaml_key(line).is_some()
            {
                continue;
            }
            let fix = match line.split_once('=') {
                Some((key, value)) if is_key(key.trim()) => {
                    format!("Write it as YAML: `{}: {}`.", key.trim(), value.trim())
                }
                _ => "Write it as `key: value` (YAML), or a `- item` under a `key:` line."
                    .to_string(),
            };
            self.push(
                i + 1,
                Severity::Warning,
                "frontmatter",
                format!(
                    "Frontmatter line `{}` is not `key: value` or list shaped.",
                    line.trim()
                ),
                fix,
            );
        }
    }

    // ---- line scans ---------------------------------------------------------

    fn scan_lines(&mut self, src: &[&str], masked: &[bool]) {
        // Footnote labels: lowercase label -> first line.
        let mut defs: Vec<(String, usize)> = Vec::new();
        let mut refs: Vec<(String, usize)> = Vec::new();
        let mut prev_quote = false;
        for (i, raw) in src.iter().enumerate() {
            let line_no = i + 1;
            if masked.get(line_no).copied().unwrap_or(false) {
                prev_quote = false;
                continue;
            }
            let unwrapped = blank_spans(raw, line_no, &self.wrapped_spans);
            let raw = unwrapped.as_str();
            let prose = mask_inline(raw);
            let quote_rest = strip_quote(&prose);
            let is_quote = quote_rest.is_some();
            if let Some(rest) = quote_rest {
                if !prev_quote {
                    self.check_alert(line_no, rest);
                }
            }
            prev_quote = is_quote;
            self.scan_wikilinks(line_no, &prose);
            self.scan_mdx(line_no, &prose);
            self.scan_fences_and_tags(line_no, raw, &prose);
            scan_footnotes(line_no, &prose, &mut defs, &mut refs);
        }
        let def_labels: HashSet<&str> = defs.iter().map(|(l, _)| l.as_str()).collect();
        let ref_labels: HashSet<&str> = refs.iter().map(|(l, _)| l.as_str()).collect();
        let mut reported = HashSet::new();
        for (label, line) in &refs {
            if !def_labels.contains(label.as_str()) && reported.insert(label.clone()) {
                self.push(
                    *line,
                    Severity::Error,
                    "dangling-footnote",
                    format!("Footnote [^{label}] has no definition; it shows as literal text."),
                    format!("Add a definition line `[^{label}]: …`, or remove the reference."),
                );
            }
        }
        for (label, line) in &defs {
            if !ref_labels.contains(label.as_str()) {
                self.push(
                    *line,
                    Severity::Error,
                    "unused-footnote",
                    format!(
                        "Footnote [^{label}] is defined but never referenced; it is not rendered."
                    ),
                    format!(
                        "Reference it with `[^{label}]` in the text, or delete the definition."
                    ),
                );
            }
        }
    }

    /// The first line of a blockquote (`rest` = after the `>` markers).
    fn check_alert(&mut self, line: usize, rest: &str) {
        let Some(after) = rest.trim_start().strip_prefix("[!") else {
            return;
        };
        let Some(close) = after.find(']') else {
            return;
        };
        let name = &after[..close];
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphabetic() || b == b'-') {
            return;
        }
        let upper = name.to_ascii_uppercase();
        let tail = &after[close + 1..];
        if !ALERT_TYPES.contains(&upper.as_str()) {
            let standard = alert_equivalent(&upper);
            self.push(
                line,
                Severity::Warning,
                "alert-type",
                format!(
                    "Alert type [!{name}] is not one GitHub renders (NOTE, TIP, IMPORTANT, \
                     WARNING, CAUTION); it shows as a plain quote."
                ),
                format!("Use `> [!{standard}]`."),
            );
        } else if tail.starts_with(['+', '-']) {
            self.push(
                line,
                Severity::Warning,
                "alert-type",
                format!(
                    "Foldable alert [!{name}]{} is Obsidian-only syntax.",
                    &tail[..1]
                ),
                format!("Use `> [!{upper}]` alone on the line."),
            );
        } else if !tail.trim().is_empty() {
            self.push(
                line,
                Severity::Warning,
                "alert-title",
                format!(
                    "Text after [!{name}] on the same line: GitHub then renders a plain quote, \
                     not an alert."
                ),
                format!(
                    "Put `> [!{upper}]` alone on the first line and \"{}\" on the next.",
                    tail.trim()
                ),
            );
        }
    }

    fn scan_wikilinks(&mut self, line: usize, prose: &str) {
        let mut from = 0;
        while let Some(open) = prose[from..].find("[[").map(|i| i + from) {
            let body_start = open + 2;
            let Some(close) = prose[body_start..].find("]]").map(|i| i + body_start) else {
                break;
            };
            let body = &prose[body_start..close];
            let after = prose[close + 2..].chars().next();
            from = close + 2;
            // `[[1]](url)` / `[[1]][ref]`: a markdown link whose text is `[1]`.
            if body.trim().is_empty() || body.contains('[') || matches!(after, Some('(' | '[')) {
                continue;
            }
            let embed = open > 0 && prose.as_bytes()[open - 1] == b'!';
            let written = format!("{}[[{body}]]", if embed { "!" } else { "" });
            self.push(
                line,
                Severity::Warning,
                "wikilink",
                format!("Obsidian wikilink `{written}` shows as literal text on GitHub."),
                format!("Write `{}`.", wikilink_fix(body, embed)),
            );
        }
    }

    fn scan_mdx(&mut self, line: usize, prose: &str) {
        let t = prose.trim_start();
        let is_import = t.starts_with("import ")
            && (t.contains(" from '") || t.contains(" from \"") || t[7..].starts_with(['\'', '"']));
        let is_export = t.strip_prefix("export ").is_some_and(|rest| {
            [
                "const ",
                "let ",
                "var ",
                "function ",
                "default ",
                "async ",
                "class ",
                "{",
            ]
            .iter()
            .any(|kw| rest.starts_with(kw))
        });
        if is_import || is_export {
            self.push(
                line,
                Severity::Warning,
                "mdx",
                format!(
                    "MDX `{}` line shows as literal text on GitHub.",
                    if is_import { "import" } else { "export" }
                ),
                "Remove it; portable markdown has no imports or exports.".to_string(),
            );
        }
    }

    fn scan_fences_and_tags(&mut self, line: usize, raw: &str, prose: &str) {
        let indent = prose.len() - prose.trim_start_matches(' ').len();
        let t = prose.trim_start();
        if indent <= 3 && t.starts_with(":::") {
            let rest = t.trim_start_matches(':').trim();
            if !rest.is_empty() {
                self.push(
                    line,
                    Severity::Warning,
                    "fenced-div",
                    format!(
                        "`{}` is a Pandoc/Quarto/MyST fenced div; GitHub shows the colons as text.",
                        t.trim_end()
                    ),
                    fenced_div_fix(rest),
                );
            }
        }
        if let Some(open) = prose.find("{%") {
            if prose[open..].contains("%}") {
                self.push(
                    line,
                    Severity::Warning,
                    "markdoc",
                    "Markdoc/Liquid `{% … %}` tag shows as literal text on GitHub.".to_string(),
                    "Replace the tag with plain markdown (a heading, list, alert or link)."
                        .to_string(),
                );
            }
        }
        if let Some(role) = myst_role(raw, prose) {
            self.push(
                line,
                Severity::Warning,
                "myst-directive",
                format!("MyST role `{{{role}}}` shows as literal text on GitHub."),
                "Write a plain markdown link or inline code instead.".to_string(),
            );
        }
        if let Some(tag) = component_tag(prose) {
            self.push(
                line,
                Severity::Warning,
                "mdx",
                format!(
                    "`<{tag}>` is read as an HTML tag (an MDX/JSX component?); GitHub drops it."
                ),
                "Use plain markdown instead, or wrap code in backticks.".to_string(),
            );
        }
    }

    // ---- links and embeds ---------------------------------------------------

    fn check_target(&mut self, t: &Target) {
        let embed = t.kind == TargetKind::Embed;
        if embed {
            let alt = t.text.split('|').next().unwrap_or("").trim();
            if alt.is_empty() {
                self.push(
                    t.line,
                    Severity::Warning,
                    "missing-alt",
                    format!("Embed `{}` has no alt text.", t.url),
                    "Describe what it shows: `![UMAP of all cells, colored by cluster](…)`; \
                     GitHub shows the alt text wherever it cannot embed."
                        .to_string(),
                );
            }
        }
        let url = t.url.trim();
        if url.is_empty() {
            return;
        }
        if let Some(fragment) = url.strip_prefix('#') {
            self.check_own_fragment(t, fragment);
            return;
        }
        let windows = is_windows_absolute(url);
        if !windows {
            if let Some(scheme) = scheme_of(url) {
                match scheme.to_ascii_lowercase().as_str() {
                    "http" if !t.text.trim_start().starts_with("www.") => self.push(
                        t.line,
                        Severity::Info,
                        "http-link",
                        format!("`{url}` is plain http."),
                        format!("Use `https://{}` if the site serves it.", &url[7..]),
                    ),
                    "file" => {
                        let local = url.trim_start_matches("file://");
                        self.absolute_path(t, url, Some(Path::new(&percent_decode(local))));
                    }
                    _ => {}
                }
                return;
            }
        }
        if url.starts_with("//") {
            return;
        }
        if windows {
            self.absolute_path(t, url, None);
            return;
        }
        let (before_fragment, fragment) = match url.split_once('#') {
            Some((p, f)) => (p, Some(f)),
            None => (url, None),
        };
        let path_part = before_fragment.split('?').next().unwrap_or("");
        let decoded = percent_decode(path_part);
        if decoded.is_empty() {
            return;
        }
        let (resolved, spelling) = if decoded == "~" || decoded.starts_with("~/") {
            (expand_tilde(&decoded), Spelling::Absolute)
        } else if decoded.starts_with('/') {
            if LOCAL_PREFIXES.iter().any(|p| decoded.starts_with(p)) {
                (PathBuf::from(&decoded), Spelling::Absolute)
            } else {
                match self.root {
                    Some(root) => (
                        root.join(decoded.trim_start_matches('/')),
                        Spelling::RootRelative,
                    ),
                    None => (PathBuf::from(&decoded), Spelling::Absolute),
                }
            }
        } else {
            (self.doc_dir.join(&decoded), Spelling::Relative)
        };
        let resolved = normalize(&resolved);
        let mut stat = self.stat(&resolved);
        let mut resolved = resolved;
        let mut spelling = spelling;
        // `/abs/path` that is not under the root but exists on this machine:
        // a local absolute path after all.
        if spelling == Spelling::RootRelative && matches!(stat, Stat::Missing) {
            let absolute = normalize(Path::new(&decoded));
            if let Stat::Found(meta) = self.stat(&absolute) {
                stat = Stat::Found(meta);
                resolved = absolute;
                spelling = Spelling::Absolute;
            }
        }
        match stat {
            Stat::Unchecked => self.note_unchecked(t.line),
            Stat::Missing => {
                self.checked_targets += 1;
                self.broken(t, url, &decoded, &resolved, spelling);
            }
            Stat::Found(meta) => {
                self.checked_targets += 1;
                if spelling == Spelling::Absolute {
                    self.absolute_path(t, url, Some(&resolved));
                }
                if embed && !meta.dir && meta.len > LARGE_EMBED_BYTES {
                    self.push(
                        t.line,
                        Severity::Warning,
                        "large-embed",
                        format!(
                            "Embed `{url}` is {:.1} MB; it loads slowly over a remote \
                             connection and GitHub will not preview it.",
                            meta.len as f64 / (1024.0 * 1024.0)
                        ),
                        "Embed a smaller export (a PNG under 10 MB, or a thumbnail) and link \
                         the full file."
                            .to_string(),
                    );
                }
                if let Some(fragment) = fragment.filter(|f| !f.is_empty()) {
                    if resolved == self.doc {
                        self.check_own_fragment(t, fragment);
                    } else if meta.file {
                        // A directory, device or FIFO has no lines or
                        // headings to check.
                        self.check_target_fragment(t, url, &resolved, meta, fragment);
                    }
                }
            }
        }
    }

    fn absolute_path(&mut self, t: &Target, url: &str, resolved: Option<&Path>) {
        let relative = resolved
            .filter(|p| p.is_absolute())
            .map(|p| encode_spaces(&relative_to(&self.doc_dir, &normalize(p))));
        let fix = match relative {
            Some(rel) => format!("Link it relative to this document's folder: `{rel}`."),
            None => "Use a path relative to this document's folder.".to_string(),
        };
        self.push(
            t.line,
            Severity::Warning,
            "absolute-path",
            format!(
                "{} `{url}` is an absolute local path; it breaks for anyone else and on GitHub.",
                if t.kind == TargetKind::Embed {
                    "Embed"
                } else {
                    "Link"
                }
            ),
            fix,
        );
    }

    fn broken(
        &mut self,
        t: &Target,
        url: &str,
        decoded: &str,
        resolved: &Path,
        spelling: Spelling,
    ) {
        let embed = t.kind == TargetKind::Embed;
        let mut fix = None;
        if let Some(found) = self.case_variant(resolved) {
            let rel = encode_spaces(&relative_to(&self.doc_dir, &found));
            fix = Some(format!(
                "Paths are case-sensitive: the file on disk is `{rel}`; link that."
            ));
        }
        if fix.is_none() && spelling == Spelling::Relative {
            if let Some(root) = self.root {
                let from_root = normalize(&root.join(decoded));
                if from_root != resolved {
                    if let Stat::Found(_) = self.stat(&from_root) {
                        let rel = encode_spaces(&relative_to(&self.doc_dir, &from_root));
                        fix = Some(format!(
                            "`{decoded}` exists relative to the workspace root; from this \
                             document's folder write `{rel}`."
                        ));
                    }
                }
            }
        }
        if fix.is_none() && !decoded.contains('%') && url.contains(' ') {
            fix = Some("Encode spaces as `%20`.".to_string());
        }
        let fix = fix.unwrap_or_else(|| {
            format!(
                "Fix the path (relative to {}) or create the file.",
                if spelling == Spelling::RootRelative {
                    "the workspace root"
                } else {
                    "this document's folder"
                }
            )
        });
        self.push(
            t.line,
            Severity::Error,
            if embed { "broken-embed" } else { "broken-link" },
            format!(
                "{} `{url}` points at `{}`, which does not exist.",
                if embed { "Embed" } else { "Link" },
                self.shown(resolved)
            ),
            fix,
        );
    }

    /// `path` for a message: workspace-relative when under the root (short,
    /// and what the user sees in the file tree), else absolute.
    fn shown(&self, path: &Path) -> String {
        self.root
            .and_then(|root| path.strip_prefix(root).ok())
            .filter(|rel| !rel.as_os_str().is_empty())
            .unwrap_or(path)
            .display()
            .to_string()
    }

    /// A sibling whose name differs from `path`'s only in case.
    fn case_variant(&mut self, path: &Path) -> Option<PathBuf> {
        let parent = path.parent()?;
        let name = path.file_name()?.to_str()?.to_lowercase();
        if self.suggest_scans >= MAX_SUGGEST_SCANS || Instant::now() >= self.deadline {
            return None;
        }
        self.suggest_scans += 1;
        std::fs::read_dir(parent)
            .ok()?
            .take(MAX_SUGGEST_ENTRIES)
            .filter_map(Result::ok)
            .find(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.to_lowercase() == name)
            })
            .map(|e| e.path())
    }

    fn check_own_fragment(&mut self, t: &Target, fragment: &str) {
        let fragment = percent_decode(fragment);
        if let Some(range) = line_range(&fragment) {
            let lines = self.own_lines;
            self.check_line_range(t, "this document", &fragment, range, lines);
            return;
        }
        if !is_anchor_fragment(&fragment) {
            return;
        }
        let want = normalize_anchor(&fragment);
        if self.own_anchors.contains(&want) {
            return;
        }
        let fix = anchor_fix(&fragment, &self.own_anchors, "a heading here");
        self.push(
            t.line,
            Severity::Error,
            "missing-anchor",
            format!("`#{fragment}` matches no heading or footnote in this document."),
            fix,
        );
    }

    fn check_target_fragment(
        &mut self,
        t: &Target,
        url: &str,
        path: &Path,
        meta: Meta,
        fragment: &str,
    ) {
        let fragment = percent_decode(fragment);
        let range = line_range(&fragment);
        let wants_anchor = range.is_none() && is_anchor_fragment(&fragment) && is_markdown(path);
        if range.is_none() && !wants_anchor {
            // A viewer locator (`#page=4`, `#row=5-9`, `#/key`, …): not checked.
            return;
        }
        let Some(facts) = self.facts(path, meta) else {
            self.note_unchecked(t.line);
            return;
        };
        let lines = facts.lines;
        let anchors = facts.anchors.clone();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| url.to_string());
        if let Some(range) = range {
            self.check_line_range(t, &name, &fragment, range, lines);
            return;
        }
        let Some(anchors) = anchors else { return };
        if anchors.contains(&normalize_anchor(&fragment)) {
            return;
        }
        let fix = anchor_fix(&fragment, &anchors, &format!("a heading in {name}"));
        self.push(
            t.line,
            Severity::Error,
            "missing-heading",
            format!("`{url}`: {name} has no heading `#{fragment}`."),
            fix,
        );
    }

    fn check_line_range(
        &mut self,
        t: &Target,
        name: &str,
        fragment: &str,
        (a, b): (usize, usize),
        lines: usize,
    ) {
        if a >= 1 && b <= lines && a <= b {
            return;
        }
        let message = if a > b {
            format!("`#{fragment}` in {name} is a backwards line range.")
        } else {
            format!(
                "`#{fragment}` is past the end of {name} ({lines} line{}).",
                if lines == 1 { "" } else { "s" }
            )
        };
        self.push(
            t.line,
            Severity::Error,
            "line-out-of-range",
            message,
            format!(
                "Point at lines 1 to {lines}, e.g. `#L{}-L{}`.",
                a.min(lines).max(1),
                b.min(lines).max(1)
            ),
        );
    }
}

// ---- helpers ----------------------------------------------------------------

/// Read `path` when it is a regular file of at most `cap` bytes (`Ok(None)`
/// past the cap). Callers stat first and open only regular files; the open
/// is still `O_NONBLOCK` and re-checked by `fstat`, so a FIFO or device
/// swapped in after that stat can neither block this worker (and its
/// `FILESYSTEM_WORK` permit) nor stream without end, and `take` bounds a
/// file that grew.
pub(crate) fn read_regular(path: &Path, cap: u64) -> std::io::Result<Option<Vec<u8>>> {
    use rustix::fs::{Mode, OFlags};
    let fd = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )?;
    let file = std::fs::File::from(fd);
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let mut bytes = Vec::with_capacity(meta.len().min(cap) as usize);
    file.take(cap + 1).read_to_end(&mut bytes)?;
    Ok((bytes.len() as u64 <= cap).then_some(bytes))
}

fn is_markdown(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "md" | "markdown" | "mdown" | "mkd"
        )
    })
}

/// Heading slugs (GitHub rule, de-duplicated with `-1`, `-2` …), footnote
/// ids and raw-HTML ids of a markdown text.
fn markdown_anchors(text: &str) -> HashSet<String> {
    let input = crate::fs::markdown_parse_input(text);
    let arena = comrak::Arena::new();
    let ast = comrak::parse_document(
        &arena,
        &input.text,
        &parse_options(input.frontmatter_lines.is_some()),
    );
    let mut anchorizer = comrak::Anchorizer::new();
    let mut out = HashSet::new();
    for node in ast.descendants() {
        match &node.data().value {
            NodeValue::Heading(_) => {
                out.insert(anchorizer.anchorize(&node.collect_text()));
            }
            NodeValue::FootnoteDefinition(def) => {
                out.insert(format!("fn-{}", def.name.to_lowercase()));
            }
            NodeValue::HtmlBlock(block) => collect_html_ids(&block.literal, &mut out),
            NodeValue::HtmlInline(html) => collect_html_ids(html, &mut out),
            _ => {}
        }
    }
    out
}

/// `id="…"` / `name="…"` values in raw HTML, lowercased.
fn collect_html_ids(html: &str, out: &mut HashSet<String>) {
    for attr in ["id=", "name="] {
        let mut from = 0;
        while let Some(at) = html[from..].find(attr).map(|i| i + from) {
            from = at + attr.len();
            let preceded = at == 0 || html.as_bytes()[at - 1].is_ascii_whitespace();
            let rest = &html[from..];
            let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
                continue;
            };
            if let Some(end) = rest[1..].find(quote) {
                if preceded && out.len() < 10_000 {
                    out.insert(rest[1..1 + end].to_lowercase());
                }
            }
        }
    }
}

/// A fragment naming a heading or anchor (not a line range or a viewer
/// locator such as `page=4`, `row=5-9`, `/json/pointer`, `t=30`).
fn is_anchor_fragment(fragment: &str) -> bool {
    !fragment.is_empty() && !fragment.contains('=') && !fragment.starts_with('/')
}

fn normalize_anchor(fragment: &str) -> String {
    let lower = fragment.to_lowercase();
    lower
        .strip_prefix("user-content-")
        .map(str::to_string)
        .unwrap_or(lower)
}

/// `L10`, `L10-L20`, `L10-20`, with optional `C` columns → (start, end).
fn line_range(fragment: &str) -> Option<(usize, usize)> {
    fn one(s: &str) -> Option<usize> {
        let s = s.strip_prefix('L').unwrap_or(s);
        let digits = s.split('C').next()?;
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        digits.parse().ok()
    }
    let rest = fragment.strip_prefix('L')?;
    match rest.split_once('-') {
        Some((a, b)) => Some((one(a)?, one(b)?)),
        None => {
            let a = one(rest)?;
            Some((a, a))
        }
    }
}

/// The fix for a fragment that names no anchor: the GitHub slug of what
/// was written when that exists, else the closest anchor, else a pointer.
fn anchor_fix(fragment: &str, anchors: &HashSet<String>, place: &str) -> String {
    let slug = comrak::Anchorizer::new().anchorize(fragment);
    if slug != fragment && anchors.contains(&slug) {
        return format!(
            "Use the GitHub slug `#{slug}` (lowercase, spaces to `-`, punctuation dropped)."
        );
    }
    let want = normalize_anchor(fragment);
    let mut best: Option<(usize, &String)> = None;
    for anchor in anchors.iter().take(2000) {
        if anchor.starts_with("fn-") || anchor.starts_with("fnref-") || anchor.len() > 200 {
            continue;
        }
        let d = if anchor.contains(&want) || want.contains(anchor.as_str()) {
            0
        } else {
            edit_distance(&want, anchor)
        };
        if best.is_none_or(|(bd, ba)| d < bd || (d == bd && anchor < ba)) {
            best = Some((d, anchor));
        }
    }
    match best {
        Some((d, anchor)) if d <= (want.len() / 3).max(3) => format!("Did you mean `#{anchor}`?"),
        _ => format!(
            "Link to {place} by its GitHub slug (lowercase, spaces to `-`), or add the heading."
        ),
    }
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().take(200).collect();
    let b: Vec<char> = b.chars().take(200).collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// A URL scheme (`https`, `mailto`, `file`), when `url` starts with one.
fn scheme_of(url: &str) -> Option<&str> {
    let colon = url.find(':')?;
    let scheme = &url[..colon];
    let mut bytes = scheme.bytes();
    let first = bytes.next()?;
    (first.is_ascii_alphabetic()
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.')))
    .then_some(scheme)
}

/// `C:\…` or `C:/…`.
fn is_windows_absolute(url: &str) -> bool {
    let b = url.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
}

fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn encode_spaces(path: &Path) -> String {
    path.to_string_lossy().replace(' ', "%20")
}

/// Resolve `.` and `..` lexically (no disk access; symlinks are the stat's
/// business).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// `target` relative to the directory `from` (both absolute, normalized).
fn relative_to(from: &Path, target: &Path) -> PathBuf {
    let from: Vec<Component> = from.components().collect();
    let to: Vec<Component> = target.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..from.len() {
        out.push("..");
    }
    for c in &to[common..] {
        out.push(c);
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// `line` with the parts of wrapped code spans on it blanked (byte ranges
/// widened to char boundaries, so UTF-8 stays intact).
fn blank_spans(line: &str, line_no: usize, spans: &[(usize, usize, usize)]) -> String {
    let mut out = line.to_string();
    for &(at, from, to) in spans {
        if at != line_no {
            continue;
        }
        let mut from = from.min(line.len());
        let mut to = to.min(line.len());
        while !line.is_char_boundary(from) {
            from -= 1;
        }
        while !line.is_char_boundary(to) {
            to += 1;
        }
        if from < to {
            out.replace_range(from..to, &" ".repeat(to - from));
        }
    }
    out
}

/// Blank inline code spans and `$…$` math (byte-for-byte spaces, so
/// positions and UTF-8 stay intact): their contents are never syntax.
fn mask_inline(line: &str) -> String {
    let mut bytes = line.as_bytes().to_vec();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let run = bytes[i..].iter().take_while(|&&b| b == b'`').count();
            let mut j = i + run;
            let mut closed = None;
            while j < bytes.len() {
                if bytes[j] == b'`' {
                    let r = bytes[j..].iter().take_while(|&&b| b == b'`').count();
                    if r == run {
                        closed = Some(j + r);
                        break;
                    }
                    j += r;
                } else {
                    j += 1;
                }
            }
            match closed {
                Some(end) => {
                    bytes[i..end].fill(b' ');
                    i = end;
                }
                None => i += run,
            }
        } else if bytes[i] == b'\\' {
            i += 2;
        } else {
            i += 1;
        }
    }
    // `$…$` on one line: the opener not followed by a space, the closer not
    // preceded by one (comrak's dollar-math rule, near enough).
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && (i == 0 || bytes[i - 1] != b'\\') {
            let opener_ok = bytes.get(i + 1).is_some_and(|b| !b.is_ascii_whitespace());
            if opener_ok {
                if let Some(end) = (i + 1..bytes.len()).find(|&j| {
                    bytes[j] == b'$' && bytes[j - 1] != b'\\' && !bytes[j - 1].is_ascii_whitespace()
                }) {
                    bytes[i..=end].fill(b' ');
                    i = end + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    String::from_utf8(bytes).unwrap_or_else(|_| line.to_string())
}

/// After the blockquote markers, when the line is a quote line.
fn strip_quote(line: &str) -> Option<&str> {
    let mut rest = line;
    let mut depth = 0;
    loop {
        let trimmed = rest.trim_start_matches(' ');
        if rest.len() - trimmed.len() > 3 {
            break;
        }
        match trimmed.strip_prefix('>') {
            Some(after) => {
                rest = after.strip_prefix(' ').unwrap_or(after);
                depth += 1;
            }
            None => break,
        }
    }
    (depth > 0).then_some(rest)
}

fn scan_footnotes(
    line: usize,
    prose: &str,
    defs: &mut Vec<(String, usize)>,
    refs: &mut Vec<(String, usize)>,
) {
    let body = strip_quote(prose).unwrap_or(prose);
    let lead = body.len() - body.trim_start().len();
    let mut from = 0;
    while let Some(at) = body[from..].find("[^").map(|i| i + from) {
        let label_start = at + 2;
        let Some(close) = body[label_start..].find(']').map(|i| i + label_start) else {
            break;
        };
        from = close + 1;
        let label = &body[label_start..close];
        if label.is_empty() || label.chars().any(char::is_whitespace) || label.len() > 100 {
            continue;
        }
        let label = label.to_lowercase();
        let is_def = at == lead && body[close + 1..].starts_with(':') && lead <= 3;
        let list = if is_def { &mut *defs } else { &mut *refs };
        if list.len() < 10_000 && !list.iter().any(|(l, _)| *l == label) {
            list.push((label, line));
        }
    }
}

/// `{role}` directly followed by a code span in the raw line (MyST role).
fn myst_role<'a>(raw: &'a str, prose: &str) -> Option<&'a str> {
    let bytes = prose.as_bytes();
    let mut from = 0;
    while let Some(open) = prose[from..].find('{').map(|i| i + from) {
        let close = prose[open..].find('}').map(|i| i + open)?;
        from = open + 1;
        let name = &raw[open + 1..close];
        let named = !name.is_empty()
            && name.as_bytes()[0].is_ascii_lowercase()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'-' | b'_'));
        if named
            && raw.as_bytes().get(close + 1) == Some(&b'`')
            && bytes.get(close + 1) == Some(&b' ')
        {
            return Some(name);
        }
    }
    None
}

/// The first capitalized tag (`<Chart`, `</Tabs>`) that is not an HTML
/// element written in capitals.
fn component_tag(prose: &str) -> Option<&str> {
    let bytes = prose.as_bytes();
    let mut from = 0;
    while let Some(lt) = prose[from..].find('<').map(|i| i + from) {
        from = lt + 1;
        let start = lt + 1 + usize::from(bytes.get(lt + 1) == Some(&b'/'));
        if !bytes.get(start).is_some_and(u8::is_ascii_uppercase) {
            continue;
        }
        let end = start
            + bytes[start..]
                .iter()
                .take_while(|b| b.is_ascii_alphanumeric() || **b == b'.' || **b == b'_')
                .count();
        let terminated = match bytes.get(end) {
            None => true,
            Some(b) => b.is_ascii_whitespace() || *b == b'/' || *b == b'>',
        };
        let name = &prose[start..end];
        // Only a tag comrak can read as one: closed by a `>` before any
        // other `<`, or ending the line (JSX wrapped over lines); `M<N
        // results` is prose.
        let rest = &prose[end..];
        let closed =
            rest.trim().is_empty() || rest.find('>').is_some_and(|gt| !rest[..gt].contains('<'));
        if !closed {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        if terminated && !HTML_ELEMENTS.split_ascii_whitespace().any(|e| e == lower) {
            return Some(name);
        }
    }
    None
}

/// HTML element names (a capitalized `<Details>` is still HTML, not MDX).
const HTML_ELEMENTS: &str = "\
    a abbr address article aside audio b bdi bdo blockquote body br button \
    caption cite code col colgroup dd del details dfn div dl dt em \
    figcaption figure footer h1 h2 h3 h4 h5 h6 header hr i iframe img ins \
    kbd li main mark nav ol p picture pre q rp rt ruby s samp section small \
    source span strong sub summary sup table tbody td tfoot th thead time tr \
    tt u ul var video wbr";

/// YAML key of a `key:` / `key: value` line.
fn yaml_key(line: &str) -> Option<&str> {
    let (key, rest) = line.split_once(':')?;
    let key = key.trim_end();
    let unquoted = key
        .strip_prefix('"')
        .and_then(|k| k.strip_suffix('"'))
        .or_else(|| key.strip_prefix('\'').and_then(|k| k.strip_suffix('\'')))
        .unwrap_or(key);
    (is_key(unquoted) && (rest.is_empty() || rest.starts_with([' ', '\t']))).then_some(unquoted)
}

fn is_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// The standard GitHub type closest to an Obsidian/Docusaurus callout type.
fn alert_equivalent(upper: &str) -> &'static str {
    match upper {
        "HINT" | "SUCCESS" | "CHECK" | "DONE" => "TIP",
        "ATTENTION" | "WARN" => "WARNING",
        "DANGER" | "ERROR" | "BUG" | "FAILURE" | "FAIL" | "MISSING" => "CAUTION",
        "IMPORTANT" => "IMPORTANT",
        _ => "NOTE",
    }
}

fn myst_fix(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "note" | "tip" | "important" | "warning" | "caution" => format!(
            "Write a GitHub alert: `> [!{}]` followed by `> ` lines.",
            name.to_ascii_uppercase()
        ),
        "math" => "Use a ```` ```math ```` fence or `$$ … $$`.".to_string(),
        "mermaid" => "Use a ```` ```mermaid ```` fence.".to_string(),
        "figure" | "image" => "Embed with `![alt text](relative/path.png)`.".to_string(),
        _ => "Use plain markdown: an alert, a fenced code block with a language, or an embed."
            .to_string(),
    }
}

fn fenced_div_fix(rest: &str) -> String {
    let class = rest
        .trim_start_matches('{')
        .trim_start_matches('.')
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    let alert = match class.as_str() {
        "NOTE" | "TIP" | "IMPORTANT" | "WARNING" | "CAUTION" => Some(class.clone()),
        "CALLOUT-NOTE" | "INFO" => Some("NOTE".to_string()),
        "CALLOUT-TIP" | "SUCCESS" => Some("TIP".to_string()),
        "CALLOUT-WARNING" => Some("WARNING".to_string()),
        "CALLOUT-IMPORTANT" => Some("IMPORTANT".to_string()),
        "CALLOUT-CAUTION" | "DANGER" => Some("CAUTION".to_string()),
        _ => None,
    };
    match alert {
        Some(t) => format!(
            "Write a GitHub alert: `> [!{t}]` followed by `> ` lines, and drop the `:::` lines."
        ),
        None => {
            "Drop the `:::` lines and use plain markdown (a heading, list, or alert).".to_string()
        }
    }
}

/// `[[target#heading|alias]]` → the portable link or embed.
fn wikilink_fix(body: &str, embed: bool) -> String {
    let (target, alias) = match body.split_once('|') {
        Some((t, a)) => (t.trim(), Some(a.trim())),
        None => (body.trim(), None),
    };
    let (file, heading) = match target.split_once('#') {
        Some((f, h)) => (f.trim(), Some(h.trim())),
        None => (target, None),
    };
    let has_ext = Path::new(file).extension().is_some();
    let file_part = if file.is_empty() {
        String::new()
    } else if has_ext || embed {
        file.replace(' ', "%20")
    } else {
        format!("{}.md", file.replace(' ', "%20"))
    };
    let fragment = heading
        .filter(|h| !h.is_empty())
        .map(|h| format!("#{}", comrak::Anchorizer::new().anchorize(h)))
        .unwrap_or_default();
    if embed {
        // `|400` after an embed is a width in both dialects.
        let alt = match alias {
            Some(a) if a.bytes().all(|b| b.is_ascii_digit()) => {
                format!("{}|{a}", file_stem(file))
            }
            Some(a) => a.to_string(),
            None => file_stem(file),
        };
        format!("![{alt}]({file_part}{fragment})")
    } else {
        let text = alias.map(str::to_string).unwrap_or_else(|| {
            heading
                .filter(|h| !h.is_empty() && file.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| target.to_string())
        });
        format!("[{text}]({file_part}{fragment})")
    }
}

fn file_stem(file: &str) -> String {
    Path::new(file)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}
