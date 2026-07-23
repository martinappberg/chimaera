//! Annotation composites — the figures pack's vocabulary.
//!
//! `panelLabel`, `scalebar`, `sigBracket`, `legend`, `colorbar`, `callout`
//! and `inset`: each sits *above* an already-placed panel, which is exactly
//! what no upstream plotting library can do for itself. They follow the
//! `diagram` pattern precisely — the file stores intent, `expand()` computes
//! primitives at render, the expansion is never written back — so retheme and
//! resize stay free and a composite works identically over an imported panel
//! and over a native chart.
//!
//! Every expansion is pure and deterministic: same object, theme and fonts →
//! byte-identical children, with ids `<composite-id>/<part>`. Problems come
//! back as strings the renderer turns into warnings; a composite with a bad
//! target still draws what it can.

use std::collections::BTreeMap;

use crate::layout::FontStack;
use crate::schema::{
    Align, CalloutObject, ColorbarObject, ConnectorObject, EndPoint, Extra, Frame, ImageObject,
    InsetObject, LegendMarker, LegendObject, Object, PanelLabelObject, PanelLabelStyle, Paragraph,
    RichParagraph, Run, ScalebarObject, ShapeObject, Side, SigBracketObject, Stroke, TextObject,
};
use crate::theme::Theme;

/// The box a `panelLabel` occupies for anchor resolution before its letter is
/// measured. Only the top-left corner matters for the typical
/// `inside-top-left` binding; the extent is a placeholder, not typography.
pub const PANEL_LABEL_NOMINAL: [f64; 2] = [24.0, 24.0];

/// Half-height of a scalebar's end ticks, in points.
const SCALEBAR_TICK: f64 = 3.0;

/// Rect slices a colorbar samples its colormap into. Enough that adjacent
/// steps are sub-pixel at slide scale; few enough that the expansion stays a
/// readable group at the export destination.
const COLORBAR_SLICES: usize = 64;

// ---------------------------------------------------------------------------
// Shared construction
// ---------------------------------------------------------------------------

/// A shape with everything defaulted; callers state only what they mean.
fn base_shape(id: String, geo: &str) -> ShapeObject {
    ShapeObject {
        id,
        kind: Default::default(),
        geo: geo.to_string(),
        d: None,
        slot: None,
        at: None,
        size: None,
        fill: None,
        fill_opacity: None,
        stroke: None,
        radius: None,
        text: Vec::new(),
        role: None,
        align: None,
        anchor: None,
        alt: None,
        link: None,
        rotation: None,
        flip_h: false,
        flip_v: false,
        extra: Extra::new(),
    }
}

fn base_text(id: String, role: &str) -> TextObject {
    TextObject {
        id,
        kind: Default::default(),
        role: Some(role.to_string()),
        slot: None,
        at: None,
        size: None,
        text: Vec::new(),
        align: None,
        valign: None,
        anchor: None,
        alt: None,
        link: None,
        rotation: None,
        extra: Extra::new(),
    }
}

fn stroke(color: &str, width: f64) -> Stroke {
    Stroke {
        color: Some(color.to_string()),
        width: Some(width),
        dash: None,
        opacity: None,
        cap: None,
        join: None,
        extra: Extra::new(),
    }
}

// ---------------------------------------------------------------------------
// panelLabel
// ---------------------------------------------------------------------------

impl PanelLabelObject {
    /// One bold text object at the label role's size × 1.2, cased by style.
    pub fn expand(&self, theme: &Theme, fonts: &FontStack) -> (Vec<Object>, Vec<String>) {
        let mut problems = Vec::new();
        let Some(at) = self.at else {
            problems.push("panelLabel has no position; give it `at` or an `anchor`".to_string());
            return (Vec::new(), problems);
        };
        let role = theme.role("label").unwrap_or_else(|| theme.body());
        let pt = role.size * 1.2;
        let cased = match self.style.unwrap_or_default() {
            PanelLabelStyle::Nature => self.label.to_lowercase(),
            PanelLabelStyle::Capital => self.label.to_uppercase(),
        };
        let w = fonts.measure(&cased, &role.family, pt, 700).max(pt * 0.6) + 2.0;
        let h = pt * role.line_height;
        let mut t = base_text(format!("{}/label", self.id), "label");
        t.at = Some(at);
        t.size = Some(self.size.unwrap_or([w, h]));
        t.text = vec![Paragraph::Rich(RichParagraph {
            runs: vec![Run {
                t: cased,
                b: Some(true),
                size: Some(pt),
                ..Run::plain(String::new())
            }],
            align: None,
            space_before: None,
            space_after: None,
            bullet: None,
            extra: Extra::new(),
        })];
        (vec![Object::Text(t)], problems)
    }
}

// ---------------------------------------------------------------------------
// scalebar
// ---------------------------------------------------------------------------

impl ScalebarObject {
    /// A bar line, two end ticks as one path, and the centered caption below.
    pub fn expand(&self, theme: &Theme, fonts: &FontStack) -> (Vec<Object>, Vec<String>) {
        let mut problems = Vec::new();
        let Some([x, y]) = self.at else {
            problems.push("scalebar has no `at`".to_string());
            return (Vec::new(), problems);
        };
        if !(self.length_pt.is_finite() && self.length_pt > 0.0) {
            problems.push(format!(
                "scalebar lengthPt {} is not a positive length",
                self.length_pt
            ));
            return (Vec::new(), problems);
        }
        let len = self.length_pt;
        let width = self.stroke.as_ref().and_then(|s| s.width).unwrap_or(2.0);
        let color = self
            .stroke
            .as_ref()
            .and_then(|s| s.color.clone())
            .unwrap_or_else(|| "@fg".to_string());
        let tick = SCALEBAR_TICK;

        let mut children = Vec::new();
        let mut bar = base_shape(format!("{}/bar", self.id), "line");
        bar.at = Some([x, y - tick]);
        bar.size = Some([len, tick * 2.0]);
        bar.stroke = Some(stroke(&color, width));
        children.push(Object::Shape(bar));

        let (x2, y0, y1) = (x + len, y - tick, y + tick);
        let mut caps = base_shape(format!("{}/caps", self.id), "path");
        caps.at = Some([x, y0]);
        caps.size = Some([len, tick * 2.0]);
        caps.d = Some(format!("M {x} {y0} L {x} {y1} M {x2} {y0} L {x2} {y1}"));
        caps.stroke = Some(stroke(&color, width));
        children.push(Object::Shape(caps));

        if let Some(label) = &self.label {
            let role = theme.role("label").unwrap_or_else(|| theme.body());
            let lh = role.size * role.line_height;
            let lw = fonts
                .measure(label, &role.family, role.size, role.weight)
                .max(len);
            let mut t = base_text(format!("{}/label", self.id), "label");
            t.at = Some([x + len / 2.0 - lw / 2.0, y + tick + 2.0]);
            t.size = Some([lw, lh]);
            t.align = Some(Align::Center);
            t.text = vec![Paragraph::Plain(label.clone())];
            children.push(Object::Text(t));
        }
        (children, problems)
    }
}

// ---------------------------------------------------------------------------
// sigBracket
// ---------------------------------------------------------------------------

impl SigBracketObject {
    /// An up-over-up bracket path spanning the two targets' top edges, plus
    /// the centered label above the crossbar. Target frames come from the
    /// caller's resolved index — the same map connectors bind through.
    pub fn expand(
        &self,
        theme: &Theme,
        fonts: &FontStack,
        frames: &BTreeMap<String, Frame>,
    ) -> (Vec<Object>, Vec<String>) {
        let mut problems = Vec::new();
        let mut resolve = |ep: &EndPoint, name: &str| -> Option<(f64, f64)> {
            let Some(id) = ep.object.as_deref() else {
                problems.push(format!("sigBracket {name} has no `object`"));
                return None;
            };
            let Some(f) = frames.get(id) else {
                problems.push(format!(
                    "sigBracket {name} names {id:?}, which has no resolved frame on this page"
                ));
                return None;
            };
            let x = match ep.side {
                Some(Side::Left) => f.x,
                Some(Side::Right) => f.right(),
                _ => f.cx(),
            };
            Some((x, f.y))
        };
        let from = resolve(&self.from, "from");
        let to = resolve(&self.to, "to");
        let (Some((x1, y1)), Some((x2, y2))) = (from, to) else {
            return (Vec::new(), problems);
        };

        let drop = self.drop_pt.unwrap_or(12.0).max(0.0);
        let bar_y = y1.min(y2) - drop;
        let (lx, rx) = if x1 <= x2 { (x1, x2) } else { (x2, x1) };

        let mut children = Vec::new();
        let mut bracket = base_shape(format!("{}/bracket", self.id), "path");
        bracket.at = Some([lx, bar_y]);
        bracket.size = Some([(rx - lx).max(1.0), (y1.max(y2) - bar_y).max(1.0)]);
        bracket.d = Some(format!(
            "M {x1} {y1} L {x1} {bar_y} L {x2} {bar_y} L {x2} {y2}"
        ));
        bracket.stroke = Some(stroke("@fg", 1.0));
        children.push(Object::Shape(bracket));

        if let Some(label) = &self.label {
            let role = theme.role("label").unwrap_or_else(|| theme.body());
            let lh = role.size * role.line_height;
            let lw = fonts
                .measure(label, &role.family, role.size, role.weight)
                .max(8.0)
                + 2.0;
            let mut t = base_text(format!("{}/label", self.id), "label");
            t.at = Some([(lx + rx) / 2.0 - lw / 2.0, bar_y - lh - 2.0]);
            t.size = Some([lw, lh]);
            t.align = Some(Align::Center);
            t.text = vec![Paragraph::Plain(label.clone())];
            children.push(Object::Text(t));
        }
        (children, problems)
    }
}

// ---------------------------------------------------------------------------
// legend
// ---------------------------------------------------------------------------

impl LegendObject {
    /// Marker + label rows, filled across `columns` in entry order. Colors
    /// default to the theme's categorical ramp — what the chart itself would
    /// have used for the same series order.
    pub fn expand(&self, theme: &Theme, fonts: &FontStack) -> (Vec<Object>, Vec<String>) {
        let mut problems = Vec::new();
        let Some([x, y]) = self.at else {
            problems.push("legend has no `at`".to_string());
            return (Vec::new(), problems);
        };
        if self.entries.is_empty() {
            problems.push("legend has no entries".to_string());
            return (Vec::new(), problems);
        }
        let cols = self.columns.unwrap_or(1).max(1) as usize;
        let role = theme.role("label").unwrap_or_else(|| theme.body());
        let row_h = (role.size * role.line_height).max(12.0);
        let row_gap = 4.0;
        let marker_w = 14.0;
        let gap = 6.0;
        let col_gap = 16.0;
        let widest = self
            .entries
            .iter()
            .map(|e| fonts.measure(&e.label, &role.family, role.size, role.weight))
            .fold(0.0, f64::max);
        let col_w = self
            .size
            .map(|s| ((s[0] - col_gap * (cols as f64 - 1.0)) / cols as f64).max(marker_w + gap))
            .unwrap_or(marker_w + gap + widest + 4.0);

        let mut children = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            let (col, row) = (i % cols, i / cols);
            let ex = x + col as f64 * (col_w + col_gap);
            let ey = y + row as f64 * (row_h + row_gap);
            let color = e
                .color
                .clone()
                .unwrap_or_else(|| theme.categorical(i).hex());

            let mid = format!("{}/entry[{i}].marker", self.id);
            let marker = match e.marker.unwrap_or_default() {
                LegendMarker::Swatch => {
                    let mut m = base_shape(mid, "rect");
                    m.at = Some([ex + 2.0, ey + (row_h - 10.0) / 2.0]);
                    m.size = Some([10.0, 10.0]);
                    m.fill = Some(color);
                    m
                }
                LegendMarker::Line => {
                    let mut m = base_shape(mid, "line");
                    m.at = Some([ex, ey]);
                    m.size = Some([marker_w, row_h]);
                    m.stroke = Some(stroke(&color, 2.0));
                    m
                }
                LegendMarker::Point => {
                    let mut m = base_shape(mid, "ellipse");
                    m.at = Some([ex + (marker_w - 7.0) / 2.0, ey + (row_h - 7.0) / 2.0]);
                    m.size = Some([7.0, 7.0]);
                    m.fill = Some(color);
                    m
                }
            };
            children.push(Object::Shape(marker));

            let mut t = base_text(format!("{}/entry[{i}].label", self.id), "label");
            t.at = Some([ex + marker_w + gap, ey]);
            t.size = Some([(col_w - marker_w - gap).max(1.0), row_h]);
            t.text = vec![Paragraph::Plain(e.label.clone())];
            children.push(Object::Text(t));
        }
        (children, problems)
    }
}

// ---------------------------------------------------------------------------
// colorbar
// ---------------------------------------------------------------------------

impl ColorbarObject {
    /// The strip as [`COLORBAR_SLICES`] literal-color rects, low→high, plus
    /// end tick labels through [`crate::chart::format_tick`] and an optional
    /// title. Vertical when the box is taller than wide; low sits at the
    /// bottom (vertical) or left (horizontal), matching every axis Board
    /// draws.
    pub fn expand(&self, theme: &Theme, _fonts: &FontStack) -> (Vec<Object>, Vec<String>) {
        let mut problems = Vec::new();
        let (Some([x, y]), Some([w, h])) = (self.at, self.size) else {
            problems.push("colorbar has no at/size; nothing to expand".to_string());
            return (Vec::new(), problems);
        };
        if crate::colormap::sample(&self.colormap, 0.0).is_none() {
            problems.push(format!(
                "colormap {:?} is not bundled; bundled maps are {}",
                self.colormap,
                crate::colormap::NAMES.join(", ")
            ));
            return (Vec::new(), problems);
        }
        let [lo, hi] = self.domain;
        if !(lo.is_finite() && hi.is_finite()) || lo == hi {
            problems.push(format!("colorbar domain [{lo}, {hi}] is degenerate"));
            return (Vec::new(), problems);
        }

        let vertical = h >= w;
        let n = COLORBAR_SLICES;
        let mut children = Vec::new();
        for i in 0..n {
            let t = (i as f64 + 0.5) / n as f64;
            // Checked above; every t samples the same table.
            let rgb = crate::colormap::sample(&self.colormap, t).expect("colormap checked");
            let mut r = base_shape(format!("{}/slice[{i}]", self.id), "rect");
            if vertical {
                let sh = h / n as f64;
                r.at = Some([x, y + h - (i as f64 + 1.0) * sh]);
                r.size = Some([w, sh]);
            } else {
                let sw = w / n as f64;
                r.at = Some([x + i as f64 * sw, y]);
                r.size = Some([sw, h]);
            }
            r.fill = Some(rgb.hex());
            children.push(Object::Shape(r));
        }

        let role = theme.role("label").unwrap_or_else(|| theme.body());
        let th = role.size * role.line_height;
        let step = (hi - lo).abs();
        let lo_text = crate::chart::format_tick(lo, step, None);
        let hi_text = crate::chart::format_tick(hi, step, None);
        let tick = |part: &str, at: [f64; 2], size: [f64; 2], text: String| -> Object {
            let mut t = base_text(format!("{}/{part}", self.id), "label");
            t.at = Some(at);
            t.size = Some(size);
            t.align = Some(Align::Center);
            t.text = vec![Paragraph::Plain(text)];
            Object::Text(t)
        };
        if vertical {
            let bw = w.max(48.0);
            let bx = x + w / 2.0 - bw / 2.0;
            children.push(tick("tick.hi", [bx, y - th - 2.0], [bw, th], hi_text));
            children.push(tick("tick.lo", [bx, y + h + 2.0], [bw, th], lo_text));
            if let Some(title) = &self.title {
                children.push(tick(
                    "title",
                    [bx, y - 2.0 * th - 6.0],
                    [bw, th],
                    title.clone(),
                ));
            }
        } else {
            let bw = 48.0;
            children.push(tick(
                "tick.lo",
                [x - bw / 2.0, y + h + 2.0],
                [bw, th],
                lo_text,
            ));
            children.push(tick(
                "tick.hi",
                [x + w - bw / 2.0, y + h + 2.0],
                [bw, th],
                hi_text,
            ));
            if let Some(title) = &self.title {
                children.push(tick("title", [x, y - th - 4.0], [w, th], title.clone()));
            }
        }
        (children, problems)
    }
}

// ---------------------------------------------------------------------------
// callout
// ---------------------------------------------------------------------------

impl CalloutObject {
    /// A `@surface` roundRect with `@accent1` stroke and bound text, plus the
    /// tail: a connector from the box to the target, arrow at the target.
    pub fn expand(&self, _theme: &Theme, _fonts: &FontStack) -> (Vec<Object>, Vec<String>) {
        let mut problems = Vec::new();
        let (Some(at), Some(size)) = (self.at, self.size) else {
            problems.push("callout has no at/size; nothing to expand".to_string());
            return (Vec::new(), problems);
        };
        let box_id = format!("{}/box", self.id);
        let mut bx = base_shape(box_id.clone(), "roundRect");
        bx.at = Some(at);
        bx.size = Some(size);
        bx.fill = Some("@surface".to_string());
        bx.stroke = Some(stroke("@accent1", 1.5));
        bx.text = self.text.clone();
        let mut children = vec![Object::Shape(bx)];

        if let Some(tail) = &self.tail {
            if tail.object.is_none() {
                problems.push("callout tail has no `object`".to_string());
            } else {
                children.push(Object::Connector(ConnectorObject {
                    id: format!("{}/tail", self.id),
                    kind: Default::default(),
                    geo: Some("straight".to_string()),
                    from: EndPoint {
                        object: Some(box_id),
                        side: None,
                        at: None,
                        extra: Extra::new(),
                    },
                    to: EndPoint {
                        object: tail.object.clone(),
                        side: tail.side,
                        at: None,
                        extra: Extra::new(),
                    },
                    stroke: Some(stroke("@accent1", 1.5)),
                    head_end: None,
                    tail_end: Some("arrow".to_string()),
                    text: Vec::new(),
                    label_at: None,
                    role: None,
                    alt: None,
                    extra: Extra::new(),
                }));
            }
        }
        (children, problems)
    }
}

// ---------------------------------------------------------------------------
// inset
// ---------------------------------------------------------------------------

impl InsetObject {
    /// The same `src` re-placed with a computed `srcRect`, a thin `@fg`
    /// border at the inset, and — when the target's frame and `pixelSize` are
    /// both known — a dashed rect over the source region on the target.
    /// The caller supplies the target image and its resolved page frame; the
    /// expansion itself never walks a page.
    pub fn expand(
        &self,
        _theme: &Theme,
        _fonts: &FontStack,
        target: Option<&ImageObject>,
        target_frame: Option<Frame>,
    ) -> (Vec<Object>, Vec<String>) {
        let mut problems = Vec::new();
        let (Some(at), Some(size)) = (self.at, self.size) else {
            problems.push("inset has no at/size; nothing to expand".to_string());
            return (Vec::new(), problems);
        };
        let Some(img) = target else {
            problems.push(format!(
                "inset target {:?} is not an image on this page",
                self.of.object
            ));
            return (Vec::new(), problems);
        };

        let [px, py, pw, ph] = self.of.px;
        let mut view = ImageObject {
            id: format!("{}/view", self.id),
            kind: Default::default(),
            src: img.src.clone(),
            slot: None,
            at: Some(at),
            size: Some(size),
            src_rect: None,
            provenance: None,
            pixel_size: img.pixel_size,
            tint: None,
            anchor: None,
            alt: None,
            link: None,
            rotation: None,
            extra: Extra::new(),
        };
        let mut mark = None;
        match img.pixel_size {
            Some([nw, nh]) if nw > 0.0 && nh > 0.0 => {
                let (l, t) = (px / nw, py / nh);
                let (r, b) = (1.0 - (px + pw) / nw, 1.0 - (py + ph) / nh);
                view.src_rect = Some([l, t, r, b]);
                if l < 0.0 || t < 0.0 || r < 0.0 || b < 0.0 {
                    problems.push(format!(
                        "inset px region [{px}, {py}, {pw}, {ph}] leaves the target's \
                         {nw}×{nh} px image"
                    ));
                }
                if let Some(tf) = target_frame {
                    if img.src_rect.is_some() {
                        problems
                            .push("inset source mark ignores the target's own srcRect".to_string());
                    }
                    let mut m = base_shape(format!("{}/mark", self.id), "rect");
                    m.at = Some([tf.x + l * tf.w, tf.y + t * tf.h]);
                    m.size = Some([pw / nw * tf.w, ph / nh * tf.h]);
                    m.stroke = Some(Stroke {
                        dash: Some(vec![3.0, 2.0]),
                        ..stroke("@fg", 1.0)
                    });
                    mark = Some(Object::Shape(m));
                }
            }
            _ => problems.push("inset needs the target's pixelSize".to_string()),
        }

        let mut children = vec![Object::Image(view)];
        let mut border = base_shape(format!("{}/border", self.id), "rect");
        border.at = Some(at);
        border.size = Some(size);
        border.stroke = Some(stroke("@fg", 1.0));
        children.push(Object::Shape(border));
        children.extend(mark);
        (children, problems)
    }
}

// ---------------------------------------------------------------------------
// Child frames — where every derived child landed
// ---------------------------------------------------------------------------

/// The laid-out frames of every composite's derived children on one page:
/// composite id → `(derived child id, frame)` pairs in expansion order
/// (which is z-order, so a hit-test walks the list backwards).
///
/// This is the pane's hit-test map for making composite children first-class
/// — selectable, draggable, addressable by their derived ids. It mirrors
/// `render.rs`'s composite dispatch exactly: the same slot/anchor frame
/// resolution, the same "hand a slot-placed composite the frame the
/// resolution decided" clone — so these rects agree with the pixels by
/// construction. Children without full geometry (connectors) are absent,
/// like everywhere else a frame map is built; expansion problems are the
/// renderer's to report, not repeated here. Pure and deterministic: same
/// board, theme and fonts → identical frames.
pub fn page_child_frames(
    board: &crate::schema::Board,
    page: &crate::schema::Page,
    theme: &Theme,
    fonts: &FontStack,
) -> BTreeMap<String, Vec<(String, Frame)>> {
    let index = crate::slots::resolve_page_frames(board, page, theme, Some(fonts));

    // The render dispatch's "placed" clone: a slot- or anchor-placed
    // composite carries no at/size of its own, so it inherits the frame the
    // resolution decided. `None` = nothing to place against, skip.
    fn placed<T: Clone>(
        o: &T,
        has_geometry: bool,
        frame: Option<Frame>,
        set: impl FnOnce(&mut T, Frame),
    ) -> Option<T> {
        if has_geometry {
            return Some(o.clone());
        }
        let f = frame?;
        let mut c = o.clone();
        set(&mut c, f);
        Some(c)
    }

    let mut out = BTreeMap::new();
    for obj in &page.objects {
        let frame = index.get(obj.id()).copied().or_else(|| obj.frame());
        let children: Vec<Object> = match obj {
            Object::Diagram(d) => {
                let Some(d) = placed(d, d.at.is_some() && d.size.is_some(), frame, |c, f| {
                    c.at = Some([f.x, f.y]);
                    c.size = Some([f.w, f.h]);
                }) else {
                    continue;
                };
                crate::diagram::expand(&d, theme, fonts).0
            }
            Object::PanelLabel(o) => {
                let Some(o) = placed(o, o.at.is_some(), frame, |c, f| {
                    c.at = Some([f.x, f.y]);
                }) else {
                    continue;
                };
                o.expand(theme, fonts).0
            }
            Object::Scalebar(o) => o.expand(theme, fonts).0,
            Object::SigBracket(o) => o.expand(theme, fonts, &index).0,
            Object::Legend(o) => o.expand(theme, fonts).0,
            Object::Colorbar(o) => {
                let Some(o) = placed(o, o.at.is_some() && o.size.is_some(), frame, |c, f| {
                    c.at = Some([f.x, f.y]);
                    c.size = Some([f.w, f.h]);
                }) else {
                    continue;
                };
                o.expand(theme, fonts).0
            }
            Object::Callout(o) => {
                let Some(o) = placed(o, o.at.is_some() && o.size.is_some(), frame, |c, f| {
                    c.at = Some([f.x, f.y]);
                    c.size = Some([f.w, f.h]);
                }) else {
                    continue;
                };
                o.expand(theme, fonts).0
            }
            Object::Inset(o) => {
                let target = page.walk().find_map(|t| match t {
                    Object::Image(i) if i.id == o.of.object => Some(i),
                    _ => None,
                });
                let target_frame = index.get(o.of.object.as_str()).copied();
                o.expand(theme, fonts, target, target_frame).0
            }
            _ => continue,
        };
        let frames: Vec<(String, Frame)> = children
            .iter()
            .filter_map(|c| c.frame().map(|f| (c.id().to_string(), f)))
            .collect();
        if !frames.is_empty() {
            out.insert(obj.id().to_string(), frames);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::Severity;
    use crate::schema::Board;

    fn theme() -> Theme {
        crate::theme::default_for(true)
    }

    fn fonts() -> FontStack {
        FontStack::new(&[])
    }

    fn obj(json: &str) -> Object {
        serde_json::from_str(json).unwrap()
    }

    fn kinds(children: &[Object]) -> Vec<&str> {
        children.iter().map(|c| c.kind()).collect()
    }

    #[test]
    fn page_child_frames_agrees_with_the_expansion() {
        let board: Board = serde_json::from_str(
            r#"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[960,540]},
                "pages":[{"id":"p1","objects":[
                  {"id":"note","type":"text","at":[48,24],"size":[300,32],"text":["hi"]},
                  {"id":"flow","type":"diagram","at":[48,80],"size":[500,400],
                   "nodes":[{"id":"a","label":"Start"},{"id":"b","label":"End"}],
                   "edges":[{"from":"a","to":"b"}]},
                  {"id":"pl","type":"panelLabel","at":[80,80],"label":"a"}]}]}"#,
        )
        .unwrap();
        let (theme, fonts) = (theme(), fonts());
        let map = page_child_frames(&board, &board.pages[0], &theme, &fonts);
        // Composites only: the text object contributes nothing.
        assert_eq!(map.keys().collect::<Vec<_>>(), ["flow", "pl"]);

        // The diagram's entries are exactly its expansion's framed children,
        // in expansion (z) order — routed edge geometry (path + arrowhead)
        // draws under the nodes, so the node shapes come last and win a
        // backwards hit-test walk.
        let Object::Diagram(d) = &board.pages[0].objects[1] else {
            panic!()
        };
        let (children, _) = crate::diagram::expand(d, &theme, &fonts);
        let expect: Vec<(String, Frame)> = children
            .iter()
            .filter_map(|c| c.frame().map(|f| (c.id().to_string(), f)))
            .collect();
        assert_eq!(
            expect.len(),
            4,
            "edge path + arrowhead + two node shapes: {expect:?}"
        );
        let got = &map["flow"];
        assert_eq!(
            got.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            ["flow/edge[0]", "flow/edge[0].arrow", "flow/a", "flow/b"]
        );
        for ((gid, gf), (eid, ef)) in got.iter().zip(&expect) {
            assert_eq!(gid, eid);
            assert_eq!((gf.x, gf.y, gf.w, gf.h), (ef.x, ef.y, ef.w, ef.h));
        }
    }

    #[test]
    fn panel_label_expands_to_one_bold_text() {
        let Object::PanelLabel(pl) =
            obj(r#"{"id":"pl-1","type":"panelLabel","at":[80,80],"style":"nature","label":"B"}"#)
        else {
            panic!("expected panelLabel")
        };
        let (children, problems) = pl.expand(&theme(), &fonts());
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(kinds(&children), ["text"]);
        let Object::Text(t) = &children[0] else {
            panic!()
        };
        assert_eq!(t.id, "pl-1/label");
        let Paragraph::Rich(p) = &t.text[0] else {
            panic!("bold run expected")
        };
        // nature style lowercases; the run is bold at label × 1.2.
        assert_eq!(p.runs[0].t, "b");
        assert_eq!(p.runs[0].b, Some(true));
        let label_pt = theme().role("label").unwrap().size;
        assert_eq!(p.runs[0].size, Some(label_pt * 1.2));

        // capital style uppercases the same stored letter.
        let Object::PanelLabel(pl) =
            obj(r#"{"id":"pl-2","type":"panelLabel","at":[80,80],"style":"capital","label":"b"}"#)
        else {
            panic!()
        };
        let (children, _) = pl.expand(&theme(), &fonts());
        let Object::Text(t) = &children[0] else {
            panic!()
        };
        let Paragraph::Rich(p) = &t.text[0] else {
            panic!()
        };
        assert_eq!(p.runs[0].t, "B");
    }

    #[test]
    fn scalebar_expands_to_bar_caps_and_label() {
        let Object::Scalebar(sb) =
            obj(r#"{"id":"sb","type":"scalebar","at":[100,200],"lengthPt":80,"label":"100 µm"}"#)
        else {
            panic!()
        };
        let (children, problems) = sb.expand(&theme(), &fonts());
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(kinds(&children), ["shape", "shape", "text"]);
        let Object::Shape(bar) = &children[0] else {
            panic!()
        };
        assert_eq!(bar.geo, "line");
        assert_eq!(bar.size, Some([80.0, 6.0]));
        assert_eq!(
            bar.stroke.as_ref().unwrap().width,
            Some(2.0),
            "default 2 pt"
        );
        let Object::Shape(caps) = &children[1] else {
            panic!()
        };
        assert_eq!(caps.geo, "path");
        // Two tiny vertical ticks at the bar's ends.
        assert_eq!(
            caps.d.as_deref(),
            Some("M 100 197 L 100 203 M 180 197 L 180 203")
        );
        let Object::Text(label) = &children[2] else {
            panic!()
        };
        assert_eq!(label.align, Some(Align::Center));
        assert_eq!(label.text[0].plain_text(), "100 µm");
        // The caption centers under the bar.
        let f = label.at.unwrap();
        let w = label.size.unwrap()[0];
        assert!((f[0] + w / 2.0 - 140.0).abs() < 1e-9, "centered on the bar");

        // No label → no text child.
        let Object::Scalebar(bare) =
            obj(r#"{"id":"sb2","type":"scalebar","at":[0,0],"lengthPt":40}"#)
        else {
            panic!()
        };
        let (children, _) = bare.expand(&theme(), &fonts());
        assert_eq!(kinds(&children), ["shape", "shape"]);
    }

    #[test]
    fn sig_bracket_spans_the_targets_top_edges() {
        let Object::SigBracket(sig) = obj(
            r#"{"id":"sig-1","type":"sigBracket","from":{"object":"a"},"to":{"object":"b"},
                "label":"p = 0.03"}"#,
        ) else {
            panic!()
        };
        let mut frames = BTreeMap::new();
        frames.insert(
            "a".to_string(),
            Frame {
                x: 100.0,
                y: 200.0,
                w: 40.0,
                h: 120.0,
            },
        );
        frames.insert(
            "b".to_string(),
            Frame {
                x: 220.0,
                y: 160.0,
                w: 40.0,
                h: 160.0,
            },
        );
        let (children, problems) = sig.expand(&theme(), &fonts(), &frames);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(kinds(&children), ["shape", "text"]);
        let Object::Shape(bracket) = &children[0] else {
            panic!()
        };
        // Legs land on each target's top-edge center; the crossbar rises
        // dropPt (default 12) above the taller target's top edge.
        assert_eq!(
            bracket.d.as_deref(),
            Some("M 120 200 L 120 148 L 240 148 L 240 160")
        );
        let Object::Text(label) = &children[1] else {
            panic!()
        };
        assert_eq!(label.text[0].plain_text(), "p = 0.03");
        assert_eq!(label.align, Some(Align::Center));
        // Centered over the bracket midpoint, above the crossbar.
        let at = label.at.unwrap();
        let size = label.size.unwrap();
        assert!(
            (at[0] + size[0] / 2.0 - 180.0).abs() < 1e-9,
            "label centered"
        );
        assert!(at[1] + size[1] <= 148.0, "label sits above the crossbar");
    }

    #[test]
    fn sig_bracket_reports_a_missing_target() {
        let Object::SigBracket(sig) = obj(
            r#"{"id":"sig","type":"sigBracket","from":{"object":"a"},"to":{"object":"ghost"}}"#,
        ) else {
            panic!()
        };
        let mut frames = BTreeMap::new();
        frames.insert(
            "a".to_string(),
            Frame {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            },
        );
        let (children, problems) = sig.expand(&theme(), &fonts(), &frames);
        assert!(children.is_empty());
        assert!(problems.iter().any(|p| p.contains("ghost")), "{problems:?}");
    }

    #[test]
    fn legend_expands_markers_and_labels() {
        let Object::Legend(lg) = obj(r##"{"id":"lg","type":"legend","at":[600,400],"entries":[
                 {"label":"before","marker":"swatch"},
                 {"label":"after","color":"@cat2","marker":"line"},
                 {"label":"outlier","color":"#ff0000","marker":"point"}]}"##)
        else {
            panic!()
        };
        let (children, problems) = lg.expand(&theme(), &fonts());
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(
            kinds(&children),
            ["shape", "text", "shape", "text", "shape", "text"]
        );
        let geos: Vec<&str> = children
            .iter()
            .filter_map(|c| match c {
                Object::Shape(s) => Some(s.geo.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(geos, ["rect", "line", "ellipse"]);
        // The first entry defaults to the theme's first categorical color.
        let Object::Shape(swatch) = &children[0] else {
            panic!()
        };
        assert_eq!(
            swatch.fill.as_deref(),
            Some(theme().categorical(0).hex().as_str())
        );
        // Stated colors pass through untouched.
        let Object::Shape(line) = &children[2] else {
            panic!()
        };
        assert_eq!(
            line.stroke.as_ref().unwrap().color.as_deref(),
            Some("@cat2")
        );
        // One column: rows stack vertically.
        let ys: Vec<f64> = children
            .iter()
            .filter_map(|c| c.frame().map(|f| f.y))
            .collect();
        assert!(ys[1] < ys[3] && ys[3] < ys[5], "rows stack: {ys:?}");
    }

    #[test]
    fn colorbar_slices_sample_the_colormap_monotonically() {
        let Object::Colorbar(cb) = obj(
            r#"{"id":"cb","type":"colorbar","at":[880,100],"size":[16,240],
                "colormap":"viridis","domain":[0,1],"title":"z-score"}"#,
        ) else {
            panic!()
        };
        let (children, problems) = cb.expand(&theme(), &fonts());
        assert!(problems.is_empty(), "{problems:?}");
        // 64 slices + lo/hi ticks + title.
        assert_eq!(children.len(), 64 + 3);
        let slices: Vec<&ShapeObject> = children
            .iter()
            .filter_map(|c| match c {
                Object::Shape(s) if s.id.contains("/slice[") => Some(s),
                _ => None,
            })
            .collect();
        assert_eq!(slices.len(), 64);
        let mut prev_y = f64::INFINITY;
        for (i, s) in slices.iter().enumerate() {
            let t = (i as f64 + 0.5) / 64.0;
            let expect = crate::colormap::sample("viridis", t).unwrap().hex();
            assert_eq!(s.fill.as_deref(), Some(expect.as_str()), "slice {i}");
            // Vertical bar, low at the bottom: y strictly decreases with i.
            let y = s.at.unwrap()[1];
            assert!(y < prev_y, "slice {i} must sit above slice {}", i - 1);
            prev_y = y;
        }
        // End ticks go through the chart's tick formatter.
        let texts: Vec<String> = children
            .iter()
            .filter_map(|c| match c {
                Object::Text(t) => Some(t.text[0].plain_text()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["1", "0", "z-score"]);
    }

    #[test]
    fn colorbar_refuses_an_unknown_colormap() {
        let Object::Colorbar(cb) = obj(r#"{"id":"cb","type":"colorbar","at":[0,0],"size":[16,100],
                "colormap":"jet","domain":[0,1]}"#)
        else {
            panic!()
        };
        let (children, problems) = cb.expand(&theme(), &fonts());
        assert!(children.is_empty());
        // Refused loudly, naming the bundled maps — never approximated.
        assert!(
            problems
                .iter()
                .any(|p| p.contains("\"jet\"") && p.contains("viridis")),
            "{problems:?}"
        );
    }

    #[test]
    fn callout_expands_to_box_and_tail_connector() {
        let Object::Callout(co) = obj(
            r#"{"id":"note","type":"callout","at":[600,200],"size":[240,96],
                "text":["3.3× median"],"tail":{"object":"chart","side":"right"}}"#,
        ) else {
            panic!()
        };
        let (children, problems) = co.expand(&theme(), &fonts());
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(kinds(&children), ["shape", "connector"]);
        let Object::Shape(bx) = &children[0] else {
            panic!()
        };
        assert_eq!(bx.geo, "roundRect");
        assert_eq!(bx.fill.as_deref(), Some("@surface"));
        assert_eq!(
            bx.stroke.as_ref().unwrap().color.as_deref(),
            Some("@accent1")
        );
        assert_eq!(bx.text[0].plain_text(), "3.3× median");
        let Object::Connector(tail) = &children[1] else {
            panic!()
        };
        assert_eq!(tail.from.object.as_deref(), Some("note/box"));
        assert_eq!(tail.to.object.as_deref(), Some("chart"));
        assert_eq!(tail.to.side, Some(Side::Right));
        assert_eq!(tail.tail_end.as_deref(), Some("arrow"));
    }

    #[test]
    fn inset_src_rect_arithmetic_is_exact() {
        let Object::Inset(inset) = obj(
            r#"{"id":"zoom","type":"inset","at":[640,320],"size":[160,120],
                "of":{"object":"micro","px":[100,50,200,100]}}"#,
        ) else {
            panic!()
        };
        let img = obj(r#"{"id":"micro","type":"image","src":"assets/micro.png",
                "at":[80,80],"size":[400,200],"pixelSize":[800,400]}"#);
        let tf = img.frame();
        let Object::Image(target) = &img else {
            panic!()
        };
        let (children, problems) = inset.expand(&theme(), &fonts(), Some(target), tf);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(kinds(&children), ["image", "shape", "shape"]);
        let Object::Image(view) = &children[0] else {
            panic!()
        };
        assert_eq!(view.src, "assets/micro.png", "the SAME src, re-placed");
        // px [100, 50, 200, 100] against 800×400: l .125, t .125, r .625, b .625.
        assert_eq!(view.src_rect, Some([0.125, 0.125, 0.625, 0.625]));
        let Object::Shape(border) = &children[1] else {
            panic!()
        };
        assert_eq!(border.at, Some([640.0, 320.0]));
        assert_eq!(
            border.stroke.as_ref().unwrap().color.as_deref(),
            Some("@fg")
        );
        // The dashed source-region mark maps px → the target's page frame:
        // x 80 + .125 × 400 = 130, y 80 + .125 × 200 = 105, 100 × 50 pt.
        let Object::Shape(mark) = &children[2] else {
            panic!()
        };
        assert_eq!(mark.at, Some([130.0, 105.0]));
        assert_eq!(mark.size, Some([100.0, 50.0]));
        assert_eq!(mark.stroke.as_ref().unwrap().dash, Some(vec![3.0, 2.0]));
    }

    #[test]
    fn inset_without_pixel_size_reports_the_problem() {
        let Object::Inset(inset) = obj(r#"{"id":"zoom","type":"inset","at":[0,0],"size":[80,80],
                "of":{"object":"micro","px":[0,0,10,10]}}"#)
        else {
            panic!()
        };
        let img =
            obj(r#"{"id":"micro","type":"image","src":"a.png","at":[80,80],"size":[400,200]}"#);
        let tf = img.frame();
        let Object::Image(target) = &img else {
            panic!()
        };
        let (children, problems) = inset.expand(&theme(), &fonts(), Some(target), tf);
        assert!(
            problems
                .iter()
                .any(|p| p == "inset needs the target's pixelSize"),
            "{problems:?}"
        );
        // Still draws what it can: the uncropped view and the border, no mark.
        assert_eq!(kinds(&children), ["image", "shape"]);
        let Object::Image(view) = &children[0] else {
            panic!()
        };
        assert_eq!(view.src_rect, None);
    }

    // --- lint ---------------------------------------------------------------

    fn lint_page(objects: &str) -> Vec<crate::normalize::Diagnostic> {
        let mut b = crate::parse(&format!(
            r#"{{"format":"chimaera.board","formatVersion":1,
                "canvas":{{"size":[960,540]}},
                "pages":[{{"id":"p1","objects":[{objects}]}}]}}"#
        ))
        .unwrap();
        crate::normalize(&mut b);
        crate::lint::lint(&b, &theme())
    }

    #[test]
    fn a_dangling_sig_bracket_target_lints_as_error() {
        let diags = lint_page(
            r#"{"id":"a","type":"shape","geo":"rect","at":[80,240],"size":[40,120]},
               {"id":"sig","type":"sigBracket","from":{"object":"a"},"to":{"object":"ghost"},
                "label":"**"}"#,
        );
        let e = diags
            .iter()
            .find(|d| d.severity == Severity::Error && d.message.contains("ghost"))
            .expect("a dangling-target error");
        assert_eq!(e.object.as_deref(), Some("sig"));
    }

    #[test]
    fn a_small_legend_warns_toward_direct_labels() {
        let diags = lint_page(
            r#"{"id":"lg","type":"legend","at":[600,400],"entries":[
                 {"label":"before"},{"label":"after"}]}"#,
        );
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Warning && d.message.contains("direct labels")),
            "{diags:?}"
        );
    }

    #[test]
    fn dangling_callout_and_inset_targets_lint_as_errors() {
        let diags = lint_page(
            r#"{"id":"note","type":"callout","at":[600,200],"size":[240,96],
                "text":["hi"],"tail":{"object":"nowhere"}},
               {"id":"zoom","type":"inset","at":[0,0],"size":[80,80],
                "of":{"object":"nothing","px":[0,0,10,10]}}"#,
        );
        for id in ["nowhere", "nothing"] {
            assert!(
                diags
                    .iter()
                    .any(|d| d.severity == Severity::Error && d.message.contains(id)),
                "no error names {id}: {diags:?}"
            );
        }
    }

    // --- the full loop -------------------------------------------------------

    /// One page carrying all seven composites over an image panel.
    const ANNOTATED: &str = r#"{
      "format": "chimaera.board", "formatVersion": 1,
      "title": "Annotated figure",
      "canvas": { "size": [960, 540] },
      "pages": [{
        "id": "fig",
        "objects": [
          { "id": "micro", "type": "image", "src": "assets/micro.png",
            "at": [80, 80], "size": [400, 200], "pixelSize": [800, 400] },
          { "id": "bar-a", "type": "shape", "geo": "rect", "at": [560, 240], "size": [40, 120],
            "fill": "@cat1" },
          { "id": "bar-b", "type": "shape", "geo": "rect", "at": [640, 200], "size": [40, 160],
            "fill": "@cat2" },
          { "id": "pl", "type": "panelLabel", "label": "a",
            "anchor": { "object": "micro", "rel": "inside-top-left" } },
          { "id": "sb", "type": "scalebar", "at": [360, 260], "lengthPt": 80,
            "label": "100 µm" },
          { "id": "sig", "type": "sigBracket", "from": { "object": "bar-a" },
            "to": { "object": "bar-b" }, "label": "p = 0.03" },
          { "id": "lg", "type": "legend", "at": [560, 400], "entries": [
            { "label": "before" }, { "label": "after" },
            { "label": "control" }, { "label": "outlier" } ] },
          { "id": "cb", "type": "colorbar", "at": [880, 96], "size": [16, 240],
            "colormap": "viridis", "domain": [0, 1], "title": "z" },
          { "id": "note", "type": "callout", "at": [600, 64], "size": [240, 88],
            "text": ["gap widens 2×"], "tail": { "object": "bar-b", "side": "top" } },
          { "id": "zoom", "type": "inset", "at": [80, 320], "size": [160, 120],
            "of": { "object": "micro", "px": [100, 50, 200, 100] } }
        ]
      }]
    }"#;

    #[test]
    fn a_board_with_all_seven_renders_and_exports() {
        let mut b: Board = crate::parse(ANNOTATED).unwrap();
        crate::normalize(&mut b);
        let theme = theme();
        let fonts = fonts();

        // Renders to a real PNG with no Error diagnostics (image placeholders
        // and expansion notes stay warnings).
        let out = crate::render::render_page(
            &b,
            0,
            &theme,
            &fonts,
            crate::render::RasterParams::default(),
        )
        .unwrap();
        assert_eq!(&out.png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(
            !out.diagnostics
                .iter()
                .any(|d| d.severity == Severity::Error),
            "{:?}",
            out.diagnostics
        );

        // Deterministic: same board, same bytes.
        let again = crate::render::render_page(
            &b,
            0,
            &theme,
            &fonts,
            crate::render::RasterParams::default(),
        )
        .unwrap();
        assert_eq!(out.png, again.png);

        // Exports to a pptx whose report covers every composite as Grouped.
        let mut bytes = Vec::new();
        let report = crate::export::write_pptx(&b, &theme, &fonts, None, &mut bytes).unwrap();
        assert_eq!(&bytes[..2], b"PK");
        for id in ["pl", "sb", "sig", "lg", "cb", "note", "zoom"] {
            let fate = report
                .objects
                .iter()
                .find(|f| f.id == id)
                .unwrap_or_else(|| panic!("no fate for {id}"));
            assert_eq!(
                fate.tier,
                crate::export::ExportTier::Grouped,
                "{id}: {}",
                fate.reason
            );
            assert!(
                fate.reason
                    .contains("annotation composite as grouped shapes"),
                "{id}: {}",
                fate.reason
            );
        }
    }

    #[test]
    fn composites_round_trip_through_the_schema() {
        let b: Board = crate::parse(ANNOTATED).unwrap();
        let json = serde_json::to_string(&b).unwrap();
        let back: Board = crate::parse(&json).unwrap();
        // None survived as Unknown, and every id kept its type string.
        for (obj, kind) in back.pages[0].objects.iter().zip([
            "image",
            "shape",
            "shape",
            "panelLabel",
            "scalebar",
            "sigBracket",
            "legend",
            "colorbar",
            "callout",
            "inset",
        ]) {
            assert_eq!(obj.kind(), kind);
            assert!(!matches!(obj, Object::Unknown(_)), "{} degraded", obj.id());
        }
    }
}
