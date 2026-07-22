//! `chimaera board` — the CLI half of the bidirectional loop.
//!
//! Everything here is a thin shell over `chimaera-board` crate functions; the
//! daemon routes wrap the same functions, which is what keeps the pane and the
//! CLI showing the same pixels. All verbs are synchronous and touch only the
//! filesystem — no daemon needs to be running.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Subcommand;

use chimaera_board::export::pdf::export_pdf;
use chimaera_board::export::svg::{export_svg, SvgVariant};
use chimaera_board::layout::FontStack;
use chimaera_board::render::{render_page, RasterParams};
use chimaera_board::theme::Theme;

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
    },
    /// Import a mermaid flowchart as a `diagram` object appended to a board.
    /// Converted once — the mermaid source rides along as provenance.
    Import {
        /// The mermaid file, or `-` for stdin.
        path: PathBuf,
        /// The board to append to; created with one page when it does not
        /// exist yet.
        #[arg(long)]
        to: PathBuf,
        /// Page id to append to; the first page when omitted.
        #[arg(long)]
        page: Option<String>,
        /// Object id for the diagram; the mermaid file's stem when omitted.
        #[arg(long)]
        id: Option<String>,
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
        } => export(&path, &format, page, out),
        BoardCmd::Import { path, to, page, id } => import_mermaid(&path, &to, page, id),
        BoardCmd::Describe { path } => {
            let board = load_normalized(&path)?.0;
            let summary = chimaera_board::journal::summary(&journal_path_for(&path));
            print!(
                "{}",
                chimaera_board::describe::describe_with_journal(&board, summary)
            );
            Ok(())
        }
        BoardCmd::Journal { path, since } => journal(&path, since),
        BoardCmd::Lint {
            path,
            theme,
            target,
        } => lint(&path, theme, target),
    }
}

/// Load, normalize, and report — the shared front half of every verb.
fn load_normalized(
    path: &Path,
) -> Result<(chimaera_board::Board, Vec<chimaera_board::Diagnostic>)> {
    let mut board = chimaera_board::load(path)?;
    let diags = chimaera_board::normalize(&mut board);
    Ok((board, diags))
}

/// The journal key for a board named on the command line. Canonicalized
/// first — the journal key is derived from the workspace-relative path, and
/// the daemon canonicalizes too, so a relative CLI path must not mint a
/// second key for the same board.
fn journal_path_for(path: &Path) -> PathBuf {
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let ws = chimaera_board::workspace_root(&abs);
    chimaera_board::journal::journal_path(&ws, &abs)
}

fn journal(path: &Path, since: Option<u64>) -> Result<()> {
    if !chimaera_board::is_board_path(path) {
        bail!(
            "not a board: {} does not end in .board.json",
            path.display()
        );
    }
    let events = chimaera_board::journal::read_since(&journal_path_for(path), since.unwrap_or(0))?;
    if events.is_empty() {
        println!("no journal events");
        return Ok(());
    }
    for event in &events {
        println!("{}", event.render());
    }
    Ok(())
}

fn resolve_theme(
    reference: Option<&str>,
    board: &chimaera_board::Board,
    ws: &Path,
) -> Result<Theme> {
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
    let mut spec: chimaera_board::show::ShowSpec = if mermaid {
        chimaera_board::show::ShowSpec {
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
        None => chimaera_board::show::preset_size(preset)
            .with_context(|| format!("unknown preset {preset:?}; use default|wide|square|tall"))?,
    };

    let cwd = std::env::current_dir().context("resolving the working directory")?;
    let ws = chimaera_board::workspace_root(&cwd);
    let theme_id = theme_ref.unwrap_or_else(|| "talk-dark".to_string());
    let theme = Theme::resolve(&theme_id, Some(&ws))?;

    let board = chimaera_board::show::build_board(&spec, size, &theme_id)?;
    if emit_board {
        print!("{}", chimaera_board::to_string(&board)?);
    }

    let fonts = FontStack::for_workspace(&ws);
    let rendered = render_page(&board, 0, &theme, &fonts, RasterParams::default())?;

    // Content-derived id unless the caller supplied an update handle.
    let card_id = id.unwrap_or_else(|| chimaera_board::show::spec_id(&raw));
    let (board_path, png_path) = match out {
        Some(p) => {
            let board_path = p.with_extension("board.json");
            (board_path, p)
        }
        None => {
            let shown = chimaera_board::ensure_shown_dir(&ws)?;
            (
                shown.join(format!("{card_id}.board.json")),
                shown.join(format!("{card_id}.png")),
            )
        }
    };
    chimaera_board::save(&board_path, &board)?;
    chimaera_board::write_atomic(&png_path, &rendered.png)?;

    if !quiet {
        let rel = |p: &Path| {
            p.strip_prefix(&ws)
                .map(|r| r.display().to_string())
                .unwrap_or_else(|_| p.display().to_string())
        };
        println!(
            "{} → {}",
            chimaera_board::show::summary(&board, &theme_id),
            rel(&board_path)
        );
        for d in rendered
            .diagnostics
            .iter()
            .filter(|d| d.severity != chimaera_board::Severity::Info)
        {
            eprintln!("{}", d.render());
        }
    }
    Ok(())
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
    if !chimaera_board::is_board_path(path) {
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
    let mut board = chimaera_board::Board::new(title, chimaera_board::Canvas::default());
    board.theme = Some(theme.to_string());
    chimaera_board::save(path, &board)?;
    // The workspace surround comes into being with the first board.
    let cwd = std::env::current_dir()?;
    chimaera_board::ensure_board_dir(&chimaera_board::workspace_root(&cwd))?;
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
    let ws = chimaera_board::workspace_root(path);
    let theme = resolve_theme(theme_ref.as_deref(), &board, &ws)?;
    let fonts = FontStack::for_workspace(&ws);
    let params = RasterParams {
        scale,
        workspace: Some(ws.clone()),
    };

    for d in diags
        .iter()
        .filter(|d| d.severity != chimaera_board::Severity::Info)
    {
        eprintln!("{}", d.render());
    }

    let pages: Vec<usize> = match page {
        Some(p) => vec![p],
        None => (0..board.pages.len()).collect(),
    };
    let single = pages.len() == 1;
    let canonical = chimaera_board::to_string(&board)?;

    for p in pages {
        let rendered = render_page(&board, p, &theme, &fonts, params.clone())?;
        let dest = match (&out, single) {
            (Some(o), true) => o.clone(),
            (Some(o), false) => o.join(format!("{}.png", board.pages[p].id)),
            (None, _) => {
                let dir = chimaera_board::ensure_board_dir(&ws)?.join("renders");
                std::fs::create_dir_all(&dir)?;
                let key = chimaera_board::render::render_key(&canonical, &theme, p, params.clone());
                dir.join(format!("{key}.png"))
            }
        };
        chimaera_board::write_atomic(&dest, &rendered.png)?;
        // The default destination is the shared content-addressed cache; an
        // explicit -o is the user's own path and never pruned.
        if out.is_none() {
            if let Some(dir) = dest.parent() {
                chimaera_board::prune_renders(dir, chimaera_board::RENDER_CACHE_CAP);
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
            .filter(|d| d.severity != chimaera_board::Severity::Info)
        {
            eprintln!("{}", d.render());
        }
    }
    Ok(())
}

/// Export a board. SVG is per page (mirroring `render`); PDF — and PPTX,
/// when its writer lands — take the whole deck as one document.
fn export(path: &Path, format: &str, page: Option<usize>, out: Option<PathBuf>) -> Result<()> {
    let (board, diags) = load_normalized(path)?;
    for d in diags
        .iter()
        .filter(|d| d.severity != chimaera_board::Severity::Info)
    {
        eprintln!("{}", d.render());
    }
    let ws = chimaera_board::workspace_root(path);
    let theme = resolve_theme(None, &board, &ws)?;
    let fonts = FontStack::for_workspace(&ws);

    // Default destination: .chimaera/board/exports/<stem>.<ext>. The writes
    // are atomic, and write_atomic creates the directory itself.
    let stem = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.trim_end_matches(".board.json").to_string())
        .unwrap_or_else(|| "board".to_string());
    let exports_dir =
        || -> Result<PathBuf> { Ok(chimaera_board::ensure_board_dir(&ws)?.join("exports")) };

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
                let svg = export_svg(&board, p, &theme, &fonts, variant)?;
                let dest = match (&out, single) {
                    (Some(o), true) => o.clone(),
                    (Some(o), false) => o.join(format!("{base}-{}.svg", board.pages[p].id)),
                    (None, true) => exports_dir()?.join(format!("{base}.svg")),
                    (None, false) => {
                        exports_dir()?.join(format!("{base}-{}.svg", board.pages[p].id))
                    }
                };
                chimaera_board::write_atomic(&dest, svg.as_bytes())?;
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
            let pdf = export_pdf(&board, &theme, &fonts)?;
            let dest = match out {
                Some(o) => o,
                None => exports_dir()?.join(format!("{stem}.pdf")),
            };
            chimaera_board::write_atomic(&dest, &pdf)?;
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
            let report = chimaera_board::export::write_pptx(&board, &theme, &fonts, &mut bytes)?;
            let dest = match out {
                Some(o) => o,
                None => exports_dir()?.join(format!("{stem}.pptx")),
            };
            chimaera_board::write_atomic(&dest, &bytes)?;
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
    let (mut diagram, notes) = chimaera_board::diagram::from_mermaid_with_notes(&src)?;
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

    if !chimaera_board::is_board_path(to) {
        bail!(
            "a board path ends in .board.json — try {}",
            to.with_extension("board.json").display()
        );
    }
    let mut board = if to.exists() {
        chimaera_board::load(to)?
    } else {
        let title = to
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.trim_end_matches(".board.json").replace(['-', '_'], " "))
            .unwrap_or_else(|| "Untitled".to_string());
        chimaera_board::Board::new(title, chimaera_board::Canvas::default())
    };

    // The diagram fills the canvas minus a margin; layout inside is computed
    // at render, so this box is all the geometry an import needs.
    let m = 48.0;
    diagram.at = Some([m, m]);
    diagram.size = Some([
        (board.canvas.width() - m * 2.0).max(200.0),
        (board.canvas.height() - m * 2.0).max(160.0),
    ]);

    let page_index = match &page {
        Some(pid) => board
            .pages
            .iter()
            .position(|p| &p.id == pid)
            .with_context(|| format!("no page {pid:?} in {}", to.display()))?,
        None => {
            if board.pages.is_empty() {
                board.pages.push(chimaera_board::Page::new("page-1"));
            }
            0
        }
    };
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
        .push(chimaera_board::Object::Diagram(diagram));
    let diags = chimaera_board::normalize(&mut board);
    let mut errors = 0;
    for d in diags
        .iter()
        .filter(|d| d.severity != chimaera_board::Severity::Info)
    {
        eprintln!("{}", d.render());
        if d.severity == chimaera_board::Severity::Error {
            errors += 1;
        }
    }
    if errors > 0 {
        bail!("{errors} error(s); nothing written");
    }
    chimaera_board::save(to, &board)?;
    println!(
        "imported {obj_id:?} · {n_nodes} nodes · {n_edges} edges → {} (page {})",
        to.display(),
        board.pages[page_index].id
    );
    Ok(())
}

fn lint(path: &Path, theme_ref: Option<String>, target: Option<String>) -> Result<()> {
    let (board, mut diags) = load_normalized(path)?;
    let ws = chimaera_board::workspace_root(path);
    let theme = resolve_theme(theme_ref.as_deref(), &board, &ws)?;

    // --target wins; the board's own canvas.target is the default. Neither
    // set → the plain legality profile, exactly as before.
    let target_id = target.or_else(|| board.canvas.target.clone());
    match target_id.as_deref() {
        None => diags.extend(chimaera_board::lint::lint(&board, &theme)),
        Some(id) => {
            let preset = chimaera_board::presets::get(id).with_context(|| {
                format!(
                    "unknown target {id:?}; presets are {}",
                    chimaera_board::presets::ids().join(", ")
                )
            })?;
            let fonts = FontStack::for_workspace(&ws);
            diags.extend(chimaera_board::lint::lint_target(
                &board, &theme, preset, &fonts,
            ));
            // The census: every top-level object's computed export fate, so
            // degradation is stated before an export, never discovered after.
            let (mut native, mut grouped, mut vector, mut raster) = (0u32, 0u32, 0u32, 0u32);
            for page in &board.pages {
                for obj in &page.objects {
                    match chimaera_board::presets::tier_of(obj).0 {
                        chimaera_board::export::ExportTier::Native => native += 1,
                        chimaera_board::export::ExportTier::Grouped => grouped += 1,
                        chimaera_board::export::ExportTier::Vector => vector += 1,
                        chimaera_board::export::ExportTier::Raster => raster += 1,
                    }
                }
            }
            println!(
                "tier census: {native} native · {grouped} grouped · {vector} vector · \
                 {raster} raster"
            );
        }
    }

    if diags.is_empty() {
        let n = board.pages.len();
        println!("clean · {n} page{}", if n == 1 { "" } else { "s" });
        return Ok(());
    }
    let mut errors = 0;
    for d in &diags {
        println!("{}", d.render());
        if d.severity == chimaera_board::Severity::Error {
            errors += 1;
        }
    }
    if errors > 0 {
        bail!("{errors} error(s)");
    }
    Ok(())
}
