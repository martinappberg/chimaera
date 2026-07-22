//! `chimaera board` — the CLI half of the bidirectional loop (`cli` feature).
//!
//! Everything here is a thin shell over this crate's engine functions; the
//! daemon routes wrap the same functions, which is what keeps the pane and the
//! CLI showing the same pixels. All verbs are synchronous and touch only the
//! filesystem — no daemon needs to be running.
//!
//! The module lives in the engine crate (not the `chimaera` binary) so BOTH
//! binaries that can end up behind the `chimaera` shim answer `board`: the
//! standalone `chimaera` CLI mounts [`BoardCmd`] as its `board` subcommand,
//! and the native app binary (whose GUI exe IS the daemon there, so
//! `current_exe()` resolves to it) dispatches `board` argv to [`run`] before
//! any Tauri init.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Subcommand;

use crate::export::pdf::export_pdf;
use crate::export::svg::{export_svg, SvgVariant};
use crate::layout::FontStack;
use crate::render::{render_page, RasterParams};
use crate::theme::Theme;

#[derive(Subcommand)]
pub enum BoardCmd {
    /// Show a result as a rendered card: spec on stdin (or --spec), a real
    /// one-page board under .chimaera/board/shown/, PNG beside it.
    Show {
        /// Read the spec from a file instead of stdin.
        #[arg(long)]
        spec: Option<PathBuf>,
        /// Treat the input as mermaid flowchart source instead of a JSON
        /// spec — `cat arch.mmd | chimaera board show --mermaid`.
        #[arg(long)]
        mermaid: bool,
        /// Title, overriding the spec's.
        #[arg(long)]
        title: Option<String>,
        /// A one-line caption under the body.
        #[arg(long)]
        note: Option<String>,
        /// Update handle: re-invoking with the same id overwrites the same
        /// card instead of minting forty of them across a sweep.
        #[arg(long)]
        id: Option<String>,
        /// Card geometry: default (720×450), wide, square, or tall.
        #[arg(long, default_value = "default")]
        preset: String,
        /// Explicit size WxH in points, overriding --preset.
        #[arg(long)]
        size: Option<String>,
        /// Theme id or path; defaults to talk-dark.
        #[arg(long)]
        theme: Option<String>,
        /// Write the PNG here instead of .chimaera/board/shown/.
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Print the normalized board JSON to stdout.
        #[arg(long)]
        emit_board: bool,
        /// Print nothing but errors.
        #[arg(long)]
        quiet: bool,
    },
    /// Create a blank board.
    New {
        /// Where to write it, e.g. talks/lab-meeting.board.json.
        path: PathBuf,
        #[arg(long)]
        title: Option<String>,
        /// Theme id recorded in the board.
        #[arg(long, default_value = "talk-dark")]
        theme: String,
    },
    /// Render a board's pages to PNG.
    Render {
        path: PathBuf,
        /// Page index (0-based); all pages when omitted.
        #[arg(long)]
        page: Option<usize>,
        /// Output file (single page) or directory (all pages). Defaults to
        /// .chimaera/board/renders/.
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Device scale.
        #[arg(long, default_value_t = 2.0)]
        scale: f64,
        #[arg(long)]
        theme: Option<String>,
    },
    /// Export a board: SVG (per page), PDF (the whole deck), PPTX.
    Export {
        path: PathBuf,
        /// Format: svg | svg-outlined | pdf | pptx. `svg` keeps real text
        /// (editable, needs the fonts); `svg-outlined` flattens glyphs to
        /// paths (renders identically without them).
        #[arg(long)]
        format: String,
        /// Page index (0-based) for the SVG variants; all pages when
        /// omitted. Does not apply to pdf/pptx, which take the whole deck.
        #[arg(long)]
        page: Option<usize>,
        /// Output file (single page) or directory (all SVG pages). Defaults
        /// to .chimaera/board/exports/<stem>.<ext>.
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Chart fidelity, pptx only: `grouped` (the default — editable
        /// vector shapes, safe in every consumer) or `native` (real c:chart
        /// parts with an embedded workbook; charts the native writer cannot
        /// express fall back per-chart with the reason in the fate line).
        /// Opt-in until the hand-verified PowerPoint "Edit Data" pass.
        #[arg(long, default_value = "grouped")]
        charts: String,
    },
    /// Import a figure (.svg/.png/.jpg → an `image` object, copied into
    /// .chimaera/board/assets/), a mermaid flowchart (.mmd → a `diagram`
    /// object, converted once with the source as provenance), or — in a
    /// build with the `pdf-import` feature — one page of a .pdf rasterized
    /// to a PNG asset, appended to a board.
    Import {
        /// The figure, mermaid, or PDF file, or `-` for mermaid on stdin.
        path: PathBuf,
        /// The board to append to; created with one page when it does not
        /// exist yet.
        #[arg(long)]
        to: PathBuf,
        /// Page id to append to; the first page when omitted.
        #[arg(long)]
        page: Option<String>,
        /// Object id; the file's stem when omitted.
        #[arg(long)]
        id: Option<String>,
        /// Recorded regenerate command for a figure import (overrides an
        /// `<!-- chimaera:regen ... -->` comment inside an SVG).
        #[arg(long)]
        regen: Option<String>,
        /// PDF only: the 1-based source page to rasterize (default 1).
        #[arg(long)]
        pdf_page: Option<usize>,
        /// PDF only: rasterization density (default 300, capped at 600).
        #[arg(long)]
        dpi: Option<f64>,
    },
    /// Adopt a shown card into the workspace: move it to
    /// boards/<id>.board.json (git-visible — that is the point), or append
    /// its pages to an existing board with --to.
    Adopt {
        /// A shown card id (resolves .chimaera/board/shown/<id>.board.json)
        /// or a path to a board file.
        shown_id_or_path: String,
        /// An existing board to append the adopted pages to, instead of
        /// moving the file.
        #[arg(long)]
        to: Option<PathBuf>,
    },
    /// Export a theme for external plotting code: the theme JSON as bundled,
    /// or a matplotlib style file mapping the theme's numbers.
    ThemeExport {
        /// Theme id or path (talk-dark, talk-light, figure-light, or a
        /// .theme.json path).
        theme_id: String,
        /// Format: json | mplstyle.
        #[arg(long)]
        format: String,
        /// Write here instead of stdout.
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Mechanically recolor an existing SVG onto a theme: its most frequent
    /// fill/stroke colors map to @bg, @body, and the categorical ramp. This
    /// is best-effort — prefer regenerating the figure on-theme
    /// (provenance.regen) when a script exists; rescheme is for what you
    /// cannot regenerate.
    Rescheme {
        /// The SVG file to recolor.
        path: PathBuf,
        /// Theme id or path.
        #[arg(long)]
        theme: String,
        /// Output SVG; defaults to <stem>-<theme>.svg beside the input.
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Print the agent-facing description: every object, its position, its
    /// content, in the same points the file uses.
    Describe { path: PathBuf },
    /// Print the board's semantic edit journal: what changed, by whom, one
    /// event per line — the cheap read of "what did the human just do".
    Journal {
        path: PathBuf,
        /// Only events with seq strictly greater than N.
        #[arg(long)]
        since: Option<u64>,
    },
    /// Check a board without rendering it.
    Lint {
        path: PathBuf,
        #[arg(long)]
        theme: Option<String>,
        /// Target preset to lint against (floors, export tiers, venue rules),
        /// e.g. talk-16x9 or pub-nature-single. Defaults to the board's
        /// canvas.target when set; plain legality lint otherwise.
        #[arg(long)]
        target: Option<String>,
        /// Append the style profile: measured near-miss findings (alignment,
        /// spacing, off-grid, overfull/underfull, margins, budgets) over
        /// resolved frames.
        #[arg(long)]
        style: bool,
        /// Escalate the measured style findings from warnings to errors.
        #[arg(long)]
        strict: bool,
        /// Repair the mechanically-unambiguous classes first (clamp
        /// off-canvas, raise sub-floor run overrides, snap near-miss edges,
        /// snap to the grid), save canonically, and print each fix.
        #[arg(long)]
        fix: bool,
    },
    /// Align, distribute, or grid a set of objects by id. Runs as the agent's
    /// hand: moves are journaled with actor `agent`.
    Arrange {
        path: PathBuf,
        /// One of: align-left | align-right | align-top | align-bottom |
        /// align-center-h | align-center-v | distribute-h | distribute-v |
        /// grid. Aligns snap to the FIRST id's edge; distributes equalize
        /// gaps between the spatial extremes.
        #[arg(long)]
        op: String,
        /// Comma-separated object ids, in order (grid fills row-major in
        /// this order).
        #[arg(long)]
        ids: String,
        /// Grid gap in points; defaults to the theme's gap.
        #[arg(long)]
        gap: Option<f64>,
        /// Grid column count; defaults to the squarest fit.
        #[arg(long)]
        cols: Option<usize>,
    },
    /// Validate a theme beyond WCAG contrast: the OKLCH lightness band, the
    /// chroma floor, and all-pairs CVD ΔE ≥ 8 under Machado 2009 over the
    /// categorical ramp — printing the computed safe series cap.
    ValidateTheme {
        /// Theme id or path (talk-dark, talk-light, figure-light, or a
        /// .theme.json path).
        theme_id: String,
    },
    /// Three-way merge two board histories per object id, git-merge-driver
    /// compatible: `merge.board.driver = chimaera board merge %O %A %B`. The
    /// result overwrites OURS; exits 0 on a clean merge, 1 with conflicts
    /// (the file still carries the ours-wins best effort — how a driver
    /// signals "look at my stderr report").
    Merge {
        /// The common ancestor (git's %O).
        base: PathBuf,
        /// Our side (git's %A) — rewritten with the merge result.
        ours: PathBuf,
        /// Their side (git's %B).
        theirs: PathBuf,
        /// Write the result here instead of overwriting OURS.
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Print the conflict report without writing anything.
        #[arg(long)]
        check: bool,
    },
}

pub fn run(cmd: BoardCmd) -> Result<()> {
    match cmd {
        BoardCmd::Show {
            spec,
            mermaid,
            title,
            note,
            id,
            preset,
            size,
            theme,
            out,
            emit_board,
            quiet,
        } => show(
            spec, mermaid, title, note, id, &preset, size, theme, out, emit_board, quiet,
        ),
        BoardCmd::New { path, title, theme } => new(&path, title, &theme),
        BoardCmd::Render {
            path,
            page,
            out,
            scale,
            theme,
        } => render(&path, page, out, scale, theme),
        BoardCmd::Export {
            path,
            format,
            page,
            out,
            charts,
        } => export(&path, &format, page, out, &charts),
        BoardCmd::Import {
            path,
            to,
            page,
            id,
            regen,
            pdf_page,
            dpi,
        } => import(&path, &to, page, id, regen, pdf_page, dpi),
        BoardCmd::Adopt {
            shown_id_or_path,
            to,
        } => adopt(&shown_id_or_path, to),
        BoardCmd::ThemeExport {
            theme_id,
            format,
            out,
        } => theme_export(&theme_id, &format, out),
        BoardCmd::Rescheme { path, theme, out } => rescheme(&path, &theme, out),
        BoardCmd::Describe { path } => {
            let board = load_normalized(&path)?.0;
            let summary = crate::journal::summary(&journal_path_for(&path));
            print!(
                "{}",
                crate::describe::describe_with_journal(&board, summary)
            );
            Ok(())
        }
        BoardCmd::Journal { path, since } => journal(&path, since),
        BoardCmd::Lint {
            path,
            theme,
            target,
            style,
            strict,
            fix,
        } => lint(&path, theme, target, style, strict, fix),
        BoardCmd::Arrange {
            path,
            op,
            ids,
            gap,
            cols,
        } => arrange(&path, &op, &ids, gap, cols),
        BoardCmd::ValidateTheme { theme_id } => validate_theme(&theme_id),
        BoardCmd::Merge {
            base,
            ours,
            theirs,
            out,
            check,
        } => merge(&base, &ours, &theirs, out, check),
    }
}

/// Load, normalize, and report — the shared front half of every verb.
fn load_normalized(path: &Path) -> Result<(crate::Board, Vec<crate::Diagnostic>)> {
    let mut board = crate::load(path)?;
    let diags = crate::normalize(&mut board);
    Ok((board, diags))
}

/// The journal key for a board named on the command line. Canonicalized
/// first — the journal key is derived from the workspace-relative path, and
/// the daemon canonicalizes too, so a relative CLI path must not mint a
/// second key for the same board.
fn journal_path_for(path: &Path) -> PathBuf {
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let ws = crate::workspace_root(&abs);
    crate::journal::journal_path(&ws, &abs)
}

fn journal(path: &Path, since: Option<u64>) -> Result<()> {
    if !crate::is_board_path(path) {
        bail!(
            "not a board: {} does not end in .board.json",
            path.display()
        );
    }
    let events = crate::journal::read_since(&journal_path_for(path), since.unwrap_or(0))?;
    if events.is_empty() {
        println!("no journal events");
        return Ok(());
    }
    for event in &events {
        println!("{}", event.render());
    }
    Ok(())
}

fn resolve_theme(reference: Option<&str>, board: &crate::Board, ws: &Path) -> Result<Theme> {
    let name = reference
        .map(String::from)
        .or_else(|| board.theme.clone())
        .unwrap_or_else(|| "talk-dark".to_string());
    Theme::resolve(&name, Some(ws))
}

#[allow(clippy::too_many_arguments)]
fn show(
    spec_path: Option<PathBuf>,
    mermaid: bool,
    title: Option<String>,
    note: Option<String>,
    id: Option<String>,
    preset: &str,
    size: Option<String>,
    theme_ref: Option<String>,
    out: Option<PathBuf>,
    emit_board: bool,
    quiet: bool,
) -> Result<()> {
    let raw = match &spec_path {
        Some(p) => {
            std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?
        }
        None => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .context("reading the spec from stdin")?;
            buf
        }
    };
    if raw.trim().is_empty() {
        bail!("no spec: pipe JSON on stdin or pass --spec FILE");
    }

    // --mermaid wraps the raw input as the spec's fourth body kind; the JSON
    // path stays exactly what it was.
    let mut spec: crate::show::ShowSpec = if mermaid {
        crate::show::ShowSpec {
            title: None,
            note: None,
            chart: None,
            table: None,
            text: None,
            mermaid: Some(raw.clone()),
        }
    } else {
        serde_json::from_str(&raw).context("parsing the show spec")?
    };
    if title.is_some() {
        spec.title = title;
    }
    if note.is_some() {
        spec.note = note;
    }

    let size = match size {
        Some(s) => parse_size(&s)?,
        None => crate::show::preset_size(preset)
            .with_context(|| format!("unknown preset {preset:?}; use default|wide|square|tall"))?,
    };

    let cwd = std::env::current_dir().context("resolving the working directory")?;
    let ws = crate::workspace_root(&cwd);
    let theme_id = theme_ref.unwrap_or_else(|| "talk-dark".to_string());
    let theme = Theme::resolve(&theme_id, Some(&ws))?;

    let board = crate::show::build_board(&spec, size, &theme_id)?;
    if emit_board {
        print!("{}", crate::to_string(&board)?);
    }

    let fonts = FontStack::for_workspace(&ws);
    let rendered = render_page(&board, 0, &theme, &fonts, RasterParams::default())?;

    // Content-derived id unless the caller supplied an update handle.
    let card_id = id.unwrap_or_else(|| crate::show::spec_id(&raw));
    let (board_path, png_path) = match out {
        Some(p) => {
            let board_path = p.with_extension("board.json");
            (board_path, p)
        }
        None => {
            let shown = crate::ensure_shown_dir(&ws)?;
            (
                shown.join(format!("{card_id}.board.json")),
                shown.join(format!("{card_id}.png")),
            )
        }
    };
    crate::save(&board_path, &board)?;
    crate::write_atomic(&png_path, &rendered.png)?;
    append_shown_event(&board_path);

    if !quiet {
        let rel = |p: &Path| {
            p.strip_prefix(&ws)
                .map(|r| r.display().to_string())
                .unwrap_or_else(|_| p.display().to_string())
        };
        println!(
            "{} → {}",
            crate::show::summary(&board, &theme_id),
            rel(&board_path)
        );
        for d in rendered
            .diagnostics
            .iter()
            .filter(|d| d.severity != crate::Severity::Info)
        {
            eprintln!("{}", d.render());
        }
    }
    Ok(())
}

/// Journal the show: a `shown` event, actor `agent` (show is the agent's
/// verb), on the written board's own journal — the surfacing signal a
/// ShownCard consumer keys on (board plan §10). Best-effort: the card is
/// already on disk and the one-line stdout is the contract, so a journal
/// failure warns rather than failing a show that succeeded.
fn append_shown_event(board_path: &Path) {
    use crate::journal::{Actor, Event, EventKind, Journal};
    let appended = Journal::open(&journal_path_for(board_path))
        .and_then(|mut journal| journal.append(Event::new(Actor::Agent, EventKind::Shown)));
    if let Err(err) = appended {
        eprintln!("note: shown journal append failed: {err:#}");
    }
}

fn parse_size(s: &str) -> Result<[f64; 2]> {
    let (w, h) = s
        .split_once(['x', 'X', '×'])
        .with_context(|| format!("--size wants WxH, got {s:?}"))?;
    Ok([
        w.trim()
            .parse()
            .with_context(|| format!("bad width {w:?}"))?,
        h.trim()
            .parse()
            .with_context(|| format!("bad height {h:?}"))?,
    ])
}

fn new(path: &Path, title: Option<String>, theme: &str) -> Result<()> {
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    if !crate::is_board_path(path) {
        bail!(
            "a board path ends in .board.json — try {}",
            path.with_extension("board.json").display()
        );
    }
    let title = title.unwrap_or_else(|| {
        path.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.trim_end_matches(".board.json").replace(['-', '_'], " "))
            .unwrap_or_else(|| "Untitled".to_string())
    });
    let mut board = crate::Board::new(title, crate::Canvas::default());
    board.theme = Some(theme.to_string());
    crate::save(path, &board)?;
    // The workspace surround comes into being with the first board.
    let cwd = std::env::current_dir()?;
    crate::ensure_board_dir(&crate::workspace_root(&cwd))?;
    println!("created {}", path.display());
    Ok(())
}

fn render(
    path: &Path,
    page: Option<usize>,
    out: Option<PathBuf>,
    scale: f64,
    theme_ref: Option<String>,
) -> Result<()> {
    let (board, diags) = load_normalized(path)?;
    let ws = crate::workspace_root(path);
    let theme = resolve_theme(theme_ref.as_deref(), &board, &ws)?;
    let fonts = FontStack::for_workspace(&ws);
    let params = RasterParams {
        scale,
        workspace: Some(ws.clone()),
    };

    for d in diags.iter().filter(|d| d.severity != crate::Severity::Info) {
        eprintln!("{}", d.render());
    }

    let pages: Vec<usize> = match page {
        Some(p) => vec![p],
        None => (0..board.pages.len()).collect(),
    };
    let single = pages.len() == 1;
    let canonical = crate::to_string(&board)?;

    for p in pages {
        let rendered = render_page(&board, p, &theme, &fonts, params.clone())?;
        let dest = match (&out, single) {
            (Some(o), true) => o.clone(),
            (Some(o), false) => o.join(format!("{}.png", board.pages[p].id)),
            (None, _) => {
                let dir = crate::ensure_board_dir(&ws)?.join("renders");
                std::fs::create_dir_all(&dir)?;
                let key = crate::render::render_key(&canonical, &theme, p, params.clone());
                dir.join(format!("{key}.png"))
            }
        };
        crate::write_atomic(&dest, &rendered.png)?;
        // The default destination is the shared content-addressed cache; an
        // explicit -o is the user's own path and never pruned.
        if out.is_none() {
            if let Some(dir) = dest.parent() {
                crate::prune_renders(dir, crate::RENDER_CACHE_CAP);
            }
        }
        println!(
            "page {} ({}) → {} · {}×{}",
            p + 1,
            board.pages[p].id,
            dest.display(),
            rendered.width,
            rendered.height
        );
        for d in rendered
            .diagnostics
            .iter()
            .filter(|d| d.severity != crate::Severity::Info)
        {
            eprintln!("{}", d.render());
        }
    }
    Ok(())
}

/// Export a board. SVG is per page (mirroring `render`); PDF — and PPTX,
/// when its writer lands — take the whole deck as one document.
fn export(
    path: &Path,
    format: &str,
    page: Option<usize>,
    out: Option<PathBuf>,
    charts: &str,
) -> Result<()> {
    // --charts is a pptx knob; validate up front so a typo is loud and a
    // stray `--charts native` on an SVG export never silently no-ops.
    let chart_fidelity = match charts {
        "grouped" => crate::export::ChartFidelity::Grouped,
        "native" => crate::export::ChartFidelity::Native,
        other => bail!("unknown --charts {other:?}: use grouped | native"),
    };
    if chart_fidelity != crate::export::ChartFidelity::Grouped && format != "pptx" {
        bail!("--charts applies to pptx only");
    }
    let (board, diags) = load_normalized(path)?;
    for d in diags.iter().filter(|d| d.severity != crate::Severity::Info) {
        eprintln!("{}", d.render());
    }
    let ws = crate::workspace_root(path);
    let theme = resolve_theme(None, &board, &ws)?;
    let fonts = FontStack::for_workspace(&ws);

    // Default destination: .chimaera/board/exports/<stem>.<ext>. The writes
    // are atomic, and write_atomic creates the directory itself.
    let stem = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.trim_end_matches(".board.json").to_string())
        .unwrap_or_else(|| "board".to_string());
    let exports_dir = || -> Result<PathBuf> { Ok(crate::ensure_board_dir(&ws)?.join("exports")) };

    match format {
        "svg" | "svg-outlined" => {
            // Two files that must not clobber each other: the outlined
            // variant carries its name.
            let (variant, base) = if format == "svg" {
                (SvgVariant::Text, stem.clone())
            } else {
                (SvgVariant::Outlined, format!("{stem}-outlined"))
            };
            let pages: Vec<usize> = match page {
                Some(p) => vec![p],
                None => (0..board.pages.len()).collect(),
            };
            let single = pages.len() == 1;
            let count = pages.len();
            for p in pages {
                let svg = export_svg(&board, p, &theme, &fonts, Some(&ws), variant)?;
                let dest = match (&out, single) {
                    (Some(o), true) => o.clone(),
                    (Some(o), false) => o.join(format!("{base}-{}.svg", board.pages[p].id)),
                    (None, true) => exports_dir()?.join(format!("{base}.svg")),
                    (None, false) => {
                        exports_dir()?.join(format!("{base}-{}.svg", board.pages[p].id))
                    }
                };
                crate::write_atomic(&dest, svg.as_bytes())?;
                println!(
                    "page {} ({}) → {}",
                    p + 1,
                    board.pages[p].id,
                    dest.display()
                );
            }
            println!("{count} page{} exported", if count == 1 { "" } else { "s" });
        }
        "pdf" => {
            if page.is_some() {
                bail!("--page does not apply to pdf: the whole deck exports as one document");
            }
            let pdf = export_pdf(&board, &theme, &fonts, Some(&ws))?;
            let dest = match out {
                Some(o) => o,
                None => exports_dir()?.join(format!("{stem}.pdf")),
            };
            crate::write_atomic(&dest, &pdf)?;
            let n = board.pages.len();
            println!(
                "{n} page{} → {}",
                if n == 1 { "" } else { "s" },
                dest.display()
            );
        }
        "pptx" => {
            if page.is_some() {
                bail!("--page does not apply to pptx: the whole deck exports as one file");
            }
            let mut bytes = Vec::new();
            let opts = crate::export::PptxOptions { chart_fidelity };
            let report = crate::export::write_pptx_with(
                &board,
                &theme,
                &fonts,
                Some(&ws),
                &opts,
                &mut bytes,
            )?;
            let dest = match out {
                Some(o) => o,
                None => exports_dir()?.join(format!("{stem}.pptx")),
            };
            crate::write_atomic(&dest, &bytes)?;
            // The degradation contract, stated per object rather than
            // discovered after the deck is opened.
            for fate in &report.objects {
                println!(
                    "  {}: {} — {}",
                    fate.id,
                    format!("{:?}", fate.tier).to_lowercase(),
                    fate.reason
                );
            }
            let n = board.pages.len();
            println!(
                "{n} page{} → {}",
                if n == 1 { "" } else { "s" },
                dest.display()
            );
        }
        other => bail!("unknown format {other:?}: use svg | svg-outlined | pdf | pptx"),
    }
    Ok(())
}

/// Parse mermaid and append the resulting `diagram` object to a board,
/// creating a one-page board when `to` does not exist. The object gets the
/// canvas minus margins; the human drags it from there.
fn import_mermaid(path: &Path, to: &Path, page: Option<String>, id: Option<String>) -> Result<()> {
    let src = if path == Path::new("-") {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("reading mermaid from stdin")?;
        buf
    } else {
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?
    };
    if src.trim().is_empty() {
        bail!("no mermaid source: pass a file, or `-` with the source on stdin");
    }
    let (mut diagram, notes) = crate::diagram::from_mermaid_with_notes(&src)?;
    for n in &notes {
        eprintln!("note: {n}");
    }
    diagram.id = id.unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| *s != "-")
            .map(String::from)
            .unwrap_or_else(|| "diagram".to_string())
    });

    let mut board = load_or_create_board(to)?;

    // The diagram fills the canvas minus a margin; layout inside is computed
    // at render, so this box is all the geometry an import needs.
    let m = 48.0;
    diagram.at = Some([m, m]);
    diagram.size = Some([
        (board.canvas.width() - m * 2.0).max(200.0),
        (board.canvas.height() - m * 2.0).max(160.0),
    ]);

    let page_index = resolve_page_index(&mut board, page.as_deref(), to)?;
    // Ids are the merge and journal key — refuse rather than auto-rename.
    if board.objects().any(|(_, o)| o.id() == diagram.id) {
        bail!(
            "id {:?} already exists in {}; pass --id",
            diagram.id,
            to.display()
        );
    }

    let (obj_id, n_nodes, n_edges) = (diagram.id.clone(), diagram.nodes.len(), diagram.edges.len());
    board.pages[page_index]
        .objects
        .push(crate::Object::Diagram(diagram));
    let diags = crate::normalize(&mut board);
    let mut errors = 0;
    for d in diags.iter().filter(|d| d.severity != crate::Severity::Info) {
        eprintln!("{}", d.render());
        if d.severity == crate::Severity::Error {
            errors += 1;
        }
    }
    if errors > 0 {
        bail!("{errors} error(s); nothing written");
    }
    crate::save(to, &board)?;
    println!(
        "imported {obj_id:?} · {n_nodes} nodes · {n_edges} edges → {} (page {})",
        to.display(),
        board.pages[page_index].id
    );
    Ok(())
}

/// Load `to`, or start a fresh one-page board when it does not exist yet —
/// the shared front half of every import.
fn load_or_create_board(to: &Path) -> Result<crate::Board> {
    if !crate::is_board_path(to) {
        bail!(
            "a board path ends in .board.json — try {}",
            to.with_extension("board.json").display()
        );
    }
    if to.exists() {
        crate::load(to)
    } else {
        let title = to
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.trim_end_matches(".board.json").replace(['-', '_'], " "))
            .unwrap_or_else(|| "Untitled".to_string());
        Ok(crate::Board::new(title, crate::Canvas::default()))
    }
}

fn resolve_page_index(board: &mut crate::Board, page: Option<&str>, to: &Path) -> Result<usize> {
    match page {
        Some(pid) => board
            .pages
            .iter()
            .position(|p| p.id == pid)
            .with_context(|| format!("no page {pid:?} in {}", to.display())),
        None => {
            if board.pages.is_empty() {
                board.pages.push(crate::Page::new("page-1"));
            }
            Ok(0)
        }
    }
}

/// Dispatch an import by what the file actually is: figures by extension or
/// magic bytes, PDFs by `.pdf` or the `%PDF-` header, mermaid by `.mmd` or a
/// flowchart/graph header. Stdin stays mermaid — piping pixels through a
/// terminal helps nobody.
fn import(
    path: &Path,
    to: &Path,
    page: Option<String>,
    id: Option<String>,
    regen: Option<String>,
    pdf_page: Option<usize>,
    dpi: Option<f64>,
) -> Result<()> {
    // The PDF flags mean nothing to the other importers; say so out loud
    // rather than silently dropping an explicit request.
    let pdf_only_note = || {
        if pdf_page.is_some() || dpi.is_some() {
            eprintln!("note: --pdf-page/--dpi apply only to PDF imports; ignored");
        }
    };
    if path == Path::new("-") {
        pdf_only_note();
        return import_mermaid(path, to, page, id);
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("svg" | "png" | "jpg" | "jpeg") => {
            pdf_only_note();
            import_figure(path, to, page, id, regen)
        }
        Some("pdf") => import_pdf(path, to, page, id, regen, pdf_page, dpi),
        Some("mmd") => {
            pdf_only_note();
            import_mermaid(path, to, page, id)
        }
        _ => {
            let bytes =
                std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            if crate::pdfimport::sniff_pdf(&bytes) {
                import_pdf(path, to, page, id, regen, pdf_page, dpi)
            } else if looks_like_mermaid(&bytes) {
                pdf_only_note();
                import_mermaid(path, to, page, id)
            } else if crate::imginfo::sniff_image(&bytes, &path.to_string_lossy())
                != crate::imginfo::ImgKind::Unknown
            {
                pdf_only_note();
                import_figure(path, to, page, id, regen)
            } else {
                bail!(
                    "cannot tell what {} is: not .mmd/.svg/.png/.jpg/.pdf, not mermaid source \
                     (flowchart/graph), and not a recognizable image",
                    path.display()
                )
            }
        }
    }
}

fn looks_like_mermaid(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("%%"))
        .is_some_and(|l| l.starts_with("flowchart") || l.starts_with("graph"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut s = String::with_capacity(64);
    for b in digest {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// `<!-- chimaera:regen COMMAND -->` inside an SVG records how to regenerate
/// it; the import lifts it into `provenance.regen`.
fn sniff_regen_comment(text: &str) -> Option<String> {
    const OPEN: &str = "<!-- chimaera:regen";
    let start = text.find(OPEN)?;
    let rest = &text[start + OPEN.len()..];
    let end = rest.find("-->")?;
    let cmd = rest[..end].trim();
    (!cmd.is_empty()).then(|| cmd.to_string())
}

/// Copy a figure into `.chimaera/board/assets/` and append it as an `image`
/// object with sniffed pixel size and provenance.
fn import_figure(
    path: &Path,
    to: &Path,
    page: Option<String>,
    id: Option<String>,
    regen: Option<String>,
) -> Result<()> {
    use crate::imginfo::ImgKind;

    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("{} has no usable file name", path.display()))?;
    let kind = crate::imginfo::sniff_image(&bytes, name);
    if kind == ImgKind::Unknown {
        bail!("{} is not a recognizable PNG, JPEG, or SVG", path.display());
    }
    if !crate::is_board_path(to) {
        bail!(
            "a board path ends in .board.json — try {}",
            to.with_extension("board.json").display()
        );
    }
    let cwd = std::env::current_dir().context("resolving the working directory")?;
    let to_abs = if to.is_absolute() {
        to.to_path_buf()
    } else {
        cwd.join(to)
    };
    let ws = crate::workspace_root(&to_abs);
    crate::ensure_board_dir(&ws)?;
    let assets = crate::board_dir(&ws).join("assets");
    std::fs::create_dir_all(&assets).with_context(|| format!("creating {}", assets.display()))?;

    // Land the asset: an identical name+bytes is reused, a name collision
    // with different bytes gets a content-hash suffix — nothing is ever
    // overwritten.
    let digest = sha256_hex(&bytes);
    let mut asset_name = name.to_string();
    let first = assets.join(&asset_name);
    if first.exists() && std::fs::read(&first).map(|b| b != bytes).unwrap_or(true) {
        let (stem, ext) = match name.rsplit_once('.') {
            Some((s, e)) => (s.to_string(), format!(".{e}")),
            None => (name.to_string(), String::new()),
        };
        asset_name = format!("{stem}-{}{ext}", &digest[..8]);
    }
    let dest = assets.join(&asset_name);
    if !dest.exists() {
        crate::write_atomic(&dest, &bytes)?;
    }
    let src_rel = format!("{}/assets/{asset_name}", crate::BOARD_DIR);

    // Natural size: sniffed pixels for rasters, document units for SVG; both
    // place at 96 dpi (unit × 0.75 pt), the convention the renderer shares.
    let (pixel_size, natural_pt) = match kind {
        ImgKind::Png | ImgKind::Jpeg => {
            let (w, h) = crate::imginfo::raster_dimensions(kind, &bytes)
                .with_context(|| format!("could not read the pixel size of {}", path.display()))?;
            (
                Some([w as f64, h as f64]),
                Some([w as f64 * 0.75, h as f64 * 0.75]),
            )
        }
        ImgKind::Svg => {
            let text = std::str::from_utf8(&bytes)
                .with_context(|| format!("{} is not valid UTF-8", path.display()))?;
            let fonts = crate::layout::FontStack::for_workspace(&ws);
            match crate::imginfo::sanitize_svg(text, fonts.db(), "probe-") {
                Ok(san) => (None, Some([san.width * 0.75, san.height * 0.75])),
                Err(e) => {
                    eprintln!("note: svg did not parse ({e}); imported, but it will render as a placeholder");
                    (None, None)
                }
            }
        }
        ImgKind::Unknown => unreachable!("refused above"),
    };

    // The flag wins; an SVG's own regen comment is the fallback.
    let regen = regen.or_else(|| {
        (kind == ImgKind::Svg)
            .then(|| {
                std::str::from_utf8(&bytes)
                    .ok()
                    .and_then(sniff_regen_comment)
            })
            .flatten()
    });

    let mut board = load_or_create_board(to)?;
    // Fit into the canvas minus a margin, preserving aspect, never upscaling.
    let m = 48.0;
    let bw = (board.canvas.width() - m * 2.0).max(200.0);
    let bh = (board.canvas.height() - m * 2.0).max(160.0);
    let size = match natural_pt {
        Some([nw, nh]) if nw > 0.0 && nh > 0.0 => {
            let scale = (bw / nw).min(bh / nh).min(1.0);
            [nw * scale, nh * scale]
        }
        _ => [bw, bh],
    };

    let obj_id = id.unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(String::from)
            .unwrap_or_else(|| "figure".to_string())
    });
    // Ids are the merge and journal key — refuse rather than auto-rename.
    if board.objects().any(|(_, o)| o.id() == obj_id) {
        bail!(
            "id {obj_id:?} already exists in {}; pass --id",
            to.display()
        );
    }
    let page_index = resolve_page_index(&mut board, page.as_deref(), to)?;

    let image = crate::schema::ImageObject {
        id: obj_id.clone(),
        kind: crate::schema::ImageKind,
        src: src_rel.clone(),
        slot: None,
        at: Some([m, m]),
        size: Some(size),
        src_rect: None,
        provenance: Some(crate::schema::Provenance {
            script: None,
            regen,
            sha256: Some(digest),
            extra: Default::default(),
        }),
        pixel_size,
        tint: None,
        anchor: None,
        alt: None,
        link: None,
        rotation: None,
        extra: Default::default(),
    };
    board.pages[page_index]
        .objects
        .push(crate::Object::Image(image));
    let diags = crate::normalize(&mut board);
    let mut errors = 0;
    for d in diags.iter().filter(|d| d.severity != crate::Severity::Info) {
        eprintln!("{}", d.render());
        if d.severity == crate::Severity::Error {
            errors += 1;
        }
    }
    if errors > 0 {
        bail!("{errors} error(s); nothing written");
    }
    crate::save(to, &board)?;
    let px_note = pixel_size
        .map(|[w, h]| format!(" · {w}×{h} px"))
        .unwrap_or_default();
    println!(
        "imported image {obj_id:?}{px_note} → {} (page {}) · asset {src_rel}",
        to.display(),
        board.pages[page_index].id
    );
    Ok(())
}

/// Rasterize one page of a PDF (feature `pdf-import`) into a PNG asset and
/// append it as an `image` object — the same landing and insertion flow as
/// `import_figure`. Provenance anchors on the *source* PDF: `sha256` is the
/// PDF's digest (regenerating it flags the panel stale) and `source` records
/// `path#page=N`. Compiled in every build; without the feature the board
/// crate's stub returns the one clear refusal.
fn import_pdf(
    path: &Path,
    to: &Path,
    page: Option<String>,
    id: Option<String>,
    regen: Option<String>,
    pdf_page: Option<usize>,
    dpi: Option<f64>,
) -> Result<()> {
    use crate::pdfimport;

    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let src_digest = sha256_hex(&bytes);
    let pdf_page = pdf_page.unwrap_or(1);
    let dpi_req = dpi.unwrap_or(300.0);
    if dpi_req > pdfimport::MAX_DPI {
        eprintln!("note: --dpi capped at {:.0}", pdfimport::MAX_DPI);
    }
    let raster = pdfimport::rasterize_pdf_page(bytes, pdf_page, dpi_req)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if !crate::is_board_path(to) {
        bail!(
            "a board path ends in .board.json — try {}",
            to.with_extension("board.json").display()
        );
    }
    let cwd = std::env::current_dir().context("resolving the working directory")?;
    let to_abs = if to.is_absolute() {
        to.to_path_buf()
    } else {
        cwd.join(to)
    };
    let ws = crate::workspace_root(&to_abs);
    crate::ensure_board_dir(&ws)?;
    let assets = crate::board_dir(&ws).join("assets");
    std::fs::create_dir_all(&assets).with_context(|| format!("creating {}", assets.display()))?;

    // Land the PNG under the figure convention: stem + source page names it,
    // a name collision with different bytes gets a content-hash suffix —
    // nothing is ever overwritten.
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("figure");
    let mut asset_name = format!("{stem}-p{pdf_page}.png");
    let first = assets.join(&asset_name);
    if first.exists()
        && std::fs::read(&first)
            .map(|b| b != raster.png)
            .unwrap_or(true)
    {
        let png_digest = sha256_hex(&raster.png);
        asset_name = format!("{stem}-p{pdf_page}-{}.png", &png_digest[..8]);
    }
    let dest = assets.join(&asset_name);
    if !dest.exists() {
        crate::write_atomic(&dest, &raster.png)?;
    }
    let src_rel = format!("{}/assets/{asset_name}", crate::BOARD_DIR);

    let mut board = load_or_create_board(to)?;
    // Fit into the canvas minus a margin, preserving aspect, never upscaling
    // — the natural size is the PDF page's own point size.
    let m = 48.0;
    let bw = (board.canvas.width() - m * 2.0).max(200.0);
    let bh = (board.canvas.height() - m * 2.0).max(160.0);
    let [nw, nh] = raster.point_size;
    let size = if nw > 0.0 && nh > 0.0 {
        let scale = (bw / nw).min(bh / nh).min(1.0);
        [nw * scale, nh * scale]
    } else {
        [bw, bh]
    };

    let obj_id = id.unwrap_or_else(|| {
        // Page 1 keeps the bare stem like every figure; later pages carry
        // the page so two panels from one PDF don't collide by default.
        if pdf_page == 1 {
            stem.to_string()
        } else {
            format!("{stem}-p{pdf_page}")
        }
    });
    // Ids are the merge and journal key — refuse rather than auto-rename.
    if board.objects().any(|(_, o)| o.id() == obj_id) {
        bail!(
            "id {obj_id:?} already exists in {}; pass --id",
            to.display()
        );
    }
    let page_index = resolve_page_index(&mut board, page.as_deref(), to)?;

    let mut extra = crate::schema::Extra::default();
    extra.insert(
        "source".to_string(),
        serde_json::Value::String(format!("{}#page={pdf_page}", path.display())),
    );
    let [px_w, px_h] = raster.pixel_size;
    let image = crate::schema::ImageObject {
        id: obj_id.clone(),
        kind: crate::schema::ImageKind,
        src: src_rel.clone(),
        slot: None,
        at: Some([m, m]),
        size: Some(size),
        src_rect: None,
        provenance: Some(crate::schema::Provenance {
            script: None,
            regen,
            sha256: Some(src_digest),
            extra,
        }),
        pixel_size: Some([px_w as f64, px_h as f64]),
        tint: None,
        anchor: None,
        alt: None,
        link: None,
        rotation: None,
        extra: Default::default(),
    };
    board.pages[page_index]
        .objects
        .push(crate::Object::Image(image));
    let diags = crate::normalize(&mut board);
    let mut errors = 0;
    for d in diags.iter().filter(|d| d.severity != crate::Severity::Info) {
        eprintln!("{}", d.render());
        if d.severity == crate::Severity::Error {
            errors += 1;
        }
    }
    if errors > 0 {
        bail!("{errors} error(s); nothing written");
    }
    crate::save(to, &board)?;
    println!(
        "imported image {obj_id:?} · {px_w}×{px_h} px · pdf page {pdf_page}/{} → {} (page {}) · asset {src_rel}",
        raster.page_count,
        to.display(),
        board.pages[page_index].id
    );
    Ok(())
}

/// `board adopt` — a shown throwaway becomes real work. Without --to the file
/// moves to boards/<id>.board.json at the workspace root, which is what makes
/// it git-visible; with --to its pages append to an existing board, re-id'ing
/// collisions with a numeric suffix and saying so.
fn adopt(shown: &str, to: Option<PathBuf>) -> Result<()> {
    let cwd = std::env::current_dir().context("resolving the working directory")?;
    let ws = crate::workspace_root(&cwd);
    let direct = PathBuf::from(shown);
    let shown_candidate = crate::board_dir(&ws)
        .join("shown")
        .join(format!("{shown}.board.json"));
    let src = if direct.exists() && crate::is_board_path(&direct) {
        direct
    } else if shown_candidate.exists() {
        shown_candidate.clone()
    } else {
        bail!(
            "no board named {shown:?}: tried {} and {}",
            direct.display(),
            shown_candidate.display()
        );
    };
    let src_board = crate::load(&src)?;

    match to {
        Some(target) => {
            if !crate::is_board_path(&target) {
                bail!(
                    "a board path ends in .board.json — got {}",
                    target.display()
                );
            }
            if !target.exists() {
                bail!(
                    "--to {} does not exist; omit --to to adopt as a new board",
                    target.display()
                );
            }
            let mut board = crate::load(&target)?;
            let (adopted, notes) = merge_pages(&mut board, src_board);
            let diags = crate::normalize(&mut board);
            let mut errors = 0;
            for d in diags.iter().filter(|d| d.severity != crate::Severity::Info) {
                eprintln!("{}", d.render());
                if d.severity == crate::Severity::Error {
                    errors += 1;
                }
            }
            if errors > 0 {
                bail!("{errors} error(s); nothing written");
            }
            crate::save(&target, &board)?;
            for n in &notes {
                println!("note: {n}");
            }
            // The semantic trace: one page-added per adopted page. The human
            // asked for the adoption, so the human is the actor.
            let mut journal = crate::journal::Journal::open(&journal_path_for(&target))?;
            for page in &adopted {
                journal.append(crate::journal::Event::new(
                    crate::journal::Actor::Human,
                    crate::journal::EventKind::PageAdded { page: page.clone() },
                ))?;
            }
            println!(
                "adopted {} page{} from {} into {} ({})",
                adopted.len(),
                if adopted.len() == 1 { "" } else { "s" },
                src.display(),
                target.display(),
                adopted.join(", ")
            );
        }
        None => {
            let stem = src
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.trim_end_matches(".board.json").to_string())
                .unwrap_or_else(|| "adopted".to_string());
            let dest = ws.join("boards").join(format!("{stem}.board.json"));
            if dest.exists() {
                bail!(
                    "{} already exists; adopt into it with --to, or pick another home by hand",
                    dest.display()
                );
            }
            let bytes =
                std::fs::read(&src).with_context(|| format!("reading {}", src.display()))?;
            crate::write_atomic(&dest, &bytes)?;
            std::fs::remove_file(&src)
                .with_context(|| format!("removing the shown copy {}", src.display()))?;
            let mut journal = crate::journal::Journal::open(&journal_path_for(&dest))?;
            for page in &src_board.pages {
                journal.append(crate::journal::Event::new(
                    crate::journal::Actor::Human,
                    crate::journal::EventKind::PageAdded {
                        page: page.id.clone(),
                    },
                ))?;
            }
            println!(
                "adopted {} → {} (git-visible now)",
                src.display(),
                dest.display()
            );
        }
    }
    Ok(())
}

/// Append `src`'s pages to `target`, re-id'ing page and object collisions
/// with a numeric suffix. Returns the adopted page ids (post-rename) and the
/// human-readable notes for every rename.
fn merge_pages(target: &mut crate::Board, src: crate::Board) -> (Vec<String>, Vec<String>) {
    use std::collections::{BTreeMap, BTreeSet};

    fn free_id(id: &str, taken: &BTreeSet<String>) -> String {
        if !taken.contains(id) {
            return id.to_string();
        }
        let mut n = 2usize;
        loop {
            let candidate = format!("{id}-{n}");
            if !taken.contains(&candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    fn collect_renames(
        objects: &[crate::Object],
        taken: &mut BTreeSet<String>,
        renames: &mut BTreeMap<String, String>,
    ) {
        for o in objects {
            let id = o.id().to_string();
            if !id.is_empty() {
                // An Unknown object's id lives inside preserved raw JSON, so
                // it cannot be renamed — it only occupies its id.
                if matches!(o, crate::Object::Unknown(_)) {
                    taken.insert(id);
                } else {
                    let new = free_id(&id, taken);
                    if new != id {
                        renames.insert(id, new.clone());
                    }
                    taken.insert(new);
                }
            }
            if let crate::Object::Group(g) = o {
                collect_renames(&g.objects, taken, renames);
            }
        }
    }

    fn fix_endpoint(ep: &mut crate::schema::EndPoint, renames: &BTreeMap<String, String>) {
        if let Some(obj) = &mut ep.object {
            if let Some(n) = renames.get(obj) {
                *obj = n.clone();
            }
        }
    }

    fn fix_anchor(anchor: &mut Option<crate::schema::Anchor>, renames: &BTreeMap<String, String>) {
        if let Some(a) = anchor {
            if let Some(obj) = &mut a.object {
                if let Some(n) = renames.get(obj) {
                    *obj = n.clone();
                }
            }
        }
    }

    fn apply_renames(objects: &mut [crate::Object], renames: &BTreeMap<String, String>) {
        use crate::Object as O;
        let fix = |s: &mut String| {
            if let Some(n) = renames.get(s) {
                *s = n.clone();
            }
        };
        for o in objects {
            match o {
                O::Text(x) => {
                    fix(&mut x.id);
                    fix_anchor(&mut x.anchor, renames);
                }
                O::Shape(x) => {
                    fix(&mut x.id);
                    fix_anchor(&mut x.anchor, renames);
                }
                O::Connector(x) => {
                    fix(&mut x.id);
                    fix_endpoint(&mut x.from, renames);
                    fix_endpoint(&mut x.to, renames);
                }
                O::Image(x) => {
                    fix(&mut x.id);
                    fix_anchor(&mut x.anchor, renames);
                }
                O::Group(x) => {
                    fix(&mut x.id);
                    apply_renames(&mut x.objects, renames);
                }
                O::Chart(x) => {
                    fix(&mut x.id);
                    fix_anchor(&mut x.anchor, renames);
                }
                O::Diagram(x) => {
                    fix(&mut x.id);
                    fix_anchor(&mut x.anchor, renames);
                }
                O::PanelLabel(x) => {
                    fix(&mut x.id);
                    fix_anchor(&mut x.anchor, renames);
                }
                O::Scalebar(x) => fix(&mut x.id),
                O::SigBracket(x) => {
                    fix(&mut x.id);
                    fix_endpoint(&mut x.from, renames);
                    fix_endpoint(&mut x.to, renames);
                }
                O::Legend(x) => fix(&mut x.id),
                O::Colorbar(x) => fix(&mut x.id),
                O::Table(x) => fix(&mut x.id),
                O::Equation(x) => fix(&mut x.id),
                O::Callout(x) => {
                    fix(&mut x.id);
                    if let Some(t) = &mut x.tail {
                        fix_endpoint(t, renames);
                    }
                }
                O::Inset(x) => {
                    fix(&mut x.id);
                    if let Some(n) = renames.get(&x.of.object) {
                        x.of.object = n.clone();
                    }
                }
                O::Unknown(_) => {}
            }
        }
    }

    let mut page_ids: BTreeSet<String> = target.pages.iter().map(|p| p.id.clone()).collect();
    let mut obj_ids: BTreeSet<String> = target.objects().map(|(_, o)| o.id().to_string()).collect();
    let mut adopted = Vec::new();
    let mut notes = Vec::new();
    for mut page in src.pages {
        let new_id = free_id(&page.id, &page_ids);
        if new_id != page.id {
            notes.push(format!(
                "page {:?} became {new_id:?} (id collision)",
                page.id
            ));
            page.id = new_id.clone();
        }
        page_ids.insert(new_id.clone());
        let mut renames = BTreeMap::new();
        collect_renames(&page.objects, &mut obj_ids, &mut renames);
        for (old, new) in &renames {
            notes.push(format!("object {old:?} became {new:?} (id collision)"));
        }
        if !renames.is_empty() {
            apply_renames(&mut page.objects, &renames);
        }
        adopted.push(new_id);
        target.pages.push(page);
    }
    (adopted, notes)
}

/// `board theme-export` — the theme's numbers for external plotting code.
fn theme_export(theme_id: &str, format: &str, out: Option<PathBuf>) -> Result<()> {
    let cwd = std::env::current_dir().context("resolving the working directory")?;
    let ws = crate::workspace_root(&cwd);
    let theme = Theme::resolve(theme_id, Some(&ws))?;
    let body = match format {
        // The bundled ids export their exact source bytes; anything else is
        // the parsed theme, pretty-printed deterministically (BTreeMaps).
        "json" => match theme_id {
            "talk-dark" => crate::theme::TALK_DARK.to_string(),
            "talk-light" => crate::theme::TALK_LIGHT.to_string(),
            "figure-light" => crate::theme::FIGURE_LIGHT.to_string(),
            _ => {
                let mut s =
                    serde_json::to_string_pretty(&theme).context("serializing the theme")?;
                s.push('\n');
                s
            }
        },
        "mplstyle" => mplstyle_for(&theme),
        other => bail!("unknown format {other:?}: use json | mplstyle"),
    };
    match out {
        Some(p) => {
            crate::write_atomic(&p, body.as_bytes())?;
            println!("{} ({format}) → {}", theme.id, p.display());
        }
        None => print!("{body}"),
    }
    Ok(())
}

/// The theme as a matplotlib style file. Hex goes without `#` — a `#` starts
/// a comment in matplotlibrc syntax — and the ordering is fixed, so the same
/// theme always exports the same bytes.
fn mplstyle_for(theme: &Theme) -> String {
    use std::fmt::Write as _;
    let hex = |rgb: crate::theme::Rgb| rgb.hex()[1..].to_string();
    let bg = hex(theme.bg());
    let body_c = hex(theme.color_or_fg(Some("@body")));
    let muted = theme
        .color("@muted")
        .map(hex)
        .unwrap_or_else(|| body_c.clone());
    let grid = theme
        .color(&theme.chart.grid)
        .map(hex)
        .unwrap_or_else(|| muted.clone());
    let body_role = theme.body();
    let label = theme.role("label").unwrap_or(body_role);
    let ramp: Vec<String> = theme
        .chart
        .categorical
        .iter()
        .filter_map(|c| theme.color(c))
        .map(|c| format!("'{}'", hex(c)))
        .collect();

    let mut s = String::new();
    let _ = writeln!(
        s,
        "# chimaera board theme {:?} as a matplotlib style",
        theme.id
    );
    let _ = writeln!(
        s,
        "# regenerate: chimaera board theme-export {} --format mplstyle",
        theme.id
    );
    let _ = writeln!(s, "figure.facecolor: {bg}");
    let _ = writeln!(s, "axes.facecolor: {bg}");
    let _ = writeln!(s, "text.color: {body_c}");
    let _ = writeln!(s, "axes.labelcolor: {body_c}");
    let _ = writeln!(s, "xtick.color: {muted}");
    let _ = writeln!(s, "ytick.color: {muted}");
    let _ = writeln!(s, "axes.prop_cycle: cycler('color', [{}])", ramp.join(", "));
    let _ = writeln!(s, "font.family: {}", body_role.family.join(", "));
    let _ = writeln!(s, "font.size: {}", label.size);
    let _ = writeln!(s, "axes.spines.top: False");
    let _ = writeln!(s, "axes.spines.right: False");
    let _ = writeln!(s, "grid.color: {grid}");
    let _ = writeln!(s, "grid.alpha: 1.0");
    let _ = writeln!(s, "lines.linewidth: {}", theme.chart.series_width);
    s
}

/// One row of the rescheme mapping table.
struct ReschemeRow {
    from: String,
    count: usize,
    target: String,
    to: Option<String>,
}

/// One color literal found in the SVG text: its byte span and parsed value.
struct ColorHit {
    start: usize,
    end: usize,
    rgb: crate::theme::Rgb,
}

/// Find every color literal in `fill`/`stroke`/`stop-color` positions — both
/// the attribute form (`fill="#..."`) and the style-property form
/// (`style="fill:#...;"`). `url(...)`, `none` and `currentColor` are not
/// colors and fall out of the parse.
fn scan_svg_colors(src: &str) -> Vec<ColorHit> {
    let mut hits = Vec::new();
    for (needle, quoted) in [
        ("fill=\"", true),
        ("stroke=\"", true),
        ("stop-color=\"", true),
        ("fill:", false),
        ("stroke:", false),
        ("stop-color:", false),
    ] {
        let mut at = 0usize;
        while let Some(p) = src[at..].find(needle) {
            let vstart = at + p + needle.len();
            let rest = &src[vstart..];
            let vlen = if quoted {
                rest.find('"').unwrap_or(rest.len())
            } else {
                rest.find([';', '"', '\'']).unwrap_or(rest.len())
            };
            let raw = &rest[..vlen];
            let value = raw.trim();
            let lead = raw.len() - raw.trim_start().len();
            if let Some(rgb) = parse_css_color(value) {
                hits.push(ColorHit {
                    start: vstart + lead,
                    end: vstart + lead + value.len(),
                    rgb,
                });
            }
            at = vstart + vlen;
        }
    }
    hits.sort_by_key(|h| h.start);
    hits
}

fn parse_css_color(v: &str) -> Option<crate::theme::Rgb> {
    if let Some(rgb) = crate::theme::parse_hex(v) {
        return Some(rgb);
    }
    let named = match v.to_ascii_lowercase().as_str() {
        "black" => "#000000",
        "white" => "#ffffff",
        "red" => "#ff0000",
        "green" => "#008000",
        "lime" => "#00ff00",
        "blue" => "#0000ff",
        "yellow" => "#ffff00",
        "orange" => "#ffa500",
        "purple" => "#800080",
        "gray" | "grey" => "#808080",
        "silver" => "#c0c0c0",
        "maroon" => "#800000",
        "navy" => "#000080",
        "teal" => "#008080",
        _ => return None,
    };
    crate::theme::parse_hex(named)
}

/// The mechanical recolor: map the SVG's own colors onto the theme.
/// Best-effort by design — luminance extremes (near-white/near-black) are
/// ground and text, and when both extremes appear the more frequent one is
/// the ground; remaining saturated colors take the categorical ramp in
/// frequency order; unsaturated leftovers are kept and say so.
fn rescheme_svg(src: &str, theme: &Theme) -> (String, Vec<ReschemeRow>) {
    let hits = scan_svg_colors(src);

    // Frequency in first-seen order, so ties break deterministically.
    let mut order: Vec<(crate::theme::Rgb, usize)> = Vec::new();
    for h in &hits {
        match order.iter_mut().find(|(c, _)| *c == h.rgb) {
            Some((_, n)) => *n += 1,
            None => order.push((h.rgb, 1)),
        }
    }
    let mut ranked: Vec<(usize, crate::theme::Rgb, usize)> = order
        .iter()
        .enumerate()
        .map(|(i, (c, n))| (i, *c, *n))
        .collect();
    ranked.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));

    let saturated =
        |c: &crate::theme::Rgb| (c.r.max(c.g).max(c.b) as i32 - c.r.min(c.g).min(c.b) as i32) >= 25;

    enum Target {
        Bg,
        Body,
        Cat(usize),
        Kept,
    }

    let mut decisions: Vec<(crate::theme::Rgb, usize, Target)> = Vec::new();
    let mut extremes = Vec::new();
    let mut mids = Vec::new();
    for (_, c, n) in ranked {
        let lum = c.luminance();
        if lum >= 0.8 || lum <= 0.05 {
            extremes.push((c, n));
        } else {
            mids.push((c, n));
        }
    }
    // The most frequent extreme is the ground; any other extreme is text on
    // that ground (a white page with black labels, or the inverse).
    let mut body_taken = false;
    for (i, (c, n)) in extremes.into_iter().enumerate() {
        if i == 0 {
            decisions.push((c, n, Target::Bg));
        } else {
            decisions.push((c, n, Target::Body));
            body_taken = true;
        }
    }
    // Otherwise the darkest unsaturated mid is the text.
    if !body_taken {
        if let Some(idx) = mids
            .iter()
            .enumerate()
            .filter(|(_, (c, _))| !saturated(c))
            .min_by(|a, b| a.1 .0.luminance().total_cmp(&b.1 .0.luminance()))
            .map(|(i, _)| i)
        {
            let (c, n) = mids.remove(idx);
            decisions.push((c, n, Target::Body));
        }
    }
    // Remaining saturated colors take the ramp, in frequency order.
    let mut cat = 0usize;
    for (c, n) in mids {
        if saturated(&c) {
            decisions.push((c, n, Target::Cat(cat)));
            cat += 1;
        } else {
            decisions.push((c, n, Target::Kept));
        }
    }

    let mut rows = Vec::new();
    let mut map: Vec<(crate::theme::Rgb, crate::theme::Rgb)> = Vec::new();
    for (c, n, t) in &decisions {
        let (label, to) = match t {
            Target::Bg => ("@bg".to_string(), theme.color("@bg")),
            Target::Body => (
                "@body".to_string(),
                theme.color("@body").or_else(|| theme.color("@fg")),
            ),
            Target::Cat(i) => {
                let refs = &theme.chart.categorical;
                if refs.is_empty() {
                    ("kept".to_string(), None)
                } else {
                    (refs[i % refs.len()].clone(), Some(theme.categorical(*i)))
                }
            }
            Target::Kept => ("kept".to_string(), None),
        };
        if let Some(to) = to {
            map.push((*c, to));
        }
        rows.push(ReschemeRow {
            from: c.hex(),
            count: *n,
            target: label,
            to: to.map(|r| r.hex()),
        });
    }

    let mut out = String::with_capacity(src.len());
    let mut pos = 0usize;
    for h in &hits {
        if h.start < pos {
            continue;
        }
        if let Some((_, to)) = map.iter().find(|(from, _)| *from == h.rgb) {
            out.push_str(&src[pos..h.start]);
            out.push_str(&to.hex());
            pos = h.end;
        }
    }
    out.push_str(&src[pos..]);
    (out, rows)
}

/// `board rescheme` — path 2 from the plan: recolor what you cannot
/// regenerate.
fn rescheme(path: &Path, theme_ref: &str, out: Option<PathBuf>) -> Result<()> {
    let src =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let ws = crate::workspace_root(path);
    let theme = Theme::resolve(theme_ref, Some(&ws))?;
    let (recolored, rows) = rescheme_svg(&src, &theme);
    if rows.is_empty() {
        bail!(
            "no fill/stroke colors found in {}; nothing to rescheme",
            path.display()
        );
    }
    for r in &rows {
        match &r.to {
            Some(hex) => println!("{} ×{} → {} {hex}", r.from, r.count, r.target),
            None => println!("{} ×{} → kept (unmapped)", r.from, r.count),
        }
    }
    let dest = out.unwrap_or_else(|| {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("figure");
        path.with_file_name(format!("{stem}-{}.svg", theme.id))
    });
    crate::write_atomic(&dest, recolored.as_bytes())?;
    println!("→ {}", dest.display());
    Ok(())
}

fn lint(
    path: &Path,
    theme_ref: Option<String>,
    target: Option<String>,
    style: bool,
    strict: bool,
    fix: bool,
) -> Result<()> {
    let ws = crate::workspace_root(path);

    // --fix runs first, on the file exactly as it stands — normalizing before
    // the fix would pre-snap the grid and hide the very classes it repairs.
    // The repaired board saves through the canonical writer; an untouched
    // board is not re-saved (no mtime churn for a no-op).
    if fix {
        let mut board = crate::load(path)?;
        let theme = resolve_theme(theme_ref.as_deref(), &board, &ws)?;
        let fixes = crate::lint::lint_fix(&mut board, &theme);
        if fixes.is_empty() {
            println!("nothing to fix");
        } else {
            crate::save(path, &board)?;
            for f in &fixes {
                println!("fixed: {f}");
            }
        }
    }

    let (board, mut diags) = load_normalized(path)?;
    let theme = resolve_theme(theme_ref.as_deref(), &board, &ws)?;

    // --target wins; the board's own canvas.target is the default. Neither
    // set → the plain legality profile, exactly as before.
    let target_id = target.or_else(|| board.canvas.target.clone());
    // Both the target and style profiles measure text, so both need the font
    // stack; the plain legality profile stays scan-free.
    let fonts = (style || target_id.is_some()).then(|| FontStack::for_workspace(&ws));
    match target_id.as_deref() {
        None => diags.extend(crate::lint::lint(&board, &theme)),
        Some(id) => {
            let preset = crate::presets::get(id).with_context(|| {
                format!(
                    "unknown target {id:?}; presets are {}",
                    crate::presets::ids().join(", ")
                )
            })?;
            let fonts = fonts.as_ref().expect("built for any target");
            diags.extend(crate::lint::lint_target(&board, &theme, preset, fonts));
            // The census: every top-level object's computed export fate, so
            // degradation is stated before an export, never discovered after.
            let (mut native, mut grouped, mut vector, mut raster) = (0u32, 0u32, 0u32, 0u32);
            for page in &board.pages {
                for obj in &page.objects {
                    match crate::presets::tier_of(obj).0 {
                        crate::export::ExportTier::Native => native += 1,
                        crate::export::ExportTier::Grouped => grouped += 1,
                        crate::export::ExportTier::Vector => vector += 1,
                        crate::export::ExportTier::Raster => raster += 1,
                    }
                }
            }
            println!(
                "tier census: {native} native · {grouped} grouped · {vector} vector · \
                 {raster} raster"
            );
        }
    }

    if style {
        diags.extend(crate::lint::lint_style(
            &board,
            &theme,
            fonts.as_ref().expect("built for --style"),
            strict,
        ));
    }

    if diags.is_empty() {
        let n = board.pages.len();
        println!("clean · {n} page{}", if n == 1 { "" } else { "s" });
        return Ok(());
    }
    let mut errors = 0;
    for d in &diags {
        println!("{}", d.render());
        if d.severity == crate::Severity::Error {
            errors += 1;
        }
    }
    if errors > 0 {
        bail!("{errors} error(s)");
    }
    Ok(())
}

/// Apply an [`crate::arrange`] op and save through the same
/// pipeline a pane gesture takes: mutate → normalize (grid snap) → canonical
/// save → journal. Run from the CLI this is the *agent's* hand, so the moves
/// journal with actor `agent` — the mirror of the daemon edit route's
/// actor-`human` appends.
fn arrange(
    path: &Path,
    op: &str,
    ids_csv: &str,
    gap: Option<f64>,
    cols: Option<usize>,
) -> Result<()> {
    let ids: Vec<&str> = ids_csv
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if ids.is_empty() {
        bail!("--ids wants a comma-separated list of object ids");
    }
    let mut board = crate::load(path)?;
    let ws = crate::workspace_root(path);
    let theme = resolve_theme(None, &board, &ws)?;

    // Prior geometry, for the journal's from/to.
    let prior: Vec<(String, Option<crate::schema::Frame>)> = ids
        .iter()
        .map(|id| {
            let f = board
                .objects()
                .find(|(_, o)| o.id() == *id)
                .and_then(|(_, o)| o.frame());
            (id.to_string(), f)
        })
        .collect();

    let moves =
        crate::arrange::arrange(&mut board, op, &ids, gap.or(Some(theme.spacing.gap)), cols)?;
    if moves.is_empty() {
        println!("nothing to move — already arranged");
        return Ok(());
    }
    crate::normalize(&mut board);
    crate::save(path, &board)?;
    for m in &moves {
        println!("{m}");
    }

    // The journal narrates the *saved* (post-normalize) geometry. Best-effort
    // by design, like the daemon's edit route: the board file is truth and
    // the journal is the audit trail.
    use crate::journal::{Actor, Event, EventKind};
    let mut events = Vec::new();
    for (id, before) in prior {
        let after = board
            .objects()
            .find(|(_, o)| o.id() == id)
            .and_then(|(_, o)| o.frame());
        if let (Some(b), Some(a)) = (before, after) {
            if (b.x, b.y) != (a.x, a.y) {
                events.push(Event::new(
                    Actor::Agent,
                    EventKind::Move {
                        object: id,
                        from: [b.x, b.y],
                        to: [a.x, a.y],
                    },
                ));
            }
        }
    }
    if !events.is_empty() {
        let appended = crate::ensure_board_dir(&ws)
            .and_then(|_| crate::journal::Journal::open(&journal_path_for(path)))
            .and_then(|mut journal| journal.append_batch(events));
        if let Err(err) = appended {
            eprintln!("warning: journal append failed: {err:#}");
        }
    }
    Ok(())
}

/// `board validate-theme`: the §9 preflight — WCAG contrast, the OKLCH
/// lightness band, the chroma floor, and all-pairs CVD under Machado 2009 —
/// plus the computed safe series cap the chart lint holds series against.
fn validate_theme(theme_id: &str) -> Result<()> {
    let cwd = std::env::current_dir().context("resolving the working directory")?;
    let ws = crate::workspace_root(&cwd);
    let theme = Theme::resolve(theme_id, Some(&ws))?;

    let ramp: Vec<crate::theme::Rgb> = theme
        .chart
        .categorical
        .iter()
        .filter_map(|r| theme.color(r))
        .collect();
    let cap = crate::cvd::safe_series_cap(&ramp);
    println!(
        "{}: safe series cap {cap} of {} ramp colors (all-pairs ΔE ≥ 8 under Machado 2009)",
        theme.id,
        ramp.len()
    );

    let findings = crate::cvd::validate_theme(&theme);
    if findings.is_empty() {
        println!("clean");
        return Ok(());
    }
    for f in &findings {
        println!("{f}");
    }
    bail!("{} finding(s)", findings.len())
}

/// `board merge` — the per-object three-way merge, shaped as a git merge
/// driver: base/ours/theirs are %O/%A/%B, the result overwrites OURS, and a
/// non-zero exit means "conflicts — the file carries my ours-wins best
/// effort" (the `bail!` after a successful write is deliberate: `main`
/// exits 1 on `Err`, which is the driver convention for a conflicted merge).
///
/// Deliberately NO journal append: a merge driver runs from bare/index
/// contexts — rebases, cherry-picks, CI — with no live session behind it,
/// and minting journal events there would fabricate interactive history for
/// a board nobody touched.
fn merge(base: &Path, ours: &Path, theirs: &Path, out: Option<PathBuf>, check: bool) -> Result<()> {
    let read = |p: &Path| -> Result<String> {
        std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))
    };
    let outcome = crate::merge::merge(&read(base)?, &read(ours)?, &read(theirs)?)?;
    // The report goes to stderr — git shows a driver's stderr to the user,
    // and stdout stays clean for scripting.
    for c in &outcome.conflicts {
        eprintln!("{}", c.render());
    }
    if check {
        if outcome.conflicts.is_empty() {
            println!("clean merge");
            return Ok(());
        }
        bail!("{} conflict(s)", outcome.conflicts.len());
    }
    let dest = out.unwrap_or_else(|| ours.to_path_buf());
    crate::save(&dest, &outcome.board)?;
    if outcome.conflicts.is_empty() {
        println!("merged → {}", dest.display());
        return Ok(());
    }
    bail!(
        "{} conflict(s); {} carries the resolutions above",
        outcome.conflicts.len(),
        dest.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board_json(json: &str) -> crate::Board {
        crate::parse(json).unwrap()
    }

    #[test]
    fn adopt_merge_re_ids_collisions_and_updates_references() {
        let mut target = board_json(
            r#"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[960,540]},
                "pages":[{"id":"page-1","objects":[
                  {"id":"title","type":"text","at":[8,8],"size":[400,80],"text":["existing"]}]}]}"#,
        );
        // The shown card collides on both the page id and an object id, and
        // carries a connector bound to the colliding object.
        let src = board_json(
            r#"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[960,540]},
                "pages":[{"id":"page-1","objects":[
                  {"id":"title","type":"text","at":[8,8],"size":[400,80],"text":["adopted"]},
                  {"id":"box","type":"shape","geo":"rect","at":[8,104],"size":[80,80]},
                  {"id":"arrow","type":"connector",
                   "from":{"object":"title"},"to":{"object":"box"}}]}]}"#,
        );
        let (adopted, notes) = merge_pages(&mut target, src);
        assert_eq!(adopted, ["page-1-2"], "{notes:?}");
        assert_eq!(target.pages.len(), 2);
        let merged = &target.pages[1];
        assert_eq!(merged.id, "page-1-2");
        let ids: Vec<&str> = merged.objects.iter().map(|o| o.id()).collect();
        assert_eq!(ids, ["title-2", "box", "arrow"], "{notes:?}");
        // The connector followed its renamed endpoint.
        let Some(crate::Object::Connector(c)) = merged.objects.iter().find(|o| o.id() == "arrow")
        else {
            panic!("connector survived the merge");
        };
        assert_eq!(c.from.object.as_deref(), Some("title-2"));
        assert_eq!(c.to.object.as_deref(), Some("box"));
        assert!(
            notes.iter().any(|n| n.contains("page-1-2")),
            "the rename is said out loud: {notes:?}"
        );
        assert!(notes.iter().any(|n| n.contains("title-2")), "{notes:?}");
        // No collision → no rename.
        assert!(!notes.iter().any(|n| n.contains("\"box\" became")));
    }

    #[test]
    fn mplstyle_export_is_deterministic_and_carries_the_ramp() {
        let theme = crate::theme::default_for(true);
        let a = mplstyle_for(&theme);
        let b = mplstyle_for(&theme);
        assert_eq!(a, b, "same theme, same bytes");
        assert!(a.contains("axes.prop_cycle: cycler('color', ["), "{a}");
        // The full 7-color categorical ramp lands in the cycler.
        let cycle_line = a
            .lines()
            .find(|l| l.starts_with("axes.prop_cycle"))
            .unwrap();
        // 7 quoted colors plus the quoted word 'color' = 16 single quotes.
        assert_eq!(cycle_line.matches('\'').count(), 16, "{cycle_line}");
        // Hex without '#': a '#' starts a comment in matplotlibrc syntax.
        assert!(!cycle_line.contains('#'), "{cycle_line}");
        assert!(a.contains("figure.facecolor: 14171c"), "{a}");
        assert!(a.contains("axes.spines.top: False"), "{a}");
        assert!(a.contains("axes.spines.right: False"), "{a}");
        assert!(a.contains("grid.color: "), "{a}");
        assert!(a.contains("lines.linewidth: 2"), "{a}");
        assert!(a.contains("font.family: Inter"), "{a}");
    }

    #[test]
    fn rescheme_maps_a_three_color_svg_onto_the_ramp() {
        // White ground, black text, three saturated series colors — red is
        // the most frequent series and must take the first ramp slot.
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
            <rect width="10" height="10" fill="#ffffff"/>
            <text style="fill:#000000;">label</text>
            <rect width="2" height="4" fill="#ff0000"/>
            <rect width="2" height="5" fill="#ff0000"/>
            <rect width="2" height="6" fill="#00cc44"/>
            <path d="M 0 0 L 1 1" stroke="#2244ff" fill="none"/>
        </svg>"##;
        let theme = crate::theme::default_for(true);
        let (out, rows) = rescheme_svg(svg, &theme);
        let row = |from: &str| rows.iter().find(|r| r.from == from).unwrap();
        assert_eq!(row("#ffffff").target, "@bg");
        assert_eq!(row("#000000").target, "@body");
        assert_eq!(row("#ff0000").target, "@cat1");
        assert_eq!(row("#ff0000").count, 2);
        assert_eq!(row("#00cc44").target, "@cat2");
        assert_eq!(row("#2244ff").target, "@cat3");
        // The recolored svg carries the resolved ramp hexes, not the originals.
        let cat1 = theme.categorical(0).hex();
        let cat2 = theme.categorical(1).hex();
        let cat3 = theme.categorical(2).hex();
        assert!(out.contains(&cat1), "{out}");
        assert!(out.contains(&cat2), "{out}");
        assert!(out.contains(&cat3), "{out}");
        assert!(!out.contains("#ff0000"), "{out}");
        assert!(!out.contains("#00cc44"), "{out}");
        // The style-property form was rewritten too.
        let body_hex = theme.color("@body").unwrap().hex();
        assert!(out.contains(&format!("fill:{body_hex}")), "{out}");
        // "none" is not a color and survives untouched.
        assert!(out.contains(r#"fill="none""#), "{out}");
        // Determinism.
        let (out2, _) = rescheme_svg(svg, &theme);
        assert_eq!(out, out2);
    }

    #[test]
    fn regen_comments_and_mermaid_sniffing_parse() {
        assert_eq!(
            sniff_regen_comment("<svg><!-- chimaera:regen snakemake --forcerun fig.svg --></svg>"),
            Some("snakemake --forcerun fig.svg".to_string())
        );
        assert_eq!(sniff_regen_comment("<svg></svg>"), None);
        assert!(looks_like_mermaid(b"%% a comment\nflowchart LR\na-->b"));
        assert!(looks_like_mermaid(b"graph TD\na-->b"));
        assert!(!looks_like_mermaid(b"<svg xmlns='x'/>"));
    }

    #[test]
    fn show_appends_a_shown_event_to_the_cards_journal() {
        // The surfacing signal (board plan §10): every show — including a
        // re-show updating the same card — appends `shown`, actor agent, to
        // the written board's own path-derived journal.
        let dir = std::env::temp_dir().join(format!("chimaera-shown-event-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".chimaera/board")).unwrap();
        let board_path = dir.join("card.board.json");
        std::fs::write(
            &board_path,
            r#"{"format":"chimaera.board","formatVersion":1,"title":"t","canvas":{"size":[100,100]},"pages":[]}"#,
        )
        .unwrap();

        append_shown_event(&board_path);
        append_shown_event(&board_path);

        let events = crate::journal::read_since(&journal_path_for(&board_path), 0).unwrap();
        let lines: Vec<String> = events.iter().map(|e| e.render()).collect();
        assert_eq!(
            lines,
            ["#1 agent showed this board", "#2 agent showed this board"],
            "{lines:?}"
        );
    }

    #[test]
    fn merge_verb_honors_the_git_driver_contract() {
        // Exit-code + overwrite semantics: OURS is rewritten even when the
        // merge conflicts (Err after the write), and --check writes nothing.
        let dir = std::env::temp_dir().join(format!("chimaera-board-merge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, fill: &str| -> PathBuf {
            let p = dir.join(name);
            std::fs::write(
                &p,
                format!(
                    r#"{{"format":"chimaera.board","formatVersion":1,"canvas":{{"size":[960,540]}},
                        "pages":[{{"id":"p1","objects":[{{"id":"box","type":"shape","geo":"rect",
                        "at":[80,80],"size":[160,80],"fill":"{fill}"}}]}}]}}"#
                ),
            )
            .unwrap();
            p
        };
        let base = write("base.board.json", "@accent1");
        let ours = write("ours.board.json", "@accent2");
        let theirs = write("theirs.board.json", "@accent3");

        // --check reports without touching OURS.
        let before = std::fs::read_to_string(&ours).unwrap();
        assert!(merge(&base, &ours, &theirs, None, true).is_err());
        assert_eq!(std::fs::read_to_string(&ours).unwrap(), before);

        // The real merge rewrites OURS with the ours-wins result AND errors,
        // which is how a driver reports conflicts to git.
        assert!(merge(&base, &ours, &theirs, None, false).is_err());
        let merged = std::fs::read_to_string(&ours).unwrap();
        assert!(merged.contains("@accent2"), "{merged}");
        assert_ne!(merged, before, "rewritten canonically");

        // A clean merge exits Ok. theirs == base → ours' edit wins silently.
        let theirs_clean = write("theirs2.board.json", "@accent1");
        merge(&base, &ours, &theirs_clean, None, false).unwrap();
        assert!(std::fs::read_to_string(&ours).unwrap().contains("@accent2"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A one-page 200×100 pt PDF with a full-bleed red rectangle, built
    /// in-test (no binary fixtures in the repo).
    fn tiny_pdf_fixture() -> Vec<u8> {
        use pdf_writer::{Content, Finish, Pdf, Rect, Ref};

        let catalog_id = Ref::new(1);
        let page_tree_id = Ref::new(2);
        let page_id = Ref::new(3);
        let content_id = Ref::new(4);
        let mut pdf = Pdf::new();
        pdf.catalog(catalog_id).pages(page_tree_id);
        pdf.pages(page_tree_id).kids([page_id]).count(1);
        let mut page = pdf.page(page_id);
        page.media_box(Rect::new(0.0, 0.0, 200.0, 100.0));
        page.parent(page_tree_id);
        page.contents(content_id);
        page.finish();
        let mut content = Content::new();
        content.set_fill_rgb(1.0, 0.0, 0.0);
        content.rect(0.0, 0.0, 200.0, 100.0);
        content.fill_nonzero();
        pdf.stream(content_id, &content.finish());
        pdf.finish()
    }

    /// A throwaway workspace with its own `.git` marker so `workspace_root`
    /// stops there instead of walking out of the temp tree.
    fn pdf_test_workspace(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-board-pdfimport-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        dir
    }

    #[cfg(feature = "pdf-import")]
    #[test]
    fn pdf_import_lands_the_png_with_provenance() {
        let dir = pdf_test_workspace("full");
        let pdf_path = dir.join("fig.pdf");
        let pdf_bytes = tiny_pdf_fixture();
        std::fs::write(&pdf_path, &pdf_bytes).unwrap();
        let board_path = dir.join("deck.board.json");

        import(&pdf_path, &board_path, None, None, None, None, None).unwrap();

        // The rendered PNG landed under the shared asset convention.
        let asset = dir.join(".chimaera/board/assets/fig-p1.png");
        let png = std::fs::read(&asset).unwrap();
        // 200×100 pt at the default 300 dpi.
        assert_eq!(crate::imginfo::png_dimensions(&png), Some((833, 416)));

        // The image object carries the source PDF's digest + origin.
        let board = crate::load(&board_path).unwrap();
        let Some(crate::Object::Image(img)) =
            board.pages[0].objects.iter().find(|o| o.id() == "fig")
        else {
            panic!("no image object landed");
        };
        assert_eq!(img.src, ".chimaera/board/assets/fig-p1.png");
        assert_eq!(img.pixel_size, Some([833.0, 416.0]));
        let prov = img.provenance.as_ref().unwrap();
        assert_eq!(
            prov.sha256.as_deref(),
            Some(sha256_hex(&pdf_bytes).as_str())
        );
        let source = prov.extra["source"].as_str().unwrap();
        assert!(source.ends_with("fig.pdf#page=1"), "{source}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(feature = "pdf-import"))]
    #[test]
    fn pdf_import_without_the_feature_says_how_to_get_it() {
        let dir = pdf_test_workspace("stub");
        let pdf_path = dir.join("fig.pdf");
        std::fs::write(&pdf_path, tiny_pdf_fixture()).unwrap();
        let err = import(
            &pdf_path,
            &dir.join("deck.board.json"),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "this build lacks pdf-import (build with --features pdf-import)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
