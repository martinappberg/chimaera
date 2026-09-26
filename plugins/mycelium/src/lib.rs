//! Mycelium: the Knowledge provider for workspaces that keep mycelium's
//! project memory (`.living/`, `todo/`, the `.mycelium` handoff), and the
//! agents' two read tools over it. Design: docs/plugin-system-plan.md
//! ("What moves where"); the format: `reader.rs`.
//!
//! Read-only: every file is read through the host (`HostFs`), which keeps
//! paths inside the workspace, refuses symlinks and caps each read. The
//! host asks `knowledge` with the stamp it holds and caches by stamp; this
//! instance keeps its own last read for the tools, which may be dropped at
//! any time (the next call re-reads).

use std::sync::Mutex;

use chimaera_plugin_api::serde_json::{self, Value};
use chimaera_plugin_api::{host, Context, Entry, Plugin, Snapshot, Stat, ToolDef, ToolResult};

mod fs;
mod reader;
mod tools;

use fs::Fs;
use reader::{Knowledge, Stamp};

/// The last snapshot this instance read, by stamp: a tool call re-stats
/// (a few host calls) and re-reads only when the stamp moved.
static LAST: Mutex<Option<(Stamp, Knowledge)>> = Mutex::new(None);

/// The workspace's files, through the host's bounded fs.
struct HostFs<'a>(&'a Context);

impl Fs for HostFs<'_> {
    fn read(&self, rel: &str, cap: u32) -> Result<Vec<u8>, String> {
        host::read(self.0, rel, cap)
    }

    fn stat(&self, rel: &str) -> Result<Stat, String> {
        host::stat(self.0, rel)
    }

    fn list(&self, rel: &str, cap: u32) -> Result<Vec<Entry>, String> {
        host::list(self.0, rel, cap)
    }
}

/// `f` over the current knowledge: planned now, read unless the last read
/// has the same stamp. `known` short-circuits: when it equals the current
/// stamp, nothing is read and `f` isn't called.
fn with_current<T>(
    fs: &impl Fs,
    known: Option<&Value>,
    f: impl FnOnce(&Stamp, &Knowledge) -> T,
) -> Option<T> {
    let plan = reader::plan(fs);
    let stamp = plan.stamp();
    if known.is_some_and(|known| serde_json::to_value(&stamp).ok().as_ref() == Some(known)) {
        return None;
    }
    let mut last = LAST.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if last.as_ref().is_none_or(|(held, _)| *held != stamp) {
        // Drop the old snapshot first: two at once would double the peak.
        *last = None;
        let knowledge = reader::read(fs, plan);
        *last = Some((stamp, knowledge));
    }
    let (stamp, knowledge) = last.as_ref().expect("filled above");
    Some(f(stamp, knowledge))
}

struct Mycelium;

impl Plugin for Mycelium {
    fn tools() -> Vec<ToolDef> {
        tools::defs()
    }

    fn instructions() -> Option<String> {
        Some(tools::INSTRUCTIONS.to_string())
    }

    fn call_tool(cx: Context, name: &str, args: Value) -> ToolResult {
        let ask = match tools::Ask::parse(name, &args) {
            Ok(ask) => ask,
            Err(refused) => return refused,
        };
        with_current(&HostFs(&cx), None, |_, knowledge| ask.answer(knowledge))
            .expect("no known stamp, so always answered")
    }

    fn knowledge(cx: Context, known: Option<Value>) -> Result<Option<Snapshot>, String> {
        with_current(&HostFs(&cx), known.as_ref(), |stamp, knowledge| {
            Ok(Snapshot {
                stamp: serde_json::to_string(stamp).map_err(|e| format!("stamp: {e}"))?,
                // Straight from the structs: their `Serialize` shape is the wire.
                data: serde_json::to_string(knowledge).map_err(|e| format!("snapshot: {e}"))?,
            })
        })
        .transpose()
    }
}

chimaera_plugin_api::export!(Mycelium);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::StdFs;
    use chimaera_plugin_api::serde_json::json;

    /// The one test on `LAST` (a process-wide static): the stamp short-cut
    /// and the re-read, through `std::fs` under the host's rules.
    #[test]
    fn the_current_snapshot_is_read_again_only_when_its_stamp_moves() {
        let root =
            std::env::temp_dir().join(format!("chimaera-mycelium-current-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".living")).unwrap();
        let decisions = root.join(".living/decisions.md");
        std::fs::write(&decisions, "### [2026-01-01] One\n**Decision**: a\n").unwrap();
        let fs = StdFs::new(&root);

        let (stamp, count) = with_current(&fs, None, |stamp, k| {
            (serde_json::to_value(stamp).unwrap(), k.decisions.len())
        })
        .unwrap();
        assert_eq!(count, 1);
        assert_eq!(stamp["files"][0][0], ".living/decisions.md");
        assert_eq!(stamp["files"][0][2], 37);
        assert_eq!(stamp["refused"], json!([]));
        // Handed its own stamp: unchanged, and nothing is read.
        assert!(with_current(&fs, Some(&stamp), |_, _| 0).is_none());

        std::fs::write(
            &decisions,
            "### [2026-01-01] One\n**Decision**: a\n\n### [2026-01-02] Two\n",
        )
        .unwrap();
        let count = with_current(&fs, Some(&stamp), |_, k| k.decisions.len());
        assert_eq!(count, Some(2));
        let _ = std::fs::remove_dir_all(&root);
    }
}
