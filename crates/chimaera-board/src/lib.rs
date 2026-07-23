//! Board — Chimaera's visual composition surface.
//!
//! A board is an ordinary `*.board` file anywhere in the workspace: a scene
//! graph of pages and objects in points, rendered by this crate to PNG or
//! JPEG, and eventually exported to native PowerPoint, PDF and SVG. `.board`
//! is JSON content under a branded extension; the legacy `*.board.json`
//! spelling still opens and renders, so existing figures keep working with no
//! migration.
//!
//! The crate is deliberately layered so the CLI, the daemon and the exporter
//! all drive the *same* functions — a second render path is how the pane and
//! the export quietly stop agreeing:
//!
//! - [`schema`] — the format itself. Lenient parsing, byte-stable writing.
//! - [`pretty`] — the canonical JSON layout.
//! - [`normalize`] — sugar expansion and the constraints that make ugly
//!   output unrepresentable, as a pure function.
//! - [`theme`] — palettes, the type scale with its per-role `minPt`, spacing.
//! - [`chart`] — marks over a plot-ready table; scales, ticks, no transforms.
//! - [`layout`] — text measurement and the slot geometry.
//! - [`render`] — scene graph to SVG to pixels.
//! - [`show`] — the one-shot "show me this" path.
//! - [`describe`] — what the agent reads back.
//! - [`lint`] — what refuses to export.

pub mod arrange;
pub mod chart;
/// The `chimaera board` CLI verbs (`cli` cargo feature — pulls clap). Mounted
/// by the standalone `chimaera` binary AND the native app binary, so the
/// daemon-written `chimaera` shim (an exec of `current_exe()`) resolves to a
/// working board CLI in both deployments.
#[cfg(feature = "cli")]
pub mod cli;
pub mod colormap;
pub mod composites;
pub mod cvd;
pub mod describe;
pub mod diagram;
pub mod equation;
pub mod export;
/// The bundled Tabler icon set (`icons` cargo feature — the manifest data is
/// gated, the `icon` object and its refusal are not). Searched by name, placed
/// recolorable, and exported as editable PPTX vector shapes.
pub mod icons;
pub mod imginfo;
pub mod journal;
pub mod layout;
pub mod lint;
pub mod merge;
pub mod normalize;
pub mod pdfimport;
pub mod presets;
pub mod pretty;
pub mod render;
pub mod schema;
pub mod show;
pub mod slots;
pub mod theme;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

pub use normalize::{normalize, Diagnostic, Severity};
pub use schema::{
    grid_cell, grid_lines, translate_object, Board, Canvas, Grid, Object, Page, FORMAT,
    FORMAT_VERSION,
};

/// The workspace-relative home for everything *around* a board.
///
/// Boards themselves live wherever they belong — `figures/fig2.board`
/// next to the manuscript — because files-as-truth means the figure travels
/// with the paper and git-diffability is a hard requirement. What lands here
/// is the managed surround: tracked themes, fonts and imported assets;
/// gitignored renders, exports and journals.
pub const BOARD_DIR: &str = ".chimaera/board";

/// Does this path name a board? Matched on the canonical `.board` suffix or
/// the legacy compound `.board.json` — both are boards (JSON content, either
/// extension). Matched on the whole suffix, not `Path::extension`, so a plain
/// `.json` is not a board and `.board.json` is recognized whole (extension
/// only ever sees the last dot segment).
pub fn is_board_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_ascii_lowercase())
        .is_some_and(|n| n.ends_with(".board") || n.ends_with(".board.json"))
}

/// A board file name without its extension: strips the legacy `.board.json`
/// (checked first — it is the longer suffix) or the canonical `.board`,
/// case-insensitively; a name that is neither is returned whole. The shared
/// stem for derived artifact names (renders, exports) and human labels, so
/// `deck.board` and the legacy `deck.board.json` both stem to `deck`.
pub fn board_stem(name: &str) -> &str {
    for suffix in [".board.json", ".board"] {
        if name.len() > suffix.len()
            && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
        {
            return &name[..name.len() - suffix.len()];
        }
    }
    name
}

/// Parse a board, leniently.
///
/// Unknown fields are preserved verbatim and unknown or malformed *objects*
/// become [`Object::Unknown`] rather than failing the parse, so a board
/// written by a newer daemon opens — and re-saves — without losing data. Only
/// JSON that is not JSON, or that is missing the structural spine, is an
/// error; the caller falls back to the plain text view with a repair banner,
/// which is genuinely useful because the file is human-readable JSON.
pub fn parse(src: &str) -> Result<Board> {
    let board: Board = serde_json::from_str(src).context("this is not a readable board")?;
    if board.format != FORMAT {
        bail!(
            "not a board: expected format {FORMAT:?}, found {:?}",
            board.format
        );
    }
    Ok(board)
}

/// Serialize a board in its canonical byte-stable form, with a trailing
/// newline. A semantically identical save is byte-identical.
pub fn to_string(board: &Board) -> Result<String> {
    let compact = serde_json::to_string(board).context("serializing the board")?;
    Ok(pretty::pretty(&compact))
}

/// Read and parse a board from disk.
pub fn load(path: &Path) -> Result<Board> {
    let src =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse(&src).with_context(|| format!("in {}", path.display()))
}

/// Write a board to disk in canonical form, creating parent directories.
/// Atomic — a board is the user's real, possibly uncommitted work, and a kill
/// mid-write must never truncate it.
pub fn save(path: &Path, board: &Board) -> Result<()> {
    write_atomic(path, to_string(board)?.as_bytes())
}

/// Hidden-temp-sibling + rename. Shared by board saves and render-cache
/// writes: the render cache is "correct by construction" only if a partial
/// write can never land at a content-addressed path — a truncated PNG there
/// would be served as a valid hit forever, because nothing ever invalidates.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("board-write");
    let tmp = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("moving into {}", path.display()))
}

/// Prune a render-cache directory: sweep entries another engine build
/// addressed — their keys embed [`render::RENDER_EPOCH`] + the crate version,
/// so no current request can ever hit them again — then cap what remains at
/// `cap` PNGs, evicting oldest-modified first. Renders are pure and
/// re-creatable, but they land in the *user's workspace* — often a
/// quota-capped NFS home — at one file per committed gesture, gitignored and
/// quickopen-hidden, so without this they are a slow invisible leak.
/// Best-effort: an unreadable entry is skipped, never fatal.
pub fn prune_renders(dir: &Path, cap: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let live_prefix = format!("{}-", render::engine_fingerprint());
    let mut pngs: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for e in entries.filter_map(|e| e.ok()) {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "png") {
            continue;
        }
        let current = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(&live_prefix));
        if !current {
            let _ = std::fs::remove_file(&path);
            // The diagnostics sidecar rides its PNG's lifetime.
            let _ = std::fs::remove_file(path.with_extension("json"));
            continue;
        }
        if let Some(modified) = e.metadata().ok().and_then(|m| m.modified().ok()) {
            pngs.push((modified, path));
        }
    }
    if pngs.len() <= cap {
        return;
    }
    pngs.sort_by_key(|(t, _)| *t);
    for (_, path) in pngs.iter().take(pngs.len() - cap) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("json"));
    }
}

/// The render-cache ceiling shared by the CLI and the daemon route.
pub const RENDER_CACHE_CAP: usize = 256;

/// Find the workspace root for `start` by walking up to a `.git` directory,
/// falling back to `start` itself. Board's managed directories hang off this.
pub fn workspace_root(start: &Path) -> PathBuf {
    let mut cur = if start.is_dir() {
        start
    } else {
        start.parent().unwrap_or(start)
    };
    loop {
        if cur.join(".git").exists() {
            return cur.to_path_buf();
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => break,
        }
    }
    if start.is_dir() {
        start.to_path_buf()
    } else {
        start.parent().unwrap_or(start).to_path_buf()
    }
}

/// `<workspace>/.chimaera/board`.
pub fn board_dir(workspace: &Path) -> PathBuf {
    workspace.join(BOARD_DIR)
}

/// Ensure `.chimaera/board/.gitignore` exists, listing the three generated
/// directories. Tracked, so the ignore rules travel with the repo.
pub fn ensure_board_dir(workspace: &Path) -> Result<PathBuf> {
    let dir = board_dir(workspace);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        std::fs::write(
            &ignore,
            "# Generated by chimaera board. Renders, exports and journals are\n\
             # reconstructible from the board, the theme and the fonts, so they\n\
             # stay on disk and out of git.\n\
             renders/\n\
             exports/\n\
             journal/\n\
             shown/\n",
        )
        .with_context(|| format!("writing {}", ignore.display()))?;
    }
    Ok(dir)
}

/// `<workspace>/.chimaera/board/shown`, with its own self-ignoring
/// `.gitignore`.
///
/// The surround's `.gitignore` is a *tracked* file, so a first-ever
/// `board show` in a fresh repo would create a tracked file as a side effect
/// of a throwaway — exactly the wear this path exists to avoid. A `.gitignore`
/// containing `*` ignores itself, so a throwaway never produces a `git status`
/// line, ever, even the first time.
pub fn ensure_shown_dir(workspace: &Path) -> Result<PathBuf> {
    let dir = board_dir(workspace).join("shown");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        std::fs::write(&ignore, "*\n").with_context(|| format!("writing {}", ignore.display()))?;
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"{
  "format": "chimaera.board",
  "formatVersion": 1,
  "title": "Parser rewrite — design review",
  "canvas": { "preset": "talk-16x9", "size": [960, 540] },
  "pages": [
    {
      "id": "cover",
      "objects": [
        {
          "id": "deck-title",
          "type": "text",
          "role": "title",
          "text": ["The parser rewrite is 3× faster"]
        }
      ]
    }
  ]
}
"#;

    #[test]
    fn parses_and_round_trips_byte_identically() {
        let board = parse(MINIMAL).unwrap();
        assert_eq!(board.pages.len(), 1);
        assert_eq!(board.pages[0].objects.len(), 1);
        let out = to_string(&board).unwrap();
        assert_eq!(out, MINIMAL, "canonical form must be a fixed point");
    }

    #[test]
    fn a_second_save_moves_no_bytes() {
        let once = to_string(&parse(MINIMAL).unwrap()).unwrap();
        let twice = to_string(&parse(&once).unwrap()).unwrap();
        assert_eq!(once, twice);
    }

    /// Chart `data.trace`/`data.inputs` are named fields with a pinned key
    /// order (after `note`, before the lenient extras) — a provenance-carrying
    /// board must be a canonical fixed point like any other.
    #[test]
    fn chart_provenance_fields_round_trip_byte_identically() {
        let src = r#"{
  "format": "chimaera.board",
  "formatVersion": 1,
  "canvas": { "size": [960, 540] },
  "pages": [
    {
      "id": "p1",
      "objects": [
        {
          "id": "lat",
          "type": "chart",
          "at": [80, 80],
          "size": [480, 320],
          "data": {
            "origin": "derived-by-agent",
            "values": [
              { "d": "Mon", "hi": 5, "lo": 1, "med": 3, "q1": 2, "q3": 4 }
            ],
            "trace": "five-number summary via numpy.percentile, seed 42",
            "inputs": ["results/latency.csv"]
          },
          "x": { "field": "d", "type": "nominal" },
          "y": { "field": "med", "type": "quantitative" },
          "marks": [
            { "mark": "box" }
          ]
        }
      ]
    }
  ]
}
"#;
        let board = parse(src).unwrap();
        let chart = board.pages[0].objects.iter().find_map(|o| match o {
            Object::Chart(c) => Some(c),
            _ => None,
        });
        let data = &chart.expect("a chart").data;
        assert!(data.trace.as_deref().unwrap().contains("seed 42"));
        assert_eq!(data.inputs.as_deref().unwrap().len(), 1);
        let out = to_string(&board).unwrap();
        assert_eq!(out, src, "canonical form must be a fixed point");
    }

    #[test]
    fn a_non_board_json_file_is_refused_by_name() {
        let err = parse(
            r#"{"format":"something-else","formatVersion":1,"canvas":{"size":[1,1]},"pages":[]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not a board"), "{err}");
    }

    #[test]
    fn prune_sweeps_other_epoch_renders_and_keeps_current_ones() {
        let dir = std::env::temp_dir().join(format!("chimaera-board-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fp = render::engine_fingerprint();
        let live = dir.join(format!("{fp}-aaaaaaaaaaaaaaaa.png"));
        // A pre-fingerprint bare-hash key, and an upgrade's foreign fingerprint.
        let bare = dir.join("bbbbbbbbbbbbbbbb.png");
        let foreign = dir.join("0.0.0r0-cccccccccccccccc.png");
        let foreign_sidecar = foreign.with_extension("json");
        for (p, bytes) in [
            (&live, &b"png"[..]),
            (&bare, &b"png"[..]),
            (&foreign, &b"png"[..]),
            (&foreign_sidecar, &b"{}"[..]),
        ] {
            std::fs::write(p, bytes).unwrap();
        }
        prune_renders(&dir, RENDER_CACHE_CAP);
        assert!(live.exists(), "a current-engine render survives under cap");
        assert!(!bare.exists(), "a pre-fingerprint render can never be hit");
        assert!(
            !foreign.exists(),
            "another engine's render can never be hit"
        );
        assert!(!foreign_sidecar.exists(), "its sidecar rides its lifetime");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn board_paths_need_the_full_suffix() {
        // Canonical `.board`.
        assert!(is_board_path(Path::new("talks/lab.board")));
        assert!(is_board_path(Path::new("A.BOARD")));
        // Legacy `.board.json` still opens.
        assert!(is_board_path(Path::new("talks/lab.board.json")));
        assert!(is_board_path(Path::new("A.BOARD.JSON")));
        assert!(!is_board_path(Path::new("package.json")));
        assert!(!is_board_path(Path::new("board.json.bak")));
        assert!(!is_board_path(Path::new("board.bak")));
        // A bare word is not a board — the suffix carries the leading dot.
        assert!(!is_board_path(Path::new("keyboard")));
    }

    #[test]
    fn board_stem_strips_either_extension() {
        assert_eq!(board_stem("deck.board"), "deck");
        assert_eq!(board_stem("deck.board.json"), "deck");
        assert_eq!(board_stem("Deck.BOARD.JSON"), "Deck");
        // Not a board name: returned whole.
        assert_eq!(board_stem("notes.txt"), "notes.txt");
        assert_eq!(board_stem(".board"), ".board");
    }
}
