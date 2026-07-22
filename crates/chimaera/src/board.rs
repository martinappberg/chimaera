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
    /// Print the agent-facing description: every object, its position, its
    /// content, in the same points the file uses.
    Describe { path: PathBuf },
    /// Check a board without rendering it.
    Lint {
        path: PathBuf,
        #[arg(long)]
        theme: Option<String>,
    },
}

pub fn run(cmd: BoardCmd) -> Result<()> {
    match cmd {
        BoardCmd::Show {
            spec,
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
            spec, title, note, id, &preset, size, theme, out, emit_board, quiet,
        ),
        BoardCmd::New { path, title, theme } => new(&path, title, &theme),
        BoardCmd::Render {
            path,
            page,
            out,
            scale,
            theme,
        } => render(&path, page, out, scale, theme),
        BoardCmd::Describe { path } => {
            let board = load_normalized(&path)?.0;
            print!("{}", chimaera_board::describe::describe(&board));
            Ok(())
        }
        BoardCmd::Lint { path, theme } => lint(&path, theme),
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

    let mut spec: chimaera_board::show::ShowSpec =
        serde_json::from_str(&raw).context("parsing the show spec")?;
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
    std::fs::write(&png_path, &rendered.png)
        .with_context(|| format!("writing {}", png_path.display()))?;

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
    let params = RasterParams { scale };

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
        let rendered = render_page(&board, p, &theme, &fonts, params)?;
        let dest = match (&out, single) {
            (Some(o), true) => o.clone(),
            (Some(o), false) => o.join(format!("{}.png", board.pages[p].id)),
            (None, _) => {
                let dir = chimaera_board::ensure_board_dir(&ws)?.join("renders");
                std::fs::create_dir_all(&dir)?;
                let key = chimaera_board::render::render_key(&canonical, &theme, p, params);
                dir.join(format!("{key}.png"))
            }
        };
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, &rendered.png)
            .with_context(|| format!("writing {}", dest.display()))?;
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

fn lint(path: &Path, theme_ref: Option<String>) -> Result<()> {
    let (board, mut diags) = load_normalized(path)?;
    let ws = chimaera_board::workspace_root(path);
    let theme = resolve_theme(theme_ref.as_deref(), &board, &ws)?;
    diags.extend(chimaera_board::lint::lint(&board, &theme));

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
