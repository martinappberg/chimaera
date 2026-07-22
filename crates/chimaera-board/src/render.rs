//! Rendering: scene graph → SVG → pixels.
//!
//! Board *emits* the SVG it rasterizes — usvg never parses anything this crate
//! did not generate — so SVG here is an internal representation, not an input
//! format. The pane shows exactly these pixels: layout truth is server-side,
//! and the one text stack ([`crate::layout`]) both measures and, via usvg's
//! text-to-path with the same `fontdb`, draws.
//!
//! Renders are content-addressed: the cache key is a digest of the canonical
//! board bytes, the theme, and the raster parameters. A render is a pure
//! function of those, so a cache hit is *correct*, not merely probably fine —
//! and the cache never needs invalidation, only pruning.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::chart::{ChartItem, TextAnchor};
use crate::layout::{css_font_family, FontStack};
use crate::normalize::{Diagnostic, Severity};
use crate::schema::{
    Align, Board, ConnectorObject, Frame, Object, Page, Paragraph, Run, Side, TableObject, VAlign,
};
use crate::theme::{Rgb, Theme};

/// Fixed table-cell padding in points. One number shared by the renderer, the
/// cell-overfull lint and the PPTX writer's `a:tcPr` margins, so the pane and
/// the deck agree about where cell text sits.
pub(crate) const TABLE_CELL_PAD_PT: f64 = 6.0;

/// The raster ceiling, in pixels. 12 Mpx is a 4K slide at 2× with headroom;
/// past it a render request is a mistake or an attack on daemon RSS, and the
/// answer is an error rather than an allocation.
pub const MAX_PIXELS: u64 = 12_000_000;

/// The largest image file the renderer will inline as a data URI. Base64
/// inflates by 4/3 and the daemon lives on shared login nodes, so an oversized
/// asset is a named diagnostic and a placeholder, never an allocation.
pub const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

/// Sub-floor text is a render *error*, not a lint finding, in slice 0 — with
/// no linter shipped yet, the renderer is the only enforcer `minPt` has, and
/// per the plan's own ranking a constraint beats a report.
#[derive(Debug)]
pub struct RenderOutput {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub diagnostics: Vec<Diagnostic>,
}

/// Raster parameters.
#[derive(Debug, Clone)]
pub struct RasterParams {
    /// Device scale; 2.0 is the default everywhere Board renders for a UI.
    pub scale: f64,
    /// Workspace root for resolving chart `data.source` files. `None` renders
    /// a source-bound chart with a "source not loaded" note instead of rows.
    /// Deliberately absent from [`render_key`]: the key hashes content, and
    /// the board's own `sha256` is what pins the file bytes.
    pub workspace: Option<PathBuf>,
}

impl Default for RasterParams {
    fn default() -> Self {
        RasterParams {
            scale: 2.0,
            workspace: None,
        }
    }
}

/// The content-addressed key for a render.
pub fn render_key(board_bytes: &str, theme: &Theme, page: usize, params: RasterParams) -> String {
    let mut h = Sha256::new();
    h.update(board_bytes.as_bytes());
    h.update(serde_json::to_string(theme).unwrap_or_default().as_bytes());
    h.update(page.to_le_bytes());
    h.update(params.scale.to_bits().to_le_bytes());
    let digest = h.finalize();
    let mut s = String::with_capacity(16);
    for b in &digest[..8] {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Render one page of a normalized board to PNG.
pub fn render_page(
    board: &Board,
    page_index: usize,
    theme: &Theme,
    fonts: &FontStack,
    params: RasterParams,
) -> Result<RenderOutput> {
    let page = board
        .pages
        .get(page_index)
        .with_context(|| format!("board has no page {page_index}"))?;

    let w = board.canvas.width();
    let h = board.canvas.height();
    // Ceiling arithmetic must not itself be the hazard: a NaN or huge canvas
    // reaching `as u64` saturates, and an unchecked product could wrap in a
    // release build — so both axes are bounded first and the product checked.
    let axis = |v: f64| -> u64 {
        let px = (v * params.scale).round();
        if px.is_finite() && px >= 0.0 {
            px.min(MAX_PIXELS as f64) as u64
        } else {
            0
        }
    };
    let px_w = axis(w);
    let px_h = axis(h);
    if px_w == 0 || px_h == 0 {
        anyhow::bail!("canvas rasterizes to zero pixels");
    }
    let total = px_w.saturating_mul(px_h);
    if total > MAX_PIXELS {
        anyhow::bail!(
            "render would be {px_w}×{px_h} px ({} Mpx), over the {} Mpx ceiling",
            total / 1_000_000,
            MAX_PIXELS / 1_000_000
        );
    }

    let mut diagnostics = Vec::new();
    let svg = page_svg(
        board,
        page,
        theme,
        fonts,
        params.workspace.as_deref(),
        &mut diagnostics,
    )?;

    let opt = usvg::Options {
        fontdb: fonts.db(),
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(&svg, &opt).context("parsing the generated SVG")?;

    let mut pixmap = tiny_skia::Pixmap::new(px_w as u32, px_h as u32)
        .context("allocating the render surface")?;
    let transform = tiny_skia::Transform::from_scale(params.scale as f32, params.scale as f32);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    let png = pixmap.encode_png().context("encoding PNG")?;
    Ok(RenderOutput {
        png,
        width: px_w as u32,
        height: px_h as u32,
        diagnostics,
    })
}

/// Encode an already-rendered page as JPEG — the fast preview an agent looks
/// at over a `connect` tunnel.
pub fn render_page_jpeg(
    board: &Board,
    page_index: usize,
    theme: &Theme,
    fonts: &FontStack,
    params: RasterParams,
    quality: u8,
) -> Result<Vec<u8>> {
    let out = render_page(board, page_index, theme, fonts, params)?;
    let img = image_from_png(&out.png)?;
    let mut jpeg = Vec::new();
    let encoder = jpeg_encoder::Encoder::new(&mut jpeg, quality);
    encoder
        .encode(
            &img.rgb,
            out.width as u16,
            out.height as u16,
            jpeg_encoder::ColorType::Rgb,
        )
        .context("encoding JPEG")?;
    Ok(jpeg)
}

struct RgbImage {
    rgb: Vec<u8>,
}

/// Decode our own PNG back to RGB for the JPEG encoder, flattening alpha on
/// white. Only ever fed bytes this module produced.
fn image_from_png(png: &[u8]) -> Result<RgbImage> {
    let pixmap = tiny_skia::Pixmap::decode_png(png).context("decoding our own PNG")?;
    let mut rgb = Vec::with_capacity(pixmap.width() as usize * pixmap.height() as usize * 3);
    for px in pixmap.pixels() {
        let c = px.demultiply();
        let a = c.alpha() as u32;
        let blend = |v: u8| ((v as u32 * a + 255 * (255 - a)) / 255) as u8;
        rgb.push(blend(c.red()));
        rgb.push(blend(c.green()));
        rgb.push(blend(c.blue()));
    }
    Ok(RgbImage { rgb })
}

// ---------------------------------------------------------------------------
// SVG emission
// ---------------------------------------------------------------------------

/// Emit one page as SVG. `pub(crate)` because the exporters ([`crate::export`])
/// reuse this exact emission — a second SVG writer is how the pane and the
/// export quietly stop agreeing.
pub(crate) fn page_svg(
    board: &Board,
    page: &Page,
    theme: &Theme,
    fonts: &FontStack,
    workspace: Option<&Path>,
    diags: &mut Vec<Diagnostic>,
) -> Result<String> {
    let w = board.canvas.width();
    let h = board.canvas.height();
    let mut s = String::with_capacity(16 * 1024);
    let _ = write!(
        s,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">"#
    );

    let bg = page
        .background
        .as_ref()
        .and_then(|b| b.fill.as_deref())
        .and_then(|f| theme.color(f))
        .unwrap_or_else(|| theme.bg());
    let _ = write!(
        s,
        r#"<rect x="0" y="0" width="{w}" height="{h}" fill="{}"/>"#,
        bg.hex()
    );

    // Slot and anchor resolution happens here, at render — never in
    // `normalize`, which would churn the file. The resolved map is the single
    // geometry truth: connectors bind to it, and slot-placed objects take
    // their frames from it.
    let (index, slot_diags) =
        crate::slots::resolve_page_frames_with_diags(board, page, theme, Some(fonts));
    diags.extend(slot_diags);

    for obj in &page.objects {
        emit_object(
            &mut s, obj, page, board, theme, fonts, workspace, &index, diags,
        );
    }

    // Page furniture: the preset's repeated objects (page number, footer,
    // logo), generated per render and drawn above content — never written
    // into the board file. The cover — the first page, or one marked by
    // intent or id — suppresses what the preset says to suppress.
    let preset_id = board
        .canvas
        .target
        .as_deref()
        .or(board.canvas.preset.as_deref());
    if let Some(preset) = preset_id.and_then(crate::presets::get) {
        let page_index = board
            .pages
            .iter()
            .position(|p| std::ptr::eq(p, page))
            .unwrap_or(0);
        let is_cover = page_index == 0
            || page.id == "cover"
            || page.intent.as_ref().is_some_and(|i| i.kind == "cover");
        for mut o in crate::presets::furniture_objects(
            preset,
            page_index,
            board.pages.len(),
            is_cover,
            [w, h],
        ) {
            // A footer whose preset carries no literal text shows the deck's
            // own title.
            if let Object::Text(t) = &mut o {
                if t.text.is_empty() {
                    if let Some(title) = &board.title {
                        t.text = vec![Paragraph::Plain(title.clone())];
                    }
                }
            }
            emit_object(
                &mut s, &o, page, board, theme, fonts, workspace, &index, diags,
            );
        }
    }

    s.push_str("</svg>");
    Ok(s)
}

#[allow(clippy::too_many_arguments)]
fn emit_object(
    s: &mut String,
    obj: &Object,
    page: &Page,
    board: &Board,
    theme: &Theme,
    fonts: &FontStack,
    workspace: Option<&Path>,
    index: &std::collections::BTreeMap<String, Frame>,
    diags: &mut Vec<Diagnostic>,
) {
    // The object's page frame as resolution decided it: slot- and
    // anchor-placed objects live only in `index`, and explicit frames are
    // there too, so this lookup is the single geometry truth. The fallback
    // covers diagram-generated children, which are never on the page.
    let frame = index.get(obj.id()).copied().or_else(|| obj.frame());

    // Off-canvas is a warning, not silence: the object may be intentionally
    // parked, but nobody parks something by accident and finds out from a
    // blank render.
    if let Some(f) = frame {
        let c = &board.canvas;
        if f.right() < 0.0 || f.bottom() < 0.0 || f.x > c.width() || f.y > c.height() {
            diags.push(
                Diagnostic::new(
                    Severity::Warning,
                    format!(
                        "off-canvas: at [{}, {}] with size [{}, {}] on a {}×{} canvas",
                        f.x,
                        f.y,
                        f.w,
                        f.h,
                        c.width(),
                        c.height()
                    ),
                )
                .at(&page.id, obj.id()),
            );
        }
    }

    match obj {
        Object::Text(t) => {
            let Some(frame) = frame else {
                diags.push(
                    Diagnostic::new(Severity::Warning, "text object has no position; skipped")
                        .at(&page.id, obj.id()),
                );
                return;
            };
            let role = t
                .role
                .as_deref()
                .and_then(|r| theme.role(r))
                .unwrap_or_else(|| theme.body());
            emit_text_block(
                s,
                &t.text,
                frame,
                role,
                t.align,
                t.valign,
                theme,
                fonts,
                &page.id,
                obj.id(),
                diags,
            );
        }
        Object::Shape(sh) => {
            let Some(frame) = frame else {
                diags.push(
                    Diagnostic::new(Severity::Warning, "shape has no position; skipped")
                        .at(&page.id, obj.id()),
                );
                return;
            };
            emit_shape(s, sh, frame, theme, diags, &page.id);
            if !sh.text.is_empty() {
                let role = sh
                    .role
                    .as_deref()
                    .and_then(|r| theme.role(r))
                    .unwrap_or_else(|| theme.body());
                // Bound text gets an inset so it never kisses the border.
                let inset = 10.0_f64.min(frame.w * 0.1).min(frame.h * 0.1);
                let inner = Frame {
                    x: frame.x + inset,
                    y: frame.y + inset,
                    w: (frame.w - inset * 2.0).max(1.0),
                    h: (frame.h - inset * 2.0).max(1.0),
                };
                emit_text_block(
                    s,
                    &sh.text,
                    inner,
                    role,
                    sh.align.or(Some(Align::Center)),
                    Some(VAlign::Middle),
                    theme,
                    fonts,
                    &page.id,
                    obj.id(),
                    diags,
                );
            }
        }
        Object::Table(t) => {
            let Some(frame) = frame else {
                diags.push(
                    Diagnostic::new(Severity::Warning, "table has no position; skipped")
                        .at(&page.id, obj.id()),
                );
                return;
            };
            emit_table(s, t, frame, theme, fonts, &page.id, diags);
        }
        Object::Connector(c) => emit_connector(s, c, theme, fonts, index, diags, &page.id),
        Object::Image(img) => {
            let Some(f) = frame else {
                diags.push(
                    Diagnostic::new(Severity::Warning, "image has no position; skipped")
                        .at(&page.id, obj.id()),
                );
                return;
            };
            emit_image(s, img, f, theme, fonts, workspace, &page.id, diags);
        }
        Object::Group(g) => {
            for child in &g.objects {
                emit_object(s, child, page, board, theme, fonts, workspace, index, diags);
            }
        }
        Object::Chart(c) => {
            let Some(frame) = frame else {
                diags.push(
                    Diagnostic::new(Severity::Warning, "chart has no position; skipped")
                        .at(&page.id, obj.id()),
                );
                return;
            };
            // Source-bound rows resolve against the caller's workspace. A
            // stale digest is an Error and the chart draws no marks — loud
            // and blocking, per the plan; export picks the Error up from
            // these diagnostics.
            let (loaded, src_problems) = crate::chart::resolve_rows(c, workspace);
            for p in src_problems {
                let sev = if p.contains("stale") {
                    Severity::Error
                } else {
                    Severity::Warning
                };
                diags.push(Diagnostic::new(sev, p).at(&page.id, obj.id()));
            }
            let scene = match loaded.as_deref() {
                Some(rows) => crate::chart::build_with_rows(c, Some(rows), frame, theme, fonts),
                None => crate::chart::build(c, frame, theme, fonts),
            };
            for p in scene.problems {
                diags.push(Diagnostic::new(Severity::Warning, p).at(&page.id, obj.id()));
            }
            for item in &scene.items {
                emit_chart_item(s, item, theme, &page.id, obj.id(), diags);
            }
            // The origin chip: where the numbers came from, visibly, on the
            // render itself. The one chart-integrity mechanism that cannot be
            // ignored because it cannot be unsubscribed from.
            let label = theme.role("label").unwrap_or_else(|| theme.body());
            let chip = c.data.origin.label();
            let chip_size = (label.size * 0.85).max(8.0);
            let muted = theme.color_or_fg(Some("@muted"));
            let _ = write!(
                s,
                r#"<text x="{}" y="{}" font-family="{}" font-size="{chip_size}" fill="{}" text-anchor="end">{}</text>"#,
                frame.right(),
                frame.bottom() + chip_size * 1.2,
                escape(&css_font_family(&label.family)),
                muted.hex(),
                escape(chip)
            );
        }
        Object::Diagram(d) => {
            let Some(f) = frame else {
                diags.push(
                    Diagnostic::new(Severity::Warning, "diagram has no position; skipped")
                        .at(&page.id, obj.id()),
                );
                return;
            };
            // `expand` reads the diagram's own at/size; a slot- or
            // anchor-placed diagram carries neither, so hand it the frame
            // resolution decided.
            let placed;
            let d = if d.at.is_none() || d.size.is_none() {
                let mut c = d.clone();
                c.at = Some([f.x, f.y]);
                c.size = Some([f.w, f.h]);
                placed = c;
                &placed
            } else {
                d
            };
            let (children, problems) = crate::diagram::expand(d, theme, fonts);
            for p in problems {
                diags.push(Diagnostic::new(Severity::Warning, p).at(&page.id, obj.id()));
            }
            // Generated children are never on the page, so the page index
            // cannot resolve a connector bound to `<diagram>/<node>`; extend
            // it with the expansion's own frames.
            let mut child_index = index.clone();
            for c in &children {
                if let Some(f) = c.frame() {
                    child_index.insert(c.id().to_string(), f);
                }
            }
            for c in &children {
                emit_object(
                    s,
                    c,
                    page,
                    board,
                    theme,
                    fonts,
                    workspace,
                    &child_index,
                    diags,
                );
            }
        }
        // The annotation composites: expand exactly like a diagram —
        // problems become warnings, children render recursively against an
        // index extended with their own frames.
        Object::PanelLabel(o) => {
            // An anchored label carries no `at` of its own; hand it the frame
            // resolution decided, exactly as a slot-placed diagram gets its.
            let placed;
            let o = if o.at.is_none() {
                let Some(f) = frame else {
                    diags.push(
                        Diagnostic::new(Severity::Warning, "panelLabel has no position; skipped")
                            .at(&page.id, obj.id()),
                    );
                    return;
                };
                let mut c = o.clone();
                c.at = Some([f.x, f.y]);
                placed = c;
                &placed
            } else {
                o
            };
            let (children, problems) = o.expand(theme, fonts);
            emit_expanded(
                s, &children, problems, obj, page, board, theme, fonts, workspace, index, diags,
            );
        }
        Object::Scalebar(o) => {
            let (children, problems) = o.expand(theme, fonts);
            emit_expanded(
                s, &children, problems, obj, page, board, theme, fonts, workspace, index, diags,
            );
        }
        Object::SigBracket(o) => {
            // Targets resolve against the same index connectors bind through,
            // so the bracket lands on slot- and anchor-placed panels too.
            let (children, problems) = o.expand(theme, fonts, index);
            emit_expanded(
                s, &children, problems, obj, page, board, theme, fonts, workspace, index, diags,
            );
        }
        Object::Legend(o) => {
            let (children, problems) = o.expand(theme, fonts);
            emit_expanded(
                s, &children, problems, obj, page, board, theme, fonts, workspace, index, diags,
            );
        }
        Object::Colorbar(o) => {
            let placed;
            let o = if o.at.is_none() || o.size.is_none() {
                let Some(f) = frame else {
                    diags.push(
                        Diagnostic::new(Severity::Warning, "colorbar has no position; skipped")
                            .at(&page.id, obj.id()),
                    );
                    return;
                };
                let mut c = o.clone();
                c.at = Some([f.x, f.y]);
                c.size = Some([f.w, f.h]);
                placed = c;
                &placed
            } else {
                o
            };
            let (children, problems) = o.expand(theme, fonts);
            emit_expanded(
                s, &children, problems, obj, page, board, theme, fonts, workspace, index, diags,
            );
        }
        Object::Callout(o) => {
            let placed;
            let o = if o.at.is_none() || o.size.is_none() {
                let Some(f) = frame else {
                    diags.push(
                        Diagnostic::new(Severity::Warning, "callout has no position; skipped")
                            .at(&page.id, obj.id()),
                    );
                    return;
                };
                let mut c = o.clone();
                c.at = Some([f.x, f.y]);
                c.size = Some([f.w, f.h]);
                placed = c;
                &placed
            } else {
                o
            };
            let (children, problems) = o.expand(theme, fonts);
            emit_expanded(
                s, &children, problems, obj, page, board, theme, fonts, workspace, index, diags,
            );
        }
        Object::Inset(o) => {
            let target = page.walk().find_map(|t| match t {
                Object::Image(i) if i.id == o.of.object => Some(i),
                _ => None,
            });
            let target_frame = index.get(o.of.object.as_str()).copied();
            let (children, problems) = o.expand(theme, fonts, target, target_frame);
            emit_expanded(
                s, &children, problems, obj, page, board, theme, fonts, workspace, index, diags,
            );
        }
        Object::Unknown(u) => {
            diags.push(
                Diagnostic::new(
                    Severity::Warning,
                    match &u.error {
                        Some(e) => format!("object of type {:?} failed to parse: {e}", u.kind),
                        None => format!(
                            "object type {:?} is not known to this build; preserved but not drawn",
                            u.kind
                        ),
                    },
                )
                .at(&page.id, &u.id),
            );
        }
    }
}

/// Draw one composite's expansion: problems become warnings on the composite,
/// and children render recursively against the page index extended with their
/// own frames — generated children are never on the page, so a connector
/// bound to `<composite>/<part>` resolves only through this extension.
#[allow(clippy::too_many_arguments)]
fn emit_expanded(
    s: &mut String,
    children: &[Object],
    problems: Vec<String>,
    obj: &Object,
    page: &Page,
    board: &Board,
    theme: &Theme,
    fonts: &FontStack,
    workspace: Option<&Path>,
    index: &std::collections::BTreeMap<String, Frame>,
    diags: &mut Vec<Diagnostic>,
) {
    for p in problems {
        diags.push(Diagnostic::new(Severity::Warning, p).at(&page.id, obj.id()));
    }
    let mut child_index = index.clone();
    for c in children {
        if let Some(f) = c.frame() {
            child_index.insert(c.id().to_string(), f);
        }
    }
    for c in children {
        emit_object(
            s,
            c,
            page,
            board,
            theme,
            fonts,
            workspace,
            &child_index,
            diags,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_text_block(
    s: &mut String,
    paragraphs: &[Paragraph],
    frame: Frame,
    role: &crate::theme::TypeRole,
    align: Option<Align>,
    valign: Option<VAlign>,
    theme: &Theme,
    fonts: &FontStack,
    page_id: &str,
    obj_id: &str,
    diags: &mut Vec<Diagnostic>,
) {
    // The floor. In slice 0 the renderer is the only enforcer minPt has, so
    // sub-floor text refuses to render at that size and clamps up, loudly.
    let mut size = role.size;
    if size < role.min_pt {
        diags.push(
            Diagnostic::new(
                Severity::Error,
                format!(
                    "text at {size} pt is under this role's {} pt floor; rendered at the floor",
                    role.min_pt
                ),
            )
            .at(page_id, obj_id),
        );
        size = role.min_pt;
    }

    let color = theme.color_or_fg(Some(&role.color));
    let family_css = css_font_family(&role.family);
    let anchor = match align.unwrap_or(Align::Left) {
        Align::Left => ("start", frame.x),
        Align::Center => ("middle", frame.cx()),
        Align::Right => ("end", frame.right()),
    };

    // Wrap and lay out all lines first so vertical alignment knows the block
    // height before anything is emitted.
    struct Line {
        text: String,
        size: f64,
        weight: u16,
        italic: bool,
        color: Rgb,
        height: f64,
        runs: Option<Vec<Run>>,
    }
    let mut lines: Vec<Line> = Vec::new();
    for p in paragraphs {
        match p {
            Paragraph::Plain(text) => {
                for l in fonts.wrap(text, &role.family, size, role.weight, frame.w) {
                    lines.push(Line {
                        text: l,
                        size,
                        weight: role.weight,
                        italic: role.italic.unwrap_or(false),
                        color,
                        height: size * role.line_height,
                        runs: None,
                    });
                }
            }
            Paragraph::Rich(rich) => {
                // Rich paragraphs keep runs together on one line per
                // paragraph in slice 0; measured wrap across styled runs
                // arrives with the editor. Overflow reports rather than
                // silently clipping.
                let joined: String = rich.runs.iter().map(|r| r.t.as_str()).collect();
                let max_size = rich.runs.iter().filter_map(|r| r.size).fold(size, f64::max);
                let width = fonts.measure(&joined, &role.family, max_size, role.weight);
                if width > frame.w + 0.5 {
                    diags.push(
                        Diagnostic::new(
                            Severity::Warning,
                            format!(
                                "styled paragraph measures {width:.0} pt against a {:.0} pt box",
                                frame.w
                            ),
                        )
                        .at(page_id, obj_id),
                    );
                }
                lines.push(Line {
                    text: joined,
                    size: max_size,
                    weight: role.weight,
                    italic: role.italic.unwrap_or(false),
                    color,
                    height: max_size * role.line_height,
                    runs: Some(rich.runs.clone()),
                });
            }
        }
    }

    let block_h: f64 = lines.iter().map(|l| l.height).sum();
    if block_h > frame.h + 0.5 {
        diags.push(
            Diagnostic::new(
                Severity::Warning,
                format!(
                    "text measures {block_h:.0} pt tall against a {:.0} pt box (overfull)",
                    frame.h
                ),
            )
            .at(page_id, obj_id),
        );
    }
    let mut y = match valign.unwrap_or(VAlign::Top) {
        VAlign::Top => frame.y,
        VAlign::Middle => frame.y + ((frame.h - block_h) / 2.0).max(0.0),
        VAlign::Bottom => frame.y + (frame.h - block_h).max(0.0),
    };

    for line in &lines {
        let baseline = y + line.size * 0.82; // cap-height seat within the line box
        match &line.runs {
            None => {
                let _ = write!(
                    s,
                    r#"<text x="{}" y="{baseline}" font-family="{}" font-size="{}" font-weight="{}"{} fill="{}" text-anchor="{}">{}</text>"#,
                    anchor.1,
                    escape(&family_css),
                    line.size,
                    line.weight,
                    if line.italic {
                        r#" font-style="italic""#
                    } else {
                        ""
                    },
                    line.color.hex(),
                    anchor.0,
                    escape(&line.text)
                );
            }
            Some(runs) => {
                let _ = write!(
                    s,
                    r#"<text x="{}" y="{baseline}" font-family="{}" font-size="{}" font-weight="{}" fill="{}" text-anchor="{}">"#,
                    anchor.1,
                    escape(&family_css),
                    line.size,
                    line.weight,
                    line.color.hex(),
                    anchor.0,
                );
                for r in runs {
                    let mut attrs = String::new();
                    if r.b == Some(true) {
                        attrs.push_str(r#" font-weight="700""#);
                    }
                    if r.i == Some(true) {
                        attrs.push_str(r#" font-style="italic""#);
                    }
                    if r.u == Some(true) {
                        attrs.push_str(r#" text-decoration="underline""#);
                    }
                    if let Some(c) = r.color.as_deref().and_then(|c| theme.color(c)) {
                        let _ = write!(attrs, r#" fill="{}""#, c.hex());
                    }
                    if let Some(sz) = r.size {
                        let clamped = sz.max(role.min_pt);
                        if clamped != sz {
                            diags.push(
                                Diagnostic::new(
                                    Severity::Error,
                                    format!(
                                        "run override {sz} pt is under the {} pt floor; \
                                         rendered at the floor",
                                        role.min_pt
                                    ),
                                )
                                .at(page_id, obj_id),
                            );
                        }
                        let _ = write!(attrs, r#" font-size="{clamped}""#);
                    }
                    if let Some(f) = r.family.as_deref() {
                        let _ = write!(
                            attrs,
                            r#" font-family="{}""#,
                            escape(&css_font_family(&[f.to_string()]))
                        );
                    }
                    let _ = write!(s, r#"<tspan{attrs}>{}</tspan>"#, escape(&r.t));
                }
                s.push_str("</text>");
            }
        }
        y += line.height;
    }
}

fn emit_shape(
    s: &mut String,
    sh: &crate::schema::ShapeObject,
    f: Frame,
    theme: &Theme,
    diags: &mut Vec<Diagnostic>,
    page_id: &str,
) {
    let fill = sh
        .fill
        .as_deref()
        .and_then(|c| theme.color(c))
        .map(|c| c.hex())
        .unwrap_or_else(|| "none".to_string());
    let fill_opacity = sh.fill_opacity.unwrap_or(1.0).clamp(0.0, 1.0);
    let mut stroke_attrs = String::new();
    if let Some(st) = &sh.stroke {
        let color = theme.color_or_fg(st.color.as_deref());
        let _ = write!(
            stroke_attrs,
            r#" stroke="{}" stroke-width="{}""#,
            color.hex(),
            st.width.unwrap_or(1.0)
        );
        if let Some(dash) = &st.dash {
            let _ = write!(
                stroke_attrs,
                r#" stroke-dasharray="{}""#,
                dash.iter()
                    .map(|d| d.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        if let Some(o) = st.opacity {
            let _ = write!(stroke_attrs, r#" stroke-opacity="{}""#, o.clamp(0.0, 1.0));
        }
    }
    let opacity_attr = if fill_opacity < 1.0 {
        format!(r#" fill-opacity="{fill_opacity}""#)
    } else {
        String::new()
    };

    match sh.geo.as_str() {
        "rect" => {
            let _ = write!(
                s,
                r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{fill}"{opacity_attr}{stroke_attrs}/>"#,
                f.x, f.y, f.w, f.h
            );
        }
        "roundRect" => {
            let r = sh.radius.unwrap_or((f.w.min(f.h) * 0.12).min(16.0));
            let _ = write!(
                s,
                r#"<rect x="{}" y="{}" width="{}" height="{}" rx="{r}" fill="{fill}"{opacity_attr}{stroke_attrs}/>"#,
                f.x, f.y, f.w, f.h
            );
        }
        "ellipse" => {
            let _ = write!(
                s,
                r#"<ellipse cx="{}" cy="{}" rx="{}" ry="{}" fill="{fill}"{opacity_attr}{stroke_attrs}/>"#,
                f.cx(),
                f.cy(),
                f.w / 2.0,
                f.h / 2.0
            );
        }
        "line" => {
            // The unbound straight line that absorbed the former `line` type.
            let _ = write!(
                s,
                r#"<line x1="{}" y1="{}" x2="{}" y2="{}"{stroke_attrs}/>"#,
                f.x,
                f.cy(),
                f.right(),
                f.cy()
            );
        }
        "triangle" => {
            let _ = write!(
                s,
                r#"<polygon points="{},{} {},{} {},{}" fill="{fill}"{opacity_attr}{stroke_attrs}/>"#,
                f.cx(),
                f.y,
                f.right(),
                f.bottom(),
                f.x,
                f.bottom()
            );
        }
        "diamond" => {
            let _ = write!(
                s,
                r#"<polygon points="{},{} {},{} {},{} {},{}" fill="{fill}"{opacity_attr}{stroke_attrs}/>"#,
                f.cx(),
                f.y,
                f.right(),
                f.cy(),
                f.cx(),
                f.bottom(),
                f.x,
                f.cy()
            );
        }
        "path" => match &sh.d {
            Some(d) => {
                let _ = write!(
                    s,
                    r#"<path d="{}" fill="{fill}"{opacity_attr}{stroke_attrs}/>"#,
                    escape(d)
                );
            }
            None => diags.push(
                Diagnostic::new(Severity::Error, "geo \"path\" requires `d`")
                    .at(page_id, &sh.id)
                    .field("d"),
            ),
        },
        other => {
            // An unknown geometry draws its box rather than nothing: the
            // author sees where the object is and the reason it looks wrong.
            diags.push(
                Diagnostic::new(
                    Severity::Warning,
                    format!("geometry {other:?} is not in this build; drew its bounding box"),
                )
                .at(page_id, &sh.id)
                .field("geo"),
            );
            let edge = theme.color("@edge").unwrap_or_else(|| theme.bg());
            let _ = write!(
                s,
                r#"<rect x="{}" y="{}" width="{}" height="{}" fill="none" stroke="{}" stroke-width="1" stroke-dasharray="4 3"/>"#,
                f.x,
                f.y,
                f.w,
                f.h,
                edge.hex()
            );
        }
    }
}

/// A table as a deterministic grid: header fill, hairline strokes, cell text
/// through the same text stack as every other glyph.
///
/// Geometry rule, stated because it is a format-visible decision: columns
/// take [`TableObject::column_widths`] (stated weights or an equal split) and
/// **rows split the box height equally** — content never resizes the grid.
/// An overfull cell reports (here and in `lint --style`) rather than silently
/// pushing later rows around, exactly as a text box reports rather than
/// autofitting.
#[allow(clippy::too_many_arguments)]
fn emit_table(
    s: &mut String,
    t: &TableObject,
    f: Frame,
    theme: &Theme,
    fonts: &FontStack,
    page_id: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let cols = t.column_count();
    if t.rows.is_empty() || cols == 0 {
        diags.push(
            Diagnostic::new(Severity::Warning, "table has no cells; skipped").at(page_id, &t.id),
        );
        return;
    }
    let widths = t.column_widths(f.w);
    let row_h = f.h / t.rows.len() as f64;

    // Z-order within the table: header ground, then the grid, then glyphs.
    if t.header {
        let surface = theme.color("@surface").unwrap_or_else(|| theme.bg());
        let _ = write!(
            s,
            r#"<rect x="{}" y="{}" width="{}" height="{row_h}" fill="{}"/>"#,
            f.x,
            f.y,
            f.w,
            surface.hex()
        );
    }
    let edge = theme.color("@edge").unwrap_or_else(|| theme.bg());
    for r in 0..=t.rows.len() {
        let y = f.y + row_h * r as f64;
        let _ = write!(
            s,
            r#"<line x1="{}" y1="{y}" x2="{}" y2="{y}" stroke="{}" stroke-width="1"/>"#,
            f.x,
            f.right(),
            edge.hex()
        );
    }
    // Column boundaries: one line per left edge, then the right edge.
    let mut x = f.x;
    let vline = |s: &mut String, x: f64| {
        let _ = write!(
            s,
            r#"<line x1="{x}" y1="{}" x2="{x}" y2="{}" stroke="{}" stroke-width="1"/>"#,
            f.y,
            f.bottom(),
            edge.hex()
        );
    };
    for width in &widths {
        vline(s, x);
        x += width;
    }
    vline(s, x);

    let role = t
        .role
        .as_deref()
        .and_then(|r| theme.role(r))
        .unwrap_or_else(|| theme.body());
    let pad = TABLE_CELL_PAD_PT;
    for (ri, row) in t.rows.iter().enumerate() {
        let mut cx = f.x;
        for (ci, cell) in row.iter().enumerate() {
            let inner = Frame {
                x: cx + pad,
                y: f.y + row_h * ri as f64 + pad,
                w: (widths[ci] - pad * 2.0).max(1.0),
                h: (row_h - pad * 2.0).max(1.0),
            };
            let paras = if t.header && ri == 0 {
                vec![TableObject::header_cell(cell)]
            } else {
                vec![cell.clone()]
            };
            emit_text_block(
                s,
                &paras,
                inner,
                role,
                Some(Align::Left),
                Some(VAlign::Top),
                theme,
                fonts,
                page_id,
                &t.id,
                diags,
            );
            cx += widths[ci];
        }
    }
}

/// The dashed box that marks an image which could not be placed — the same
/// visual the PPTX writer degrades to, so the pane and the deck agree about
/// what "missing" looks like.
fn image_placeholder(s: &mut String, f: Frame, theme: &Theme) {
    let edge = theme.color("@edge").unwrap_or_else(|| theme.bg());
    let _ = write!(
        s,
        r#"<rect x="{}" y="{}" width="{}" height="{}" fill="none" stroke="{}" stroke-width="1" stroke-dasharray="4 3"/>"#,
        f.x,
        f.y,
        f.w,
        f.h,
        edge.hex()
    );
}

/// The visible sub-rect of a `w`×`h` natural space selected by an
/// `a:srcRect`-style fractional crop `[l, t, r, b]` (fractions cut from each
/// side). Returns `(x, y, w, h)` in natural units; a degenerate crop clamps
/// to at least one unit rather than inverting.
pub(crate) fn crop_rect(src_rect: Option<[f64; 4]>, w: f64, h: f64) -> (f64, f64, f64, f64) {
    let sane = |v: f64| {
        if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    let [l, t, r, b] = src_rect.unwrap_or([0.0; 4]);
    let (l, t, r, b) = (sane(l), sane(t), sane(r), sane(b));
    let cw = ((1.0 - l - r) * w).max(1.0);
    let ch = ((1.0 - t - b) * h).max(1.0);
    (l * w, t * h, cw, ch)
}

/// Draw an image object for real: PNG/JPEG inline as a data URI inside a
/// nested `<svg>` viewport (which is how `srcRect` crops without touching the
/// pixels), SVG inlined after the usvg sanitize round-trip. Every failure is
/// a named diagnostic plus the dashed placeholder — a blank box with no
/// reason is the one outcome this function refuses to produce.
#[allow(clippy::too_many_arguments)]
fn emit_image(
    s: &mut String,
    img: &crate::schema::ImageObject,
    f: Frame,
    theme: &Theme,
    fonts: &FontStack,
    workspace: Option<&Path>,
    page_id: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let degrade = |s: &mut String, diags: &mut Vec<Diagnostic>, msg: String| {
        diags.push(Diagnostic::new(Severity::Warning, msg).at(page_id, &img.id));
        image_placeholder(s, f, theme);
    };

    let Some(ws) = workspace else {
        degrade(
            s,
            diags,
            format!(
                "image {:?} needs a workspace root to resolve; a placeholder marks its box",
                img.src
            ),
        );
        return;
    };
    let src_path = {
        let p = Path::new(&img.src);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            ws.join(p)
        }
    };
    let len = match std::fs::metadata(&src_path) {
        Ok(m) => m.len(),
        Err(_) => {
            degrade(
                s,
                diags,
                format!(
                    "image source {} not found; a placeholder marks its box",
                    src_path.display()
                ),
            );
            return;
        }
    };
    if len > MAX_IMAGE_BYTES {
        degrade(
            s,
            diags,
            format!(
                "image {} is {len} bytes, over the {MAX_IMAGE_BYTES} byte inline ceiling; \
                 a placeholder marks its box",
                src_path.display()
            ),
        );
        return;
    }
    let bytes = match std::fs::read(&src_path) {
        Ok(b) => b,
        Err(e) => {
            degrade(
                s,
                diags,
                format!(
                    "image source {} is unreadable ({e}); a placeholder marks its box",
                    src_path.display()
                ),
            );
            return;
        }
    };

    match crate::imginfo::sniff_image(&bytes, &img.src) {
        kind @ (crate::imginfo::ImgKind::Png | crate::imginfo::ImgKind::Jpeg) => {
            let Some((w_px, h_px)) = crate::imginfo::raster_dimensions(kind, &bytes) else {
                degrade(
                    s,
                    diags,
                    format!(
                        "could not read the natural pixel size of {}; a placeholder marks its box",
                        src_path.display()
                    ),
                );
                return;
            };
            let (w_px, h_px) = (w_px as f64, h_px as f64);
            if let Some([pw, ph]) = img.pixel_size {
                if (pw - w_px).abs() > 0.5 || (ph - h_px).abs() > 0.5 {
                    diags.push(
                        Diagnostic::new(
                            Severity::Warning,
                            format!(
                                "pixelSize [{pw}, {ph}] disagrees with the file's natural \
                                 {w_px}×{h_px} px"
                            ),
                        )
                        .at(page_id, &img.id)
                        .field("pixelSize"),
                    );
                }
            }
            if img.tint.is_some() {
                diags.push(
                    Diagnostic::new(
                        Severity::Warning,
                        format!(
                            "tint applies to SVG sources only; {:?} is a raster",
                            img.src
                        ),
                    )
                    .at(page_id, &img.id)
                    .field("tint"),
                );
            }
            let (cx, cy, cw, ch) = crop_rect(img.src_rect, w_px, h_px);
            if f.w > 0.0 && f.h > 0.0 {
                let dpi_x = cw / (f.w / 72.0);
                let dpi_y = ch / (f.h / 72.0);
                diags.push(
                    Diagnostic::new(
                        Severity::Info,
                        format!(
                            "effective resolution {dpi_x:.0}×{dpi_y:.0} dpi at the placed size \
                             ({cw:.0}×{ch:.0} px into {}×{} pt)",
                            f.w, f.h
                        ),
                    )
                    .at(page_id, &img.id),
                );
            }
            let mime = match kind {
                crate::imginfo::ImgKind::Png => "image/png",
                _ => "image/jpeg",
            };
            let b64 = crate::imginfo::base64_encode(&bytes);
            let _ = write!(
                s,
                r#"<svg x="{}" y="{}" width="{}" height="{}" viewBox="{cx} {cy} {cw} {ch}" preserveAspectRatio="none"><image x="0" y="0" width="{w_px}" height="{h_px}" preserveAspectRatio="none" href="data:{mime};base64,{b64}"/></svg>"#,
                f.x, f.y, f.w, f.h
            );
        }
        crate::imginfo::ImgKind::Svg => {
            let Ok(text) = std::str::from_utf8(&bytes) else {
                degrade(
                    s,
                    diags,
                    format!(
                        "svg {} is not valid UTF-8; a placeholder marks its box",
                        src_path.display()
                    ),
                );
                return;
            };
            // Namespace the figure's internal ids by the object id so two
            // inlined figures cannot capture each other's defs.
            let prefix: String = img
                .id
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .chain("-".chars())
                .collect();
            let san = match crate::imginfo::sanitize_svg(text, fonts.db(), &prefix) {
                Ok(v) => v,
                Err(e) => {
                    degrade(
                        s,
                        diags,
                        format!(
                            "svg {} failed to parse ({e}); a placeholder marks its box",
                            src_path.display()
                        ),
                    );
                    return;
                }
            };
            if let Some([pw, ph]) = img.pixel_size {
                if (pw - san.width).abs() > 0.5 || (ph - san.height).abs() > 0.5 {
                    diags.push(
                        Diagnostic::new(
                            Severity::Warning,
                            format!(
                                "pixelSize [{pw}, {ph}] disagrees with the svg's natural {}×{} \
                                 units",
                                san.width, san.height
                            ),
                        )
                        .at(page_id, &img.id)
                        .field("pixelSize"),
                    );
                }
            }
            let mut xml = san.xml;
            if let Some(t) = img.tint.as_deref() {
                match theme.color(t) {
                    None => diags.push(
                        Diagnostic::new(
                            Severity::Warning,
                            format!("tint {t:?} does not resolve in this theme"),
                        )
                        .at(page_id, &img.id)
                        .field("tint"),
                    ),
                    Some(rgb) => match crate::imginfo::apply_tint(&xml, &rgb.hex()) {
                        Ok(tinted) => xml = tinted,
                        Err(n) => diags.push(
                            Diagnostic::new(
                                Severity::Warning,
                                format!("tint needs a monochrome source (found {n} colors)"),
                            )
                            .at(page_id, &img.id)
                            .field("tint"),
                        ),
                    },
                }
            }
            let (cx, cy, cw, ch) = crop_rect(img.src_rect, san.width, san.height);
            let _ = write!(
                s,
                r#"<svg x="{}" y="{}" width="{}" height="{}" viewBox="{cx} {cy} {cw} {ch}" preserveAspectRatio="none">{xml}</svg>"#,
                f.x, f.y, f.w, f.h
            );
        }
        crate::imginfo::ImgKind::Unknown => {
            degrade(
                s,
                diags,
                format!(
                    "image {} is not a recognizable PNG, JPEG, or SVG; a placeholder marks its box",
                    src_path.display()
                ),
            );
        }
    }
}

fn emit_connector(
    s: &mut String,
    c: &ConnectorObject,
    theme: &Theme,
    fonts: &FontStack,
    index: &std::collections::BTreeMap<String, Frame>,
    diags: &mut Vec<Diagnostic>,
    page_id: &str,
) {
    let resolve = |ep: &crate::schema::EndPoint, other: Option<(f64, f64)>| -> Option<(f64, f64)> {
        if let Some(at) = ep.at {
            return Some((at[0], at[1]));
        }
        let id = ep.object.as_deref()?;
        let f = index.get(id)?;
        let side = ep.side.unwrap_or_else(|| {
            // Unstated side: face the other endpoint, which is what a human
            // means when they say "connect A to B".
            match other {
                Some((ox, _)) if ox < f.x => Side::Left,
                Some((ox, _)) if ox > f.right() => Side::Right,
                Some((_, oy)) if oy < f.y => Side::Top,
                Some(_) => Side::Bottom,
                None => Side::Center,
            }
        });
        Some(match side {
            Side::Top => (f.cx(), f.y),
            Side::Right => (f.right(), f.cy()),
            Side::Bottom => (f.cx(), f.bottom()),
            Side::Left => (f.x, f.cy()),
            Side::Center => (f.cx(), f.cy()),
        })
    };

    // Two passes so an unstated side can face the other end.
    let to_rough = resolve(&c.to, None);
    let Some(from) = resolve(&c.from, to_rough) else {
        diags.push(
            Diagnostic::new(
                Severity::Warning,
                format!(
                    "connector endpoint {:?} does not resolve; skipped",
                    c.from.object.as_deref().unwrap_or("<no object>")
                ),
            )
            .at(page_id, &c.id),
        );
        return;
    };
    let Some(to) = resolve(&c.to, Some(from)) else {
        diags.push(
            Diagnostic::new(
                Severity::Warning,
                format!(
                    "connector endpoint {:?} does not resolve; skipped",
                    c.to.object.as_deref().unwrap_or("<no object>")
                ),
            )
            .at(page_id, &c.id),
        );
        return;
    };

    let stroke = c
        .stroke
        .as_ref()
        .and_then(|st| st.color.as_deref())
        .and_then(|col| theme.color(col))
        .unwrap_or_else(|| theme.color_or_fg(None));
    let width = c.stroke.as_ref().and_then(|st| st.width).unwrap_or(1.5);

    let _ = write!(
        s,
        r#"<line x1="{}" y1="{}" x2="{}" y2="{}" stroke="{}" stroke-width="{width}""#,
        from.0,
        from.1,
        to.0,
        to.1,
        stroke.hex()
    );
    if let Some(dash) = c.stroke.as_ref().and_then(|st| st.dash.as_ref()) {
        let _ = write!(
            s,
            r#" stroke-dasharray="{}""#,
            dash.iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    s.push_str("/>");

    // Arrowheads as explicit polygons, not <marker> defs: markers scale with
    // stroke width differently across renderers, and an explicit triangle is
    // exactly what the PPTX writer will emit later anyway.
    let angle = (to.1 - from.1).atan2(to.0 - from.0);
    let mut head = |tip: (f64, f64), ang: f64| {
        let len = (width * 4.0).max(6.0);
        let spread = 0.46_f64;
        let a = (
            tip.0 - len * (ang - spread).cos(),
            tip.1 - len * (ang - spread).sin(),
        );
        let b = (
            tip.0 - len * (ang + spread).cos(),
            tip.1 - len * (ang + spread).sin(),
        );
        let _ = write!(
            s,
            r#"<polygon points="{},{} {},{} {},{}" fill="{}"/>"#,
            tip.0,
            tip.1,
            a.0,
            a.1,
            b.0,
            b.1,
            stroke.hex()
        );
    };
    if c.tail_end.as_deref() == Some("arrow") {
        head(to, angle);
    }
    if c.head_end.as_deref() == Some("arrow") {
        head(from, angle + std::f64::consts::PI);
    }

    // The bound edge label, at label_at along the path.
    if !c.text.is_empty() {
        let t = c.label_at.unwrap_or(0.5).clamp(0.0, 1.0);
        let (lx, ly) = (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
        let role = c
            .role
            .as_deref()
            .and_then(|r| theme.role(r))
            .or_else(|| theme.role("label"))
            .unwrap_or_else(|| theme.body());
        let text: String = c
            .text
            .iter()
            .map(|p| p.plain_text())
            .collect::<Vec<_>>()
            .join(" ");
        let w = fonts.measure(&text, &role.family, role.size, role.weight);
        // A halo of the page ground so the label stays readable over the line.
        let bg = theme.bg();
        let pad = 3.0;
        let _ = write!(
            s,
            r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{}"/>"#,
            lx - w / 2.0 - pad,
            ly - role.size * 0.7 - pad,
            w + pad * 2.0,
            role.size + pad * 2.0,
            bg.hex()
        );
        let _ = write!(
            s,
            r#"<text x="{lx}" y="{}" font-family="{}" font-size="{}" fill="{}" text-anchor="middle">{}</text>"#,
            ly + role.size * 0.3,
            escape(&css_font_family(&role.family)),
            role.size,
            theme.color_or_fg(Some(&role.color)).hex(),
            escape(&text)
        );
    }
}

fn emit_chart_item(
    s: &mut String,
    item: &ChartItem,
    _theme: &Theme,
    _page_id: &str,
    _obj_id: &str,
    _diags: &mut [Diagnostic],
) {
    match item {
        ChartItem::Rect {
            x,
            y,
            w,
            h,
            fill,
            opacity,
        } => {
            let op = if *opacity < 1.0 {
                format!(r#" fill-opacity="{opacity}""#)
            } else {
                String::new()
            };
            let _ = write!(
                s,
                r#"<rect x="{x}" y="{y}" width="{w}" height="{h}" fill="{}"{op}/>"#,
                fill.hex()
            );
        }
        ChartItem::Path {
            points,
            stroke,
            width,
            dash,
        } => {
            if points.len() < 2 {
                return;
            }
            let d: String = points
                .iter()
                .enumerate()
                .map(|(i, (x, y))| {
                    if i == 0 {
                        format!("M{x} {y}")
                    } else {
                        format!(" L{x} {y}")
                    }
                })
                .collect();
            let dash_attr = match dash {
                Some(d) if !d.is_empty() => format!(
                    r#" stroke-dasharray="{}""#,
                    d.iter()
                        .map(|v| v.to_string())
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
                _ => String::new(),
            };
            let _ = write!(
                s,
                r#"<path d="{d}" fill="none" stroke="{}" stroke-width="{width}"{dash_attr} stroke-linecap="round" stroke-linejoin="round"/>"#,
                stroke.hex()
            );
        }
        ChartItem::Circle { cx, cy, r, fill } => {
            let _ = write!(
                s,
                r#"<circle cx="{cx}" cy="{cy}" r="{r}" fill="{}"/>"#,
                fill.hex()
            );
        }
        ChartItem::Polygon {
            points,
            fill,
            opacity,
        } => {
            if points.len() < 3 {
                return;
            }
            let d: String = points
                .iter()
                .enumerate()
                .map(|(i, (x, y))| {
                    if i == 0 {
                        format!("M{x} {y}")
                    } else {
                        format!(" L{x} {y}")
                    }
                })
                .collect();
            let op = if *opacity < 1.0 {
                format!(r#" fill-opacity="{opacity}""#)
            } else {
                String::new()
            };
            let _ = write!(s, r#"<path d="{d} Z" fill="{}"{op}/>"#, fill.hex());
        }
        ChartItem::Text {
            x,
            y,
            text,
            size,
            weight,
            color,
            anchor,
            families,
        } => {
            let a = match anchor {
                TextAnchor::Start => "start",
                TextAnchor::Middle => "middle",
                TextAnchor::End => "end",
            };
            let _ = write!(
                s,
                r#"<text x="{x}" y="{y}" font-family="{}" font-size="{size}" font-weight="{weight}" fill="{}" text-anchor="{a}">{}</text>"#,
                escape(&css_font_family(families)),
                color.hex(),
                escape(text)
            );
        }
    }
}

/// XML-escape text content and attribute values. Everything user-authored
/// passes through here before touching the SVG — the board is agent-written
/// input and gets no chance to inject markup into our own render.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::FontStack;

    fn board(json: &str) -> Board {
        let mut b = crate::parse(json).unwrap();
        crate::normalize(&mut b);
        b
    }

    const DECK: &str = r#"{
      "format": "chimaera.board", "formatVersion": 1,
      "title": "t",
      "canvas": { "size": [960, 540] },
      "pages": [{
        "id": "p1",
        "objects": [
          { "id": "title", "type": "text", "role": "title", "at": [72, 64], "size": [816, 80],
            "text": ["The parser rewrite is 3× faster"] },
          { "id": "chart", "type": "chart", "at": [72, 176], "size": [480, 288],
            "data": { "origin": "command", "values": [
              {"f": "large", "ms": 812, "build": "before"},
              {"f": "large", "ms": 244, "build": "after"}]},
            "x": {"field": "f"}, "y": {"field": "ms"}, "color": {"field": "build"},
            "marks": [{"mark": "bar", "stack": "group"}] },
          { "id": "callout", "type": "shape", "geo": "roundRect", "at": [600, 200], "size": [288, 96],
            "fill": "@surface", "stroke": {"color": "@accent1", "width": 1.5},
            "text": [{"runs": [{"t": "3.3× median", "b": true}]}] },
          { "id": "arrow", "type": "connector", "geo": "straight",
            "from": {"object": "callout", "side": "left"},
            "to": {"object": "chart", "side": "right"},
            "stroke": {"color": "@fg", "width": 1.5}, "tailEnd": "arrow",
            "text": ["median"] }
        ]
      }]
    }"#;

    #[test]
    fn a_full_page_renders_to_a_real_png() {
        let b = board(DECK);
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let out = render_page(&b, 0, &theme, &fonts, RasterParams::default()).unwrap();
        assert_eq!(out.width, 1920);
        assert_eq!(out.height, 1080);
        assert_eq!(&out.png[..8], b"\x89PNG\r\n\x1a\n");
        // Sub-floor errors would mean the theme's own roles violate their own
        // floors — that must never happen with bundled themes.
        assert!(
            !out.diagnostics
                .iter()
                .any(|d| d.severity == Severity::Error),
            "{:?}",
            out.diagnostics
        );
    }

    #[test]
    fn rendering_is_deterministic() {
        let b = board(DECK);
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let a = render_page(&b, 0, &theme, &fonts, RasterParams::default()).unwrap();
        let c = render_page(&b, 0, &theme, &fonts, RasterParams::default()).unwrap();
        assert_eq!(a.png, c.png, "same board, same bytes");
    }

    #[test]
    fn the_pixel_ceiling_refuses_rather_than_allocating() {
        let mut b = board(DECK);
        b.canvas.size = [100_000.0, 100_000.0];
        let err = render_page(
            &b,
            0,
            &crate::theme::default_for(true),
            &FontStack::new(&[]),
            RasterParams::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("ceiling"), "{err}");
    }

    #[test]
    fn markup_in_text_cannot_reach_the_svg() {
        let b = board(
            r#"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[400,200]},
                "pages":[{"id":"p","objects":[
                  {"id":"t","type":"text","at":[8,8],"size":[384,80],
                   "text":["<script>alert(1)</script> & \"quotes\""]}]}]}"#,
        );
        let theme = crate::theme::default_for(false);
        let fonts = FontStack::new(&[]);
        let mut diags = Vec::new();
        let svg = page_svg(&b, &b.pages[0], &theme, &fonts, None, &mut diags).unwrap();
        assert!(!svg.contains("<script"), "unescaped markup: {svg}");
        assert!(svg.contains("&lt;script&gt;"));
    }

    #[test]
    fn a_table_renders_its_grid_and_cell_text() {
        let b = board(
            r#"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[960,540]},
                "pages":[{"id":"p","objects":[
                  {"id":"tb","type":"table","at":[80,80],"size":[480,160],"header":true,
                   "rows":[["Fixture","Before","After"],
                           ["large.json","812","244"]]}]}]}"#,
        );
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let mut diags = Vec::new();
        let svg = page_svg(&b, &b.pages[0], &theme, &fonts, None, &mut diags).unwrap();
        for text in ["Fixture", "Before", "After", "large.json", "812", "244"] {
            assert!(svg.contains(text), "missing cell text {text:?}: {svg}");
        }
        // The hairline grid: 3 horizontal lines (2 rows) + 4 vertical (3 cols),
        // all in the theme's edge color.
        let edge = theme.color("@edge").unwrap().hex();
        let grid_lines = svg
            .matches(&format!(r#"stroke="{edge}" stroke-width="1""#))
            .count();
        assert_eq!(grid_lines, 7, "{svg}");
        // The header row ground is the surface token.
        let surface = theme.color("@surface").unwrap().hex();
        assert!(
            svg.contains(&format!(r#"height="80" fill="{surface}""#)),
            "{svg}"
        );
        // Header cells render bold; body cells at the role weight.
        assert!(svg.contains(r#"font-weight="700""#), "{svg}");
        assert!(
            !diags.iter().any(|d| d.severity == Severity::Error),
            "{diags:?}"
        );
    }

    #[test]
    fn an_overfull_table_cell_reports_rather_than_resizing() {
        let b = board(
            r#"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[960,540]},
                "pages":[{"id":"p","objects":[
                  {"id":"tb","type":"table","at":[80,80],"size":[240,48],
                   "rows":[["a cell holding far more prose than a 24 pt row can seat","b"],
                           ["c","d"]]}]}]}"#,
        );
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let mut diags = Vec::new();
        let _ = page_svg(&b, &b.pages[0], &theme, &fonts, None, &mut diags).unwrap();
        assert!(
            diags
                .iter()
                .any(|d| d.object.as_deref() == Some("tb") && d.message.contains("overfull")),
            "{diags:?}"
        );
    }

    #[test]
    fn jpeg_preview_encodes() {
        let b = board(DECK);
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let jpeg = render_page_jpeg(
            &b,
            0,
            &theme,
            &fonts,
            RasterParams {
                scale: 1.0,
                ..Default::default()
            },
            80,
        )
        .unwrap();
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "JPEG SOI marker");
    }

    #[test]
    fn render_keys_change_with_content_and_params() {
        let theme = crate::theme::default_for(true);
        let a = render_key("board-a", &theme, 0, RasterParams::default());
        let b = render_key("board-b", &theme, 0, RasterParams::default());
        let c = render_key("board-a", &theme, 1, RasterParams::default());
        let d = render_key(
            "board-a",
            &theme,
            0,
            RasterParams {
                scale: 1.0,
                ..Default::default()
            },
        );
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
        assert_eq!(a, render_key("board-a", &theme, 0, RasterParams::default()));
    }

    #[test]
    fn an_unknown_geometry_draws_its_box_and_says_so() {
        let b = board(
            r#"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[400,200]},
                "pages":[{"id":"p","objects":[
                  {"id":"s","type":"shape","geo":"dodecahedron","at":[8,8],"size":[80,80]}]}]}"#,
        );
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let out = render_page(
            &b,
            0,
            &theme,
            &fonts,
            RasterParams {
                scale: 1.0,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(out
            .diagnostics
            .iter()
            .any(|d| d.message.contains("dodecahedron")));
    }

    #[test]
    fn preset_furniture_draws_on_content_pages_but_not_the_cover() {
        let b = board(
            r#"{"format":"chimaera.board","formatVersion":1,"title":"Review",
                "canvas":{"preset":"design-review","size":[960,540]},
                "pages":[
                  {"id":"cover","objects":[
                    {"id":"t","type":"text","role":"title","at":[72,224],"size":[816,88],
                     "text":["The parser rewrite"]}]},
                  {"id":"bench","objects":[
                    {"id":"h","type":"text","role":"heading","at":[72,64],"size":[816,56],
                     "text":["Numbers"]}]}]}"#,
        );
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let mut diags = Vec::new();
        let cover = page_svg(&b, &b.pages[0], &theme, &fonts, None, &mut diags).unwrap();
        let second = page_svg(&b, &b.pages[1], &theme, &fonts, None, &mut diags).unwrap();
        // design-review suppresses the page number on the cover, not after.
        assert!(!cover.contains("1 / 2"), "{cover}");
        assert!(second.contains("2 / 2"), "{second}");
        // The footer carries the board title on both (empty preset text).
        assert!(second.contains("Review"), "{second}");
        // Furniture is generated per render, never written into the board.
        assert!(!crate::to_string(&b).unwrap().contains("furniture/"));
    }

    // --- images ---------------------------------------------------------

    fn tmp_workspace(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-board-render-img-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn solid_png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut pm = tiny_skia::Pixmap::new(w, h).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(
            rgba[0], rgba[1], rgba[2], rgba[3],
        ));
        pm.encode_png().unwrap()
    }

    fn image_board(src: &str, extra_fields: &str) -> Board {
        board(&format!(
            r#"{{"format":"chimaera.board","formatVersion":1,
                "canvas":{{"size":[96, 96]}},
                "pages":[{{"id":"p","objects":[
                  {{"id":"fig","type":"image","src":"{src}",
                   "at":[8,8],"size":[80,80]{extra_fields}}}]}}]}}"#
        ))
    }

    fn render_ws(b: &Board, ws: &std::path::Path) -> RenderOutput {
        render_page(
            b,
            0,
            &crate::theme::default_for(true),
            &FontStack::new(&[]),
            RasterParams {
                scale: 1.0,
                workspace: Some(ws.to_path_buf()),
            },
        )
        .unwrap()
    }

    #[test]
    fn a_png_lands_as_real_pixels_via_a_data_uri() {
        let ws = tmp_workspace("png");
        std::fs::write(ws.join("fig.png"), solid_png(2, 2, [255, 0, 0, 255])).unwrap();
        let b = image_board("fig.png", "");
        let out = render_ws(&b, &ws);
        assert!(
            !out.diagnostics
                .iter()
                .any(|d| d.severity == Severity::Error),
            "{:?}",
            out.diagnostics
        );
        // The Info diagnostic carries the computed effective dpi.
        assert!(
            out.diagnostics
                .iter()
                .any(|d| d.severity == Severity::Info && d.message.contains("dpi")),
            "{:?}",
            out.diagnostics
        );
        let pm = tiny_skia::Pixmap::decode_png(&out.png).unwrap();
        let px = pm.pixel(48, 48).unwrap().demultiply();
        assert_eq!(
            (px.red(), px.green(), px.blue()),
            (255, 0, 0),
            "the fixture color shows at the image's center"
        );
    }

    #[test]
    fn src_rect_crop_arithmetic_and_a_cropped_render() {
        // Fractions cut from each side of the natural space.
        assert_eq!(crop_rect(None, 200.0, 100.0), (0.0, 0.0, 200.0, 100.0));
        assert_eq!(
            crop_rect(Some([0.25, 0.1, 0.25, 0.4]), 200.0, 100.0),
            (50.0, 10.0, 100.0, 50.0)
        );
        // A degenerate crop clamps to one unit instead of inverting.
        assert_eq!(crop_rect(Some([0.9, 0.0, 0.9, 0.0]), 10.0, 10.0).2, 1.0);
        assert_eq!(
            crop_rect(Some([f64::NAN, 0.0, 0.0, 0.0]), 10.0, 10.0).0,
            0.0
        );

        // Left pixel red, right pixel blue; cropping the left half away must
        // show only blue.
        let mut pm = tiny_skia::Pixmap::new(2, 1).unwrap();
        {
            let px = pm.pixels_mut();
            px[0] = tiny_skia::ColorU8::from_rgba(255, 0, 0, 255).premultiply();
            px[1] = tiny_skia::ColorU8::from_rgba(0, 0, 255, 255).premultiply();
        }
        let ws = tmp_workspace("crop");
        std::fs::write(ws.join("half.png"), pm.encode_png().unwrap()).unwrap();
        let b = image_board("half.png", r#","srcRect":[0.5, 0, 0, 0]"#);
        let out = render_ws(&b, &ws);
        let rendered = tiny_skia::Pixmap::decode_png(&out.png).unwrap();
        // Probe well inside the crop: bilinear sampling blends at the source
        // pixel boundary, but deep in the visible half only blue remains.
        let px = rendered.pixel(80, 48).unwrap().demultiply();
        assert_eq!(
            (px.red(), px.green(), px.blue()),
            (0, 0, 255),
            "only the uncropped half may show"
        );
    }

    #[test]
    fn a_monochrome_svg_tints_and_a_polychrome_one_warns() {
        let ws = tmp_workspace("tint");
        std::fs::write(
            ws.join("mono.svg"),
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
                <rect width="10" height="10" fill="#333333"/></svg>"##,
        )
        .unwrap();
        std::fs::write(
            ws.join("poly.svg"),
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
                <rect width="4" height="10" fill="#ff0000"/>
                <rect x="4" width="3" height="10" fill="#00ff00"/>
                <rect x="7" width="3" height="10" fill="#0000ff"/></svg>"##,
        )
        .unwrap();
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);

        let b = image_board("mono.svg", r#","tint":"@accent1""#);
        let mut diags = Vec::new();
        let svg = page_svg(&b, &b.pages[0], &theme, &fonts, Some(&ws), &mut diags).unwrap();
        let accent = theme.color("@accent1").unwrap().hex();
        assert!(svg.contains(&accent), "tinted to the theme token: {svg}");
        assert!(!svg.contains("#333333"), "{svg}");
        assert!(
            !diags.iter().any(|d| d.message.contains("tint")),
            "{diags:?}"
        );

        let b = image_board("poly.svg", r#","tint":"@accent1""#);
        let mut diags = Vec::new();
        let svg = page_svg(&b, &b.pages[0], &theme, &fonts, Some(&ws), &mut diags).unwrap();
        assert!(
            diags.iter().any(|d| d
                .message
                .contains("tint needs a monochrome source (found 3 colors)")),
            "{diags:?}"
        );
        assert!(svg.contains("#ff0000"), "untinted polychrome kept: {svg}");
    }

    #[test]
    fn a_missing_image_keeps_the_placeholder_and_names_the_path() {
        let ws = tmp_workspace("missing");
        let b = image_board("does/not/exist.png", "");
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let mut diags = Vec::new();
        let svg = page_svg(&b, &b.pages[0], &theme, &fonts, Some(&ws), &mut diags).unwrap();
        assert!(svg.contains("stroke-dasharray"), "placeholder box: {svg}");
        assert!(
            diags.iter().any(|d| d.severity == Severity::Warning
                && d.message.contains("exist.png")
                && d.message.contains("not found")),
            "{diags:?}"
        );
    }

    #[test]
    fn no_workspace_means_a_placeholder_with_the_reason() {
        let b = image_board("fig.png", "");
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let mut diags = Vec::new();
        let svg = page_svg(&b, &b.pages[0], &theme, &fonts, None, &mut diags).unwrap();
        assert!(svg.contains("stroke-dasharray"), "{svg}");
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("needs a workspace root")),
            "{diags:?}"
        );
    }

    #[test]
    fn an_inlined_svg_is_sanitized_before_it_reaches_the_page() {
        let ws = tmp_workspace("sanitize");
        std::fs::write(
            ws.join("dirty.svg"),
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
                <script>alert(1)</script>
                <rect width="10" height="10" fill="#00aa00"/></svg>"##,
        )
        .unwrap();
        let b = image_board("dirty.svg", "");
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let mut diags = Vec::new();
        let svg = page_svg(&b, &b.pages[0], &theme, &fonts, Some(&ws), &mut diags).unwrap();
        assert!(!svg.contains("<script"), "{svg}");
        assert!(svg.contains("#00aa00"), "the drawing survives: {svg}");
        // And the whole page still parses through the render stack.
        let opt = usvg::Options {
            fontdb: fonts.db(),
            ..Default::default()
        };
        usvg::Tree::from_str(&svg, &opt).expect("page with an inlined figure parses");
    }
}
