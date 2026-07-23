//! Slot layouts and page-geometry resolution.
//!
//! Slots are the primary authoring path: an agent writes `"slot": "body-left"`
//! and never touches a coordinate, which is where the second-largest share of
//! "beauty" comes from — gaps identical by construction, margins respected by
//! construction, zero pixel decisions per object. A free `at`/`size` stays
//! legal as the escape hatch and *wins* when both are present.
//!
//! Everything here is pure data plus pure functions: a layout is a named map
//! of [`Frame`]s computed from the canvas size and the theme's spacing, and
//! [`resolve_page_frames`] is the single geometry truth shared by render,
//! `describe`, and the exporters. Resolution never writes to the board file —
//! byte-stability is a format property and slot geometry is derived, not
//! stored.
//!
//! Anchors resolve here too, *after* slots, so an anchored callout binds to a
//! slot-placed chart's resolved frame rather than to nothing. `px`/`data`
//! anchors need the target's pixel/data frame and resolve with the figures
//! pack — they are reported, not guessed at.

use std::collections::BTreeMap;

use crate::layout::FontStack;
use crate::normalize::{Diagnostic, Severity};
use crate::schema::{Anchor, Board, Frame, Object, Page};
use crate::theme::{Spacing, Theme};

/// Every layout this build knows. One family per canvas geometry; these are
/// the talk family, parameterized by canvas size and theme spacing so a
/// different preset (A4 figure, poster) scales rather than forks.
pub const LAYOUT_NAMES: &[&str] = &[
    "title",
    "title-body",
    "two-up",
    "image-left",
    "full-bleed",
    "grid-2x2",
    "1+2",
    "3-across",
    "quote",
    "section",
    "hero-caption",
    "title-3up",
];

/// The named slots of one layout, as page-point frames.
///
/// Slot names come from a shared vocabulary (`title`, `subtitle`, `body`,
/// `body-left`, `body-right`, `cell-1..4`, `media`, `caption`, `quote`,
/// `attribution`) that layouts reuse — an object moved from `two-up` to
/// `grid-2x2` keeps a meaningful slot without renaming.
///
/// Band heights snap to the spacing grid so the numbers read like a designer
/// chose them; splits divide exactly so gaps are identical by construction.
/// Returns `None` for an unknown layout name or a canvas smaller than its
/// margins.
pub fn layout(name: &str, canvas: [f64; 2], spacing: &Spacing) -> Option<BTreeMap<String, Frame>> {
    let [w, h] = canvas;
    let [mt, mr, mb, ml] = spacing.margin;
    // The content rect every layout except full-bleed lives inside.
    let b = Frame {
        x: ml,
        y: mt,
        w: w - ml - mr,
        h: h - mt - mb,
    };
    if !(b.w > 0.0 && b.h > 0.0 && b.w.is_finite() && b.h.is_finite()) {
        return None;
    }
    let gap = spacing.gap.max(0.0);
    let grid = if spacing.grid > 0.0 {
        spacing.grid
    } else {
        8.0
    };
    let snap = |v: f64| (v / grid).round() * grid;
    let ext = |v: f64| snap(v).max(grid);

    // Shared bands: the title band and the body region under it.
    let th = ext(0.16 * b.h);
    let by = b.y + th + gap;
    let bh = (b.h - th - gap).max(1.0);
    let half_w = ((b.w - gap) / 2.0).max(1.0);
    let third_w = ((b.w - 2.0 * gap) / 3.0).max(1.0);

    let f = |x: f64, y: f64, w: f64, h: f64| Frame {
        x,
        y,
        w: w.max(1.0),
        h: h.max(1.0),
    };

    let slots: Vec<(&str, Frame)> = match name {
        // A centered opening block: title with a subtitle under it.
        "title" => {
            let ty = b.y + snap(0.26 * b.h);
            let t = f(b.x, ty, b.w, ext(0.22 * b.h));
            let s = f(b.x, t.bottom() + gap, b.w, ext(0.12 * b.h));
            vec![("title", t), ("subtitle", s)]
        }
        // The workhorse: a title band over one body region.
        "title-body" => vec![
            ("title", f(b.x, b.y, b.w, th)),
            ("body", f(b.x, by, b.w, bh)),
        ],
        // Title over two equal columns — comparison, chart + prose.
        "two-up" => vec![
            ("title", f(b.x, b.y, b.w, th)),
            ("body-left", f(b.x, by, half_w, bh)),
            ("body-right", f(b.x + half_w + gap, by, half_w, bh)),
        ],
        // Media on the left 55%, text on the right.
        "image-left" => {
            let mw = ext(0.55 * b.w - gap / 2.0).min(b.w - gap - 1.0);
            vec![
                ("media", f(b.x, b.y, mw, b.h)),
                ("body", f(b.x + mw + gap, b.y, b.w - mw - gap, b.h)),
            ]
        }
        // One image, the whole canvas, margins ignored on purpose.
        "full-bleed" => vec![("media", f(0.0, 0.0, w, h))],
        // Title over a 2×2 figure grid, cells numbered reading order.
        "grid-2x2" => {
            let rh = ((bh - gap) / 2.0).max(1.0);
            let x2 = b.x + half_w + gap;
            let y2 = by + rh + gap;
            vec![
                ("title", f(b.x, b.y, b.w, th)),
                ("cell-1", f(b.x, by, half_w, rh)),
                ("cell-2", f(x2, by, half_w, rh)),
                ("cell-3", f(b.x, y2, half_w, rh)),
                ("cell-4", f(x2, y2, half_w, rh)),
            ]
        }
        // One tall panel left, two stacked right.
        "1+2" => {
            let rh = ((bh - gap) / 2.0).max(1.0);
            let x2 = b.x + half_w + gap;
            vec![
                ("title", f(b.x, b.y, b.w, th)),
                ("body-left", f(b.x, by, half_w, bh)),
                ("cell-1", f(x2, by, half_w, rh)),
                ("cell-2", f(x2, by + rh + gap, half_w, rh)),
            ]
        }
        // Three full-height panels, no title band.
        "3-across" => (0..3)
            .map(|i| {
                let names = ["cell-1", "cell-2", "cell-3"];
                (
                    names[i],
                    f(b.x + i as f64 * (third_w + gap), b.y, third_w, b.h),
                )
            })
            .collect(),
        // A big centered quote with its attribution.
        "quote" => {
            let inset = snap(0.08 * b.w);
            let q = f(
                b.x + inset,
                b.y + snap(0.18 * b.h),
                b.w - 2.0 * inset,
                ext(0.38 * b.h),
            );
            let a = f(
                b.x + inset,
                q.bottom() + gap,
                b.w - 2.0 * inset,
                ext(0.10 * b.h),
            );
            vec![("quote", q), ("attribution", a)]
        }
        // A section break: the title sits low, like a chapter page.
        "section" => {
            let t = f(b.x, b.y + snap(0.52 * b.h), b.w, ext(0.20 * b.h));
            let s = f(b.x, t.bottom() + gap, b.w, ext(0.10 * b.h));
            vec![("title", t), ("subtitle", s)]
        }
        // A dominant image with a caption band under it.
        "hero-caption" => {
            let ch = ext(0.10 * b.h);
            vec![
                ("media", f(b.x, b.y, b.w, b.h - ch - gap)),
                ("caption", f(b.x, b.bottom() - ch, b.w, ch)),
            ]
        }
        // Title over three panels — the plan's demonstration layout.
        "title-3up" => {
            let mut v = vec![("title", f(b.x, b.y, b.w, th))];
            let names = ["cell-1", "cell-2", "cell-3"];
            for (i, n) in names.iter().enumerate() {
                v.push((n, f(b.x + i as f64 * (third_w + gap), by, third_w, bh)));
            }
            v
        }
        _ => return None,
    };

    Some(
        slots
            .into_iter()
            .map(|(n, fr)| (n.to_string(), fr))
            .collect(),
    )
}

/// One slot of one layout, for callers that don't need the whole map.
pub fn slot_frame(
    layout_name: &str,
    slot: &str,
    canvas: [f64; 2],
    spacing: &Spacing,
) -> Option<Frame> {
    layout(layout_name, canvas, spacing)?.get(slot).copied()
}

/// What a page measurably contains — the input to [`select_layout`]. Derived
/// entirely from the page, so selection stays a pure function.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContentMetrics {
    /// Placeable objects: text, shapes, and visuals. Connectors, groups and
    /// unknowns don't occupy a slot and don't count.
    pub object_count: usize,
    /// Any chart, image, or diagram — something a media/figure slot exists for.
    pub has_chart_or_image: bool,
    pub text_objects: usize,
    /// The longest text object's character count, a proxy for "is this a
    /// heading or a paragraph".
    pub longest_text_chars: usize,
    /// A text object carrying the `quote` role.
    pub has_quote_role: bool,
}

/// Measure a page for layout selection.
pub fn page_metrics(page: &Page) -> ContentMetrics {
    let mut m = ContentMetrics::default();
    for obj in page.walk() {
        match obj {
            Object::Text(t) => {
                m.object_count += 1;
                m.text_objects += 1;
                let chars: usize = t.text.iter().map(|p| p.plain_text().chars().count()).sum();
                m.longest_text_chars = m.longest_text_chars.max(chars);
                if t.role.as_deref() == Some("quote") {
                    m.has_quote_role = true;
                }
            }
            // An icon is decorative geometry like a shape — it occupies a slot
            // and counts, but does not claim a figure slot the way a chart does.
            Object::Shape(_) | Object::Icon(_) => m.object_count += 1,
            // A table is panel-like content: it claims a body slot exactly as
            // a chart does, so it counts as a visual for layout selection —
            // and an equation is a placed picture, the same kind of claim.
            Object::Chart(_)
            | Object::Image(_)
            | Object::Diagram(_)
            | Object::Table(_)
            | Object::Equation(_) => {
                m.object_count += 1;
                m.has_chart_or_image = true;
            }
            // Annotation composites sit over slot content; they never claim a
            // slot of their own, so they do not count.
            Object::Group(_)
            | Object::Connector(_)
            | Object::PanelLabel(_)
            | Object::Scalebar(_)
            | Object::SigBracket(_)
            | Object::Legend(_)
            | Object::Colorbar(_)
            | Object::Callout(_)
            | Object::Inset(_)
            | Object::Unknown(_) => {}
        }
    }
    m
}

/// Pick a layout: f(intent kind, measured content) → layout name.
///
/// Deterministic and total — every input maps to a known layout, so a page
/// with slots always resolves. The mapping, in the plan's spirit:
///
/// | intent          | layout                                              |
/// |-----------------|-----------------------------------------------------|
/// | cover / title   | `title`                                             |
/// | section         | `section`                                           |
/// | quote           | `quote`                                             |
/// | agenda, data    | `title-body`                                        |
/// | comparison      | `two-up`                                            |
/// | metrics         | `grid-2x2` when ≥3 visuals, else `title-body`       |
/// | claim-evidence  | `two-up` when a visual has ≥2 texts beside it, else `title-body` |
/// | none / unknown  | by content: quote role → `quote`; visual + ≥2 texts → `two-up`; a lone short text → `title`; else `title-body` |
pub fn select_layout(intent_kind: Option<&str>, m: &ContentMetrics) -> &'static str {
    let visuals = m.object_count.saturating_sub(m.text_objects);
    match intent_kind {
        Some("cover") | Some("title") => "title",
        Some("section") => "section",
        Some("quote") => "quote",
        Some("agenda") | Some("data") => "title-body",
        Some("metrics") => {
            if visuals >= 3 {
                "grid-2x2"
            } else {
                "title-body"
            }
        }
        Some("comparison") => "two-up",
        Some("claim-evidence") => {
            if m.has_chart_or_image && m.text_objects >= 2 {
                "two-up"
            } else {
                "title-body"
            }
        }
        _ => {
            if m.has_quote_role {
                "quote"
            } else if m.has_chart_or_image && m.text_objects >= 2 {
                "two-up"
            } else if !m.has_chart_or_image && m.object_count <= 2 && m.longest_text_chars <= 60 {
                "title"
            } else {
                "title-body"
            }
        }
    }
}

/// The anchor an object carries, where its type has one.
pub fn anchor_of(obj: &Object) -> Option<&Anchor> {
    match obj {
        Object::Text(o) => o.anchor.as_ref(),
        Object::Shape(o) => o.anchor.as_ref(),
        Object::Image(o) => o.anchor.as_ref(),
        Object::Chart(o) => o.anchor.as_ref(),
        Object::Diagram(o) => o.anchor.as_ref(),
        Object::PanelLabel(o) => o.anchor.as_ref(),
        Object::Group(_)
        | Object::Table(_)
        | Object::Connector(_)
        | Object::Equation(_)
        | Object::Icon(_)
        | Object::Scalebar(_)
        | Object::SigBracket(_)
        | Object::Legend(_)
        | Object::Colorbar(_)
        | Object::Callout(_)
        | Object::Inset(_)
        | Object::Unknown(_) => None,
    }
}

/// The object's declared size, independent of whether it has an `at`.
fn explicit_size(obj: &Object) -> Option<[f64; 2]> {
    match obj {
        Object::Text(o) => o.size,
        Object::Shape(o) => o.size,
        Object::Image(o) => o.size,
        Object::Group(o) => o.size,
        Object::Table(o) => o.size,
        Object::Chart(o) => o.size,
        Object::Diagram(o) => o.size,
        Object::Equation(o) => o.size,
        Object::Icon(o) => o.size,
        // An anchored letter needs *a* box before its glyph is measured; the
        // nominal keeps `inside-top-left` (the typical binding) exact.
        Object::PanelLabel(o) => o.size.or(Some(crate::composites::PANEL_LABEL_NOMINAL)),
        Object::Legend(o) => o.size,
        Object::Colorbar(o) => o.size,
        Object::Callout(o) => o.size,
        Object::Inset(o) => o.size,
        Object::Connector(_) | Object::Scalebar(_) | Object::SigBracket(_) | Object::Unknown(_) => {
            None
        }
    }
}

/// Resolve every object's page frame: the single geometry truth shared by
/// render, `describe`, and the exporters. See
/// [`resolve_page_frames_with_diags`] for what it reports along the way.
pub fn resolve_page_frames(
    board: &Board,
    page: &Page,
    theme: &Theme,
    fonts: Option<&FontStack>,
) -> BTreeMap<String, Frame> {
    resolve_page_frames_with_diags(board, page, theme, fonts).0
}

/// [`resolve_page_frames`], with the diagnostics resolution produced.
///
/// Precedence, exactly and in order:
///
/// 1. **Explicit `at` + `size` wins over a slot** (with an Info diagnostic
///    naming the override) — the escape hatch stays real.
/// 2. **A slot fills in** where explicit geometry is absent, from the page's
///    `layout` — or, when the page has slotted objects but no layout, from
///    [`select_layout`] over the page's own measured content.
/// 3. **Anchors resolve last**, against already-resolved frames, so an
///    anchored object binds to a slot-placed target. A resolving anchor is a
///    positional *binding* and overrides the object's own `at`; a dangling
///    target warns and falls back to the explicit position (or skips, with a
///    warning, when there is none). `px`/`data` anchors are left unresolved
///    with an Info — they resolve with the figures pack.
///
/// Anchors resolve in z order against the map as it stands, so an anchor may
/// reference an earlier-anchored object; chains never loop because each
/// object resolves exactly once. `fonts` is accepted (and unused today) so
/// measurement-driven resolution can arrive without an API break; `describe`
/// passes `None` rather than paying a font-database scan.
pub fn resolve_page_frames_with_diags(
    board: &Board,
    page: &Page,
    theme: &Theme,
    fonts: Option<&FontStack>,
) -> (BTreeMap<String, Frame>, Vec<Diagnostic>) {
    let _ = fonts; // reserved for measured (content-aware) resolution
    let mut diags = Vec::new();
    let canvas = board.canvas.size;

    let any_slot = page.walk().any(|o| o.slot().is_some());
    let layout_name: Option<String> = match &page.layout {
        Some(l) => Some(l.clone()),
        None if any_slot => Some(
            select_layout(
                page.intent.as_ref().map(|i| i.kind.as_str()),
                &page_metrics(page),
            )
            .to_string(),
        ),
        None => None,
    };
    let slots = layout_name
        .as_deref()
        .and_then(|n| layout(n, canvas, &theme.spacing));
    if let Some(name) = layout_name.as_deref() {
        if slots.is_none() {
            let mut d = Diagnostic::new(
                Severity::Warning,
                format!(
                    "layout {name:?} is not a known slot layout (known: {}); slots on this page \
                     do not resolve",
                    LAYOUT_NAMES.join(", ")
                ),
            )
            .field("layout");
            d.page = Some(page.id.clone());
            diags.push(d);
        }
    }

    // Pass 1 — slots and explicit geometry.
    let mut frames: BTreeMap<String, Frame> = BTreeMap::new();
    for obj in page.walk() {
        let id = obj.id();
        match (obj.frame(), obj.slot()) {
            (Some(f), Some(slot)) => {
                if slots.as_ref().is_some_and(|s| s.contains_key(slot)) {
                    diags.push(
                        Diagnostic::new(
                            Severity::Info,
                            format!("explicit geometry overrides slot {slot:?}"),
                        )
                        .at(&page.id, id)
                        .field("at"),
                    );
                }
                frames.insert(id.to_string(), f);
            }
            (Some(f), None) => {
                frames.insert(id.to_string(), f);
            }
            (None, Some(slot)) => match slots.as_ref().and_then(|s| s.get(slot)) {
                Some(f) => {
                    frames.insert(id.to_string(), *f);
                }
                None => {
                    if let (Some(name), true) = (layout_name.as_deref(), slots.is_some()) {
                        diags.push(
                            Diagnostic::new(
                                Severity::Warning,
                                format!("slot {slot:?} is not in layout {name:?}"),
                            )
                            .at(&page.id, id)
                            .field("slot"),
                        );
                    }
                }
            },
            (None, None) => {}
        }
    }

    // Pass 2 — anchors, against resolved frames.
    for obj in page.walk() {
        let Some(a) = anchor_of(obj) else { continue };
        let id = obj.id();

        if a.px.is_some() || a.data.is_some() {
            diags.push(
                Diagnostic::new(
                    Severity::Info,
                    "px/data anchors resolve with the figures pack; left unresolved",
                )
                .at(&page.id, id)
                .field("anchor"),
            );
            continue;
        }

        let size = explicit_size(obj).or_else(|| frames.get(id).map(|f| [f.w, f.h]));

        // Absolute passthrough — the default spelling of a position.
        if let Some(at) = a.at {
            let Some([w, h]) = size else {
                diags.push(
                    Diagnostic::new(
                        Severity::Warning,
                        "anchored object has no size; the anchor cannot produce a frame",
                    )
                    .at(&page.id, id)
                    .field("size"),
                );
                continue;
            };
            frames.insert(
                id.to_string(),
                Frame {
                    x: at[0],
                    y: at[1],
                    w,
                    h,
                },
            );
            continue;
        }

        let Some(target_id) = a.object.as_deref() else {
            continue; // an anchor with nothing to bind to says nothing
        };
        let Some(target) = frames.get(target_id).copied() else {
            let msg = if frames.contains_key(id) {
                format!("anchor target {target_id:?} does not resolve; rendered at its explicit position")
            } else {
                format!(
                    "anchor target {target_id:?} does not resolve and the object has no explicit \
                     position; skipped"
                )
            };
            diags.push(
                Diagnostic::new(Severity::Warning, msg)
                    .at(&page.id, id)
                    .field("anchor.object"),
            );
            continue;
        };
        let Some([w, h]) = size else {
            diags.push(
                Diagnostic::new(
                    Severity::Warning,
                    "anchored object has no size; the anchor cannot produce a frame",
                )
                .at(&page.id, id)
                .field("size"),
            );
            continue;
        };

        let rel = a.rel.as_deref().unwrap_or("center-of");
        let [dx, dy] = a.offset.unwrap_or([0.0, 0.0]);
        let center_x = target.x + (target.w - w) / 2.0;
        let center_y = target.y + (target.h - h) / 2.0;
        let (x, y) = match rel {
            "above" => (center_x, target.y - h),
            "below" => (center_x, target.bottom()),
            "left-of" => (target.x - w, center_y),
            "right-of" => (target.right(), center_y),
            "inside-top-left" => (target.x, target.y),
            "inside-top-right" => (target.right() - w, target.y),
            "inside-bottom-left" => (target.x, target.bottom() - h),
            "inside-bottom-right" => (target.right() - w, target.bottom() - h),
            "center-of" => (center_x, center_y),
            other => {
                diags.push(
                    Diagnostic::new(
                        Severity::Warning,
                        format!(
                            "anchor rel {other:?} is not in the vocabulary: above, below, \
                             left-of, right-of, inside-top-left, inside-top-right, \
                             inside-bottom-left, inside-bottom-right, center-of"
                        ),
                    )
                    .at(&page.id, id)
                    .field("anchor.rel"),
                );
                continue;
            }
        };
        frames.insert(
            id.to_string(),
            Frame {
                x: x + dx,
                y: y + dy,
                w,
                h,
            },
        );
    }

    (frames, diags)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Board;
    use crate::theme;

    const EPS: f64 = 1e-9;

    fn spacing() -> Spacing {
        theme::default_for(true).spacing
    }

    fn board(json: &str) -> Board {
        let mut b = crate::parse(json).unwrap();
        crate::normalize(&mut b);
        b
    }

    fn page_board(page: &str) -> Board {
        board(&format!(
            r#"{{"format":"chimaera.board","formatVersion":1,
                "canvas":{{"size":[960,540]}},
                "pages":[{page}]}}"#
        ))
    }

    fn resolve(b: &Board) -> (BTreeMap<String, Frame>, Vec<Diagnostic>) {
        let theme = theme::default_for(true);
        resolve_page_frames_with_diags(b, &b.pages[0], &theme, None)
    }

    #[test]
    fn every_layout_has_disjoint_slots_inside_the_canvas() {
        let sp = spacing();
        for name in LAYOUT_NAMES {
            let slots = layout(name, [960.0, 540.0], &sp)
                .unwrap_or_else(|| panic!("{name} is listed but not implemented"));
            assert!(!slots.is_empty(), "{name} has no slots");
            let v: Vec<(&String, &Frame)> = slots.iter().collect();
            for (slot, f) in &v {
                assert!(
                    f.x >= -EPS
                        && f.y >= -EPS
                        && f.right() <= 960.0 + EPS
                        && f.bottom() <= 540.0 + EPS,
                    "{name}/{slot} leaves the canvas: {f:?}"
                );
                if *name != "full-bleed" {
                    // Everything except full-bleed stays inside the margins.
                    assert!(
                        f.x >= sp.margin[3] - EPS
                            && f.y >= sp.margin[0] - EPS
                            && f.right() <= 960.0 - sp.margin[1] + EPS
                            && f.bottom() <= 540.0 - sp.margin[2] + EPS,
                        "{name}/{slot} violates the margins: {f:?}"
                    );
                }
            }
            for i in 0..v.len() {
                for j in i + 1..v.len() {
                    let (na, a) = v[i];
                    let (nb, b) = v[j];
                    let overlap = a.x < b.right() - EPS
                        && b.x < a.right() - EPS
                        && a.y < b.bottom() - EPS
                        && b.y < a.bottom() - EPS;
                    assert!(!overlap, "{name}: {na} overlaps {nb}: {a:?} vs {b:?}");
                }
            }
        }
    }

    #[test]
    fn slot_frame_reads_one_slot() {
        let sp = spacing();
        let all = layout("title-body", [960.0, 540.0], &sp).unwrap();
        assert_eq!(
            slot_frame("title-body", "title", [960.0, 540.0], &sp),
            all.get("title").copied()
        );
        assert!(slot_frame("title-body", "media", [960.0, 540.0], &sp).is_none());
        assert!(slot_frame("no-such", "title", [960.0, 540.0], &sp).is_none());
    }

    #[test]
    fn layout_selection_is_deterministic_and_maps_intents() {
        let one_visual = ContentMetrics {
            object_count: 2,
            has_chart_or_image: true,
            text_objects: 1,
            longest_text_chars: 30,
            has_quote_role: false,
        };
        assert_eq!(select_layout(Some("cover"), &one_visual), "title");
        assert_eq!(select_layout(Some("section"), &one_visual), "section");
        assert_eq!(select_layout(Some("quote"), &one_visual), "quote");
        assert_eq!(select_layout(Some("agenda"), &one_visual), "title-body");
        assert_eq!(select_layout(Some("data"), &one_visual), "title-body");
        assert_eq!(select_layout(Some("comparison"), &one_visual), "two-up");
        // claim-evidence with one visual and one text: title over the visual.
        assert_eq!(
            select_layout(Some("claim-evidence"), &one_visual),
            "title-body"
        );
        // …with prose beside the visual: two-up.
        let visual_and_text = ContentMetrics {
            object_count: 3,
            text_objects: 2,
            ..one_visual
        };
        assert_eq!(
            select_layout(Some("claim-evidence"), &visual_and_text),
            "two-up"
        );
        // metrics with three visuals: the figure grid.
        let three_visuals = ContentMetrics {
            object_count: 4,
            text_objects: 1,
            ..one_visual
        };
        assert_eq!(select_layout(Some("metrics"), &three_visuals), "grid-2x2");
        assert_eq!(select_layout(Some("metrics"), &one_visual), "title-body");
        // No intent: content decides, deterministically.
        let quote_page = ContentMetrics {
            has_quote_role: true,
            ..one_visual
        };
        assert_eq!(select_layout(None, &quote_page), "quote");
        for m in [one_visual, visual_and_text, three_visuals] {
            assert_eq!(
                select_layout(Some("claim-evidence"), &m),
                select_layout(Some("claim-evidence"), &m),
                "selection must be a pure function"
            );
        }
    }

    #[test]
    fn a_slot_placed_object_takes_the_slot_frame() {
        let b = page_board(
            r#"{"id":"p","layout":"title-body","objects":[
                 {"id":"heading","type":"text","role":"title","slot":"title",
                  "text":["The parser rewrite is 3x faster"]},
                 {"id":"panel","type":"shape","geo":"rect","slot":"body"}]}"#,
        );
        let (frames, diags) = resolve(&b);
        // Content rect on 960×540 with margin [64,72,64,72]: x 72, y 64,
        // w 816, h 412; title band snaps to 64, gap 24.
        assert_eq!(
            frames.get("heading"),
            Some(&Frame {
                x: 72.0,
                y: 64.0,
                w: 816.0,
                h: 64.0
            })
        );
        assert_eq!(
            frames.get("panel"),
            Some(&Frame {
                x: 72.0,
                y: 152.0,
                w: 816.0,
                h: 324.0
            })
        );
        assert!(
            !diags.iter().any(|d| d.severity >= Severity::Warning),
            "{diags:?}"
        );
    }

    #[test]
    fn a_page_without_a_layout_selects_one_from_intent_and_content() {
        let b = page_board(
            r#"{"id":"p","intent":{"kind":"claim-evidence"},"objects":[
                 {"id":"heading","type":"text","role":"title","slot":"title",
                  "text":["One claim"]}]}"#,
        );
        let (frames, _) = resolve(&b);
        // claim-evidence with one text and no visual → title-body.
        assert_eq!(
            frames.get("heading"),
            Some(&Frame {
                x: 72.0,
                y: 64.0,
                w: 816.0,
                h: 64.0
            })
        );
    }

    #[test]
    fn explicit_geometry_wins_over_the_slot_with_an_info() {
        let b = page_board(
            r#"{"id":"p","layout":"title-body","objects":[
                 {"id":"heading","type":"text","slot":"title","at":[80,80],"size":[160,80],
                  "text":["hand-placed"]}]}"#,
        );
        let (frames, diags) = resolve(&b);
        assert_eq!(
            frames.get("heading"),
            Some(&Frame {
                x: 80.0,
                y: 80.0,
                w: 160.0,
                h: 80.0
            })
        );
        assert!(
            diags.iter().any(|d| d.severity == Severity::Info
                && d.message.contains("explicit geometry overrides slot")),
            "{diags:?}"
        );
    }

    #[test]
    fn a_slot_missing_from_the_layout_warns() {
        let b = page_board(
            r#"{"id":"p","layout":"title-body","objects":[
                 {"id":"x","type":"shape","geo":"rect","slot":"cell-4"}]}"#,
        );
        let (frames, diags) = resolve(&b);
        assert!(!frames.contains_key("x"));
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Warning && d.message.contains("cell-4")),
            "{diags:?}"
        );
    }

    #[test]
    fn every_anchor_rel_resolves_with_exact_arithmetic() {
        // Target at [96, 96], 192×96 → right 288, bottom 192. Anchored
        // objects are 48×32, so centering offsets are 72 (x) and 32 (y).
        let cases = [
            ("above", 168.0, 64.0),
            ("below", 168.0, 192.0),
            ("left-of", 48.0, 128.0),
            ("right-of", 288.0, 128.0),
            ("inside-top-left", 96.0, 96.0),
            ("inside-top-right", 240.0, 96.0),
            ("inside-bottom-left", 96.0, 160.0),
            ("inside-bottom-right", 240.0, 160.0),
            ("center-of", 168.0, 128.0),
        ];
        let anchored: String = cases
            .iter()
            .map(|(rel, _, _)| {
                format!(
                    r#",{{"id":"a-{rel}","type":"shape","geo":"rect","size":[48,32],
                        "anchor":{{"object":"target","rel":"{rel}"}}}}"#
                )
            })
            .collect();
        let b = page_board(&format!(
            r#"{{"id":"p","objects":[
                 {{"id":"target","type":"shape","geo":"rect","at":[96,96],"size":[192,96]}}
                 {anchored}]}}"#
        ));
        let (frames, diags) = resolve(&b);
        for (rel, x, y) in cases {
            assert_eq!(
                frames.get(&format!("a-{rel}")),
                Some(&Frame {
                    x,
                    y,
                    w: 48.0,
                    h: 32.0
                }),
                "rel {rel}"
            );
        }
        assert!(
            !diags.iter().any(|d| d.severity >= Severity::Warning),
            "{diags:?}"
        );
    }

    #[test]
    fn anchor_offsets_add_after_the_rel() {
        let b = page_board(
            r#"{"id":"p","objects":[
                 {"id":"target","type":"shape","geo":"rect","at":[96,96],"size":[192,96]},
                 {"id":"note","type":"shape","geo":"rect","size":[48,32],
                  "anchor":{"object":"target","rel":"below","offset":[8,16]}}]}"#,
        );
        let (frames, _) = resolve(&b);
        assert_eq!(
            frames.get("note"),
            Some(&Frame {
                x: 176.0,
                y: 208.0,
                w: 48.0,
                h: 32.0
            })
        );
    }

    #[test]
    fn anchor_at_is_an_absolute_passthrough() {
        let b = page_board(
            r#"{"id":"p","objects":[
                 {"id":"note","type":"shape","geo":"rect","size":[48,32],
                  "anchor":{"at":[40,48]}}]}"#,
        );
        let (frames, _) = resolve(&b);
        assert_eq!(
            frames.get("note"),
            Some(&Frame {
                x: 40.0,
                y: 48.0,
                w: 48.0,
                h: 32.0
            })
        );
    }

    #[test]
    fn an_anchor_binds_to_a_slot_resolved_target() {
        // The target has no `at` at all — only a slot. The anchor must see
        // the slot-resolved frame, which is the whole point of resolving
        // anchors after slots.
        let b = page_board(
            r#"{"id":"p","layout":"title-body","objects":[
                 {"id":"panel","type":"shape","geo":"rect","slot":"body"},
                 {"id":"badge","type":"shape","geo":"rect","size":[48,32],
                  "anchor":{"object":"panel","rel":"inside-top-right"}}]}"#,
        );
        let (frames, _) = resolve(&b);
        // body slot: [72, 152] 816×324 → right edge 888.
        assert_eq!(
            frames.get("badge"),
            Some(&Frame {
                x: 840.0,
                y: 152.0,
                w: 48.0,
                h: 32.0
            })
        );
    }

    #[test]
    fn a_dangling_anchor_warns_and_falls_back_to_explicit_at() {
        let b = page_board(
            r#"{"id":"p","objects":[
                 {"id":"note","type":"shape","geo":"rect","at":[80,80],"size":[48,32],
                  "anchor":{"object":"ghost","rel":"above"}},
                 {"id":"lost","type":"shape","geo":"rect","size":[48,32],
                  "anchor":{"object":"ghost","rel":"above"}}]}"#,
        );
        let (frames, diags) = resolve(&b);
        assert_eq!(
            frames.get("note"),
            Some(&Frame {
                x: 80.0,
                y: 80.0,
                w: 48.0,
                h: 32.0
            })
        );
        assert!(!frames.contains_key("lost"));
        let warns: Vec<_> = diags
            .iter()
            .filter(|d| d.severity == Severity::Warning && d.message.contains("ghost"))
            .collect();
        assert_eq!(warns.len(), 2, "{diags:?}");
        assert!(warns.iter().any(|d| d.message.contains("skipped")));
    }

    #[test]
    fn px_and_data_anchors_stay_unresolved_with_an_info() {
        let b = page_board(
            r#"{"id":"p","objects":[
                 {"id":"target","type":"image","src":"a.png","at":[96,96],"size":[192,96]},
                 {"id":"mark","type":"shape","geo":"rect","size":[16,16],
                  "anchor":{"object":"target","px":[512,300]}}]}"#,
        );
        let (frames, diags) = resolve(&b);
        assert!(!frames.contains_key("mark"));
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Info && d.message.contains("figures pack")),
            "{diags:?}"
        );
    }

    #[test]
    fn an_unknown_rel_warns_and_does_not_place() {
        let b = page_board(
            r#"{"id":"p","objects":[
                 {"id":"target","type":"shape","geo":"rect","at":[96,96],"size":[192,96]},
                 {"id":"note","type":"shape","geo":"rect","size":[48,32],
                  "anchor":{"object":"target","rel":"beneath-ish"}}]}"#,
        );
        let (frames, diags) = resolve(&b);
        assert!(!frames.contains_key("note"));
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Warning && d.message.contains("beneath-ish")),
            "{diags:?}"
        );
    }

    #[test]
    fn an_unknown_page_layout_warns_once() {
        let b = page_board(
            r#"{"id":"p","layout":"pinterest-board","objects":[
                 {"id":"x","type":"text","slot":"title","text":["hi"]}]}"#,
        );
        let (frames, diags) = resolve(&b);
        assert!(!frames.contains_key("x"));
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Warning && d.message.contains("pinterest-board")),
            "{diags:?}"
        );
    }

    #[test]
    fn a_connector_binds_to_a_slot_placed_objects_resolved_frame() {
        use crate::layout::FontStack;
        // The panel has ONLY a slot; the connector's endpoint must land on
        // the slot-resolved right edge, not be skipped as unresolvable.
        let b = page_board(
            r#"{"id":"p","layout":"title-body","objects":[
                 {"id":"panel","type":"shape","geo":"rect","slot":"body"},
                 {"id":"arrow","type":"connector",
                  "from":{"object":"panel","side":"right"},
                  "to":{"at":[940,300]}}]}"#,
        );
        let theme = theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let mut diags = Vec::new();
        let svg =
            crate::render::page_svg(&b, &b.pages[0], &theme, &fonts, None, &mut diags).unwrap();
        // body slot: [72, 152] 816×324 → right edge x 888, mid-height y 314.
        assert!(
            svg.contains(r#"x1="888" y1="314""#),
            "connector must start at the resolved slot edge: {svg}"
        );
        assert!(
            !diags.iter().any(|d| d.message.contains("does not resolve")),
            "{diags:?}"
        );
    }

    #[test]
    fn a_slot_placed_text_renders_without_a_position_warning() {
        use crate::layout::FontStack;
        let b = page_board(
            r#"{"id":"p","layout":"title-body","objects":[
                 {"id":"heading","type":"text","role":"title","slot":"title",
                  "text":["Slot-placed"]}]}"#,
        );
        let theme = theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let mut diags = Vec::new();
        let svg =
            crate::render::page_svg(&b, &b.pages[0], &theme, &fonts, None, &mut diags).unwrap();
        assert!(svg.contains("Slot-placed"), "{svg}");
        assert!(
            !diags.iter().any(|d| d.message.contains("no position")),
            "{diags:?}"
        );
    }

    #[test]
    fn resolution_is_deterministic() {
        let b = page_board(
            r#"{"id":"p","layout":"two-up","objects":[
                 {"id":"t","type":"text","slot":"title","text":["x"]},
                 {"id":"l","type":"shape","geo":"rect","slot":"body-left"},
                 {"id":"n","type":"shape","geo":"rect","size":[48,32],
                  "anchor":{"object":"l","rel":"below"}}]}"#,
        );
        let (a, _) = resolve(&b);
        let (c, _) = resolve(&b);
        assert_eq!(a, c);
    }
}
