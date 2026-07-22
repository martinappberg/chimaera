//! A pure-Rust OOXML writer: a normalized board out as a native, *editable*
//! `.pptx` — real text boxes, real preset and custom geometry, real
//! connectors — with a declared per-object fate.
//!
//! No Python, no shelling out: the daemon is one static binary on login nodes
//! with no system dependencies, so the subset of DrawingML Board emits is
//! written directly. Two custGeom rules are non-negotiable, both verified to
//! fail against shipping OOXML consumers (see docs/board-plan.md §3.6):
//!
//! 1. **Never emit `a:arcTo`** — its parameterization is unlike SVG's and
//!    consumers disagree to the point of drawing nothing. Every arc in SVG
//!    path data is flattened to cubic Béziers (≤4 segments per arc).
//! 2. **`a:path` coordinates are shape-local EMU with `w`/`h` equal to the
//!    shape's `ext`** — a normalized path space renders at ~4% of intended
//!    size in a real consumer.
//!
//! Layout decisions this writer bakes in, so they are stated rather than
//! discovered: `geo: "line"` exports as a two-point custGeom path (matching
//! the renderer's horizontal mid-box line exactly, where the `line` preset
//! would draw a corner-to-corner diagonal); a connector's bound label exports
//! as a separate small text shape at `label_at` along the resolved segment;
//! text objects with `role: "title"` export as plain shapes in v1 — real
//! `p:ph` placeholders are a later slice, and each such object's fate says so.
//!
//! Determinism: identical input produces identical bytes. Fixed part order,
//! fixed zip mtime (2000-01-01), fixed docProps dates, integer-only geometry
//! (EMU = pt × 12700), and no wall clock anywhere.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Write as _;

use anyhow::{Context, Result};

use super::{ExportReport, ExportTier, ObjectFate};
use crate::imginfo::{jpeg_dimensions, png_dimensions, sniff_image, ImgKind};
use crate::layout::FontStack;
use crate::schema::{
    Align, Board, ChartObject, ConnectorObject, EndPoint, Frame, GroupObject, ImageObject, Object,
    Page, Paragraph, Run, ShapeObject, Side, Stroke, TableObject, TextObject, VAlign,
};
use crate::theme::{Rgb, Theme, TypeRole};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub(super) const XML_DECL: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#;
pub(super) const NS_A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
pub(super) const NS_R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const NS_P: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
pub(super) const NS_C: &str = "http://schemas.openxmlformats.org/drawingml/2006/chart";

const REL_OFFICE_DOC: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";
const REL_CORE: &str =
    "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties";
const REL_APP: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties";
const REL_SLIDE_MASTER: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster";
const REL_SLIDE_LAYOUT: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout";
const REL_SLIDE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide";
const REL_THEME: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme";
const REL_PRES_PROPS: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/presProps";
const REL_NOTES_MASTER: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesMaster";
const REL_NOTES_SLIDE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesSlide";
const REL_IMAGE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
const REL_HYPERLINK: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink";
const REL_TABLE_STYLES: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/tableStyles";

/// The style GUID every emitted `a:tbl` names. tableStyles.xml is one empty
/// `a:tblStyleLst` carrying only this `def` — Board styles every cell
/// explicitly, so no consumer's built-in table styling can restyle a deck.
/// The GUID is PowerPoint's own default ("Medium Style 2 - Accent 1"), which
/// keeps "Insert row" in a real consumer styling sanely.
const TABLE_STYLE_ID: &str = "{5C22544A-7EE6-4342-B048-85BDC9FD1C3A}";
const REL_CHART: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart";
const REL_PACKAGE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/package";

/// The empty root group every `p:spTree` opens with.
const ROOT_GRP: &str = concat!(
    r#"<p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>"#,
    r#"<p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/>"#,
    r#"<a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr>"#
);

// ---------------------------------------------------------------------------
// Arithmetic
// ---------------------------------------------------------------------------

/// Points to EMU, exactly: 1 pt = 12700 EMU. Non-finite input degrades to 0
/// rather than panicking — malformed geometry is a reported fate, not a crash.
pub(super) fn emu(pt: f64) -> i64 {
    if !pt.is_finite() {
        return 0;
    }
    (pt * 12700.0).round() as i64
}

/// Points to DrawingML font size units (hundredths of a point), clamped to
/// the schema's legal 1..4000 pt range.
fn sz100(pt: f64) -> i64 {
    if !pt.is_finite() {
        return 100;
    }
    ((pt * 100.0).round() as i64).clamp(100, 400_000)
}

/// A 0..=1 fraction as a DrawingML percentage (thousandths of a percent).
fn pct100k(v: f64) -> i64 {
    if !v.is_finite() {
        return 100_000;
    }
    (v.clamp(0.0, 1.0) * 100_000.0).round() as i64
}

/// Degrees to DrawingML 60000ths-of-a-degree.
fn rot60k(deg: f64) -> i64 {
    if !deg.is_finite() {
        return 0;
    }
    (deg.rem_euclid(360.0) * 60_000.0).round() as i64
}

/// XML-escape text content and attribute values. Every user-authored string
/// passes through here — a board is agent-written input and gets no chance to
/// inject markup.
pub(super) fn esc(text: &str) -> String {
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

fn hex6(rgb: Rgb) -> String {
    format!("{:02X}{:02X}{:02X}", rgb.r, rgb.g, rgb.b)
}

// ---------------------------------------------------------------------------
// Colors
// ---------------------------------------------------------------------------

/// A color reference as DrawingML. `@bg`/`@fg`/`@accent1` map onto the
/// generated `clrScheme` (`bg1`/`tx1`/`accent1`) so a slide pasted into a
/// themed deck re-themes natively; every other token — and every literal —
/// resolves through the theme and lands as `srgbClr`.
fn color_choice(theme: &Theme, reference: Option<&str>, alpha: Option<f64>) -> String {
    let alpha_child = match alpha {
        Some(a) if a < 1.0 => format!(r#"<a:alpha val="{}"/>"#, pct100k(a)),
        _ => String::new(),
    };
    if let Some(r) = reference {
        let scheme = match r {
            "@bg" => Some("bg1"),
            "@fg" => Some("tx1"),
            "@accent1" => Some("accent1"),
            _ => None,
        };
        if let Some(name) = scheme {
            if theme.color(r).is_some() {
                return if alpha_child.is_empty() {
                    format!(r#"<a:schemeClr val="{name}"/>"#)
                } else {
                    format!(r#"<a:schemeClr val="{name}">{alpha_child}</a:schemeClr>"#)
                };
            }
        }
    }
    srgb(theme.color_or_fg(reference), alpha)
}

/// A resolved RGB as `srgbClr`, with an optional alpha child.
pub(super) fn srgb(rgb: Rgb, alpha: Option<f64>) -> String {
    match alpha {
        Some(a) if a < 1.0 => format!(
            r#"<a:srgbClr val="{}"><a:alpha val="{}"/></a:srgbClr>"#,
            hex6(rgb),
            pct100k(a)
        ),
        _ => format!(r#"<a:srgbClr val="{}"/>"#, hex6(rgb)),
    }
}

fn solid_fill(theme: &Theme, reference: Option<&str>, alpha: Option<f64>) -> String {
    format!(
        "<a:solidFill>{}</a:solidFill>",
        color_choice(theme, reference, alpha)
    )
}

// ---------------------------------------------------------------------------
// Geometry: xfrm, presets, custGeom
// ---------------------------------------------------------------------------

/// A shape transform. Extents are clamped to ≥1 EMU — a zero-extent xfrm is
/// degenerate in real consumers, and 1 EMU (1/914400 in) is invisible.
fn xfrm(f: Frame, rot: Option<f64>, flip_h: bool, flip_v: bool) -> String {
    let mut attrs = String::new();
    if let Some(r) = rot {
        if r != 0.0 && r.is_finite() {
            let _ = write!(attrs, r#" rot="{}""#, rot60k(r));
        }
    }
    if flip_h {
        attrs.push_str(r#" flipH="1""#);
    }
    if flip_v {
        attrs.push_str(r#" flipV="1""#);
    }
    format!(
        r#"<a:xfrm{attrs}><a:off x="{}" y="{}"/><a:ext cx="{}" cy="{}"/></a:xfrm>"#,
        emu(f.x),
        emu(f.y),
        emu(f.w).max(1),
        emu(f.h).max(1)
    )
}

/// A group transform with an identity child space: `chOff` = `off`,
/// `chExt` = `ext`, because grouped children carry page-absolute geometry.
fn grp_xfrm(f: Frame) -> String {
    let (x, y) = (emu(f.x), emu(f.y));
    let (cx, cy) = (emu(f.w).max(1), emu(f.h).max(1));
    format!(
        r#"<a:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{cx}" cy="{cy}"/><a:chOff x="{x}" y="{y}"/><a:chExt cx="{cx}" cy="{cy}"/></a:xfrm>"#
    )
}

fn prst_geom(prst: &str, av: &str) -> String {
    if av.is_empty() {
        format!(r#"<a:prstGeom prst="{prst}"><a:avLst/></a:prstGeom>"#)
    } else {
        format!(r#"<a:prstGeom prst="{prst}"><a:avLst>{av}</a:avLst></a:prstGeom>"#)
    }
}

/// One segment of a parsed path, in page-space points. Arcs and quadratics
/// are already gone by the time this exists — everything is lines and cubics.
#[derive(Debug, Clone, Copy)]
enum Seg {
    Move([f64; 2]),
    Line([f64; 2]),
    Cubic([f64; 2], [f64; 2], [f64; 2]),
    Close,
}

/// Segments as `a:custGeom`, in shape-local EMU with `w`/`h` equal to the
/// shape's ext (rule 2). Never emits `a:arcTo` (rule 1) — there is no arc
/// variant to emit.
fn cust_geom(segs: &[Seg], f: Frame) -> String {
    let w = emu(f.w).max(1);
    let h = emu(f.h).max(1);
    let px = |p: [f64; 2]| (emu(p[0] - f.x), emu(p[1] - f.y));
    let mut path = String::new();
    for seg in segs {
        match seg {
            Seg::Move(p) => {
                let (x, y) = px(*p);
                let _ = write!(path, r#"<a:moveTo><a:pt x="{x}" y="{y}"/></a:moveTo>"#);
            }
            Seg::Line(p) => {
                let (x, y) = px(*p);
                let _ = write!(path, r#"<a:lnTo><a:pt x="{x}" y="{y}"/></a:lnTo>"#);
            }
            Seg::Cubic(c1, c2, p) => {
                let (x1, y1) = px(*c1);
                let (x2, y2) = px(*c2);
                let (x, y) = px(*p);
                let _ = write!(
                    path,
                    r#"<a:cubicBezTo><a:pt x="{x1}" y="{y1}"/><a:pt x="{x2}" y="{y2}"/><a:pt x="{x}" y="{y}"/></a:cubicBezTo>"#
                );
            }
            Seg::Close => path.push_str("<a:close/>"),
        }
    }
    format!(
        r#"<a:custGeom><a:avLst/><a:gdLst/><a:ahLst/><a:cxnLst/><a:rect l="0" t="0" r="{w}" b="{h}"/><a:pathLst><a:path w="{w}" h="{h}">{path}</a:path></a:pathLst></a:custGeom>"#
    )
}

// ---------------------------------------------------------------------------
// SVG path parsing (M L H V C S Q T A Z, absolute and relative)
// ---------------------------------------------------------------------------

struct Toks<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Toks<'a> {
    fn new(s: &'a str) -> Self {
        Toks {
            b: s.as_bytes(),
            i: 0,
        }
    }
    fn skip_sep(&mut self) {
        while self.i < self.b.len()
            && (self.b[self.i].is_ascii_whitespace() || self.b[self.i] == b',')
        {
            self.i += 1;
        }
    }
    fn done(&mut self) -> bool {
        self.skip_sep();
        self.i >= self.b.len()
    }
    fn next_cmd(&mut self) -> Option<char> {
        self.skip_sep();
        let c = *self.b.get(self.i)? as char;
        if c.is_ascii_alphabetic() {
            self.i += 1;
            Some(c)
        } else {
            None
        }
    }
    fn num(&mut self) -> Result<f64, String> {
        self.skip_sep();
        let start = self.i;
        if self.i < self.b.len() && (self.b[self.i] == b'+' || self.b[self.i] == b'-') {
            self.i += 1;
        }
        let mut seen_dot = false;
        while self.i < self.b.len() {
            match self.b[self.i] {
                b'0'..=b'9' => self.i += 1,
                b'.' if !seen_dot => {
                    seen_dot = true;
                    self.i += 1;
                }
                _ => break,
            }
        }
        if self.i < self.b.len() && (self.b[self.i] == b'e' || self.b[self.i] == b'E') {
            let mark = self.i;
            self.i += 1;
            if self.i < self.b.len() && (self.b[self.i] == b'+' || self.b[self.i] == b'-') {
                self.i += 1;
            }
            let digits = self.i;
            while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
                self.i += 1;
            }
            if self.i == digits {
                self.i = mark; // not an exponent after all
            }
        }
        let text =
            std::str::from_utf8(&self.b[start..self.i]).map_err(|_| "bad utf8".to_string())?;
        let v: f64 = text
            .parse()
            .map_err(|_| format!("expected a number at byte {start}"))?;
        if !v.is_finite() {
            return Err(format!("non-finite number at byte {start}"));
        }
        Ok(v)
    }
    fn pair(&mut self) -> Result<[f64; 2], String> {
        Ok([self.num()?, self.num()?])
    }
}

/// Parse SVG path data into lines and cubics, flattening arcs. Errors are
/// strings; the caller degrades the shape with a reported fate — malformed
/// input never panics and never half-draws.
fn parse_svg_path(d: &str) -> Result<Vec<Seg>, String> {
    let mut t = Toks::new(d);
    let mut segs = Vec::new();
    let mut cur = [0.0f64, 0.0];
    let mut start = [0.0f64, 0.0];
    let mut prev_cubic: Option<[f64; 2]> = None;
    let mut prev_quad: Option<[f64; 2]> = None;
    let mut cmd: Option<char> = None;

    while !t.done() {
        let pos_before = t.i;
        if let Some(c) = t.next_cmd() {
            cmd = Some(c);
        }
        let c = cmd.ok_or_else(|| "path does not start with a command".to_string())?;
        if segs.is_empty() && !matches!(c, 'M' | 'm') {
            return Err(format!("path starts with {c:?}, not a moveto"));
        }
        let rel = c.is_ascii_lowercase();
        let base = if rel { cur } else { [0.0, 0.0] };
        let mut keep_cubic = false;
        let mut keep_quad = false;
        match c.to_ascii_uppercase() {
            'M' => {
                let p = t.pair()?;
                cur = [base[0] + p[0], base[1] + p[1]];
                start = cur;
                segs.push(Seg::Move(cur));
                // Subsequent pairs are implicit linetos.
                cmd = Some(if rel { 'l' } else { 'L' });
            }
            'L' => {
                let p = t.pair()?;
                cur = [base[0] + p[0], base[1] + p[1]];
                segs.push(Seg::Line(cur));
            }
            'H' => {
                let x = t.num()?;
                cur = [base[0] + x, cur[1]];
                segs.push(Seg::Line(cur));
            }
            'V' => {
                let y = t.num()?;
                cur = [cur[0], base[1] + y];
                segs.push(Seg::Line(cur));
            }
            'C' => {
                let c1 = t.pair()?;
                let c2 = t.pair()?;
                let p = t.pair()?;
                let c1 = [base[0] + c1[0], base[1] + c1[1]];
                let c2 = [base[0] + c2[0], base[1] + c2[1]];
                cur = [base[0] + p[0], base[1] + p[1]];
                segs.push(Seg::Cubic(c1, c2, cur));
                prev_cubic = Some(c2);
                keep_cubic = true;
            }
            'S' => {
                let c2 = t.pair()?;
                let p = t.pair()?;
                let c1 = match prev_cubic {
                    Some(pc) => [2.0 * cur[0] - pc[0], 2.0 * cur[1] - pc[1]],
                    None => cur,
                };
                let c2 = [base[0] + c2[0], base[1] + c2[1]];
                cur = [base[0] + p[0], base[1] + p[1]];
                segs.push(Seg::Cubic(c1, c2, cur));
                prev_cubic = Some(c2);
                keep_cubic = true;
            }
            'Q' => {
                let q = t.pair()?;
                let p = t.pair()?;
                let q = [base[0] + q[0], base[1] + q[1]];
                let end = [base[0] + p[0], base[1] + p[1]];
                segs.push(quad_to_cubic(cur, q, end));
                cur = end;
                prev_quad = Some(q);
                keep_quad = true;
            }
            'T' => {
                let p = t.pair()?;
                let q = match prev_quad {
                    Some(pq) => [2.0 * cur[0] - pq[0], 2.0 * cur[1] - pq[1]],
                    None => cur,
                };
                let end = [base[0] + p[0], base[1] + p[1]];
                segs.push(quad_to_cubic(cur, q, end));
                cur = end;
                prev_quad = Some(q);
                keep_quad = true;
            }
            'A' => {
                let rx = t.num()?;
                let ry = t.num()?;
                let rot = t.num()?;
                let large = t.num()? != 0.0;
                let sweep = t.num()? != 0.0;
                let p = t.pair()?;
                let end = [base[0] + p[0], base[1] + p[1]];
                arc_to_cubics(cur, rx, ry, rot, large, sweep, end, &mut segs);
                cur = end;
            }
            'Z' => {
                segs.push(Seg::Close);
                cur = start;
            }
            other => return Err(format!("unsupported path command {other:?}")),
        }
        if !keep_cubic {
            prev_cubic = None;
        }
        if !keep_quad {
            prev_quad = None;
        }
        if t.i == pos_before {
            // A command that consumed nothing (a bare repeated `Z` before a
            // number, say) would loop forever; refuse instead.
            return Err("path data stalled on an unexpected token".to_string());
        }
    }
    if segs.is_empty() {
        return Err("empty path".to_string());
    }
    Ok(segs)
}

fn quad_to_cubic(p0: [f64; 2], q: [f64; 2], p3: [f64; 2]) -> Seg {
    let c1 = [
        p0[0] + 2.0 / 3.0 * (q[0] - p0[0]),
        p0[1] + 2.0 / 3.0 * (q[1] - p0[1]),
    ];
    let c2 = [
        p3[0] + 2.0 / 3.0 * (q[0] - p3[0]),
        p3[1] + 2.0 / 3.0 * (q[1] - p3[1]),
    ];
    Seg::Cubic(c1, c2, p3)
}

/// An SVG elliptical arc as ≤4 cubic Béziers (~1e-4 relative error), per the
/// SVG endpoint-to-center conversion (spec appendix F.6.5). This is what
/// makes rule 1 free: no consumer ever sees an `a:arcTo`.
#[allow(clippy::too_many_arguments)]
fn arc_to_cubics(
    from: [f64; 2],
    rx: f64,
    ry: f64,
    xrot_deg: f64,
    large: bool,
    sweep: bool,
    to: [f64; 2],
    out: &mut Vec<Seg>,
) {
    let mut rx = rx.abs();
    let mut ry = ry.abs();
    let degenerate = rx < 1e-9
        || ry < 1e-9
        || ((from[0] - to[0]).abs() < 1e-12 && (from[1] - to[1]).abs() < 1e-12);
    if degenerate {
        out.push(Seg::Line(to));
        return;
    }
    let phi = xrot_deg.to_radians();
    let (sinp, cosp) = phi.sin_cos();
    let dx2 = (from[0] - to[0]) / 2.0;
    let dy2 = (from[1] - to[1]) / 2.0;
    let x1p = cosp * dx2 + sinp * dy2;
    let y1p = -sinp * dx2 + cosp * dy2;
    let lam = x1p * x1p / (rx * rx) + y1p * y1p / (ry * ry);
    if lam > 1.0 {
        let s = lam.sqrt();
        rx *= s;
        ry *= s;
    }
    let num = (rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p).max(0.0);
    let den = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
    let mut coef = if den.abs() < 1e-12 {
        0.0
    } else {
        (num / den).sqrt()
    };
    if large == sweep {
        coef = -coef;
    }
    let cxp = coef * rx * y1p / ry;
    let cyp = -coef * ry * x1p / rx;
    let cx = cosp * cxp - sinp * cyp + (from[0] + to[0]) / 2.0;
    let cy = sinp * cxp + cosp * cyp + (from[1] + to[1]) / 2.0;
    let ang = |x: f64, y: f64| y.atan2(x);
    let theta1 = ang((x1p - cxp) / rx, (y1p - cyp) / ry);
    let mut dtheta = ang((-x1p - cxp) / rx, (-y1p - cyp) / ry) - theta1;
    let tau = std::f64::consts::TAU;
    if !sweep && dtheta > 0.0 {
        dtheta -= tau;
    }
    if sweep && dtheta < 0.0 {
        dtheta += tau;
    }
    let n = ((dtheta.abs() / std::f64::consts::FRAC_PI_2).ceil() as usize).clamp(1, 4);
    let delta = dtheta / n as f64;
    let alpha = 4.0 / 3.0 * (delta / 4.0).tan();
    let point = |t: f64| {
        [
            cx + rx * t.cos() * cosp - ry * t.sin() * sinp,
            cy + rx * t.cos() * sinp + ry * t.sin() * cosp,
        ]
    };
    let deriv = |t: f64| {
        [
            -rx * t.sin() * cosp - ry * t.cos() * sinp,
            -rx * t.sin() * sinp + ry * t.cos() * cosp,
        ]
    };
    let mut th = theta1;
    for i in 0..n {
        let th2 = th + delta;
        let p0 = point(th);
        let p3 = if i == n - 1 { to } else { point(th2) };
        let d0 = deriv(th);
        let d3 = deriv(th2);
        let c1 = [p0[0] + alpha * d0[0], p0[1] + alpha * d0[1]];
        let c2 = [p3[0] - alpha * d3[0], p3[1] - alpha * d3[1]];
        out.push(Seg::Cubic(c1, c2, p3));
        th = th2;
    }
}

// ---------------------------------------------------------------------------
// Strokes
// ---------------------------------------------------------------------------

/// A stroke as `a:ln`. `extra_ends` carries connector arrowheads
/// (`a:headEnd`/`a:tailEnd`), which the schema orders after the join.
fn line_xml(theme: &Theme, stroke: &Stroke, default_width: f64, extra_ends: &str) -> String {
    let w = stroke.width.unwrap_or(default_width);
    let cap = match stroke.cap.as_deref() {
        Some("round") => r#" cap="rnd""#,
        Some("square") => r#" cap="sq""#,
        Some("butt") => r#" cap="flat""#,
        _ => "",
    };
    let fill = solid_fill(theme, stroke.color.as_deref(), stroke.opacity);
    let dash = dash_xml(stroke.dash.as_deref(), w);
    let join = match stroke.join.as_deref() {
        Some("bevel") => "<a:bevel/>",
        Some("miter") => r#"<a:miter lim="800000"/>"#,
        Some("round") => "<a:round/>",
        _ => "",
    };
    format!(
        r#"<a:ln w="{}"{cap}>{fill}{dash}{join}{extra_ends}</a:ln>"#,
        emu(w).max(1)
    )
}

const NO_LINE: &str = "<a:ln><a:noFill/></a:ln>";

/// A dash pattern. Two-element patterns map to the visually nearest preset
/// (`sysDot`/`dash`/`lgDash`); anything longer becomes `a:custDash`, whose
/// `d`/`sp` are percentages of the line width.
fn dash_xml(dash: Option<&[f64]>, width: f64) -> String {
    let Some(d) = dash else {
        return String::new();
    };
    if d.is_empty() || d.iter().any(|v| !v.is_finite() || *v < 0.0) {
        return String::new();
    }
    let w = width.max(0.25);
    if d.len() == 2 {
        let on = d[0] / w;
        let val = if on <= 1.5 {
            "sysDot"
        } else if on <= 4.5 {
            "dash"
        } else {
            "lgDash"
        };
        return format!(r#"<a:prstDash val="{val}"/>"#);
    }
    let mut pairs: Vec<f64> = d.to_vec();
    if pairs.len() % 2 == 1 {
        pairs.extend_from_slice(d); // SVG semantics: an odd list repeats doubled
    }
    let mut s = String::from("<a:custDash>");
    for ch in pairs.chunks(2) {
        let _ = write!(
            s,
            r#"<a:ds d="{}" sp="{}"/>"#,
            ((ch[0] / w) * 100_000.0).round().max(1.0) as i64,
            ((ch[1] / w) * 100_000.0).round().max(1.0) as i64
        );
    }
    s.push_str("</a:custDash>");
    s
}

// ---------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------

/// The family that actually resolves for a stack — the same resolution the
/// renderer draws with, so the deck names the face the board measured.
fn resolved_family(fonts: &FontStack, families: &[String], weight: u16) -> String {
    if let Some(f) = fonts.resolve(families, weight, false) {
        return f.family;
    }
    families
        .iter()
        .find(|f| {
            !matches!(
                f.to_ascii_lowercase().as_str(),
                "sans-serif" | "serif" | "monospace" | "cursive" | "fantasy"
            )
        })
        .cloned()
        .unwrap_or_else(|| "Calibri".to_string())
}

// ---------------------------------------------------------------------------
// The slide writer
// ---------------------------------------------------------------------------

struct Rel {
    kind: &'static str,
    target: String,
    external: bool,
}

struct Media {
    name: String,
    bytes: Vec<u8>,
}

/// State shared across all slides of one export.
struct Shared {
    media: Vec<Media>,
    media_by_src: BTreeMap<String, usize>,
    exts: BTreeSet<&'static str>,
    fates: Vec<ObjectFate>,
    /// Native `c:chart` parts (opt-in), numbered `chart1…` across the deck;
    /// each carries its own embedded workbook.
    charts: Vec<super::chart_xml::NativeChartPart>,
}

/// Everything needed while emitting one slide's `p:spTree`.
struct SlideWriter<'a> {
    theme: &'a Theme,
    fonts: &'a FontStack,
    /// The page being emitted — composite expansions (an inset's target
    /// image) look their subjects up here.
    page: &'a Page,
    /// Workspace root for source-bound chart data and image files. Without
    /// it, bound sources export as loud problems rather than silently empty.
    workspace: Option<std::path::PathBuf>,
    /// Page-space frames by object id, for connector endpoint resolution.
    index: BTreeMap<String, Frame>,
    /// Grouped shapes by default; `Native` tries a real `c:chart` per chart
    /// and degrades per-chart with a stated reason.
    chart_fidelity: ChartFidelity,
    xml: String,
    rels: Vec<Rel>,
    next_id: u32,
    shared: &'a mut Shared,
}

impl SlideWriter<'_> {
    fn rel(&mut self, kind: &'static str, target: String, external: bool) -> String {
        self.rels.push(Rel {
            kind,
            target,
            external,
        });
        format!("rId{}", self.rels.len())
    }

    fn sid(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn fate(&mut self, id: &str, tier: ExportTier, reason: impl Into<String>) {
        self.shared.fates.push(ObjectFate {
            id: id.to_string(),
            tier,
            reason: reason.into(),
        });
    }

    /// Embed image bytes under `ppt/media/`, deduplicated by source path.
    fn media_rel(&mut self, src: &str, bytes: Vec<u8>, ext: &'static str) -> String {
        let idx = match self.shared.media_by_src.get(src) {
            Some(i) => *i,
            None => {
                let idx = self.shared.media.len();
                self.shared.media.push(Media {
                    name: format!("image{}.{ext}", idx + 1),
                    bytes,
                });
                self.shared.media_by_src.insert(src.to_string(), idx);
                self.shared.exts.insert(ext);
                idx
            }
        };
        let name = self.shared.media[idx].name.clone();
        self.rel(REL_IMAGE, format!("../media/{name}"), false)
    }

    /// `p:cNvPr` with an optional description (alt text) and hyperlink.
    fn cnvpr(&mut self, name: &str, alt: Option<&str>, link: Option<&str>) -> String {
        let id = self.sid();
        let descr = match alt {
            Some(a) => format!(r#" descr="{}""#, esc(a)),
            None => String::new(),
        };
        match link {
            Some(url) => {
                let rid = self.rel(REL_HYPERLINK, url.to_string(), true);
                format!(
                    r#"<p:cNvPr id="{id}" name="{}"{descr}><a:hlinkClick r:id="{rid}"/></p:cNvPr>"#,
                    esc(name)
                )
            }
            None => format!(r#"<p:cNvPr id="{id}" name="{}"{descr}/>"#, esc(name)),
        }
    }
}

// ---------------------------------------------------------------------------
// Text bodies
// ---------------------------------------------------------------------------

struct TextSpec<'a> {
    paragraphs: &'a [Paragraph],
    role: &'a TypeRole,
    align: Option<Align>,
    valign: Option<VAlign>,
    /// Uniform bodyPr inset in points (bound shape text keeps the renderer's
    /// inset; plain text boxes are flush like the renderer draws them).
    inset: f64,
    wrap: bool,
}

/// A `p:txBody`. Always `<a:noAutofit/>` — autofit is the trap that silently
/// rescales text at the destination (§3.5).
fn tx_body(w: &mut SlideWriter, spec: &TextSpec) -> String {
    let anchor = match spec.valign.unwrap_or(VAlign::Top) {
        VAlign::Top => "t",
        VAlign::Middle => "ctr",
        VAlign::Bottom => "b",
    };
    let wrap = if spec.wrap { "square" } else { "none" };
    let ins = emu(spec.inset).max(0);
    let mut s = format!(
        r#"<p:txBody><a:bodyPr wrap="{wrap}" lIns="{ins}" tIns="{ins}" rIns="{ins}" bIns="{ins}" anchor="{anchor}"><a:noAutofit/></a:bodyPr><a:lstStyle/>"#
    );
    if spec.paragraphs.is_empty() {
        s.push_str("<a:p/>");
    }
    for p in spec.paragraphs {
        match p {
            Paragraph::Plain(text) => {
                let ppr = ppr_xml(spec.align, None, None, None, "", w.theme);
                let run = Run::plain(text.clone());
                let r = run_xml(w, &run, spec.role);
                let _ = write!(s, "<a:p>{ppr}{r}</a:p>");
            }
            Paragraph::Rich(rich) => {
                let bullet_font = if rich.bullet.is_some() {
                    resolved_family(w.fonts, &spec.role.family, spec.role.weight)
                } else {
                    String::new()
                };
                let ppr = ppr_xml(
                    rich.align.or(spec.align),
                    rich.space_before,
                    rich.space_after,
                    rich.bullet.as_deref(),
                    &bullet_font,
                    w.theme,
                );
                let _ = write!(s, "<a:p>{ppr}");
                for run in &rich.runs {
                    let r = run_xml(w, run, spec.role);
                    s.push_str(&r);
                }
                s.push_str("</a:p>");
            }
        }
    }
    s.push_str("</p:txBody>");
    s
}

fn ppr_xml(
    align: Option<Align>,
    space_before: Option<f64>,
    space_after: Option<f64>,
    bullet: Option<&str>,
    bullet_font: &str,
    _theme: &Theme,
) -> String {
    let mut attrs = String::new();
    match align {
        Some(Align::Center) => attrs.push_str(r#" algn="ctr""#),
        Some(Align::Right) => attrs.push_str(r#" algn="r""#),
        Some(Align::Left) | None => {}
    }
    let mut children = String::new();
    if let Some(v) = space_before {
        let _ = write!(
            children,
            r#"<a:spcBef><a:spcPts val="{}"/></a:spcBef>"#,
            sz100(v)
        );
    }
    if let Some(v) = space_after {
        let _ = write!(
            children,
            r#"<a:spcAft><a:spcPts val="{}"/></a:spcAft>"#,
            sz100(v)
        );
    }
    if let Some(b) = bullet {
        let _ = write!(
            children,
            r#"<a:buFont typeface="{}"/><a:buChar char="{}"/>"#,
            esc(bullet_font),
            esc(b)
        );
    }
    if attrs.is_empty() && children.is_empty() {
        String::new()
    } else if children.is_empty() {
        format!("<a:pPr{attrs}/>")
    } else {
        format!("<a:pPr{attrs}>{children}</a:pPr>")
    }
}

/// One run: size ×100, bold from the run or the role's weight, everything
/// unset inheriting from the role through the theme — the same resolution the
/// renderer applies.
fn run_xml(w: &mut SlideWriter, run: &Run, role: &TypeRole) -> String {
    let size = run.size.unwrap_or(role.size);
    let bold = run.b.unwrap_or(role.weight >= 600);
    let italic = run.i.unwrap_or_else(|| role.italic.unwrap_or(false));
    let mut attrs = format!(r#" sz="{}""#, sz100(size));
    if bold {
        attrs.push_str(r#" b="1""#);
    }
    if italic {
        attrs.push_str(r#" i="1""#);
    }
    if run.u == Some(true) {
        attrs.push_str(r#" u="sng""#);
    }
    let color_ref = run.color.as_deref().unwrap_or(&role.color);
    let fill = solid_fill(w.theme, Some(color_ref), None);
    let family = match run.family.as_deref() {
        Some(f) => resolved_family(w.fonts, std::slice::from_ref(&f.to_string()), role.weight),
        None => resolved_family(w.fonts, &role.family, role.weight),
    };
    let hlink = match run.link.as_deref() {
        Some(url) => {
            let rid = w.rel(REL_HYPERLINK, url.to_string(), true);
            format!(r#"<a:hlinkClick r:id="{rid}"/>"#)
        }
        None => String::new(),
    };
    format!(
        r#"<a:r><a:rPr{attrs}>{fill}<a:latin typeface="{}"/>{hlink}</a:rPr><a:t>{}</a:t></a:r>"#,
        esc(&family),
        esc(&run.t)
    )
}

// ---------------------------------------------------------------------------
// Object emission
// ---------------------------------------------------------------------------

fn emit_object(w: &mut SlideWriter, obj: &Object) {
    // The object's page frame as slot/anchor resolution decided it: the
    // resolved map is the single geometry truth (render.rs draws from the
    // same lookup), so a slot-placed object exports exactly where the pane
    // shows it. The fallback covers composite-generated children, which are
    // never on the page and so never in the map.
    let frame = w.index.get(obj.id()).copied().or_else(|| obj.frame());
    match obj {
        Object::Text(t) => emit_text(w, t, frame),
        Object::Shape(sh) => emit_shape(w, sh, frame),
        Object::Connector(c) => emit_connector(w, c),
        Object::Image(img) => emit_image(w, img, frame),
        Object::Group(g) => emit_group(w, g, frame),
        Object::Table(t) => emit_table(w, t, frame),
        Object::Chart(c) => emit_chart(w, c, frame),
        Object::Diagram(d) => emit_diagram(w, d, frame),
        Object::Equation(eq) => emit_equation(w, eq, frame),
        Object::PanelLabel(o) => {
            let (children, problems) = o.expand(w.theme, w.fonts);
            emit_composite(w, &o.id, children, problems);
        }
        Object::Scalebar(o) => {
            let (children, problems) = o.expand(w.theme, w.fonts);
            emit_composite(w, &o.id, children, problems);
        }
        Object::SigBracket(o) => {
            let (children, problems) = o.expand(w.theme, w.fonts, &w.index);
            emit_composite(w, &o.id, children, problems);
        }
        Object::Legend(o) => {
            let (children, problems) = o.expand(w.theme, w.fonts);
            emit_composite(w, &o.id, children, problems);
        }
        Object::Colorbar(o) => {
            let (children, problems) = o.expand(w.theme, w.fonts);
            emit_composite(w, &o.id, children, problems);
        }
        Object::Callout(o) => {
            let (children, problems) = o.expand(w.theme, w.fonts);
            emit_composite(w, &o.id, children, problems);
        }
        Object::Inset(o) => {
            let page = w.page;
            let target = page.walk().find_map(|t| match t {
                Object::Image(i) if i.id == o.of.object => Some(i),
                _ => None,
            });
            let target_frame = w.index.get(o.of.object.as_str()).copied();
            let (children, problems) = o.expand(w.theme, w.fonts, target, target_frame);
            emit_composite(w, &o.id, children, problems);
        }
        Object::Unknown(u) => {
            let why = match &u.error {
                Some(e) => format!("skipped: object of type {:?} failed to parse: {e}", u.kind),
                None => format!("skipped: object type {:?} unknown to this build", u.kind),
            };
            w.fate(&u.id, ExportTier::Raster, why);
        }
    }
}

/// A table as a native `a:tbl` inside a `p:graphicFrame` — the highest tier
/// in the degradation contract, editable in every consumer.
///
/// Every cell is styled explicitly (fill, borders, margins on `a:tcPr`;
/// tableStyles.xml is one empty `a:tblStyleLst`), and each cell's text body
/// goes through [`tx_body`] — the same `a:p`/`a:r`/`a:rPr` writer every text
/// shape uses (C6: one text stack, run boundaries preserved). Geometry
/// matches the renderer exactly: [`TableObject::column_widths`] for columns,
/// rows splitting the frame height equally, with the last column/row
/// absorbing EMU rounding so the grid sums to the frame's ext.
fn emit_table(w: &mut SlideWriter, t: &TableObject, frame: Option<Frame>) {
    let Some(f) = frame else {
        w.fate(
            &t.id,
            ExportTier::Raster,
            "skipped: no geometry (slot unresolved)",
        );
        return;
    };
    let cols = t.column_count();
    if t.rows.is_empty() || cols == 0 {
        w.fate(&t.id, ExportTier::Raster, "skipped: table has no cells");
        return;
    }
    let role = role_of(w.theme, t.role.as_deref()).clone();

    let total_w = emu(f.w).max(1);
    let mut grid_cols: Vec<i64> = t
        .column_widths(f.w)
        .iter()
        .map(|width| emu(*width).max(1))
        .collect();
    let acc: i64 = grid_cols[..cols - 1].iter().sum();
    grid_cols[cols - 1] = (total_w - acc).max(1);

    let total_h = emu(f.h).max(1);
    let nrows = t.rows.len();
    let row_h_emu = (total_h / nrows as i64).max(1);
    let last_row_h = (total_h - row_h_emu * (nrows as i64 - 1)).max(1);

    let nv = w.cnvpr(&t.id, t.alt.as_deref(), None);
    let _ = write!(
        w.xml,
        concat!(
            r#"<p:graphicFrame><p:nvGraphicFramePr>{nv}"#,
            r#"<p:cNvGraphicFramePr><a:graphicFrameLocks noGrp="1"/></p:cNvGraphicFramePr>"#,
            r#"<p:nvPr/></p:nvGraphicFramePr>"#,
            r#"<p:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{cx}" cy="{cy}"/></p:xfrm>"#,
            r#"<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table">"#,
            r#"<a:tbl><a:tblPr{first_row}><a:tableStyleId>{style}</a:tableStyleId></a:tblPr><a:tblGrid>"#
        ),
        nv = nv,
        x = emu(f.x),
        y = emu(f.y),
        cx = total_w,
        cy = total_h,
        first_row = if t.header { r#" firstRow="1""# } else { "" },
        style = TABLE_STYLE_ID,
    );
    for gc in &grid_cols {
        let _ = write!(w.xml, r#"<a:gridCol w="{gc}"/>"#);
    }
    w.xml.push_str("</a:tblGrid>");

    // The renderer's grid, restated as explicit per-cell properties: fixed
    // margins, hairline @edge borders on every side, @surface header ground.
    let pad = emu(crate::render::TABLE_CELL_PAD_PT).max(0);
    let hairline = emu(1.0).max(1);
    let edge_fill = solid_fill(w.theme, Some("@edge"), None);
    let borders = format!(
        r#"<a:lnL w="{hairline}" cap="flat">{edge_fill}</a:lnL><a:lnR w="{hairline}" cap="flat">{edge_fill}</a:lnR><a:lnT w="{hairline}" cap="flat">{edge_fill}</a:lnT><a:lnB w="{hairline}" cap="flat">{edge_fill}</a:lnB>"#
    );
    let header_fill = solid_fill(w.theme, Some("@surface"), None);

    for (ri, row) in t.rows.iter().enumerate() {
        let h = if ri + 1 == nrows {
            last_row_h
        } else {
            row_h_emu
        };
        let _ = write!(w.xml, r#"<a:tr h="{h}">"#);
        for ci in 0..cols {
            // Ragged rows fill out with empty cells: an a:tr must carry a tc
            // per grid column or consumers mis-align the row.
            let paras: Vec<Paragraph> = match row.get(ci) {
                Some(cell) if t.header && ri == 0 => vec![TableObject::header_cell(cell)],
                Some(cell) => vec![cell.clone()],
                None => Vec::new(),
            };
            let body = tx_body(
                w,
                &TextSpec {
                    paragraphs: &paras,
                    role: &role,
                    align: None,
                    valign: None,
                    inset: 0.0,
                    wrap: true,
                },
            );
            // A cell body is the same CT_TextBody as a shape's; only the
            // qualifying prefix differs inside a:tbl.
            let inner = &body["<p:txBody>".len()..body.len() - "</p:txBody>".len()];
            let fill = if t.header && ri == 0 {
                header_fill.as_str()
            } else {
                ""
            };
            let _ = write!(
                w.xml,
                r#"<a:tc><a:txBody>{inner}</a:txBody><a:tcPr marL="{pad}" marR="{pad}" marT="{pad}" marB="{pad}">{borders}{fill}</a:tcPr></a:tc>"#
            );
        }
        w.xml.push_str("</a:tr>");
    }
    w.xml
        .push_str("</a:tbl></a:graphicData></a:graphic></p:graphicFrame>");
    w.fate(&t.id, ExportTier::Native, "native table (a:tbl)");
}

/// A diagram as a group of the very primitives its expansion produces —
/// the same `diagram::expand` the renderer draws, so the deck and the pane
/// cannot disagree. One fate for the composite: the generated children are
/// not board objects and would only be report noise.
fn emit_diagram(w: &mut SlideWriter, d: &crate::schema::DiagramObject, frame: Option<Frame>) {
    let Some(f) = frame else {
        w.fate(
            &d.id,
            ExportTier::Raster,
            "skipped: no geometry (slot unresolved)",
        );
        return;
    };
    let (children, problems) = crate::diagram::expand(d, w.theme, w.fonts);
    let mut reason = String::from("diagram exports as grouped shapes");
    if !problems.is_empty() {
        let _ = write!(reason, " ({})", problems.join("; "));
    }
    if children.is_empty() {
        w.fate(&d.id, ExportTier::Grouped, reason);
        return;
    }
    // Generated children are never on the page, so the page index cannot
    // resolve a connector bound to a generated node id; extend it for the
    // duration of the expansion, exactly as the renderer does.
    let saved_index = w.index.clone();
    for c in &children {
        if let Some(cf) = c.frame() {
            w.index.insert(c.id().to_string(), cf);
        }
    }
    let fates_before = w.shared.fates.len();
    let nv = w.cnvpr(&d.id, d.alt.as_deref(), None);
    let _ = write!(
        w.xml,
        r#"<p:grpSp><p:nvGrpSpPr>{nv}<p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr>{}</p:grpSpPr>"#,
        grp_xfrm(f)
    );
    for c in &children {
        emit_object(w, c);
    }
    w.xml.push_str("</p:grpSp>");
    w.shared.fates.truncate(fates_before);
    w.index = saved_index;
    w.fate(&d.id, ExportTier::Grouped, reason);
}

/// An annotation composite as a group of the primitives its expansion
/// produces — the same `expand` the renderer draws, exactly as `emit_diagram`
/// does. One fate for the composite; the group's box is the union of its
/// children's frames, because a bracket or scalebar stores no box of its own.
fn emit_composite(w: &mut SlideWriter, id: &str, children: Vec<Object>, problems: Vec<String>) {
    let mut reason = String::from("annotation composite as grouped shapes");
    if !problems.is_empty() {
        let _ = write!(reason, " ({})", problems.join("; "));
    }
    let mut hull: Option<Frame> = None;
    for c in &children {
        let Some(f) = c.frame() else { continue };
        hull = Some(match hull {
            None => f,
            Some(hb) => {
                let x = hb.x.min(f.x);
                let y = hb.y.min(f.y);
                Frame {
                    x,
                    y,
                    w: hb.right().max(f.right()) - x,
                    h: hb.bottom().max(f.bottom()) - y,
                }
            }
        });
    }
    let Some(f) = hull else {
        // Nothing expanded (a dangling target, a missing position): the fate
        // carries the reason and nothing lands on the slide.
        w.fate(id, ExportTier::Grouped, reason);
        return;
    };
    // Children may bind to each other (a callout's tail to its box) — extend
    // the index for the duration of the expansion, exactly as the renderer
    // does, and fold the children's fates into the composite's one.
    let saved_index = w.index.clone();
    for c in &children {
        if let Some(cf) = c.frame() {
            w.index.insert(c.id().to_string(), cf);
        }
    }
    let fates_before = w.shared.fates.len();
    let nv = w.cnvpr(id, None, None);
    let _ = write!(
        w.xml,
        r#"<p:grpSp><p:nvGrpSpPr>{nv}<p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr>{}</p:grpSpPr>"#,
        grp_xfrm(f)
    );
    for c in &children {
        emit_object(w, c);
    }
    w.xml.push_str("</p:grpSp>");
    w.shared.fates.truncate(fates_before);
    w.index = saved_index;
    w.fate(id, ExportTier::Grouped, reason);
}

fn role_of<'a>(theme: &'a Theme, name: Option<&str>) -> &'a TypeRole {
    name.and_then(|r| theme.role(r))
        .unwrap_or_else(|| theme.body())
}

fn emit_text(w: &mut SlideWriter, t: &TextObject, frame: Option<Frame>) {
    let Some(f) = frame else {
        w.fate(
            &t.id,
            ExportTier::Raster,
            "skipped: no geometry (slot unresolved)",
        );
        return;
    };
    let role = role_of(w.theme, t.role.as_deref()).clone();
    let nv = w.cnvpr(&t.id, t.alt.as_deref(), t.link.as_deref());
    let body = tx_body(
        w,
        &TextSpec {
            paragraphs: &t.text,
            role: &role,
            align: t.align,
            valign: t.valign,
            inset: 0.0,
            wrap: true,
        },
    );
    let _ = write!(
        w.xml,
        r#"<p:sp><p:nvSpPr>{nv}<p:cNvSpPr txBox="1"/><p:nvPr/></p:nvSpPr><p:spPr>{}{}<a:noFill/>{NO_LINE}</p:spPr>{body}</p:sp>"#,
        xfrm(f, t.rotation, false, false),
        prst_geom("rect", "")
    );
    let reason = match t.role.as_deref() {
        Some("title") | Some("heading") => {
            "native text; exported as a plain shape — p:ph placeholder wiring ships later"
        }
        _ => "native text",
    };
    w.fate(&t.id, ExportTier::Native, reason);
}

fn emit_shape(w: &mut SlideWriter, sh: &ShapeObject, frame: Option<Frame>) {
    let Some(f) = frame else {
        w.fate(
            &sh.id,
            ExportTier::Raster,
            "skipped: no geometry (slot unresolved)",
        );
        return;
    };
    let mut tier = ExportTier::Native;
    let mut reason = String::from("native geometry");
    let geom = match sh.geo.as_str() {
        "rect" => prst_geom("rect", ""),
        "ellipse" => prst_geom("ellipse", ""),
        "triangle" => prst_geom("triangle", ""),
        "diamond" => prst_geom("diamond", ""),
        "roundRect" => {
            let r = sh.radius.unwrap_or((f.w.min(f.h) * 0.12).min(16.0));
            let denom = f.w.min(f.h).max(f64::EPSILON);
            let adj = ((r / denom) * 100_000.0).round().clamp(0.0, 50_000.0) as i64;
            prst_geom(
                "roundRect",
                &format!(r#"<a:gd name="adj" fmla="val {adj}"/>"#),
            )
        }
        "line" => {
            // The unbound straight line: a two-point custGeom at mid-height,
            // exactly what the renderer draws (the `line` preset would draw a
            // corner-to-corner diagonal instead).
            reason = "native geometry (line as a two-point custGeom path)".to_string();
            cust_geom(
                &[Seg::Move([f.x, f.cy()]), Seg::Line([f.right(), f.cy()])],
                f,
            )
        }
        "path" => match sh.d.as_deref().map(parse_svg_path) {
            Some(Ok(segs)) => {
                reason = "native custom geometry (arcs flattened to cubics)".to_string();
                cust_geom(&segs, f)
            }
            Some(Err(e)) => {
                tier = ExportTier::Vector;
                reason = format!("path data failed to parse ({e}); exported its bounding box");
                prst_geom("rect", "")
            }
            None => {
                tier = ExportTier::Vector;
                reason = "geo \"path\" without `d`; exported its bounding box".to_string();
                prst_geom("rect", "")
            }
        },
        other => {
            tier = ExportTier::Vector;
            reason = format!(
                "geometry {other:?} is not mapped in this build; exported its bounding box"
            );
            prst_geom("rect", "")
        }
    };

    let fill = match (sh.geo.as_str(), sh.fill.as_deref()) {
        ("line", _) | (_, None) => "<a:noFill/>".to_string(),
        (_, Some(c)) => solid_fill(w.theme, Some(c), sh.fill_opacity),
    };
    let ln = match &sh.stroke {
        Some(st) => line_xml(w.theme, st, 1.0, ""),
        None => NO_LINE.to_string(),
    };

    let nv = w.cnvpr(&sh.id, sh.alt.as_deref(), sh.link.as_deref());
    let body = if sh.text.is_empty() {
        String::new()
    } else {
        // The renderer's bound-text inset, so the box reads identically.
        let inset = 10.0_f64.min(f.w * 0.1).min(f.h * 0.1).max(0.0);
        let role = role_of(w.theme, sh.role.as_deref()).clone();
        tx_body(
            w,
            &TextSpec {
                paragraphs: &sh.text,
                role: &role,
                align: sh.align.or(Some(Align::Center)),
                valign: Some(VAlign::Middle),
                inset,
                wrap: true,
            },
        )
    };
    let _ = write!(
        w.xml,
        r#"<p:sp><p:nvSpPr>{nv}<p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr>{}{geom}{fill}{ln}</p:spPr>{body}</p:sp>"#,
        xfrm(f, sh.rotation, sh.flip_h, sh.flip_v)
    );
    w.fate(&sh.id, tier, reason);
}

/// Resolve a connector endpoint to a page-space point. This replicates the
/// renderer's `emit_connector` resolution exactly (including the face-the-
/// other-end default for an unstated side) — the two must agree or the pane
/// and the deck show different arrows.
fn resolve_endpoint(
    ep: &EndPoint,
    other: Option<(f64, f64)>,
    index: &BTreeMap<String, Frame>,
) -> Option<(f64, f64)> {
    if let Some(at) = ep.at {
        return Some((at[0], at[1]));
    }
    let id = ep.object.as_deref()?;
    let f = index.get(id)?;
    let side = ep.side.unwrap_or_else(|| match other {
        Some((ox, _)) if ox < f.x => Side::Left,
        Some((ox, _)) if ox > f.right() => Side::Right,
        Some((_, oy)) if oy < f.y => Side::Top,
        Some(_) => Side::Bottom,
        None => Side::Center,
    });
    Some(match side {
        Side::Top => (f.cx(), f.y),
        Side::Right => (f.right(), f.cy()),
        Side::Bottom => (f.cx(), f.bottom()),
        Side::Left => (f.x, f.cy()),
        Side::Center => (f.cx(), f.cy()),
    })
}

fn emit_connector(w: &mut SlideWriter, c: &ConnectorObject) {
    let to_rough = resolve_endpoint(&c.to, None, &w.index);
    let Some(from) = resolve_endpoint(&c.from, to_rough, &w.index) else {
        w.fate(
            &c.id,
            ExportTier::Raster,
            "skipped: `from` endpoint does not resolve",
        );
        return;
    };
    let Some(to) = resolve_endpoint(&c.to, Some(from), &w.index) else {
        w.fate(
            &c.id,
            ExportTier::Raster,
            "skipped: `to` endpoint does not resolve",
        );
        return;
    };

    // The cxnSp's OWN xfrm, from the two resolved points: off at the top-left
    // of the segment box, flips orienting local (0,0)→(cx,cy) as from→to.
    // Never omitted, never zero-extent — PowerPoint renders the *stored*
    // geometry on open, and Keynote and Google Slides never reroute.
    let f = Frame {
        x: from.0.min(to.0),
        y: from.1.min(to.1),
        w: (to.0 - from.0).abs(),
        h: (to.1 - from.1).abs(),
    };
    let flip_h = to.0 < from.0;
    let flip_v = to.1 < from.1;

    let prst = match c.geo.as_deref() {
        Some("bent") => "bentConnector3",
        _ => "straightConnector1",
    };
    let mut ends = String::new();
    if c.head_end.as_deref() == Some("arrow") {
        ends.push_str(r#"<a:headEnd type="triangle"/>"#);
    }
    if c.tail_end.as_deref() == Some("arrow") {
        ends.push_str(r#"<a:tailEnd type="triangle"/>"#);
    }
    let default_stroke = Stroke {
        color: None,
        width: None,
        dash: None,
        opacity: None,
        cap: None,
        join: None,
        extra: Default::default(),
    };
    let ln = line_xml(
        w.theme,
        c.stroke.as_ref().unwrap_or(&default_stroke),
        1.5,
        &ends,
    );

    let nv = w.cnvpr(&c.id, c.alt.as_deref(), None);
    let _ = write!(
        w.xml,
        r#"<p:cxnSp><p:nvCxnSpPr>{nv}<p:cNvCxnSpPr/><p:nvPr/></p:nvCxnSpPr><p:spPr>{}{}{ln}</p:spPr></p:cxnSp>"#,
        xfrm(f, None, flip_h, flip_v),
        prst_geom(prst, "")
    );

    let mut reason = String::from("native connector");
    if !c.text.is_empty() {
        // The bound edge label: a separate small text shape at `label_at`
        // along the segment, haloed with the page ground exactly as the
        // renderer draws it.
        let t = c.label_at.unwrap_or(0.5).clamp(0.0, 1.0);
        let (lx, ly) = (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
        let role = c
            .role
            .as_deref()
            .and_then(|r| w.theme.role(r))
            .or_else(|| w.theme.role("label"))
            .unwrap_or_else(|| w.theme.body())
            .clone();
        let text: String = c
            .text
            .iter()
            .map(|p| p.plain_text())
            .collect::<Vec<_>>()
            .join(" ");
        let tw = w.fonts.measure(&text, &role.family, role.size, role.weight);
        let pad = 3.0;
        let lf = Frame {
            x: lx - tw / 2.0 - pad,
            y: ly - role.size * 0.7 - pad,
            w: tw + pad * 2.0,
            h: role.size + pad * 2.0,
        };
        let paragraphs = [Paragraph::Plain(text)];
        let nv = w.cnvpr(&format!("{}-label", c.id), None, None);
        let body = tx_body(
            w,
            &TextSpec {
                paragraphs: &paragraphs,
                role: &role,
                align: Some(Align::Center),
                valign: Some(VAlign::Middle),
                inset: 0.0,
                wrap: false,
            },
        );
        let halo = solid_fill(w.theme, Some("@bg"), None);
        let _ = write!(
            w.xml,
            r#"<p:sp><p:nvSpPr>{nv}<p:cNvSpPr txBox="1"/><p:nvPr/></p:nvSpPr><p:spPr>{}{}{halo}{NO_LINE}</p:spPr>{body}</p:sp>"#,
            xfrm(lf, None, false, false),
            prst_geom("rect", "")
        );
        reason.push_str("; edge label exported as a separate text shape at labelAt");
    }
    w.fate(&c.id, ExportTier::Native, reason);
}

/// A dashed placeholder box where pixels could not land — the same visual the
/// renderer draws for an image it cannot place.
fn placeholder_sp(w: &mut SlideWriter, name: &str, alt: Option<&str>, f: Frame) {
    let edge = w
        .theme
        .color("@edge")
        .unwrap_or_else(|| w.theme.color_or_fg(None));
    let edge_fill = format!("<a:solidFill>{}</a:solidFill>", srgb(edge, None));
    let nv = w.cnvpr(name, alt, None);
    let _ = write!(
        w.xml,
        r#"<p:sp><p:nvSpPr>{nv}<p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr>{}{}<a:noFill/><a:ln w="12700">{edge_fill}<a:prstDash val="dash"/></a:ln></p:spPr></p:sp>"#,
        xfrm(f, None, false, false),
        prst_geom("rect", "")
    );
}

fn emit_image(w: &mut SlideWriter, img: &ImageObject, frame: Option<Frame>) {
    // Geometry precedence: the resolved frame (slot- or anchor-placed, or
    // explicit at+size — the map encodes it) wins; a bare `at` without a
    // `size` is not in the map and keeps the intrinsic-dimension fallback.
    let at = frame.map(|f| [f.x, f.y]).or(img.at);
    let size_hint = frame.map(|f| [f.w, f.h]).or(img.size);
    let Some(at) = at else {
        w.fate(
            &img.id,
            ExportTier::Raster,
            "skipped: no geometry (slot unresolved)",
        );
        return;
    };
    let fallback_frame = |size: Option<[f64; 2]>| Frame {
        x: at[0],
        y: at[1],
        w: size.map(|s| s[0]).unwrap_or(100.0),
        h: size.map(|s| s[1]).unwrap_or(75.0),
    };
    // Relative srcs resolve against the workspace root, matching the
    // renderer; absolute paths and workspace-less callers read as given.
    let src_path = match (&w.workspace, std::path::Path::new(&img.src).is_relative()) {
        (Some(ws), true) => ws.join(&img.src),
        _ => std::path::PathBuf::from(&img.src),
    };
    let bytes = match std::fs::read(&src_path) {
        Ok(b) => b,
        Err(_) => {
            placeholder_sp(w, &img.id, img.alt.as_deref(), fallback_frame(size_hint));
            w.fate(
                &img.id,
                ExportTier::Raster,
                format!("source {:?} not found; placeholder box exported", img.src),
            );
            return;
        }
    };
    let (dims, ext) = match sniff_image(&bytes, &img.src) {
        ImgKind::Png => (png_dimensions(&bytes), "png"),
        ImgKind::Jpeg => (jpeg_dimensions(&bytes), "jpeg"),
        ImgKind::Svg => {
            emit_image_svg(w, img, at, size_hint, &bytes);
            return;
        }
        ImgKind::Unknown => {
            placeholder_sp(w, &img.id, img.alt.as_deref(), fallback_frame(size_hint));
            w.fate(
                &img.id,
                ExportTier::Raster,
                "unrecognized image format; placeholder box exported",
            );
            return;
        }
    };
    // Missing size falls back to the intrinsic pixel size at 96 dpi (px ×
    // 0.75 pt), the CSS convention the rest of the world already assumes.
    let size = size_hint.or_else(|| dims.map(|(pw, ph)| [pw as f64 * 0.75, ph as f64 * 0.75]));
    let Some(size) = size else {
        placeholder_sp(w, &img.id, img.alt.as_deref(), fallback_frame(None));
        w.fate(
            &img.id,
            ExportTier::Raster,
            "no size and undecodable intrinsic dimensions; placeholder box exported",
        );
        return;
    };
    let f = Frame {
        x: at[0],
        y: at[1],
        w: size[0],
        h: size[1],
    };
    let rid = w.media_rel(&img.src, bytes, ext);
    let src_rect = match img.src_rect {
        Some([l, t, r, b]) => format!(
            r#"<a:srcRect l="{}" t="{}" r="{}" b="{}"/>"#,
            pct100k(l),
            pct100k(t),
            pct100k(r),
            pct100k(b)
        ),
        None => String::new(),
    };
    let nv = w.cnvpr(&img.id, img.alt.as_deref(), img.link.as_deref());
    let _ = write!(
        w.xml,
        r#"<p:pic><p:nvPicPr>{nv}<p:cNvPicPr><a:picLocks noChangeAspect="1"/></p:cNvPicPr><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="{rid}"/>{src_rect}<a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr>{}{}</p:spPr></p:pic>"#,
        xfrm(f, img.rotation, false, false),
        prst_geom("rect", "")
    );
    let kind = if ext == "png" { "PNG" } else { "JPEG" };
    w.fate(
        &img.id,
        ExportTier::Native,
        format!("{kind} embedded natively"),
    );
}

/// An SVG image as a `p:pic` carrying both bodies: a PNG rasterized at 2× the
/// placed size (what every consumer can show) and the sanitized SVG itself as
/// an `svgBlip` extension (what modern PowerPoint renders as a real vector) —
/// the progressive enhancement from the plan. The SVG that lands in the
/// package is the usvg round-trip, never the raw file: an imported figure is
/// untrusted markup and the sanitize pass is what strips anything script-ish.
fn emit_image_svg(
    w: &mut SlideWriter,
    img: &ImageObject,
    at: [f64; 2],
    size_hint: Option<[f64; 2]>,
    bytes: &[u8],
) {
    let fallback = Frame {
        x: at[0],
        y: at[1],
        w: size_hint.map(|s| s[0]).unwrap_or(100.0),
        h: size_hint.map(|s| s[1]).unwrap_or(75.0),
    };
    let degrade = |w: &mut SlideWriter, reason: String| {
        placeholder_sp(w, &img.id, img.alt.as_deref(), fallback);
        w.fate(&img.id, ExportTier::Raster, reason);
    };
    let Ok(text) = std::str::from_utf8(bytes) else {
        degrade(
            w,
            "svg source is not valid UTF-8; placeholder box exported".to_string(),
        );
        return;
    };
    let prefix: String = img
        .id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .chain("-".chars())
        .collect();
    let san = match crate::imginfo::sanitize_svg(text, w.fonts.db(), &prefix) {
        Ok(v) => v,
        Err(e) => {
            degrade(
                w,
                format!("svg failed to parse ({e}); placeholder box exported"),
            );
            return;
        }
    };
    // Missing size falls back to the document's own units at 96 dpi (unit ×
    // 0.75 pt), matching the raster path's convention.
    let size = size_hint.unwrap_or([san.width * 0.75, san.height * 0.75]);
    let f = Frame {
        x: at[0],
        y: at[1],
        w: size[0],
        h: size[1],
    };
    let px = |v: f64| -> u64 {
        let p = (v * 2.0).round();
        if p.is_finite() && p >= 1.0 {
            p.min(crate::render::MAX_PIXELS as f64) as u64
        } else {
            1
        }
    };
    let (px_w, px_h) = (px(size[0]), px(size[1]));
    if px_w.saturating_mul(px_h) > crate::render::MAX_PIXELS {
        degrade(
            w,
            format!(
                "svg fallback raster would be {px_w}×{px_h} px, over the {} Mpx ceiling; \
                 placeholder box exported",
                crate::render::MAX_PIXELS / 1_000_000
            ),
        );
        return;
    }
    let png = match crate::imginfo::rasterize_svg(&san.xml, w.fonts.db(), px_w as u32, px_h as u32)
    {
        Ok(p) => p,
        Err(e) => {
            degrade(
                w,
                format!("svg rasterization failed ({e}); placeholder box exported"),
            );
            return;
        }
    };
    let png_rid = w.media_rel(&format!("{}#svg-png", img.src), png, "png");
    let svg_rid = w.media_rel(&format!("{}#svg", img.src), san.xml.into_bytes(), "svg");
    let src_rect = match img.src_rect {
        Some([l, t, r, b]) => format!(
            r#"<a:srcRect l="{}" t="{}" r="{}" b="{}"/>"#,
            pct100k(l),
            pct100k(t),
            pct100k(r),
            pct100k(b)
        ),
        None => String::new(),
    };
    let nv = w.cnvpr(&img.id, img.alt.as_deref(), img.link.as_deref());
    let _ = write!(
        w.xml,
        r#"<p:pic><p:nvPicPr>{nv}<p:cNvPicPr><a:picLocks noChangeAspect="1"/></p:cNvPicPr><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="{png_rid}"><a:extLst><a:ext uri="{{96DAC541-7B7A-43D3-8B79-37D633B846F1}}"><asvg:svgBlip xmlns:asvg="http://schemas.microsoft.com/office/drawing/2016/SVG/main" r:embed="{svg_rid}"/></a:ext></a:extLst></a:blip>{src_rect}<a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr>{}{}</p:spPr></p:pic>"#,
        xfrm(f, img.rotation, false, false),
        prst_geom("rect", "")
    );
    w.fate(
        &img.id,
        ExportTier::Vector,
        "svg embedded with PNG fallback (svgBlip)",
    );
}

/// An equation as a `p:pic`: the typeset outline SVG fitted into the frame
/// (aspect preserved, centered — matching the renderer's placement exactly),
/// rasterized to PNG at 2× the placed size through the same machinery SVG
/// images use, with the SVG itself beside it as an `svgBlip` (the plan's
/// picture-quality enhancement: modern PowerPoint gets vector, everyone else
/// gets the PNG, nobody gets worse). The required `alt` — the LaTeX source —
/// lands as `p:cNvPr/@descr`. Fate `raster` per the degradation contract;
/// the native OMML arm is deliberately not built in v1.
fn emit_equation(w: &mut SlideWriter, eq: &crate::schema::EquationObject, frame: Option<Frame>) {
    let Some(f) = frame else {
        w.fate(
            &eq.id,
            ExportTier::Raster,
            "skipped: no geometry (slot unresolved)",
        );
        return;
    };
    let degrade = |w: &mut SlideWriter, reason: String| {
        placeholder_sp(w, &eq.id, Some(&eq.alt), f);
        w.fate(&eq.id, ExportTier::Raster, reason);
    };
    let em = match eq.em_size {
        Some(v) if v.is_finite() && v > 0.0 => v,
        _ => w.theme.body().size,
    };
    let svg = match crate::equation::render_tex_svg(&eq.tex, em) {
        Ok(svg) => svg,
        Err(e) => {
            degrade(
                w,
                format!("equation did not render ({e}); placeholder box exported"),
            );
            return;
        }
    };
    // Fit the natural box into the frame preserving aspect, centered — the
    // placed picture is the fitted rect, so the destination never stretches.
    let scale = (f.w / svg.width_pt).min(f.h / svg.height_pt);
    if !(scale.is_finite() && scale > 0.0) {
        degrade(
            w,
            "equation frame is degenerate; placeholder box exported".to_string(),
        );
        return;
    }
    let fitted = Frame {
        x: f.x + (f.w - svg.width_pt * scale) / 2.0,
        y: f.y + (f.h - svg.height_pt * scale) / 2.0,
        w: svg.width_pt * scale,
        h: svg.height_pt * scale,
    };
    // The theme foreground inks glyph fills and rules, exactly as the
    // renderer embeds it.
    let ink = w.theme.color_or_fg(None).hex();
    let doc = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {} {}" fill="{ink}">{}</svg>"#,
        svg.width_pt,
        svg.height_pt,
        svg.body.replace("currentColor", &ink)
    );
    let px = |v: f64| -> u32 {
        let p = (v * 2.0).round();
        if p.is_finite() && p >= 1.0 {
            p.min(crate::render::MAX_PIXELS as f64) as u32
        } else {
            1
        }
    };
    let (px_w, px_h) = (px(fitted.w), px(fitted.h));
    if (px_w as u64).saturating_mul(px_h as u64) > crate::render::MAX_PIXELS {
        degrade(
            w,
            format!(
                "equation raster would be {px_w}×{px_h} px, over the {} Mpx ceiling; \
                 placeholder box exported",
                crate::render::MAX_PIXELS / 1_000_000
            ),
        );
        return;
    }
    let png = match crate::imginfo::rasterize_svg(&doc, w.fonts.db(), px_w, px_h) {
        Ok(p) => p,
        Err(e) => {
            degrade(
                w,
                format!("equation rasterization failed ({e}); placeholder box exported"),
            );
            return;
        }
    };
    let png_rid = w.media_rel(&format!("equation:{}#png", eq.id), png, "png");
    let svg_rid = w.media_rel(&format!("equation:{}#svg", eq.id), doc.into_bytes(), "svg");
    let nv = w.cnvpr(&eq.id, Some(&eq.alt), None);
    let _ = write!(
        w.xml,
        r#"<p:pic><p:nvPicPr>{nv}<p:cNvPicPr><a:picLocks noChangeAspect="1"/></p:cNvPicPr><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="{png_rid}"><a:extLst><a:ext uri="{{96DAC541-7B7A-43D3-8B79-37D633B846F1}}"><asvg:svgBlip xmlns:asvg="http://schemas.microsoft.com/office/drawing/2016/SVG/main" r:embed="{svg_rid}"/></a:ext></a:extLst></a:blip><a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr>{}{}</p:spPr></p:pic>"#,
        xfrm(fitted, None, false, false),
        prst_geom("rect", "")
    );
    w.fate(
        &eq.id,
        ExportTier::Raster,
        "equation exported as a picture (PNG + svgBlip); native OMML is a later arm",
    );
}

fn emit_group(w: &mut SlideWriter, g: &GroupObject, frame: Option<Frame>) {
    let Some(f) = frame else {
        // No positioned children means no envelope to draw; the children
        // (which individually report their own fates) export ungrouped.
        w.fate(
            &g.id,
            ExportTier::Vector,
            "group has no resolved box; children exported ungrouped",
        );
        for child in &g.objects {
            emit_object(w, child);
        }
        return;
    };
    let nv = w.cnvpr(&g.id, g.alt.as_deref(), None);
    let _ = write!(
        w.xml,
        r#"<p:grpSp><p:nvGrpSpPr>{nv}<p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr>{}</p:grpSpPr>"#,
        grp_xfrm(f)
    );
    w.fate(
        &g.id,
        ExportTier::Native,
        "native group with identity child space",
    );
    for child in &g.objects {
        emit_object(w, child);
    }
    w.xml.push_str("</p:grpSp>");
}

/// A chart as a group of real editable shapes — rects, freeform paths,
/// ellipses and *text* shapes, so every label stays text at the destination.
/// This is the vector tier that is still text-editable. Under
/// [`ChartFidelity::Native`] (opt-in) a chart that maps cleanly onto a
/// `c:barChart`/`c:lineChart`/`c:scatterChart` becomes a real chart part
/// instead; anything the native writer cannot express degrades per-chart to
/// these grouped shapes with the reason in its fate.
fn emit_chart(w: &mut SlideWriter, c: &ChartObject, frame: Option<Frame>) {
    let Some(f) = frame else {
        w.fate(
            &c.id,
            ExportTier::Raster,
            "skipped: no geometry (slot unresolved)",
        );
        return;
    };
    // Source-bound rows resolve against the same workspace the renderer
    // uses — without this, a CSV-bound chart exports as empty axes with a
    // fate line that reads like success.
    let (loaded, src_problems) = crate::chart::resolve_rows(c, w.workspace.as_deref());
    let mut native_reason: Option<String> = None;
    if w.chart_fidelity == ChartFidelity::Native {
        match super::chart_xml::build_native(c, loaded.as_deref(), w.theme) {
            Ok(part) => {
                emit_native_chart(w, c, f, part);
                return;
            }
            Err(why) => native_reason = Some(why),
        }
    }
    let scene = match loaded.as_deref() {
        Some(rows) => crate::chart::build_with_rows(c, Some(rows), f, w.theme, w.fonts),
        None => crate::chart::build(c, f, w.theme, w.fonts),
    };
    let mut reason = match native_reason {
        Some(why) => format!("native chart unsupported: {why}; exported as grouped shapes"),
        None => {
            String::from("chart exports as grouped shapes; native c:chart is a later optimization")
        }
    };
    let problems: Vec<String> = src_problems
        .into_iter()
        .chain(scene.problems.iter().cloned())
        .collect();
    if !problems.is_empty() {
        let _ = write!(reason, " ({})", problems.join("; "));
    }
    if scene.items.is_empty() {
        w.fate(&c.id, ExportTier::Grouped, reason);
        return;
    }
    let nv = w.cnvpr(&c.id, c.alt.as_deref(), c.link.as_deref());
    let _ = write!(
        w.xml,
        r#"<p:grpSp><p:nvGrpSpPr>{nv}<p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr>{}</p:grpSpPr>"#,
        grp_xfrm(f)
    );
    for (i, item) in scene.items.iter().enumerate() {
        emit_chart_item(w, &c.id, i, item);
    }
    w.xml.push_str("</p:grpSp>");
    w.fate(&c.id, ExportTier::Grouped, reason);
}

/// Host a native chart part on the slide: a `p:graphicFrame` with the chart
/// graphic reference; the part itself (plus its embedded workbook) is
/// registered on `Shared` and written by the package assembler.
fn emit_native_chart(
    w: &mut SlideWriter,
    c: &ChartObject,
    f: Frame,
    part: super::chart_xml::NativeChartPart,
) {
    let n = w.shared.charts.len() + 1;
    w.shared.charts.push(part);
    let rid = w.rel(REL_CHART, format!("../charts/chart{n}.xml"), false);
    let nv = w.cnvpr(&c.id, c.alt.as_deref(), c.link.as_deref());
    let _ = write!(
        w.xml,
        concat!(
            "<p:graphicFrame><p:nvGraphicFramePr>{nv}<p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr>",
            r#"<p:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{cx}" cy="{cy}"/></p:xfrm>"#,
            r#"<a:graphic><a:graphicData uri="{ns_c}">"#,
            r#"<c:chart xmlns:c="{ns_c}" xmlns:r="{ns_r}" r:id="{rid}"/>"#,
            "</a:graphicData></a:graphic></p:graphicFrame>"
        ),
        nv = nv,
        x = emu(f.x),
        y = emu(f.y),
        cx = emu(f.w).max(1),
        cy = emu(f.h).max(1),
        ns_c = NS_C,
        ns_r = NS_R,
        rid = rid,
    );
    w.fate(
        &c.id,
        ExportTier::Native,
        "native c:chart with an embedded workbook (opt-in; the desktop-PowerPoint \
         Edit Data pass is not yet hand-verified)",
    );
}

fn emit_chart_item(w: &mut SlideWriter, chart_id: &str, i: usize, item: &crate::chart::ChartItem) {
    use crate::chart::{ChartItem, TextAnchor};
    let name = format!("{chart_id}-{i}");
    match item {
        ChartItem::Rect {
            x,
            y,
            w: rw,
            h: rh,
            fill,
            opacity,
        } => {
            let f = Frame {
                x: *x,
                y: *y,
                w: *rw,
                h: *rh,
            };
            let nv = w.cnvpr(&name, None, None);
            let _ = write!(
                w.xml,
                r#"<p:sp><p:nvSpPr>{nv}<p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr>{}{}<a:solidFill>{}</a:solidFill>{NO_LINE}</p:spPr></p:sp>"#,
                xfrm(f, None, false, false),
                prst_geom("rect", ""),
                srgb(*fill, Some(*opacity))
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
            let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for (px, py) in points {
                x0 = x0.min(*px);
                y0 = y0.min(*py);
                x1 = x1.max(*px);
                y1 = y1.max(*py);
            }
            let f = Frame {
                x: x0,
                y: y0,
                w: x1 - x0,
                h: y1 - y0,
            };
            let mut segs = Vec::with_capacity(points.len());
            segs.push(Seg::Move([points[0].0, points[0].1]));
            for (px, py) in &points[1..] {
                segs.push(Seg::Line([*px, *py]));
            }
            let dash_frag = dash_xml(dash.as_deref(), *width);
            let nv = w.cnvpr(&name, None, None);
            let _ = write!(
                w.xml,
                r#"<p:sp><p:nvSpPr>{nv}<p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr>{}{}<a:noFill/><a:ln w="{}" cap="rnd"><a:solidFill>{}</a:solidFill>{dash_frag}<a:round/></a:ln></p:spPr></p:sp>"#,
                xfrm(f, None, false, false),
                cust_geom(&segs, f),
                emu(*width).max(1),
                srgb(*stroke, None)
            );
        }
        ChartItem::Circle { cx, cy, r, fill } => {
            let f = Frame {
                x: cx - r,
                y: cy - r,
                w: r * 2.0,
                h: r * 2.0,
            };
            let nv = w.cnvpr(&name, None, None);
            let _ = write!(
                w.xml,
                r#"<p:sp><p:nvSpPr>{nv}<p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr>{}{}<a:solidFill>{}</a:solidFill>{NO_LINE}</p:spPr></p:sp>"#,
                xfrm(f, None, false, false),
                prst_geom("ellipse", ""),
                srgb(*fill, None)
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
            let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for (px, py) in points {
                x0 = x0.min(*px);
                y0 = y0.min(*py);
                x1 = x1.max(*px);
                y1 = y1.max(*py);
            }
            let f = Frame {
                x: x0,
                y: y0,
                w: x1 - x0,
                h: y1 - y0,
            };
            let mut segs = Vec::with_capacity(points.len() + 1);
            segs.push(Seg::Move([points[0].0, points[0].1]));
            for (px, py) in &points[1..] {
                segs.push(Seg::Line([*px, *py]));
            }
            segs.push(Seg::Close);
            let nv = w.cnvpr(&name, None, None);
            let _ = write!(
                w.xml,
                r#"<p:sp><p:nvSpPr>{nv}<p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr>{}{}<a:solidFill>{}</a:solidFill>{NO_LINE}</p:spPr></p:sp>"#,
                xfrm(f, None, false, false),
                cust_geom(&segs, f),
                srgb(*fill, Some(*opacity))
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
            // Chart text is positioned by (anchor point, baseline); a text
            // shape wants a box. Measure with the same stack the chart was
            // laid out with, so the box lands where the renderer drew it.
            let tw = w.fonts.measure(text, families, *size, *weight);
            let m = w.fonts.metrics(families, *size, *weight);
            let x0 = match anchor {
                TextAnchor::Start => *x,
                TextAnchor::Middle => x - tw / 2.0,
                TextAnchor::End => x - tw,
            };
            let pad = 1.0;
            let f = Frame {
                x: x0 - pad,
                y: y - m.ascent,
                w: tw + pad * 2.0,
                h: m.height.max(*size),
            };
            let algn = match anchor {
                TextAnchor::Start => "",
                TextAnchor::Middle => r#" algn="ctr""#,
                TextAnchor::End => r#" algn="r""#,
            };
            let bold = if *weight >= 600 { r#" b="1""# } else { "" };
            let family = resolved_family(w.fonts, families, *weight);
            let nv = w.cnvpr(&name, None, None);
            let _ = write!(
                w.xml,
                r#"<p:sp><p:nvSpPr>{nv}<p:cNvSpPr txBox="1"/><p:nvPr/></p:nvSpPr><p:spPr>{}{}<a:noFill/>{NO_LINE}</p:spPr><p:txBody><a:bodyPr wrap="none" lIns="0" tIns="0" rIns="0" bIns="0" anchor="t"><a:noAutofit/></a:bodyPr><a:lstStyle/><a:p><a:pPr{algn}/><a:r><a:rPr sz="{}"{bold}><a:solidFill>{}</a:solidFill><a:latin typeface="{}"/></a:rPr><a:t>{}</a:t></a:r></a:p></p:txBody></p:sp>"#,
                xfrm(f, None, false, false),
                prst_geom("rect", ""),
                sz100(*size),
                srgb(*color, None),
                esc(&family),
                esc(text)
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Package parts
// ---------------------------------------------------------------------------

fn rels_xml(rels: &[Rel]) -> String {
    let mut s = format!(
        r#"{XML_DECL}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#
    );
    for (i, r) in rels.iter().enumerate() {
        let mode = if r.external {
            r#" TargetMode="External""#
        } else {
            ""
        };
        let _ = write!(
            s,
            r#"<Relationship Id="rId{}" Type="{}" Target="{}"{mode}/>"#,
            i + 1,
            r.kind,
            esc(&r.target)
        );
    }
    s.push_str("</Relationships>");
    s
}

/// `ppt/theme/theme1.xml`, generated from the board theme: the palette's
/// roles become the `clrScheme` (so `@bg`/`@fg`/`@accent1` re-theme natively)
/// and the resolved title/body families become the `fontScheme`.
fn theme_xml(theme: &Theme, fonts: &FontStack) -> String {
    let pal = |token: &str, fallback: Rgb| -> String {
        theme
            .color(&format!("@{token}"))
            .map(hex6)
            .unwrap_or_else(|| hex6(fallback))
    };
    let fg = theme.color_or_fg(None);
    let bg = theme.bg();
    let dk1 = hex6(fg);
    let lt1 = hex6(bg);
    let dk2 = pal("body", fg);
    let lt2 = pal("surface", bg);
    let accent1 = pal("accent1", fg);
    let accents: Vec<String> = (1..=5)
        .map(|i| {
            theme
                .color(&format!("@cat{i}"))
                .map(hex6)
                .unwrap_or_else(|| accent1.clone())
        })
        .collect();
    let hlink = accent1.clone();
    let fol_hlink = pal("muted", fg);

    let title_role = theme.role("title").unwrap_or_else(|| theme.body());
    let major = resolved_family(fonts, &title_role.family, title_role.weight);
    let body_role = theme.body();
    let minor = resolved_family(fonts, &body_role.family, body_role.weight);

    let clr = format!(
        concat!(
            r#"<a:clrScheme name="chimaera">"#,
            r#"<a:dk1><a:srgbClr val="{dk1}"/></a:dk1>"#,
            r#"<a:lt1><a:srgbClr val="{lt1}"/></a:lt1>"#,
            r#"<a:dk2><a:srgbClr val="{dk2}"/></a:dk2>"#,
            r#"<a:lt2><a:srgbClr val="{lt2}"/></a:lt2>"#,
            r#"<a:accent1><a:srgbClr val="{a1}"/></a:accent1>"#,
            r#"<a:accent2><a:srgbClr val="{a2}"/></a:accent2>"#,
            r#"<a:accent3><a:srgbClr val="{a3}"/></a:accent3>"#,
            r#"<a:accent4><a:srgbClr val="{a4}"/></a:accent4>"#,
            r#"<a:accent5><a:srgbClr val="{a5}"/></a:accent5>"#,
            r#"<a:accent6><a:srgbClr val="{a6}"/></a:accent6>"#,
            r#"<a:hlink><a:srgbClr val="{hl}"/></a:hlink>"#,
            r#"<a:folHlink><a:srgbClr val="{fhl}"/></a:folHlink>"#,
            r#"</a:clrScheme>"#
        ),
        dk1 = dk1,
        lt1 = lt1,
        dk2 = dk2,
        lt2 = lt2,
        a1 = accent1,
        a2 = accents[0],
        a3 = accents[1],
        a4 = accents[2],
        a5 = accents[3],
        a6 = accents[4],
        hl = hlink,
        fhl = fol_hlink,
    );
    let font = format!(
        concat!(
            r#"<a:fontScheme name="chimaera">"#,
            r#"<a:majorFont><a:latin typeface="{major}"/><a:ea typeface=""/><a:cs typeface=""/></a:majorFont>"#,
            r#"<a:minorFont><a:latin typeface="{minor}"/><a:ea typeface=""/><a:cs typeface=""/></a:minorFont>"#,
            r#"</a:fontScheme>"#
        ),
        major = esc(&major),
        minor = esc(&minor),
    );
    // The mandatory fmtScheme, inert: solid phClr fills and plain lines, so
    // nothing here can restyle what the slides state explicitly.
    let solid = r#"<a:solidFill><a:schemeClr val="phClr"/></a:solidFill>"#;
    let ln = |w: u32| {
        format!(
            r#"<a:ln w="{w}" cap="flat" cmpd="sng" algn="ctr"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:prstDash val="solid"/></a:ln>"#
        )
    };
    let fmt = format!(
        concat!(
            r#"<a:fmtScheme name="chimaera">"#,
            "<a:fillStyleLst>{s}{s}{s}</a:fillStyleLst>",
            "<a:lnStyleLst>{l1}{l2}{l3}</a:lnStyleLst>",
            "<a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst>",
            "<a:bgFillStyleLst>{s}{s}{s}</a:bgFillStyleLst>",
            "</a:fmtScheme>"
        ),
        s = solid,
        l1 = ln(9525),
        l2 = ln(12700),
        l3 = ln(19050),
    );
    format!(
        r#"{XML_DECL}<a:theme xmlns:a="{NS_A}" name="chimaera"><a:themeElements>{clr}{font}{fmt}</a:themeElements><a:objectDefaults/><a:extraClrSchemeLst/></a:theme>"#
    )
}

const CLR_MAP: &str = concat!(
    r#"bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" "#,
    r#"accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" "#,
    r#"hlink="hlink" folHlink="folHlink""#
);

fn slide_master_xml() -> String {
    format!(
        concat!(
            r#"{decl}<p:sldMaster xmlns:a="{a}" xmlns:r="{r}" xmlns:p="{p}">"#,
            r#"<p:cSld><p:bg><p:bgPr><a:solidFill><a:schemeClr val="bg1"/></a:solidFill><a:effectLst/></p:bgPr></p:bg>"#,
            r#"<p:spTree>{root}</p:spTree></p:cSld>"#,
            r#"<p:clrMap {clrmap}/>"#,
            r#"<p:sldLayoutIdLst><p:sldLayoutId id="2147483649" r:id="rId1"/></p:sldLayoutIdLst>"#,
            r#"</p:sldMaster>"#
        ),
        decl = XML_DECL,
        a = NS_A,
        r = NS_R,
        p = NS_P,
        root = ROOT_GRP,
        clrmap = CLR_MAP,
    )
}

fn slide_layout_xml() -> String {
    format!(
        concat!(
            r#"{decl}<p:sldLayout xmlns:a="{a}" xmlns:r="{r}" xmlns:p="{p}" type="blank" preserve="1">"#,
            r#"<p:cSld name="Blank"><p:spTree>{root}</p:spTree></p:cSld>"#,
            r#"<p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr>"#,
            r#"</p:sldLayout>"#
        ),
        decl = XML_DECL,
        a = NS_A,
        r = NS_R,
        p = NS_P,
        root = ROOT_GRP,
    )
}

fn notes_master_xml() -> String {
    format!(
        concat!(
            r#"{decl}<p:notesMaster xmlns:a="{a}" xmlns:r="{r}" xmlns:p="{p}">"#,
            r#"<p:cSld><p:spTree>{root}</p:spTree></p:cSld>"#,
            r#"<p:clrMap {clrmap}/>"#,
            r#"</p:notesMaster>"#
        ),
        decl = XML_DECL,
        a = NS_A,
        r = NS_R,
        p = NS_P,
        root = ROOT_GRP,
        clrmap = CLR_MAP,
    )
}

/// A plain-text notes slide: one body placeholder, one paragraph per line.
fn notes_slide_xml(notes: &str) -> String {
    let mut paras = String::new();
    for line in notes.split('\n') {
        if line.is_empty() {
            paras.push_str("<a:p/>");
        } else {
            let _ = write!(paras, "<a:p><a:r><a:t>{}</a:t></a:r></a:p>", esc(line));
        }
    }
    format!(
        concat!(
            r#"{decl}<p:notes xmlns:a="{a}" xmlns:r="{r}" xmlns:p="{p}">"#,
            r#"<p:cSld><p:spTree>{root}"#,
            r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="Notes Placeholder 1"/>"#,
            r#"<p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr>"#,
            r#"<p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr>"#,
            r#"<p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/>{paras}</p:txBody></p:sp>"#,
            r#"</p:spTree></p:cSld>"#,
            r#"<p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr>"#,
            r#"</p:notes>"#
        ),
        decl = XML_DECL,
        a = NS_A,
        r = NS_R,
        p = NS_P,
        root = ROOT_GRP,
        paras = paras,
    )
}

fn core_xml(board: &Board) -> String {
    // A fixed epoch, never the wall clock: identical input, identical bytes.
    let stamp = "2000-01-01T00:00:00Z";
    let title = board.title.as_deref().unwrap_or("Board");
    format!(
        concat!(
            r#"{decl}<cp:coreProperties "#,
            r#"xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" "#,
            r#"xmlns:dc="http://purl.org/dc/elements/1.1/" "#,
            r#"xmlns:dcterms="http://purl.org/dc/terms/" "#,
            r#"xmlns:dcmitype="http://purl.org/dc/dcmitype/" "#,
            r#"xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">"#,
            r#"<dc:title>{title}</dc:title>"#,
            r#"<dc:creator>chimaera board</dc:creator>"#,
            r#"<cp:lastModifiedBy>chimaera board</cp:lastModifiedBy>"#,
            r#"<dcterms:created xsi:type="dcterms:W3CDTF">{stamp}</dcterms:created>"#,
            r#"<dcterms:modified xsi:type="dcterms:W3CDTF">{stamp}</dcterms:modified>"#,
            r#"</cp:coreProperties>"#
        ),
        decl = XML_DECL,
        title = esc(title),
        stamp = stamp,
    )
}

fn app_xml(slide_count: usize) -> String {
    format!(
        concat!(
            r#"{decl}<Properties "#,
            r#"xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties" "#,
            r#"xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes">"#,
            r#"<Application>chimaera board</Application>"#,
            r#"<Slides>{n}</Slides>"#,
            r#"<PresentationFormat>Custom</PresentationFormat>"#,
            r#"</Properties>"#
        ),
        decl = XML_DECL,
        n = slide_count,
    )
}

// ---------------------------------------------------------------------------
// The writer
// ---------------------------------------------------------------------------

/// How a `chart` object lands in the deck.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ChartFidelity {
    /// The default: a group of editable vector shapes with real text —
    /// editable in every consumer, including the ones that flatten `c:chart`.
    #[default]
    Grouped,
    /// Opt-in: a real `c:chart` part with an embedded minimal workbook where
    /// the chart maps cleanly (plain/grouped/stacked bars, lines, scatters on
    /// category or linear axes); everything else degrades per-chart to
    /// grouped shapes with the reason in its fate. Stays opt-in until the
    /// plan's hand-verified "Edit Data opens" pass in desktop PowerPoint
    /// (docs/board-plan.md §11).
    Native,
}

/// Options for [`write_pptx_with`]. Non-exhaustive by convention: construct
/// via `PptxOptions::default()` and set fields, so new knobs never break
/// callers.
#[derive(Debug, Clone, Copy, Default)]
pub struct PptxOptions {
    pub chart_fidelity: ChartFidelity,
}

/// Write a normalized board as a native `.pptx`, returning the per-object
/// fate report.
///
/// Image `src` paths are read as given (the caller resolves them against the
/// workspace before calling, or runs from the workspace root); a missing or
/// undecodable source degrades to a placeholder box with a reported fate, so
/// this function never fails on content — only on I/O to `out`.
pub fn write_pptx(
    board: &Board,
    theme: &Theme,
    fonts: &FontStack,
    workspace: Option<&std::path::Path>,
    out: &mut impl std::io::Write,
) -> Result<ExportReport> {
    write_pptx_with(board, theme, fonts, workspace, &PptxOptions::default(), out)
}

/// [`write_pptx`] with explicit [`PptxOptions`] — the default options write
/// byte-identical output to `write_pptx`.
pub fn write_pptx_with(
    board: &Board,
    theme: &Theme,
    fonts: &FontStack,
    workspace: Option<&std::path::Path>,
    opts: &PptxOptions,
    out: &mut impl std::io::Write,
) -> Result<ExportReport> {
    let mut shared = Shared {
        media: Vec::new(),
        media_by_src: BTreeMap::new(),
        exts: BTreeSet::new(),
        fates: Vec::new(),
        charts: Vec::new(),
    };

    // Build every slide first: media, fates and content types accumulate.
    let mut slides: Vec<(String, Vec<Rel>)> = Vec::new();
    for (i, page) in board.pages.iter().enumerate() {
        slides.push(build_slide(
            board,
            page,
            i + 1,
            theme,
            fonts,
            workspace,
            opts,
            &mut shared,
        ));
    }
    let notes: Vec<Option<&str>> = board.pages.iter().map(|p| p.notes.as_deref()).collect();
    let any_notes = notes.iter().any(Option::is_some);
    // Decks without a table stay byte-identical: the tableStyles part, its
    // content type and its rel exist only when an a:tbl references them.
    let any_table = board.objects().any(|(_, o)| matches!(o, Object::Table(_)));

    // --- presentation.xml + rels --------------------------------------
    let mut pres_rels: Vec<Rel> = vec![Rel {
        kind: REL_SLIDE_MASTER,
        target: "slideMasters/slideMaster1.xml".to_string(),
        external: false,
    }];
    for i in 0..slides.len() {
        pres_rels.push(Rel {
            kind: REL_SLIDE,
            target: format!("slides/slide{}.xml", i + 1),
            external: false,
        });
    }
    let notes_master_rid = if any_notes {
        pres_rels.push(Rel {
            kind: REL_NOTES_MASTER,
            target: "notesMasters/notesMaster1.xml".to_string(),
            external: false,
        });
        Some(format!("rId{}", pres_rels.len()))
    } else {
        None
    };
    pres_rels.push(Rel {
        kind: REL_PRES_PROPS,
        target: "presProps.xml".to_string(),
        external: false,
    });
    pres_rels.push(Rel {
        kind: REL_THEME,
        target: "theme/theme1.xml".to_string(),
        external: false,
    });
    if any_table {
        pres_rels.push(Rel {
            kind: REL_TABLE_STYLES,
            target: "tableStyles.xml".to_string(),
            external: false,
        });
    }

    let mut sld_ids = String::new();
    for i in 0..slides.len() {
        let _ = write!(
            sld_ids,
            r#"<p:sldId id="{}" r:id="rId{}"/>"#,
            256 + i,
            2 + i
        );
    }
    let notes_master_lst = match &notes_master_rid {
        Some(rid) => {
            format!(r#"<p:notesMasterIdLst><p:notesMasterId r:id="{rid}"/></p:notesMasterIdLst>"#)
        }
        None => String::new(),
    };
    let presentation = format!(
        concat!(
            r#"{decl}<p:presentation xmlns:a="{a}" xmlns:r="{r}" xmlns:p="{p}">"#,
            r#"<p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst>"#,
            "{notes_masters}",
            r#"<p:sldIdLst>{sld_ids}</p:sldIdLst>"#,
            r#"<p:sldSz cx="{cx}" cy="{cy}"/>"#,
            r#"<p:notesSz cx="6858000" cy="9144000"/>"#,
            r#"</p:presentation>"#
        ),
        decl = XML_DECL,
        a = NS_A,
        r = NS_R,
        p = NS_P,
        notes_masters = notes_master_lst,
        sld_ids = sld_ids,
        cx = emu(board.canvas.width()).max(1),
        cy = emu(board.canvas.height()).max(1),
    );

    // --- [Content_Types].xml ------------------------------------------
    let mut ct = format!(
        concat!(
            r#"{decl}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">"#,
            r#"<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>"#,
            r#"<Default Extension="xml" ContentType="application/xml"/>"#
        ),
        decl = XML_DECL
    );
    for ext in &shared.exts {
        let ctype = match *ext {
            "png" => "image/png",
            "jpeg" => "image/jpeg",
            "svg" => "image/svg+xml",
            _ => continue,
        };
        let _ = write!(ct, r#"<Default Extension="{ext}" ContentType="{ctype}"/>"#);
    }
    if !shared.charts.is_empty() {
        // The embedded chart workbooks under ppt/embeddings/.
        ct.push_str(
            r#"<Default Extension="xlsx" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"/>"#,
        );
    }
    let over = |part: &str, ctype: &str| -> String {
        format!(r#"<Override PartName="{part}" ContentType="{ctype}"/>"#)
    };
    ct.push_str(&over(
        "/ppt/presentation.xml",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml",
    ));
    ct.push_str(&over(
        "/ppt/presProps.xml",
        "application/vnd.openxmlformats-officedocument.presentationml.presProps+xml",
    ));
    ct.push_str(&over(
        "/ppt/theme/theme1.xml",
        "application/vnd.openxmlformats-officedocument.theme+xml",
    ));
    ct.push_str(&over(
        "/ppt/slideMasters/slideMaster1.xml",
        "application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml",
    ));
    ct.push_str(&over(
        "/ppt/slideLayouts/slideLayout1.xml",
        "application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml",
    ));
    for i in 0..slides.len() {
        ct.push_str(&over(
            &format!("/ppt/slides/slide{}.xml", i + 1),
            "application/vnd.openxmlformats-officedocument.presentationml.slide+xml",
        ));
    }
    for i in 0..shared.charts.len() {
        ct.push_str(&over(
            &format!("/ppt/charts/chart{}.xml", i + 1),
            "application/vnd.openxmlformats-officedocument.drawingml.chart+xml",
        ));
    }
    if any_notes {
        ct.push_str(&over(
            "/ppt/notesMasters/notesMaster1.xml",
            "application/vnd.openxmlformats-officedocument.presentationml.notesMaster+xml",
        ));
        for (i, n) in notes.iter().enumerate() {
            if n.is_some() {
                ct.push_str(&over(
                    &format!("/ppt/notesSlides/notesSlide{}.xml", i + 1),
                    "application/vnd.openxmlformats-officedocument.presentationml.notesSlide+xml",
                ));
            }
        }
    }
    if any_table {
        ct.push_str(&over(
            "/ppt/tableStyles.xml",
            "application/vnd.openxmlformats-officedocument.presentationml.tableStyles+xml",
        ));
    }
    ct.push_str(&over(
        "/docProps/core.xml",
        "application/vnd.openxmlformats-package.core-properties+xml",
    ));
    ct.push_str(&over(
        "/docProps/app.xml",
        "application/vnd.openxmlformats-officedocument.extended-properties+xml",
    ));
    ct.push_str("</Types>");

    // --- Assemble the package, in fixed order --------------------------
    let root_rels = rels_xml(&[
        Rel {
            kind: REL_OFFICE_DOC,
            target: "ppt/presentation.xml".to_string(),
            external: false,
        },
        Rel {
            kind: REL_CORE,
            target: "docProps/core.xml".to_string(),
            external: false,
        },
        Rel {
            kind: REL_APP,
            target: "docProps/app.xml".to_string(),
            external: false,
        },
    ]);
    let master_rels = rels_xml(&[
        Rel {
            kind: REL_SLIDE_LAYOUT,
            target: "../slideLayouts/slideLayout1.xml".to_string(),
            external: false,
        },
        Rel {
            kind: REL_THEME,
            target: "../theme/theme1.xml".to_string(),
            external: false,
        },
    ]);
    let layout_rels = rels_xml(&[Rel {
        kind: REL_SLIDE_MASTER,
        target: "../slideMasters/slideMaster1.xml".to_string(),
        external: false,
    }]);

    let mut parts: Vec<(String, Vec<u8>)> = vec![
        ("[Content_Types].xml".to_string(), ct.into_bytes()),
        ("_rels/.rels".to_string(), root_rels.into_bytes()),
        ("docProps/core.xml".to_string(), core_xml(board).into_bytes()),
        (
            "docProps/app.xml".to_string(),
            app_xml(slides.len()).into_bytes(),
        ),
        ("ppt/presentation.xml".to_string(), presentation.into_bytes()),
        (
            "ppt/_rels/presentation.xml.rels".to_string(),
            rels_xml(&pres_rels).into_bytes(),
        ),
        (
            "ppt/presProps.xml".to_string(),
            format!(
                r#"{XML_DECL}<p:presentationPr xmlns:a="{NS_A}" xmlns:r="{NS_R}" xmlns:p="{NS_P}"/>"#
            )
            .into_bytes(),
        ),
        (
            "ppt/theme/theme1.xml".to_string(),
            theme_xml(theme, fonts).into_bytes(),
        ),
        (
            "ppt/slideMasters/slideMaster1.xml".to_string(),
            slide_master_xml().into_bytes(),
        ),
        (
            "ppt/slideMasters/_rels/slideMaster1.xml.rels".to_string(),
            master_rels.into_bytes(),
        ),
        (
            "ppt/slideLayouts/slideLayout1.xml".to_string(),
            slide_layout_xml().into_bytes(),
        ),
        (
            "ppt/slideLayouts/_rels/slideLayout1.xml.rels".to_string(),
            layout_rels.into_bytes(),
        ),
    ];
    if any_notes {
        parts.push((
            "ppt/notesMasters/notesMaster1.xml".to_string(),
            notes_master_xml().into_bytes(),
        ));
        parts.push((
            "ppt/notesMasters/_rels/notesMaster1.xml.rels".to_string(),
            rels_xml(&[Rel {
                kind: REL_THEME,
                target: "../theme/theme1.xml".to_string(),
                external: false,
            }])
            .into_bytes(),
        ));
    }
    if any_table {
        parts.push((
            "ppt/tableStyles.xml".to_string(),
            format!(r#"{XML_DECL}<a:tblStyleLst xmlns:a="{NS_A}" def="{TABLE_STYLE_ID}"/>"#)
                .into_bytes(),
        ));
    }
    for (i, (xml, rels)) in slides.iter().enumerate() {
        parts.push((
            format!("ppt/slides/slide{}.xml", i + 1),
            xml.clone().into_bytes(),
        ));
        parts.push((
            format!("ppt/slides/_rels/slide{}.xml.rels", i + 1),
            rels_xml(rels).into_bytes(),
        ));
    }
    for (i, cp) in shared.charts.iter().enumerate() {
        parts.push((
            format!("ppt/charts/chart{}.xml", i + 1),
            cp.xml.clone().into_bytes(),
        ));
        // rId1 in a chart part's rels is always its embedded workbook — the
        // chartSpace's c:externalData counts on it.
        parts.push((
            format!("ppt/charts/_rels/chart{}.xml.rels", i + 1),
            rels_xml(&[Rel {
                kind: REL_PACKAGE,
                target: format!("../embeddings/data{}.xlsx", i + 1),
                external: false,
            }])
            .into_bytes(),
        ));
        parts.push((
            format!("ppt/embeddings/data{}.xlsx", i + 1),
            cp.xlsx.clone(),
        ));
    }
    for (i, n) in notes.iter().enumerate() {
        let Some(text) = n else { continue };
        parts.push((
            format!("ppt/notesSlides/notesSlide{}.xml", i + 1),
            notes_slide_xml(text).into_bytes(),
        ));
        parts.push((
            format!("ppt/notesSlides/_rels/notesSlide{}.xml.rels", i + 1),
            rels_xml(&[
                Rel {
                    kind: REL_NOTES_MASTER,
                    target: "../notesMasters/notesMaster1.xml".to_string(),
                    external: false,
                },
                Rel {
                    kind: REL_SLIDE,
                    target: format!("../slides/slide{}.xml", i + 1),
                    external: false,
                },
            ])
            .into_bytes(),
        ));
    }
    for m in &shared.media {
        parts.push((format!("ppt/media/{}", m.name), m.bytes.clone()));
    }

    // --- Zip, deterministically ----------------------------------------
    // ZipWriter needs Seek, which `out` does not promise; a deck is small,
    // so the archive is assembled in memory and written through once.
    let cursor = std::io::Cursor::new(Vec::new());
    let mut zw = zip::ZipWriter::new(cursor);
    let mtime = zip::DateTime::from_date_and_time(2000, 1, 1, 0, 0, 0)
        .expect("a constant, valid zip datetime");
    let zip_opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(mtime);
    for (name, bytes) in &parts {
        zw.start_file(name.clone(), zip_opts)
            .with_context(|| format!("starting zip entry {name}"))?;
        zw.write_all(bytes)
            .with_context(|| format!("writing zip entry {name}"))?;
    }
    let cursor = zw.finish().context("finishing the pptx archive")?;
    out.write_all(cursor.get_ref())
        .context("writing the pptx bytes")?;

    Ok(ExportReport {
        objects: shared.fates,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_slide(
    board: &Board,
    page: &Page,
    slide_no: usize,
    theme: &Theme,
    fonts: &FontStack,
    workspace: Option<&std::path::Path>,
    opts: &PptxOptions,
    shared: &mut Shared,
) -> (String, Vec<Rel>) {
    // Slot and anchor resolution — the same single geometry truth the
    // renderer draws from. Slot-placed objects live only in this map (they
    // carry no at/size of their own), and connector endpoints bind to it.
    let index = crate::slots::resolve_page_frames(board, page, theme, Some(fonts));
    let mut w = SlideWriter {
        theme,
        fonts,
        page,
        workspace: workspace.map(|p| p.to_path_buf()),
        index,
        chart_fidelity: opts.chart_fidelity,
        xml: String::new(),
        rels: Vec::new(),
        next_id: 2,
        shared,
    };
    // rId1 is always the layout.
    w.rel(
        REL_SLIDE_LAYOUT,
        "../slideLayouts/slideLayout1.xml".to_string(),
        false,
    );
    for obj in &page.objects {
        emit_object(&mut w, obj);
    }
    if page.notes.is_some() {
        w.rel(
            REL_NOTES_SLIDE,
            format!("../notesSlides/notesSlide{slide_no}.xml"),
            false,
        );
    }
    let bg_ref = page
        .background
        .as_ref()
        .and_then(|b| b.fill.as_deref())
        .unwrap_or("@bg");
    let bg = color_choice(theme, Some(bg_ref), None);
    let xml = format!(
        concat!(
            r#"{decl}<p:sld xmlns:a="{a}" xmlns:r="{r}" xmlns:p="{p}">"#,
            r#"<p:cSld><p:bg><p:bgPr><a:solidFill>{bg}</a:solidFill><a:effectLst/></p:bgPr></p:bg>"#,
            r#"<p:spTree>{root}{shapes}</p:spTree></p:cSld>"#,
            r#"<p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr>"#,
            r#"</p:sld>"#
        ),
        decl = XML_DECL,
        a = NS_A,
        r = NS_R,
        p = NS_P,
        bg = bg,
        root = ROOT_GRP,
        shapes = w.xml,
    );
    (xml, w.rels)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    fn board(json: &str) -> Board {
        let mut b = crate::parse(json).unwrap();
        crate::normalize(&mut b);
        b
    }

    fn write(b: &Board) -> (Vec<u8>, ExportReport) {
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let mut out = Vec::new();
        let report = write_pptx(b, &theme, &fonts, None, &mut out).unwrap();
        (out, report)
    }

    fn read_part(bytes: &[u8], name: &str) -> String {
        let mut ar = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
        let mut f = ar
            .by_name(name)
            .unwrap_or_else(|_| panic!("missing {name}"));
        let mut s = String::new();
        f.read_to_string(&mut s).unwrap();
        s
    }

    /// A ~30-line well-formedness check: balanced tags, balanced attribute
    /// quotes, and no raw ampersands. Not a validator — a tripwire.
    fn assert_well_formed(xml: &str) {
        let mut stack: Vec<String> = Vec::new();
        let b = xml.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'<' {
                let end = xml[i..].find('>').map(|e| i + e).expect("unclosed tag");
                let tag = &xml[i + 1..end];
                if tag.starts_with('?') || tag.starts_with("!--") {
                    // declaration or comment
                } else if let Some(name) = tag.strip_prefix('/') {
                    assert_eq!(
                        stack.pop().as_deref(),
                        Some(name.trim()),
                        "mismatched close"
                    );
                } else {
                    assert_eq!(tag.matches('"').count() % 2, 0, "unbalanced quotes: {tag}");
                    if !tag.ends_with('/') {
                        let name = tag.split_whitespace().next().unwrap_or("").to_string();
                        assert!(!name.is_empty(), "empty tag name");
                        stack.push(name);
                    }
                }
                i = end + 1;
            } else {
                if b[i] == b'&' {
                    let rest = &xml[i..xml.len().min(i + 6)];
                    let ok = ["&amp;", "&lt;", "&gt;", "&quot;", "&apos;", "&#"]
                        .iter()
                        .any(|e| rest.starts_with(e));
                    assert!(ok, "raw ampersand near {rest:?}");
                }
                i += 1;
            }
        }
        assert!(stack.is_empty(), "unclosed tags: {stack:?}");
    }

    const DECK: &str = r#"{
      "format": "chimaera.board", "formatVersion": 1,
      "title": "Parser rewrite — design review",
      "canvas": { "size": [960, 540] },
      "pages": [
        {
          "id": "cover",
          "background": { "fill": "@bg" },
          "objects": [
            { "id": "deck-title", "type": "text", "role": "title", "at": [72, 64], "size": [816, 80],
              "text": ["The parser rewrite is 3× faster"], "alt": "deck title" },
            { "id": "subtitle", "type": "text", "role": "subtitle", "at": [72, 160], "size": [816, 48],
              "text": [{ "runs": [
                { "t": "and it deletes the " },
                { "t": "retry hack", "b": true, "color": "@accent1", "size": 25 },
                { "t": " for good", "i": true, "u": true, "link": "https://example.com/doi" }
              ] }] }
          ],
          "notes": "Open with the claim, not the design."
        },
        {
          "id": "bench",
          "objects": [
            { "id": "heading", "type": "text", "role": "heading", "at": [72, 48], "size": [816, 48],
              "text": ["Parse time drops on every fixture"] },
            { "id": "bench-chart", "type": "chart", "at": [72, 120], "size": [460, 300],
              "data": { "origin": "command", "values": [
                { "fixture": "large.json", "ms": 812, "build": "before" },
                { "fixture": "large.json", "ms": 244, "build": "after" },
                { "fixture": "small.json", "ms": 91, "build": "before" },
                { "fixture": "small.json", "ms": 30, "build": "after" } ] },
              "x": { "field": "fixture", "type": "nominal" },
              "y": { "field": "ms", "type": "quantitative", "title": "Parse time (ms)" },
              "color": { "field": "build" },
              "marks": [ { "mark": "bar", "stack": "group" } ] },
            { "id": "callout", "type": "shape", "geo": "roundRect",
              "at": [580, 170], "size": [300, 110], "radius": 8,
              "fill": "@surface", "fillOpacity": 0.9,
              "stroke": { "color": "@accent1", "width": 1.5, "dash": [4, 3] },
              "text": [{ "runs": [{ "t": "3.3× median", "b": true }] }],
              "link": "https://example.com/bench" },
            { "id": "arrow-1", "type": "connector", "geo": "straight",
              "from": { "object": "callout", "side": "left" },
              "to": { "object": "bench-chart", "side": "right" },
              "stroke": { "color": "@fg", "width": 1.5 }, "tailEnd": "arrow",
              "text": ["median"], "labelAt": 0.4 },
            { "id": "legend-group", "type": "group", "objects": [
              { "id": "swatch", "type": "shape", "geo": "rect", "at": [600, 420], "size": [16, 16], "fill": "@accent1" },
              { "id": "swatch-label", "type": "text", "at": [624, 418], "size": [200, 20], "text": ["after"] } ] },
            { "id": "wing", "type": "shape", "geo": "path",
              "at": [80, 440], "size": [120, 60],
              "d": "M 80 470 A 30 30 0 0 1 140 470 L 200 470 L 200 500 L 80 500 Z",
              "fill": "@accent1" },
            { "id": "missing-figure", "type": "image", "src": "does/not/exist.png",
              "at": [700, 420], "size": [120, 80] },
            { "id": "mystery", "type": "hologram", "beam": true }
          ]
        }
      ]
    }"#;

    #[test]
    fn a_full_deck_writes_a_valid_package() {
        let b = board(DECK);
        let (bytes, report) = write(&b);
        let mut ar = zip::ZipArchive::new(std::io::Cursor::new(bytes.clone())).unwrap();
        let names: Vec<String> = (0..ar.len())
            .map(|i| ar.by_index(i).unwrap().name().to_string())
            .collect();
        for required in [
            "[Content_Types].xml",
            "_rels/.rels",
            "docProps/core.xml",
            "docProps/app.xml",
            "ppt/presentation.xml",
            "ppt/_rels/presentation.xml.rels",
            "ppt/presProps.xml",
            "ppt/theme/theme1.xml",
            "ppt/slideMasters/slideMaster1.xml",
            "ppt/slideMasters/_rels/slideMaster1.xml.rels",
            "ppt/slideLayouts/slideLayout1.xml",
            "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
            "ppt/notesMasters/notesMaster1.xml",
            "ppt/slides/slide1.xml",
            "ppt/slides/slide2.xml",
            "ppt/slides/_rels/slide1.xml.rels",
            "ppt/slides/_rels/slide2.xml.rels",
            "ppt/notesSlides/notesSlide1.xml",
        ] {
            assert!(names.iter().any(|n| n == required), "missing {required}");
        }
        // Every XML part is well-formed.
        for name in &names {
            if name.ends_with(".xml") || name.ends_with(".rels") {
                assert_well_formed(&read_part(&bytes, name));
            }
        }
        // The slide size is exact EMU arithmetic.
        let pres = read_part(&bytes, "ppt/presentation.xml");
        assert!(
            pres.contains(r#"<p:sldSz cx="12192000" cy="6858000"/>"#),
            "{pres}"
        );
        // Styled runs land with sz = pt × 100 and real run properties.
        let slide1 = read_part(&bytes, "ppt/slides/slide1.xml");
        assert!(slide1.contains(r#"sz="2500""#), "run size × 100: {slide1}");
        assert!(slide1.contains(r#"b="1""#));
        assert!(slide1.contains(r#"u="sng""#));
        assert!(slide1.contains("<a:noAutofit/>"));
        // @accent1 maps onto the scheme; @surface resolves to srgb.
        assert!(
            slide1.contains(r#"<a:schemeClr val="accent1"/>"#),
            "{slide1}"
        );
        let slide2 = read_part(&bytes, "ppt/slides/slide2.xml");
        assert!(
            slide2.contains("<a:srgbClr val=\"1C2027\""),
            "@surface: {slide2}"
        );
        // Fill alpha exports as <a:alpha>.
        assert!(slide2.contains(r#"<a:alpha val="90000"/>"#), "{slide2}");
        // The connector carries its own xfrm and a triangle tail.
        assert!(slide2.contains("<p:cxnSp>"));
        assert!(slide2.contains(r#"<a:tailEnd type="triangle"/>"#));
        // Alt text survives as descr.
        assert!(slide1.contains(r#"descr="deck title""#));
        // Hyperlinks land as rels.
        let rels1 = read_part(&bytes, "ppt/slides/_rels/slide1.xml.rels");
        assert!(rels1.contains("example.com/doi"), "{rels1}");
        assert!(rels1.contains(r#"TargetMode="External""#));
        // The chart became a group with real text labels.
        assert!(slide2.contains("<p:grpSp>"));
        assert!(slide2.contains("<a:t>large.json</a:t>"), "{slide2}");
        // Notes are plain text.
        let notes = read_part(&bytes, "ppt/notesSlides/notesSlide1.xml");
        assert!(notes.contains("Open with the claim"));
        // The report exists and is serializable.
        assert!(!report.objects.is_empty());
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains(r#""tier":"grouped""#), "{json}");
    }

    #[test]
    fn identical_input_writes_identical_bytes() {
        let b = board(DECK);
        let (a, _) = write(&b);
        let (c, _) = write(&b);
        assert_eq!(a, c, "the export must be deterministic");
    }

    #[test]
    fn custgeom_flattens_arcs_and_stays_in_local_space() {
        // Grid-aligned geometry: normalize snaps frames to the 8 pt grid,
        // and the path data must sit inside the snapped frame.
        let b = board(
            r##"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[400,300]},
                "pages":[{"id":"p","objects":[
                  {"id":"blob","type":"shape","geo":"path",
                   "at":[8,8],"size":[104,56],
                   "d":"M 8 40 A 24 24 0 0 1 56 40 L 112 40 L 112 64 L 8 64 Z",
                   "fill":"#ff0000"}]}]}"##,
        );
        let (bytes, report) = write(&b);
        let slide = read_part(&bytes, "ppt/slides/slide1.xml");
        // Rule 1: no arcTo, ever. The arc became cubics.
        assert!(!slide.contains("arcTo"), "{slide}");
        assert!(slide.contains("<a:cubicBezTo>"), "{slide}");
        // Rule 2: path space equals the ext (104 × 56 pt).
        assert!(
            slide.contains(r#"<a:path w="1320800" h="711200">"#),
            "{slide}"
        );
        // Every coordinate sits inside [0, ext].
        let mut rest = slide.as_str();
        let mut seen = 0;
        while let Some(p) = rest.find("<a:pt x=\"") {
            rest = &rest[p + 9..];
            let x: i64 = rest[..rest.find('"').unwrap()].parse().unwrap();
            let yq = rest.find("y=\"").unwrap();
            let ytail = &rest[yq + 3..];
            let y: i64 = ytail[..ytail.find('"').unwrap()].parse().unwrap();
            assert!((0..=1_320_800).contains(&x), "x {x} out of [0, ext]");
            assert!((0..=711_200).contains(&y), "y {y} out of [0, ext]");
            seen += 1;
        }
        assert!(seen >= 4, "expected path points, saw {seen}");
        assert_eq!(report.objects[0].tier, ExportTier::Native);
    }

    #[test]
    fn emu_arithmetic_is_exact() {
        assert_eq!(emu(1.0), 12_700);
        assert_eq!(emu(0.5), 6_350);
        assert_eq!(emu(72.0), 914_400);
        assert_eq!(emu(960.0), 12_192_000);
        assert_eq!(emu(540.0), 6_858_000);
        assert_eq!(emu(f64::NAN), 0);
        assert_eq!(sz100(20.0), 2_000);
        assert_eq!(sz100(12.5), 1_250);
    }

    #[test]
    fn markup_in_text_is_escaped_everywhere() {
        let b = board(
            r#"{"format":"chimaera.board","formatVersion":1,
                "title":"<script>&\" deck",
                "canvas":{"size":[400,200]},
                "pages":[{"id":"p","objects":[
                  {"id":"t","type":"text","at":[8,8],"size":[380,60],
                   "text":["<script>&\" title"]}]}]}"#,
        );
        let (bytes, _) = write(&b);
        let slide = read_part(&bytes, "ppt/slides/slide1.xml");
        assert!(!slide.contains("<script>"), "{slide}");
        assert!(slide.contains("&lt;script&gt;&amp;&quot; title"), "{slide}");
        let core = read_part(&bytes, "docProps/core.xml");
        assert!(!core.contains("<script>"), "{core}");
        assert_well_formed(&slide);
        assert_well_formed(&core);
    }

    #[test]
    fn the_report_covers_every_object_id() {
        let b = board(DECK);
        let (_, report) = write(&b);
        let expected: Vec<&str> = b.objects().map(|(_, o)| o.id()).collect();
        let got: Vec<&str> = report.objects.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(got.len(), expected.len(), "{got:?} vs {expected:?}");
        for id in expected {
            assert!(got.contains(&id), "no fate for {id:?}");
        }
        // The named degradations say why.
        let fate = |id: &str| report.objects.iter().find(|f| f.id == id).unwrap();
        assert_eq!(fate("bench-chart").tier, ExportTier::Grouped);
        assert_eq!(fate("missing-figure").tier, ExportTier::Raster);
        assert!(fate("missing-figure").reason.contains("not found"));
        assert!(fate("mystery").reason.starts_with("skipped:"));
        assert!(fate("deck-title").reason.contains("placeholder"));
    }

    #[test]
    fn slot_placed_objects_export_at_their_resolved_frames() {
        // No explicit at/size anywhere: every frame comes from the "two-up"
        // layout through slots::resolve_page_frames — the regression here is
        // a slide that opens with zero shapes while the report says native.
        let b = board(
            r#"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[960,540]},
                "pages":[{"id":"p","layout":"two-up","objects":[
                  {"id":"slot-title","type":"text","role":"title","slot":"title",
                   "text":["Slots place this title"]},
                  {"id":"bench","type":"chart","slot":"body-left",
                   "data":{"origin":"command","values":[{"f":"a","v":1},{"f":"b","v":2}]},
                   "x":{"field":"f","type":"nominal"},
                   "y":{"field":"v","type":"quantitative"},
                   "marks":[{"mark":"bar"}]},
                  {"id":"panel","type":"shape","geo":"rect","slot":"body-right","fill":"@surface"},
                  {"id":"tie","type":"connector","geo":"straight",
                   "from":{"object":"panel","side":"left"},
                   "to":{"object":"bench","side":"right"}},
                  {"id":"lost","type":"text","slot":"left-rail","text":["never lands"]}]}]}"#,
        );
        let (bytes, report) = write(&b);
        let slide = read_part(&bytes, "ppt/slides/slide1.xml");
        // The title round-trips as real text, and the slide has real shapes.
        assert!(slide.contains("Slots place this title"), "{slide}");
        assert!(slide.matches("<p:sp>").count() >= 2, "{slide}");
        // Talk-dark spacing (margin [64,72,64,72], gap 24) puts two-up's
        // title band at (72, 64) and body-left at (72, 152): the exported
        // xfrms sit exactly on the resolved frames.
        assert!(
            slide.contains(r#"<a:off x="914400" y="812800"/>"#),
            "title frame: {slide}"
        );
        assert!(
            slide.contains(r#"<a:chOff x="914400" y="1930400"/>"#),
            "chart frame: {slide}"
        );
        // The connector binds to the resolved frame edges: panel's left edge
        // (492, 314) to the chart's right edge (468, 314), so its own xfrm
        // is off=(468, 314) EMU with flipH (and the honest 1-EMU height).
        assert!(slide.contains("<p:cxnSp>"), "{slide}");
        assert!(
            slide.contains(r#"<a:xfrm flipH="1"><a:off x="5943600" y="3987800"/><a:ext cx="304800" cy="1"/></a:xfrm>"#),
            "connector xfrm: {slide}"
        );
        // Fates: placed objects are native/grouped; the dangling slot is
        // honest about why nothing landed.
        let fate = |id: &str| report.objects.iter().find(|f| f.id == id).unwrap();
        assert_eq!(fate("slot-title").tier, ExportTier::Native);
        assert_eq!(fate("bench").tier, ExportTier::Grouped);
        assert_eq!(fate("tie").tier, ExportTier::Native);
        assert!(
            fate("lost")
                .reason
                .contains("no geometry (slot unresolved)"),
            "{:?}",
            fate("lost").reason
        );
        assert!(!slide.contains("never lands"), "{slide}");
    }

    #[test]
    fn a_table_exports_as_a_native_a_tbl() {
        let b = board(
            r#"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[960,540]},
                "pages":[{"id":"p","objects":[
                  {"id":"bench-table","type":"table","at":[80,80],"size":[480,160],
                   "header":true,"columns":[2,1,1],"alt":"benchmark table",
                   "rows":[["Fixture","Before","After"],
                           ["large.json","812",{"runs":[{"t":"244","b":true}]}]]}]}]}"#,
        );
        let (bytes, report) = write(&b);
        let slide = read_part(&bytes, "ppt/slides/slide1.xml");
        assert_well_formed(&slide);

        // A native table in a graphicFrame, styled per cell.
        assert!(slide.contains("<p:graphicFrame>"), "{slide}");
        assert!(
            slide.contains(r#"uri="http://schemas.openxmlformats.org/drawingml/2006/table""#),
            "{slide}"
        );
        assert!(
            slide.contains(&format!(
                r#"<a:tblPr firstRow="1"><a:tableStyleId>{TABLE_STYLE_ID}</a:tableStyleId>"#
            )),
            "{slide}"
        );
        // 2:1:1 weights over 480 pt → 240/120/120 pt columns, in EMU, summing
        // exactly to the frame ext.
        assert_eq!(slide.matches("<a:gridCol").count(), 3, "{slide}");
        assert!(slide.contains(r#"<a:gridCol w="3048000"/>"#), "{slide}");
        assert!(slide.contains(r#"<a:gridCol w="1524000"/>"#), "{slide}");
        // Two rows splitting 160 pt equally.
        assert_eq!(slide.matches("<a:tr ").count(), 2, "{slide}");
        assert!(slide.contains(r#"<a:tr h="1016000">"#), "{slide}");
        // Six cells, each a real a:txBody through the shared run writer.
        assert_eq!(slide.matches("<a:tc>").count(), 6, "{slide}");
        assert!(slide.contains("<a:t>Fixture</a:t>"), "{slide}");
        assert!(slide.contains("<a:t>244</a:t>"), "{slide}");
        // Fixed 6 pt cell margins and hairline borders on every cell.
        assert!(slide.contains(r#"marL="76200""#), "{slide}");
        assert_eq!(slide.matches("<a:lnL ").count(), 6, "{slide}");
        // Alt text survives as descr.
        assert!(slide.contains(r#"descr="benchmark table""#), "{slide}");

        // The minimal tableStyles part, its content type and its rel.
        let styles = read_part(&bytes, "ppt/tableStyles.xml");
        assert!(
            styles.contains(&format!(
                r#"<a:tblStyleLst xmlns:a="{NS_A}" def="{TABLE_STYLE_ID}"/>"#
            )),
            "{styles}"
        );
        let ct = read_part(&bytes, "[Content_Types].xml");
        assert!(ct.contains("tableStyles+xml"), "{ct}");
        let pres_rels = read_part(&bytes, "ppt/_rels/presentation.xml.rels");
        assert!(pres_rels.contains("tableStyles.xml"), "{pres_rels}");

        // Fate: the highest tier, with the reason naming a:tbl.
        let fate = report
            .objects
            .iter()
            .find(|f| f.id == "bench-table")
            .unwrap();
        assert_eq!(fate.tier, ExportTier::Native);
        assert!(fate.reason.contains("a:tbl"), "{}", fate.reason);
    }

    #[test]
    fn a_deck_without_tables_carries_no_tablestyles_part() {
        let b = board(DECK);
        let (bytes, _) = write(&b);
        let mut ar = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        assert!(
            ar.by_name("ppt/tableStyles.xml").is_err(),
            "tableStyles must exist only when a table references it"
        );
    }

    #[test]
    fn an_svg_image_lands_as_png_plus_svgblip() {
        let dir =
            std::env::temp_dir().join(format!("chimaera-board-pptx-svg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let svg_path = dir.join("panel.svg");
        std::fs::write(
            &svg_path,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10">
                <script>alert(1)</script>
                <rect width="20" height="10" fill="#cc0000"/></svg>"##,
        )
        .unwrap();
        let b = board(&format!(
            r#"{{"format":"chimaera.board","formatVersion":1,
                "canvas":{{"size":[400,300]}},
                "pages":[{{"id":"p","objects":[
                  {{"id":"panel","type":"image","src":{src:?},
                   "at":[8,8],"size":[160,80]}}]}}]}}"#,
            src = svg_path.to_str().unwrap()
        ));
        let (bytes, report) = write(&b);

        // Both bodies land under media/: the 2× PNG fallback and the
        // sanitized SVG itself.
        let mut ar = zip::ZipArchive::new(std::io::Cursor::new(bytes.clone())).unwrap();
        let names: Vec<String> = (0..ar.len())
            .map(|i| ar.by_index(i).unwrap().name().to_string())
            .collect();
        let media_png = names
            .iter()
            .find(|n| n.starts_with("ppt/media/") && n.ends_with(".png"))
            .expect("a PNG fallback in media/");
        assert!(
            names
                .iter()
                .any(|n| n.starts_with("ppt/media/") && n.ends_with(".svg")),
            "{names:?}"
        );

        // The PNG fallback is 2× the placed 160×80 pt box.
        let mut png_bytes = Vec::new();
        std::io::Read::read_to_end(&mut ar.by_name(media_png).unwrap(), &mut png_bytes).unwrap();
        assert_eq!(png_dimensions(&png_bytes), Some((320, 160)));

        // The embedded SVG is the sanitized round-trip, never the raw file.
        let media_svg = names
            .iter()
            .find(|n| n.starts_with("ppt/media/") && n.ends_with(".svg"))
            .unwrap();
        let svg_body = read_part(&bytes, media_svg);
        assert!(!svg_body.contains("<script"), "{svg_body}");

        // The slide carries the svgBlip extension next to the blip.
        let slide = read_part(&bytes, "ppt/slides/slide1.xml");
        assert!(
            slide.contains(r#"uri="{96DAC541-7B7A-43D3-8B79-37D633B846F1}""#),
            "{slide}"
        );
        assert!(slide.contains("<asvg:svgBlip"), "{slide}");
        assert_well_formed(&slide);

        // Content types declare the svg extension.
        let ct = read_part(&bytes, "[Content_Types].xml");
        assert!(
            ct.contains(r#"Extension="svg" ContentType="image/svg+xml""#),
            "{ct}"
        );

        // The fate says exactly what happened.
        let fate = report.objects.iter().find(|f| f.id == "panel").unwrap();
        assert_eq!(fate.tier, ExportTier::Vector);
        assert_eq!(fate.reason, "svg embedded with PNG fallback (svgBlip)");
    }

    #[cfg(feature = "math")]
    #[test]
    fn an_equation_exports_as_a_picture_with_svgblip_and_the_latex_as_alt() {
        let b = board(
            r#"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[400,300]},
                "pages":[{"id":"p","objects":[
                  {"id":"quad","type":"equation","at":[40,40],"size":[320,160],
                   "tex":"\\frac{-b \\pm \\sqrt{b^2-4ac}}{2a}",
                   "alt":"\\frac{-b \\pm \\sqrt{b^2-4ac}}{2a}"}]}]}"#,
        );
        let (bytes, report) = write(&b);

        // Both bodies land under media/: the 2× PNG and the outline SVG.
        let mut ar = zip::ZipArchive::new(std::io::Cursor::new(bytes.clone())).unwrap();
        let names: Vec<String> = (0..ar.len())
            .map(|i| ar.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(
            names
                .iter()
                .any(|n| n.starts_with("ppt/media/") && n.ends_with(".png")),
            "{names:?}"
        );
        let media_svg = names
            .iter()
            .find(|n| n.starts_with("ppt/media/") && n.ends_with(".svg"))
            .expect("the equation SVG in media/");

        // The embedded SVG carries real glyph outlines, inked with the theme.
        let svg_body = read_part(&bytes, media_svg);
        assert!(svg_body.contains("<path"), "{svg_body}");
        assert!(svg_body.contains("<use"), "{svg_body}");
        assert!(!svg_body.contains("currentColor"), "{svg_body}");

        // The slide: one p:pic with the svgBlip extension beside the PNG
        // blip, and the LaTeX riding as the picture's alt text.
        let slide = read_part(&bytes, "ppt/slides/slide1.xml");
        assert!(slide.contains("<asvg:svgBlip"), "{slide}");
        assert!(
            slide.contains(r#"descr="\frac{-b \pm \sqrt{b^2-4ac}}{2a}""#),
            "{slide}"
        );
        assert_well_formed(&slide);

        // Fate: raster (the plan pins equation v1 to picture), reason
        // naming the future OMML arm.
        let fate = report.objects.iter().find(|f| f.id == "quad").unwrap();
        assert_eq!(fate.tier, ExportTier::Raster);
        assert!(fate.reason.contains("OMML"), "{}", fate.reason);
        assert!(fate.reason.contains("svgBlip"), "{}", fate.reason);
    }

    #[cfg(not(feature = "math"))]
    #[test]
    fn without_the_math_feature_an_equation_exports_a_placeholder_with_the_reason() {
        let b = board(
            r#"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[400,300]},
                "pages":[{"id":"p","objects":[
                  {"id":"quad","type":"equation","at":[40,40],"size":[320,160],
                   "tex":"E = mc^2","alt":"E = mc^2"}]}]}"#,
        );
        let (bytes, report) = write(&b);
        let slide = read_part(&bytes, "ppt/slides/slide1.xml");
        assert_well_formed(&slide);
        let fate = report.objects.iter().find(|f| f.id == "quad").unwrap();
        assert_eq!(fate.tier, ExportTier::Raster);
        assert!(fate.reason.contains("math feature"), "{}", fate.reason);
    }

    /// Fidelity oracle against python-pptx, deliberately not required by CI
    /// in this crate: install `python3` + `pip install python-pptx`, then
    /// `cargo test -p chimaera-board -- --ignored python_pptx_oracle`.
    #[test]
    #[ignore = "needs python3 + python-pptx"]
    fn python_pptx_oracle() {
        let b = board(DECK);
        let (bytes, _) = write(&b);
        let path = std::env::temp_dir().join("chimaera-board-pptx-oracle.pptx");
        std::fs::write(&path, &bytes).unwrap();
        let script = r#"
import sys
from pptx import Presentation
from pptx.util import Emu
p = Presentation(sys.argv[1])
assert len(p.slides) == 2, f"slides: {len(p.slides)}"
assert p.slide_width == 12192000 and p.slide_height == 6858000
texts = []
for slide in p.slides:
    for shape in slide.shapes:
        if shape.has_text_frame:
            texts.append(shape.text_frame.text)
joined = "\n".join(texts)
assert "The parser rewrite is 3× faster" in joined, joined
assert "3.3× median" in joined, joined
assert "large.json" in joined, "chart labels must stay real text: " + joined
notes = p.slides[0].notes_slide.notes_text_frame.text
assert "Open with the claim" in notes, notes
print("oracle ok")
"#;
        let out = std::process::Command::new("python3")
            .arg("-c")
            .arg(script)
            .arg(&path)
            .output()
            .expect("python3 must be on PATH for the oracle test");
        assert!(
            out.status.success(),
            "python-pptx oracle failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
