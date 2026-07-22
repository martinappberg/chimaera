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

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::chart::{ChartItem, TextAnchor};
use crate::layout::{css_font_family, FontStack};
use crate::normalize::{Diagnostic, Severity};
use crate::schema::{
    Align, Board, ConnectorObject, Frame, Object, Page, Paragraph, Run, Side, VAlign,
};
use crate::theme::{Rgb, Theme};

/// The raster ceiling, in pixels. 12 Mpx is a 4K slide at 2× with headroom;
/// past it a render request is a mistake or an attack on daemon RSS, and the
/// answer is an error rather than an allocation.
pub const MAX_PIXELS: u64 = 12_000_000;

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
#[derive(Debug, Clone, Copy)]
pub struct RasterParams {
    /// Device scale; 2.0 is the default everywhere Board renders for a UI.
    pub scale: f64,
}

impl Default for RasterParams {
    fn default() -> Self {
        RasterParams { scale: 2.0 }
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
    let px_w = (w * params.scale).round() as u64;
    let px_h = (h * params.scale).round() as u64;
    if px_w == 0 || px_h == 0 {
        anyhow::bail!("canvas rasterizes to zero pixels");
    }
    if px_w * px_h > MAX_PIXELS {
        anyhow::bail!(
            "render would be {px_w}×{px_h} px ({} Mpx), over the {} Mpx ceiling",
            px_w * px_h / 1_000_000,
            MAX_PIXELS / 1_000_000
        );
    }

    let mut diagnostics = Vec::new();
    let svg = page_svg(board, page, theme, fonts, &mut diagnostics)?;

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

fn page_svg(
    board: &Board,
    page: &Page,
    theme: &Theme,
    fonts: &FontStack,
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

    let index = crate::normalize::index_page(page);

    for obj in &page.objects {
        emit_object(&mut s, obj, page, board, theme, fonts, &index, diags);
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
    index: &std::collections::BTreeMap<String, Frame>,
    diags: &mut Vec<Diagnostic>,
) {
    // Off-canvas is a warning, not silence: the object may be intentionally
    // parked, but nobody parks something by accident and finds out from a
    // blank render.
    if let Some(f) = obj.frame() {
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
            let Some(frame) = obj.frame() else {
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
            let Some(frame) = obj.frame() else {
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
        Object::Connector(c) => emit_connector(s, c, theme, fonts, index, diags, &page.id),
        Object::Image(img) => {
            diags.push(
                Diagnostic::new(
                    Severity::Warning,
                    format!(
                        "image {:?} is not rendered in this slice; a placeholder marks its box",
                        img.src
                    ),
                )
                .at(&page.id, obj.id()),
            );
            if let Some(f) = obj.frame() {
                let edge = theme.color("@edge").unwrap_or(theme.bg());
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
        Object::Group(g) => {
            for child in &g.objects {
                emit_object(s, child, page, board, theme, fonts, index, diags);
            }
        }
        Object::Chart(c) => {
            let Some(frame) = obj.frame() else {
                diags.push(
                    Diagnostic::new(Severity::Warning, "chart has no position; skipped")
                        .at(&page.id, obj.id()),
                );
                return;
            };
            let scene = crate::chart::build(c, frame, theme, fonts);
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
        let svg = page_svg(&b, &b.pages[0], &theme, &fonts, &mut diags).unwrap();
        assert!(!svg.contains("<script"), "unescaped markup: {svg}");
        assert!(svg.contains("&lt;script&gt;"));
    }

    #[test]
    fn jpeg_preview_encodes() {
        let b = board(DECK);
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let jpeg =
            render_page_jpeg(&b, 0, &theme, &fonts, RasterParams { scale: 1.0 }, 80).unwrap();
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "JPEG SOI marker");
    }

    #[test]
    fn render_keys_change_with_content_and_params() {
        let theme = crate::theme::default_for(true);
        let a = render_key("board-a", &theme, 0, RasterParams::default());
        let b = render_key("board-b", &theme, 0, RasterParams::default());
        let c = render_key("board-a", &theme, 1, RasterParams::default());
        let d = render_key("board-a", &theme, 0, RasterParams { scale: 1.0 });
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
        let out = render_page(&b, 0, &theme, &fonts, RasterParams { scale: 1.0 }).unwrap();
        assert!(out
            .diagnostics
            .iter()
            .any(|d| d.message.contains("dodecahedron")));
    }
}
