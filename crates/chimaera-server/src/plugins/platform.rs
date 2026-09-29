//! The 0.2 manifest tables: `[[views]]`, `[[files]]`, `[[actions]]` and
//! `[[settings]]`, their checks, and the path matching file kinds and
//! actions use. Design: docs/plugin-platform-plan.md §3 (screens), §5
//! (files) and §9 (settings).
//!
//! Everything here is data a manifest declares; the host acts on it
//! elsewhere (`screens`, `files`, `pdata`). A 0.1 manifest can't carry any
//! of it (`super::validate`).

use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::{json, Value};

use super::Manifest;

const VIEWS_MAX: usize = 32;
const FILE_KINDS_MAX: usize = 16;
const ACTIONS_MAX: usize = 32;
const SETTINGS_MAX: usize = 64;
const PATTERNS_MAX: usize = 16;
const PATTERN_LEN_MAX: usize = 128;
const TITLE_MAX: usize = 80;
const DESCRIPTION_MAX: usize = 400;
const OPTIONS_MAX: usize = 32;
/// A file kind's event debounce: 300 ms unless it says, at most 5 s.
pub(crate) const DEBOUNCE_DEFAULT_MS: u64 = 300;
const DEBOUNCE_MAX_MS: u64 = 5_000;

/// Where a view is drawn.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Slot {
    /// Its own tab (quick-open, the card, an action opens it).
    Tab,
    /// On the workspace dashboard.
    Panel,
    /// A claimed file's view (`[[files]] view`).
    File,
    /// A chip in a file view's toolbar.
    Status,
    /// A section inside the plugin's own Extensions card.
    Card,
}

impl Slot {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Slot::Tab => "tab",
            Slot::Panel => "panel",
            Slot::File => "file",
            Slot::Status => "status",
            Slot::Card => "card",
        }
    }
}

/// `[[views]]`.
#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct ViewDecl {
    pub(crate) id: String,
    /// What the tab, panel or chip is called (required: every view has a
    /// name a screen reader can say).
    pub(crate) title: String,
    pub(crate) slot: Slot,
    /// An icon from chimaera's set, by name.
    #[serde(default)]
    pub(crate) icon: Option<String>,
}

/// `[[files]]`: files matching `match` open in `view` where the plugin is
/// active (Open as text is always one click away).
#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileKind {
    #[serde(rename = "match")]
    pub(crate) patterns: Vec<String>,
    pub(crate) view: String,
    /// The kind's name ("LaTeX"): the Open with menu says it.
    pub(crate) label: String,
    /// How long a burst of changes to one file settles before the plugin
    /// hears `file-changed` / `file-saved`.
    #[serde(default)]
    pub(crate) debounce_ms: Option<u64>,
}

/// `[[actions]]`: an item in matching files' toolbar and menu; a click
/// calls the plugin's `on-action` with view `""`.
#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileAction {
    #[serde(rename = "match")]
    pub(crate) patterns: Vec<String>,
    pub(crate) label: String,
    pub(crate) action: String,
    #[serde(default)]
    pub(crate) icon: Option<String>,
}

/// A setting's type.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SettingType {
    Bool,
    Enum,
    String,
    Number,
    /// A workspace-relative path.
    Path,
}

/// Whose value it is.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SettingScope {
    /// One value for this host.
    Host,
    /// One value per workspace.
    #[default]
    Workspace,
}

/// `[[settings]]`.
#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct SettingDecl {
    pub(crate) key: String,
    #[serde(rename = "type")]
    pub(crate) kind: SettingType,
    pub(crate) default: Value,
    pub(crate) label: String,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) scope: SettingScope,
    /// `enum`'s choices.
    #[serde(default)]
    pub(crate) options: Vec<String>,
    /// `number`'s bounds.
    #[serde(default)]
    pub(crate) min: Option<f64>,
    #[serde(default)]
    pub(crate) max: Option<f64>,
}

impl SettingDecl {
    /// `value` if it is one this setting can hold, in its canonical form.
    pub(crate) fn check(&self, value: &Value) -> Result<Value, String> {
        let bad = |what: &str| Err(format!("{}: {what}", self.key));
        match self.kind {
            SettingType::Bool => match value {
                Value::Bool(_) => Ok(value.clone()),
                _ => bad("is true or false"),
            },
            SettingType::Enum => match value.as_str() {
                Some(v) if self.options.iter().any(|o| o == v) => Ok(value.clone()),
                _ => bad(&format!("is one of {}", self.options.join(", "))),
            },
            SettingType::String => match value.as_str() {
                Some(v) if v.len() <= 1024 => Ok(value.clone()),
                Some(_) => bad("is at most 1 KiB"),
                None => bad("is text"),
            },
            SettingType::Number => match value.as_f64() {
                Some(n) if self.min.is_some_and(|m| n < m) => bad("is under its minimum"),
                Some(n) if self.max.is_some_and(|m| n > m) => bad("is over its maximum"),
                Some(_) => Ok(value.clone()),
                None => bad("is a number"),
            },
            SettingType::Path => match value.as_str() {
                Some("") => Ok(value.clone()),
                Some(v) if plain_relative(v) && v.len() <= 1024 => Ok(value.clone()),
                _ => bad("is a path inside the workspace, without `..`"),
            },
        }
    }

    /// The declaration on the wire (Settings → Plugins draws it).
    pub(crate) fn json(&self) -> Value {
        json!({
            "key": self.key,
            "type": self.kind,
            "default": self.default,
            "label": self.label,
            "description": self.description,
            "scope": self.scope,
            "options": self.options,
            "min": self.min,
            "max": self.max,
        })
    }
}

impl serde::Serialize for SettingType {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(match self {
            SettingType::Bool => "bool",
            SettingType::Enum => "enum",
            SettingType::String => "string",
            SettingType::Number => "number",
            SettingType::Path => "path",
        })
    }
}

impl serde::Serialize for SettingScope {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(match self {
            SettingScope::Host => "host",
            SettingScope::Workspace => "workspace",
        })
    }
}

/// A name a manifest gives (a view id, an action, a setting key): what
/// rides a URL segment and a JSON key.
pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
        && !name.starts_with('.')
}

/// A relative path of plain components.
fn plain_relative(path: &str) -> bool {
    !path.is_empty()
        && std::path::Path::new(path)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

fn check_patterns(what: &str, patterns: &[String]) -> Result<(), String> {
    if patterns.is_empty() || patterns.len() > PATTERNS_MAX {
        return Err(format!("{what}: `match` names 1–{PATTERNS_MAX} patterns"));
    }
    for p in patterns {
        let ok = !p.is_empty()
            && p.len() <= PATTERN_LEN_MAX
            && !p.starts_with('/')
            && !p.split('/').any(|part| part == ".." || part == ".")
            && !p.contains('\\');
        if !ok {
            return Err(format!(
                "{what}: pattern {p:?} must be a relative glob (`*.tex`, `docs/**/*.md`)"
            ));
        }
    }
    Ok(())
}

fn short(what: &str, text: &str, max: usize) -> Result<(), String> {
    if text.trim().is_empty() || text.len() > max {
        return Err(format!("{what} must be 1–{max} characters"));
    }
    Ok(())
}

/// The 0.2 tables' consistency.
pub(crate) fn validate(m: &Manifest) -> Result<(), String> {
    if m.views.len() > VIEWS_MAX
        || m.files.len() > FILE_KINDS_MAX
        || m.actions.len() > ACTIONS_MAX
        || m.settings.len() > SETTINGS_MAX
    {
        return Err(format!(
            "at most {VIEWS_MAX} views, {FILE_KINDS_MAX} file kinds, {ACTIONS_MAX} actions and \
             {SETTINGS_MAX} settings"
        ));
    }
    let mut ids = BTreeSet::new();
    for v in &m.views {
        if !valid_name(&v.id) {
            return Err(format!(
                "view id {:?} must be 1–64 letters, digits, `_`, `-` or `.`",
                v.id
            ));
        }
        if !ids.insert(v.id.as_str()) {
            return Err(format!("view {:?} is declared twice", v.id));
        }
        short(&format!("view {}'s title", v.id), &v.title, TITLE_MAX)?;
    }
    for f in &m.files {
        check_patterns(&format!("[[files]] {}", f.label), &f.patterns)?;
        short("a [[files]] label", &f.label, TITLE_MAX)?;
        match m.views.iter().find(|v| v.id == f.view) {
            Some(v) if v.slot == Slot::File => {}
            Some(_) => {
                return Err(format!(
                    "[[files]] {}: view {:?} is not a file view (slot = \"file\")",
                    f.label, f.view
                ))
            }
            None => {
                return Err(format!(
                    "[[files]] {}: no view {:?} in [[views]]",
                    f.label, f.view
                ))
            }
        }
        if f.debounce_ms.is_some_and(|d| d > DEBOUNCE_MAX_MS) {
            return Err(format!(
                "[[files]] {}: debounce_ms is at most {DEBOUNCE_MAX_MS}",
                f.label
            ));
        }
    }
    let mut actions = BTreeSet::new();
    for a in &m.actions {
        if !valid_name(&a.action) {
            return Err(format!(
                "action {:?} must be 1–64 letters, digits, `_`, `-` or `.`",
                a.action
            ));
        }
        if !actions.insert(a.action.as_str()) {
            return Err(format!("action {:?} is declared twice", a.action));
        }
        short(&format!("action {}'s label", a.action), &a.label, TITLE_MAX)?;
        check_patterns(&format!("action {}", a.action), &a.patterns)?;
    }
    let mut keys = BTreeSet::new();
    for s in &m.settings {
        if !valid_name(&s.key) {
            return Err(format!(
                "setting key {:?} must be 1–64 letters, digits, `_`, `-` or `.`",
                s.key
            ));
        }
        if !keys.insert(s.key.as_str()) {
            return Err(format!("setting {:?} is declared twice", s.key));
        }
        short(&format!("setting {}'s label", s.key), &s.label, TITLE_MAX)?;
        if let Some(d) = &s.description {
            short(
                &format!("setting {}'s description", s.key),
                d,
                DESCRIPTION_MAX,
            )?;
        }
        if s.kind == SettingType::Enum
            && (s.options.is_empty()
                || s.options.len() > OPTIONS_MAX
                || s.options
                    .iter()
                    .any(|o| o.is_empty() || o.len() > TITLE_MAX))
        {
            return Err(format!(
                "setting {}: an enum lists 1–{OPTIONS_MAX} options",
                s.key
            ));
        }
        if s.kind != SettingType::Enum && !s.options.is_empty() {
            return Err(format!("setting {}: only an enum has options", s.key));
        }
        s.check(&s.default)
            .map_err(|e| format!("setting {}'s default: {e}", s.key))?;
    }
    Ok(())
}

/// Whether workspace-relative `path` matches `pattern`: `*` and `?` stay
/// within one path component, `**` spans any number; a pattern without a
/// `/` matches the file name wherever it is (`*.tex` matches
/// `chapters/intro.tex`).
pub(crate) fn matches(pattern: &str, path: &str) -> bool {
    if !pattern.contains('/') {
        let name = path.rsplit('/').next().unwrap_or(path);
        return glob(pattern.as_bytes(), name.as_bytes());
    }
    let pat: Vec<&str> = pattern.split('/').collect();
    let parts: Vec<&str> = path.split('/').collect();
    components(&pat, &parts)
}

fn components(pat: &[&str], parts: &[&str]) -> bool {
    match pat.split_first() {
        None => parts.is_empty(),
        Some((&"**", rest)) => (0..=parts.len()).any(|skip| components(rest, &parts[skip..])),
        Some((first, rest)) => match parts.split_first() {
            Some((part, more)) => glob(first.as_bytes(), part.as_bytes()) && components(rest, more),
            None => false,
        },
    }
}

/// `*` and `?` within one component (no `/` in either side here).
fn glob(pat: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while t < text.len() {
        if p < pat.len() && (pat[p] == b'?' || pat[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == b'*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if let Some(s) = star {
            p = s + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

/// The file kind of `m` that claims `path`, if any.
pub(crate) fn file_kind<'a>(m: &'a Manifest, path: &str) -> Option<&'a FileKind> {
    m.files
        .iter()
        .find(|f| f.patterns.iter().any(|p| matches(p, path)))
}

/// The 0.2 tables on the wire (the card, the file-kind dispatch, the file
/// menus, Settings → Plugins).
pub(crate) fn wire(m: &Manifest) -> Value {
    json!({
        "views": m.views.iter().map(|v| json!({
            "id": v.id, "title": v.title, "slot": v.slot.as_str(), "icon": v.icon,
        })).collect::<Vec<_>>(),
        "files": m.files.iter().map(|f| json!({
            "match": f.patterns, "view": f.view, "label": f.label,
        })).collect::<Vec<_>>(),
        "actions": m.actions.iter().map(|a| json!({
            "match": a.patterns, "label": a.label, "action": a.action, "icon": a.icon,
        })).collect::<Vec<_>>(),
        "settings": m.settings.iter().map(SettingDecl::json).collect::<Vec<_>>(),
    })
}

/// The platform's per-daemon half (on `AppState`): output folders,
/// durable data, surfaces, screens' invalidation and activity, and file
/// events. Hot state except `pdata` and the output folders themselves.
pub(crate) struct Platform {
    /// Where output folders live (`output::root_for`).
    pub(crate) output_root: std::path::PathBuf,
    pub(crate) usage: std::sync::Mutex<super::output::Usage>,
    pub(crate) data: super::pdata::PluginData,
    pub(crate) surfaces: std::sync::Mutex<super::surfaces::Surfaces>,
    pub(crate) screens: std::sync::Mutex<super::screens::Screens>,
    pub(crate) files: std::sync::Mutex<super::files::Files>,
    pub(crate) files_wake: std::sync::Arc<tokio::sync::Notify>,
    /// The file-events worker is running (`files::spawn_worker`).
    pub(crate) worker: std::sync::atomic::AtomicBool,
}

impl Platform {
    pub(crate) fn new(data_dir: &std::path::Path) -> Self {
        Platform {
            output_root: super::output::root_for(data_dir),
            usage: Default::default(),
            data: Default::default(),
            surfaces: Default::default(),
            screens: Default::default(),
            files: Default::default(),
            files_wake: Default::default(),
            worker: Default::default(),
        }
    }
}

/// A removed plugin: its surfaces, screens' state, file events, durable
/// data and output folders.
pub(crate) async fn forget_plugin(state: &std::sync::Arc<crate::AppState>, plugin: &str) {
    let p = &state.plugin_platform;
    crate::lock(&p.surfaces).forget_plugin(plugin);
    crate::lock(&p.screens).forget_plugin(plugin);
    crate::lock(&p.files).forget_plugin(plugin);
    let state = state.clone();
    let plugin = plugin.to_string();
    let _ = tokio::task::spawn_blocking(move || {
        super::pdata::forget_plugin(&state, &plugin);
        super::output::forget_plugin(&state, &plugin);
    })
    .await;
}

/// A deleted workspace: every plugin's platform state for it, off the
/// caller's path.
pub(crate) fn forget_workspace(state: &std::sync::Arc<crate::AppState>, ws: &str) {
    let p = &state.plugin_platform;
    crate::lock(&p.surfaces).forget_workspace(ws);
    crate::lock(&p.screens).forget_workspace(ws);
    crate::lock(&p.files).forget_workspace(ws);
    let state = state.clone();
    let ws = ws.to_string();
    tokio::task::spawn_blocking(move || {
        super::pdata::forget_workspace(&state, &ws);
        super::output::forget_workspace(&state.plugin_platform.output_root, &ws);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_like_a_shell() {
        assert!(matches("*.tex", "main.tex"));
        assert!(matches("*.tex", "chapters/intro.tex"));
        assert!(!matches("*.tex", "main.texx"));
        assert!(matches("docs/*.md", "docs/a.md"));
        assert!(!matches("docs/*.md", "docs/sub/a.md"));
        assert!(matches("docs/**/*.md", "docs/a.md"));
        assert!(matches("docs/**/*.md", "docs/sub/deeper/a.md"));
        assert!(matches("**/Makefile", "Makefile"));
        assert!(matches("ma?n.typ", "main.typ"));
        assert!(matches("*", "anything"));
        assert!(!matches("a*b", "a/b"));
    }

    fn manifest(extra: &str) -> Result<Manifest, String> {
        super::super::parse_manifest(&format!(
            "id = \"demo\"\nname = \"Demo\"\nversion = \"0.1.0\"\nsummary = \"x\"\napi = \"0.2\"\n{extra}"
        ))
    }

    #[test]
    fn the_tables_are_checked() {
        let good = manifest(
            r#"
[[views]]
id = "document"
title = "Document"
slot = "file"

[[files]]
match = ["*.tex"]
view = "document"
label = "LaTeX"

[[actions]]
match = ["*.md"]
label = "Export PDF"
action = "export-pdf"

[[settings]]
key = "engine"
type = "enum"
options = ["pdflatex", "xelatex"]
default = "pdflatex"
label = "Engine"
"#,
        )
        .unwrap();
        assert_eq!(file_kind(&good, "a/b.tex").unwrap().label, "LaTeX");
        assert!(file_kind(&good, "a/b.md").is_none());

        for (bad, why) in [
            (
                "[[files]]\nmatch = [\"*.tex\"]\nview = \"nope\"\nlabel = \"L\"",
                "no view",
            ),
            (
                "[[views]]\nid = \"t\"\ntitle = \"T\"\nslot = \"tab\"\n[[files]]\nmatch = [\"*.tex\"]\nview = \"t\"\nlabel = \"L\"",
                "not a file view",
            ),
            (
                "[[views]]\nid = \"a b\"\ntitle = \"T\"\nslot = \"tab\"",
                "view id",
            ),
            (
                "[[settings]]\nkey = \"n\"\ntype = \"number\"\ndefault = \"x\"\nlabel = \"N\"",
                "default",
            ),
            (
                "[[settings]]\nkey = \"e\"\ntype = \"enum\"\ndefault = \"c\"\noptions = [\"a\"]\nlabel = \"E\"",
                "default",
            ),
            (
                "[[actions]]\nmatch = [\"../*.md\"]\nlabel = \"X\"\naction = \"x\"",
                "relative glob",
            ),
        ] {
            let err = manifest(bad).err().unwrap_or_default();
            assert!(err.contains(why), "{bad}: {err}");
        }
        // 0.1 can't declare them.
        let err = super::super::parse_manifest(
            "id = \"demo\"\nname = \"Demo\"\nversion = \"0.1.0\"\nsummary = \"x\"\napi = \"0.1\"\n\
             [[views]]\nid = \"t\"\ntitle = \"T\"\nslot = \"tab\"",
        )
        .err()
        .unwrap();
        assert!(err.contains("0.2"), "{err}");
    }

    #[test]
    fn settings_check_their_values() {
        let m = manifest(
            "[[settings]]\nkey = \"n\"\ntype = \"number\"\ndefault = 3\nmin = 1\nmax = 5\nlabel = \"N\"\n\
             [[settings]]\nkey = \"p\"\ntype = \"path\"\ndefault = \"\"\nlabel = \"P\"",
        )
        .unwrap();
        let n = &m.settings[0];
        assert!(n.check(&json!(4)).is_ok());
        assert!(n.check(&json!(9)).is_err());
        assert!(n.check(&json!("4")).is_err());
        let p = &m.settings[1];
        assert!(p.check(&json!("main.tex")).is_ok());
        assert!(p.check(&json!("../x")).is_err());
        assert!(p.check(&json!("/etc")).is_err());
    }
}
