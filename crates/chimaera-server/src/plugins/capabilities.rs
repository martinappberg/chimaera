//! What a plugin can do, derived from its manifest: the one list the card
//! shows, the trust prompt asks about, and the host enforces. Design:
//! docs/plugin-platform-plan.md §1 ("Capabilities").
//!
//! - **Atoms.** Each capability is one atom, a short JSON array such as
//!   `["access","files","read"]` or `["agent-tool","post_note"]`. A plugin's
//!   capabilities are the set of its atoms; "asks for more" is set
//!   difference, "covered" is subset.
//! - **The digest** is the SHA-256 of the sorted atoms under a version line.
//!   A kind of atom added later (screens, programs, downloads) changes no
//!   digest of a plugin that doesn't use it: absent means no atom.
//! - **`[access]`** names the three reads a 0.1 plugin could make without
//!   saying so. A 0.1 manifest without it is read as exactly what 0.1
//!   allowed (files, the Timeline with notes, sessions); from 0.2 on an
//!   absent key means none. `hostfns` enforces it on every call.
//! - **The tier** follows the atoms, never the author's word: a plugin that
//!   runs or downloads a program is privileged.

use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::{json, Value};

use super::Manifest;

/// Bumped only if the atom grammar itself changes meaning (never for a new
/// kind of atom).
const DIGEST_VERSION: &str = "chimaera-caps/1";

/// `[access] files`.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum FilesAccess {
    None,
    Read,
}

/// `[access] timeline`, ordered: `notes` includes `read`.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub(crate) enum TimelineAccess {
    None,
    Read,
    Notes,
}

/// `[access] sessions`.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SessionsAccess {
    None,
    Read,
}

/// `[access]` as written; a key left out takes the API version's default.
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccessDecl {
    #[serde(default)]
    files: Option<FilesAccess>,
    #[serde(default)]
    timeline: Option<TimelineAccess>,
    #[serde(default)]
    sessions: Option<SessionsAccess>,
}

/// What a plugin may read through the host, resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Access {
    pub(crate) files: FilesAccess,
    pub(crate) timeline: TimelineAccess,
    pub(crate) sessions: SessionsAccess,
}

impl Access {
    /// Nothing: an instance made before its manifest was known never reads.
    pub(crate) const NONE: Access = Access {
        files: FilesAccess::None,
        timeline: TimelineAccess::None,
        sessions: SessionsAccess::None,
    };

    /// What a manifest that says nothing may do: a 0.1 plugin keeps exactly
    /// what 0.1 let it do without saying (now shown on its card); a later
    /// API gets nothing it didn't declare.
    fn implied(api: &str) -> Access {
        if api == "0.1" {
            Access {
                files: FilesAccess::Read,
                timeline: TimelineAccess::Notes,
                sessions: SessionsAccess::Read,
            }
        } else {
            Access::NONE
        }
    }

    pub(crate) fn of(m: &Manifest) -> Access {
        let base = Access::implied(&m.api);
        Access {
            files: m.access.files.unwrap_or(base.files),
            timeline: m.access.timeline.unwrap_or(base.timeline),
            sessions: m.access.sessions.unwrap_or(base.sessions),
        }
    }
}

/// Sandboxed plugins cannot leave the WebAssembly sandbox except through
/// bounded host calls; privileged ones run or download programs, which no
/// sandbox bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tier {
    Sandboxed,
    Privileged,
}

impl Tier {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Tier::Sandboxed => "sandboxed",
            Tier::Privileged => "privileged",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Tier> {
        match s {
            "sandboxed" => Some(Tier::Sandboxed),
            "privileged" => Some(Tier::Privileged),
            _ => None,
        }
    }
}

/// The atom kinds that make a plugin privileged (P8 adds their atoms).
const PRIVILEGED_KINDS: &[&str] = &["program", "download"];

/// A plugin's capabilities: a set of atoms.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Caps {
    atoms: BTreeSet<String>,
}

fn atom(parts: &[&str]) -> String {
    json!(parts).to_string()
}

/// One line of the card's **Can** list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Line {
    pub(crate) text: String,
    /// A line only a privileged plugin has (programs, downloads).
    pub(crate) privileged: bool,
}

impl Caps {
    /// The capabilities `m` declares (and, for a 0.1 manifest, implies).
    pub(crate) fn of(m: &Manifest) -> Caps {
        let mut atoms = BTreeSet::new();
        let access = Access::of(m);
        if access.files == FilesAccess::Read {
            atoms.insert(atom(&["access", "files", "read"]));
        }
        if access.timeline >= TimelineAccess::Read {
            atoms.insert(atom(&["access", "timeline", "read"]));
        }
        if access.timeline >= TimelineAccess::Notes {
            atoms.insert(atom(&["access", "timeline", "notes"]));
        }
        if access.sessions == SessionsAccess::Read {
            atoms.insert(atom(&["access", "sessions", "read"]));
        }
        for tool in &m.provides.mcp_tools {
            atoms.insert(atom(&["agent-tool", tool]));
        }
        if m.provides.hears(super::EventKind::Hook) {
            atoms.insert(atom(&["hook-line"]));
        }
        if m.provides.knowledge.is_some() {
            atoms.insert(atom(&["knowledge"]));
        }
        if m.setup.is_some() {
            atoms.insert(atom(&["setup-prompt"]));
        }
        for (agent, req) in m
            .requires
            .agent_plugins
            .iter()
            .chain(m.recommends.agent_plugins.iter())
        {
            atoms.insert(atom(&["agent-plugin", agent, &req.id, &req.marketplace]));
        }
        Caps { atoms }
    }

    /// The capability digest: what the lock records the maintainers
    /// approved, what a trust record says the user approved, and how an
    /// update is compared.
    pub(crate) fn digest(&self) -> String {
        let mut text = String::from(DIGEST_VERSION);
        for a in &self.atoms {
            text.push('\n');
            text.push_str(a);
        }
        crate::fs::sha256_hex(text.as_bytes())
    }

    pub(crate) fn tier(&self) -> Tier {
        let privileged = self
            .atoms
            .iter()
            .any(|a| PRIVILEGED_KINDS.iter().any(|k| kind(a) == *k));
        if privileged {
            Tier::Privileged
        } else {
            Tier::Sandboxed
        }
    }

    /// Everything `other` can do, this can too.
    pub(crate) fn covers(&self, other: &Caps) -> bool {
        other.atoms.is_subset(&self.atoms)
    }

    /// What this asks for that `base` doesn't.
    pub(crate) fn beyond(&self, base: &Caps) -> Caps {
        Caps {
            atoms: self.atoms.difference(&base.atoms).cloned().collect(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.atoms.is_empty()
    }

    /// The atoms themselves (the trust prompt's wire carries them, so a
    /// client can tell two lists apart without re-deriving them).
    pub(crate) fn atoms(&self) -> impl Iterator<Item = &str> {
        self.atoms.iter().map(String::as_str)
    }

    /// The card's **Can** list, in words: the same text the trust prompt,
    /// the growth callout and `chimaera plugin add` show.
    pub(crate) fn lines(&self) -> Vec<Line> {
        let parsed: Vec<Vec<String>> = self
            .atoms
            .iter()
            .filter_map(|a| serde_json::from_str::<Vec<String>>(a).ok())
            .collect();
        let has = |parts: &[&str]| {
            parsed
                .iter()
                .any(|p| p.iter().map(String::as_str).eq(parts.iter().copied()))
        };
        let of_kind = |k: &'static str| of_kind(&parsed, k);
        let mut lines = Vec::new();
        let mut plain = |text: String| {
            lines.push(Line {
                text,
                privileged: false,
            })
        };
        if has(&["access", "files", "read"]) {
            plain("Reads files in this workspace".into());
        }
        match (
            has(&["access", "timeline", "read"]),
            has(&["access", "timeline", "notes"]),
        ) {
            (_, true) => plain("Reads the Timeline and posts notes to it".into()),
            (true, false) => plain("Reads the Timeline".into()),
            _ => {}
        }
        if has(&["access", "sessions", "read"]) {
            plain("Sees this workspace's sessions (their names and kinds)".into());
        }
        let tools: Vec<&str> = of_kind("agent-tool")
            .filter_map(|p| p.get(1).map(String::as_str))
            .collect();
        if !tools.is_empty() {
            let n = tools.len();
            plain(format!(
                "Gives agents {n} tool{}: {}",
                if n == 1 { "" } else { "s" },
                tools.join(" · ")
            ));
        }
        if has(&["hook-line"]) {
            plain(
                "Adds a short line to what claude sees when a session starts or you send a prompt"
                    .into(),
            );
        }
        if has(&["knowledge"]) {
            plain("Fills the Knowledge view".into());
        }
        for p in of_kind("agent-plugin") {
            if let [_, agent, id, marketplace] = p.as_slice() {
                plain(format!(
                    "Offers an agent-side plugin for {agent}: {id} from {marketplace} \
                     (installed only on your click; it then runs inside {agent})"
                ));
            }
        }
        if has(&["setup-prompt"]) {
            plain("Has a setup prompt it sends to an agent you choose, shown in full first".into());
        }
        // Atoms this daemon has no words for (a newer kind): said plainly
        // rather than left off the list.
        let known = [
            "access",
            "agent-tool",
            "hook-line",
            "knowledge",
            "agent-plugin",
            "setup-prompt",
        ];
        for p in &parsed {
            let k = p.first().map(String::as_str).unwrap_or("");
            if !known.contains(&k) {
                lines.push(Line {
                    text: format!(
                        "Something this chimaera can't describe yet: {}",
                        p.join(" ")
                    ),
                    privileged: PRIVILEGED_KINDS.contains(&k),
                });
            }
        }
        lines
    }

    /// The Can list on the wire: `[{text, privileged}]`.
    pub(crate) fn lines_json(&self) -> Value {
        json!(self
            .lines()
            .into_iter()
            .map(|l| json!({"text": l.text, "privileged": l.privileged}))
            .collect::<Vec<_>>())
    }
}

/// The parsed atoms of one kind.
fn of_kind<'a>(parsed: &'a [Vec<String>], k: &'a str) -> impl Iterator<Item = &'a Vec<String>> {
    parsed
        .iter()
        .filter(move |p| p.first().is_some_and(|x| x == k))
}

/// An atom's kind: its first element.
fn kind(atom: &str) -> &str {
    atom.strip_prefix("[\"")
        .and_then(|rest| rest.split('"').next())
        .unwrap_or("")
}

/// What `chimaera plugin caps <plugin.toml>` prints: a manifest's tier,
/// capability digest and Can list, as the lock and the card record them.
pub fn describe_manifest(text: &str) -> Result<Value, String> {
    let m = super::parse_manifest(text)?;
    Ok(json!({
        "id": m.id,
        "version": m.version,
        "tier": m.caps.tier().as_str(),
        "caps": m.caps.digest(),
        "can": m.caps.lines_json(),
        "atoms": m.caps.atoms().collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(extra: &str) -> Manifest {
        super::super::parse_manifest(&format!(
            "id = \"demo\"\nname = \"Demo\"\nversion = \"0.1.0\"\nsummary = \"x\"\n{extra}"
        ))
        .unwrap()
    }

    #[test]
    fn a_0_1_manifest_without_access_keeps_what_0_1_allowed() {
        let m = manifest("api = \"0.1\"\n");
        assert_eq!(
            Access::of(&m),
            Access {
                files: FilesAccess::Read,
                timeline: TimelineAccess::Notes,
                sessions: SessionsAccess::Read,
            }
        );
        let text: Vec<String> = m.caps.lines().into_iter().map(|l| l.text).collect();
        assert_eq!(
            text,
            [
                "Reads files in this workspace",
                "Reads the Timeline and posts notes to it",
                "Sees this workspace's sessions (their names and kinds)",
            ]
        );
        // A later API gets nothing it didn't declare.
        let later = manifest("api = \"0.2\"\n");
        assert_eq!(Access::of(&later), Access::NONE);
        assert!(later.caps.is_empty());
    }

    #[test]
    fn access_narrows_key_by_key() {
        let m = manifest("api = \"0.1\"\n[access]\ntimeline = \"none\"\n");
        let a = Access::of(&m);
        assert_eq!(a.timeline, TimelineAccess::None);
        assert_eq!(
            a.files,
            FilesAccess::Read,
            "a key left out keeps 0.1's default"
        );
        let m = manifest("api = \"0.1\"\n[access]\ntimeline = \"read\"\n");
        assert!(m
            .caps
            .covers(&manifest("api = \"0.1\"\n[access]\ntimeline = \"none\"\n").caps));
        assert!(
            !m.caps.covers(&manifest("api = \"0.1\"\n").caps),
            "notes is more than read"
        );
        assert!(super::super::parse_manifest(
            "id = \"d\"\nname = \"D\"\nversion = \"0.1.0\"\nsummary = \"x\"\napi = \"0.1\"\n\
             [access]\nnetwork = \"yes\"\n"
        )
        .is_err());
    }

    #[test]
    fn the_digest_is_stable_order_free_and_sees_every_change() {
        let a = manifest(
            "api = \"0.1\"\n[provides]\nmcp_tools = [\"b_tool\", \"a_tool\"]\nevents = [\"hook\"]\n",
        );
        let b = manifest(
            "api = \"0.1\"\n[provides]\nevents = [\"hook\"]\nmcp_tools = [\"a_tool\", \"b_tool\"]\n",
        );
        assert_eq!(a.caps.digest(), b.caps.digest(), "order doesn't matter");
        let c = manifest("api = \"0.1\"\n[provides]\nmcp_tools = [\"a_tool\", \"b_tool\"]\n");
        assert_ne!(
            a.caps.digest(),
            c.caps.digest(),
            "the hook line is a capability"
        );
        assert!(a.caps.covers(&c.caps) && !c.caps.covers(&a.caps));
        let grown: Vec<String> = a
            .caps
            .beyond(&c.caps)
            .lines()
            .into_iter()
            .map(|l| l.text)
            .collect();
        assert_eq!(
            grown,
            ["Adds a short line to what claude sees when a session starts or you send a prompt"]
        );
        // A pinned value: the digest is what the lock records, so its
        // definition must never drift silently.
        assert_eq!(
            manifest("api = \"0.2\"\n").caps.digest(),
            crate::fs::sha256_hex(DIGEST_VERSION.as_bytes())
        );
    }

    #[test]
    fn the_tier_follows_the_atoms() {
        let m = manifest("api = \"0.1\"\n");
        assert_eq!(m.caps.tier(), Tier::Sandboxed);
        let mut caps = m.caps.clone();
        caps.atoms.insert(atom(&["program", "latexmk"]));
        assert_eq!(caps.tier(), Tier::Privileged);
        assert_eq!(kind(&atom(&["program", "latexmk"])), "program");
        let line = caps.lines().pop().unwrap();
        assert!(line.privileged, "{line:?}");
    }

    #[test]
    fn indirect_paths_are_on_the_list() {
        let m = manifest(
            "api = \"0.1\"\n[setup]\nprompt = \"Set up.\"\n\
             [recommends.agent_plugins.claude]\nid = \"x@y\"\nmarketplace = \"acme/x\"\n",
        );
        let text: Vec<String> = m.caps.lines().into_iter().map(|l| l.text).collect();
        assert!(
            text.iter().any(|t| t.contains("x@y from acme/x")),
            "{text:?}"
        );
        assert!(text.iter().any(|t| t.contains("setup prompt")), "{text:?}");
    }
}
