//! Target presets — the four axes that make Board domain-neutral.
//!
//! A preset carries **geometry** (canvas, margins, the physical column width),
//! **floors** (`minPt` scaling, line weight, effective DPI, export tier),
//! **page furniture** (page number, footer, logo — repeated objects stamped on
//! every page), and **rules and refusals** (which content rules apply *here*).
//! Switching preset atomically remaps all four: Nature-single → Cell is one
//! click, and so is design-review → exec-update — the same mechanism doing
//! ordinary work.
//!
//! The shipped family is **general first** (`talk-16x9`, `design-review`,
//! `exec-update`, `teaching`, `readme-image`, `poster-a0`); the figures pack
//! (`pub-nature-single`, `pub-cell`, `pub-plos`) is a pack, not the product's
//! identity. Rules like "every axis label carries a unit" and the
//! caption-integrity check are publication-preset *data*, not universal
//! predicates — a bar chart of signups by channel has no unit.
//!
//! Everything here is pure data plus pure functions; nothing reads a clock or
//! the filesystem, so furniture and tiers stay deterministic render inputs.

use std::sync::OnceLock;

use crate::export::ExportTier;
use crate::schema::{Align, Object, Paragraph, TextKind, TextObject, VAlign};

/// One target preset: the unit `lint --target` and export gate against.
#[derive(Debug, Clone)]
pub struct Preset {
    pub id: &'static str,
    pub geometry: PresetGeometry,
    pub floors: Floors,
    pub furniture: Vec<Furniture>,
    pub rules: Rules,
}

/// Axis 1 — geometry.
#[derive(Debug, Clone, Copy)]
pub struct PresetGeometry {
    /// `[width, height]` in points.
    pub canvas: [f64; 2],
    /// `[top, right, bottom, left]` in points, matching `Spacing::margin`.
    pub margins: [f64; 4],
    /// The physical column width the venue specifies, when it specifies one.
    /// Carried so an exporter can state real-world scale; the point canvas is
    /// already sized to it.
    pub mm_width: Option<f64>,
}

/// Axis 2 — floors. Below a floor is an [`crate::Severity::Error`], never a
/// warning: §3.5 makes anti-ugly a format property.
#[derive(Debug, Clone, Copy)]
pub struct Floors {
    /// Multiplier over each role's own `minPt`. 1.0 trusts the theme; a
    /// stricter venue raises every role's floor proportionally (PLOS's 8 pt
    /// minimum over a 5 pt `label` floor is a 1.6 scale).
    pub min_pt_scale: f64,
    /// The thinnest stroke this target accepts, in points. Journal guidance
    /// is ≥0.5 pt for most venues; large-format print wants ≥1 pt.
    pub min_line_width_pt: f64,
    /// The lowest effective DPI a raster image may land at, at placed size.
    pub min_effective_dpi: f64,
    /// The lowest [`ExportTier`] any object may export at. `Raster` accepts
    /// everything; `Vector` is the publication floor that refuses pixels.
    pub export_floor: ExportTier,
}

/// Axis 3 — one piece of page furniture: a repeated object the preset stamps
/// on every page. Survives to PPTX *layout* emission eventually, so it lands
/// as real master content rather than copies.
#[derive(Debug, Clone)]
pub struct Furniture {
    pub kind: FurnitureKind,
    /// The named slot this furniture occupies, e.g. `footer` — kept as data
    /// so the slot family can reposition it per layout.
    pub slot: String,
    /// Literal text. `None` on a `Footer` means "the board's title": the
    /// emitted object carries empty text and the render call site fills it in
    /// (see [`furniture_objects`]), because this module never sees the board.
    pub text: Option<String>,
    /// A cover page usually carries no page number.
    pub suppress_on_cover: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FurnitureKind {
    PageNumber,
    Footer,
    Logo,
}

/// Axis 4 — rules and refusals: which content rules apply at this venue.
#[derive(Debug, Clone)]
pub struct Rules {
    /// Every chart axis title must carry a unit-ish parenthetical — `(s)`,
    /// `(a.u.)`. A publication rule; a talk axis titled "Signups" is fine.
    pub require_axis_units: bool,
    /// The caption must state *n*, the error-bar definition, and the test.
    /// Enforced by the figures pack's caption lint when captions land
    /// (slice 5); carried as data now so presets need no migration.
    pub require_caption_integrity: bool,
    /// Advisory feature names this venue refuses — `pie`, `second-y`,
    /// `histogram`. The board skill reads these before authoring; lint checks
    /// the (currently tiny) expressible surface. A preset refusal reads as
    /// the venue arguing with the content, which is true and useful — a
    /// global refusal would read as the tool arguing with the user.
    pub refuses: Vec<String>,
}

fn permissive_floors() -> Floors {
    Floors {
        min_pt_scale: 1.0,
        min_line_width_pt: 0.25,
        min_effective_dpi: 96.0,
        export_floor: ExportTier::Raster,
    }
}

fn publication_floors() -> Floors {
    Floors {
        min_pt_scale: 1.0,
        min_line_width_pt: 0.5,
        min_effective_dpi: 300.0,
        export_floor: ExportTier::Vector,
    }
}

fn permissive_rules() -> Rules {
    Rules {
        require_axis_units: false,
        require_caption_integrity: false,
        refuses: Vec::new(),
    }
}

fn publication_rules() -> Rules {
    Rules {
        require_axis_units: true,
        require_caption_integrity: true,
        refuses: vec![
            "pie".to_string(),
            "second-y".to_string(),
            "histogram".to_string(),
        ],
    }
}

const TALK_MARGINS: [f64; 4] = [64.0, 72.0, 64.0, 72.0];

fn build() -> Vec<Preset> {
    vec![
        // The default: a 16:9 talk. No furniture, permissive everywhere.
        Preset {
            id: "talk-16x9",
            geometry: PresetGeometry {
                canvas: [960.0, 540.0],
                margins: TALK_MARGINS,
                mm_width: None,
            },
            floors: permissive_floors(),
            furniture: Vec::new(),
            rules: permissive_rules(),
        },
        // A working deck for a design review: footer (the board title) and a
        // page number, both so "as discussed on page 4" means something.
        Preset {
            id: "design-review",
            geometry: PresetGeometry {
                canvas: [960.0, 540.0],
                margins: TALK_MARGINS,
                mm_width: None,
            },
            floors: permissive_floors(),
            furniture: vec![
                Furniture {
                    kind: FurnitureKind::Footer,
                    slot: "footer".to_string(),
                    text: None,
                    suppress_on_cover: false,
                },
                Furniture {
                    kind: FurnitureKind::PageNumber,
                    slot: "page-number".to_string(),
                    text: None,
                    suppress_on_cover: true,
                },
            ],
            rules: permissive_rules(),
        },
        // An executive update refuses nothing — a pie chart here is allowed,
        // which is the four-axis point: refusals are venue data, not dogma.
        Preset {
            id: "exec-update",
            geometry: PresetGeometry {
                canvas: [960.0, 540.0],
                margins: TALK_MARGINS,
                mm_width: None,
            },
            floors: permissive_floors(),
            furniture: vec![Furniture {
                kind: FurnitureKind::PageNumber,
                slot: "page-number".to_string(),
                text: None,
                suppress_on_cover: true,
            }],
            rules: permissive_rules(),
        },
        Preset {
            id: "teaching",
            geometry: PresetGeometry {
                canvas: [960.0, 540.0],
                margins: TALK_MARGINS,
                mm_width: None,
            },
            floors: permissive_floors(),
            furniture: vec![Furniture {
                kind: FurnitureKind::Footer,
                slot: "footer".to_string(),
                text: None,
                suppress_on_cover: false,
            }],
            rules: permissive_rules(),
        },
        // A single image destined for a README: one page, no furniture.
        Preset {
            id: "readme-image",
            geometry: PresetGeometry {
                canvas: [800.0, 450.0],
                margins: [32.0, 32.0, 32.0, 32.0],
                mm_width: None,
            },
            floors: permissive_floors(),
            furniture: Vec::new(),
            rules: permissive_rules(),
        },
        // A0 portrait (841 × 1189 mm at 72 pt/in). Print at arm's length
        // forgives DPI but not hairlines: sub-1 pt strokes vanish at plot.
        Preset {
            id: "poster-a0",
            geometry: PresetGeometry {
                canvas: [2384.0, 3370.0],
                margins: [96.0, 96.0, 96.0, 96.0],
                mm_width: Some(841.0),
            },
            floors: Floors {
                min_pt_scale: 1.0,
                min_line_width_pt: 1.0,
                min_effective_dpi: 150.0,
                export_floor: ExportTier::Raster,
            },
            furniture: Vec::new(),
            rules: permissive_rules(),
        },
        // --- The figures pack -------------------------------------------
        // Nature single column: 89 mm wide (252.28 pt); height is free up to
        // a full page, so the canvas height here is a working default the
        // author resizes. The figure-light theme's per-role minPt values are
        // already Nature-compatible, so the scale stays 1.0.
        Preset {
            id: "pub-nature-single",
            geometry: PresetGeometry {
                canvas: [252.28, 200.0],
                margins: [8.0, 8.0, 8.0, 8.0],
                mm_width: Some(89.0),
            },
            floors: publication_floors(),
            furniture: Vec::new(),
            rules: publication_rules(),
        },
        // Cell single column: 85 mm (240.94 pt); Cell's stated text range is
        // 6–8 pt, so the floor scales the theme's 5 pt minima up to 6.
        Preset {
            id: "pub-cell",
            geometry: PresetGeometry {
                canvas: [240.94, 200.0],
                margins: [8.0, 8.0, 8.0, 8.0],
                mm_width: Some(85.0),
            },
            floors: Floors {
                min_pt_scale: 1.2,
                ..publication_floors()
            },
            furniture: Vec::new(),
            rules: publication_rules(),
        },
        // PLOS: max figure width 19.05 cm (7.5 in = 540 pt), minimum text
        // size 8 pt (a 1.6 scale over the theme's 5 pt floors) — and PLOS
        // requires **Arial, not Helvetica**, the specific trap that bounces
        // submissions. The figure family stack keeps Arial ahead of the other
        // system faces, but the bundled brand sans now leads it; a strict PLOS
        // export pins Arial by editing the theme's family stack.
        Preset {
            id: "pub-plos",
            geometry: PresetGeometry {
                canvas: [540.0, 360.0],
                margins: [8.0, 8.0, 8.0, 8.0],
                mm_width: Some(190.5),
            },
            floors: Floors {
                min_pt_scale: 1.6,
                ..publication_floors()
            },
            furniture: Vec::new(),
            rules: publication_rules(),
        },
    ]
}

static PRESETS: OnceLock<Vec<Preset>> = OnceLock::new();

/// Every bundled preset, in family order (general first, figures pack last).
pub fn all() -> &'static [Preset] {
    PRESETS.get_or_init(build).as_slice()
}

/// Resolve a preset by id.
pub fn get(id: &str) -> Option<&'static Preset> {
    all().iter().find(|p| p.id == id)
}

/// The bundled ids, for "unknown target" error messages.
pub fn ids() -> Vec<&'static str> {
    all().iter().map(|p| p.id).collect()
}

// ---------------------------------------------------------------------------
// Furniture
// ---------------------------------------------------------------------------

/// Height of the furniture band, sitting inside the bottom margin.
const FURNITURE_H: f64 = 24.0;
/// Distance from the canvas bottom to the top of the band.
const FURNITURE_INSET: f64 = 32.0;

// WIRE: render.rs's page_svg draws furniture with one line after the page's
// own objects — for each `o` in
// `presets::furniture_objects(preset, page_index, page_count, page_index == 0, [w, h])`
// call `emit_object(&mut s, &o, page, board, theme, fonts, &index, diags)`,
// where `preset` resolved via `presets::get` from `board.canvas.target`
// (falling back to `board.canvas.preset`); a `Footer` object arriving with
// empty text takes `board.title` at that call site. Furniture draws above
// content (last in z), and is generated per render — never written back into
// the board file.

/// The furniture objects for one page of a preset: small text objects in the
/// bottom margin band, in role `label`, with ids under `furniture/`.
///
/// Pure and deterministic: the same inputs always produce the same objects,
/// so renders stay content-addressed. A `Footer` or `Logo` whose preset
/// carries no literal text is emitted with empty text for the caller to fill
/// (the board title); `PageNumber` text is always `"<page> / <count>"`.
pub fn furniture_objects(
    preset: &Preset,
    page_index: usize,
    page_count: usize,
    is_cover: bool,
    canvas: [f64; 2],
) -> Vec<Object> {
    let m = preset.geometry.margins;
    let y = (canvas[1] - FURNITURE_INSET).max(0.0);
    let mut out = Vec::new();
    for f in &preset.furniture {
        if is_cover && f.suppress_on_cover {
            continue;
        }
        let (id, align, x, w, text) = match f.kind {
            FurnitureKind::PageNumber => {
                let w = 96.0_f64.min(canvas[0]);
                (
                    "furniture/page-number",
                    Align::Right,
                    (canvas[0] - m[1] - w).max(0.0),
                    w,
                    Some(format!("{} / {}", page_index + 1, page_count)),
                )
            }
            FurnitureKind::Footer => (
                "furniture/footer",
                Align::Center,
                m[3].min(canvas[0]),
                (canvas[0] - m[1] - m[3]).max(0.0),
                f.text.clone(),
            ),
            FurnitureKind::Logo => (
                "furniture/logo",
                Align::Left,
                m[3].min(canvas[0]),
                160.0_f64.min(canvas[0]),
                f.text.clone(),
            ),
        };
        out.push(Object::Text(TextObject {
            id: id.to_string(),
            kind: TextKind,
            role: Some("label".to_string()),
            slot: Some(f.slot.clone()),
            at: Some([x, y]),
            size: Some([w, FURNITURE_H]),
            text: text.map(|t| vec![Paragraph::Plain(t)]).unwrap_or_default(),
            align: Some(align),
            valign: Some(VAlign::Middle),
            anchor: None,
            alt: None,
            link: None,
            rotation: None,
            extra: crate::schema::Extra::new(),
        }));
    }
    out
}

// ---------------------------------------------------------------------------
// Export tiers per instance
// ---------------------------------------------------------------------------

/// Fidelity rank of a tier: higher is more faithful. The order the export
/// floor compares by — `Raster` < `Vector` < `Grouped` < `Native`.
pub fn tier_rank(tier: ExportTier) -> u8 {
    match tier {
        ExportTier::Raster => 0,
        ExportTier::Vector => 1,
        ExportTier::Grouped => 2,
        ExportTier::Native => 3,
    }
}

/// The export tier this object lands at, with the reason — computed from the
/// object alone, so the preflight, `describe`, and the census in
/// `lint --target` all state the same fate the exporter delivers.
pub fn tier_of(object: &Object) -> (ExportTier, &'static str) {
    match object {
        Object::Text(_) => (ExportTier::Native, "editable text at the destination"),
        Object::Shape(_) => (ExportTier::Native, "native shape geometry"),
        Object::Connector(_) => (ExportTier::Native, "native connector"),
        Object::Table(_) => (ExportTier::Native, "native table (a:tbl)"),
        Object::Chart(_) => (
            ExportTier::Grouped,
            "chart decomposes to editable primitives",
        ),
        Object::Diagram(_) => (
            ExportTier::Grouped,
            "diagram expands to editable primitives",
        ),
        Object::Icon(_) => (
            ExportTier::Grouped,
            "icon exports as editable vector shapes",
        ),
        Object::PanelLabel(_)
        | Object::Scalebar(_)
        | Object::SigBracket(_)
        | Object::Legend(_)
        | Object::Colorbar(_)
        | Object::Callout(_)
        | Object::Inset(_) => (
            ExportTier::Grouped,
            "annotation composite as grouped shapes",
        ),
        Object::Image(img) => {
            if img.src.to_ascii_lowercase().ends_with(".svg") {
                (ExportTier::Vector, "svg image places as vector")
            } else {
                (ExportTier::Raster, "raster pixels at placed size")
            }
        }
        // The plan's degradation contract pins equation v1 to "picture" at
        // every slide target; the svgBlip beside the PNG is picture-quality
        // enhancement, not a tier change.
        Object::Equation(_) => (
            ExportTier::Raster,
            "equation exports as a picture (PNG + svgBlip); native OMML is a later arm",
        ),
        // A group takes its lowest child's tier (and that child's reason):
        // one rasterized member drags the whole group's fate down, which is
        // exactly what the census must surface.
        Object::Group(g) => g
            .objects
            .iter()
            .map(tier_of)
            .min_by_key(|(t, _)| tier_rank(*t))
            .unwrap_or((ExportTier::Native, "empty group emits nothing")),
        Object::Unknown(_) => (ExportTier::Raster, "skipped: unknown object type"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_preset_id_resolves() {
        let expect = [
            "talk-16x9",
            "design-review",
            "exec-update",
            "teaching",
            "readme-image",
            "poster-a0",
            "pub-nature-single",
            "pub-cell",
            "pub-plos",
        ];
        assert_eq!(ids(), expect, "the shipped family, general first");
        for id in expect {
            let p = get(id).unwrap_or_else(|| panic!("{id} must resolve"));
            assert_eq!(p.id, id);
            assert!(p.geometry.canvas[0] > 0.0 && p.geometry.canvas[1] > 0.0);
            assert!(p.floors.min_pt_scale > 0.0);
            assert!(p.floors.min_line_width_pt > 0.0);
            assert!(p.floors.min_effective_dpi > 0.0);
        }
        assert!(get("nope").is_none());
    }

    #[test]
    fn the_figures_pack_carries_the_publication_rules() {
        let nature = get("pub-nature-single").unwrap();
        assert_eq!(nature.geometry.mm_width, Some(89.0));
        assert!(nature.rules.require_axis_units);
        assert!(nature.rules.require_caption_integrity);
        for refused in ["pie", "second-y", "histogram"] {
            assert!(nature.rules.refuses.iter().any(|r| r == refused));
        }
        assert_eq!(nature.floors.export_floor, ExportTier::Vector);
        // exec-update refuses nothing — a pie chart is allowed there.
        assert!(get("exec-update").unwrap().rules.refuses.is_empty());
        // PLOS raises the theme's 5 pt floors to its 8 pt minimum.
        let plos = get("pub-plos").unwrap();
        assert!((plos.floors.min_pt_scale - 1.6).abs() < 1e-9);
    }

    #[test]
    fn furniture_objects_sit_inside_the_canvas() {
        let p = get("design-review").unwrap();
        let canvas = p.geometry.canvas;
        let objs = furniture_objects(p, 1, 5, false, canvas);
        assert_eq!(objs.len(), 2, "footer + page number");
        for o in &objs {
            let f = o.frame().expect("furniture is placed");
            assert!(f.x >= 0.0 && f.y >= 0.0, "{}: {f:?}", o.id());
            assert!(
                f.right() <= canvas[0] && f.bottom() <= canvas[1],
                "{}: {f:?} leaves the {canvas:?} canvas",
                o.id()
            );
            assert!(o.id().starts_with("furniture/"), "{}", o.id());
        }
        let Some(Object::Text(num)) = objs.iter().find(|o| o.id() == "furniture/page-number")
        else {
            panic!("no page number in {objs:?}")
        };
        assert_eq!(num.text[0].plain_text(), "2 / 5");
        assert_eq!(num.role.as_deref(), Some("label"));
    }

    #[test]
    fn furniture_is_suppressed_on_the_cover_when_flagged() {
        let p = get("design-review").unwrap();
        let objs = furniture_objects(p, 0, 5, true, p.geometry.canvas);
        assert!(
            objs.iter().all(|o| o.id() != "furniture/page-number"),
            "the page number suppresses on the cover"
        );
        assert!(
            objs.iter().any(|o| o.id() == "furniture/footer"),
            "the footer does not"
        );
    }

    fn obj(json: &str) -> Object {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn tier_of_covers_every_object_variant() {
        let cases = [
            (
                r#"{"id":"t","type":"text","text":["hi"]}"#,
                ExportTier::Native,
            ),
            (
                r#"{"id":"s","type":"shape","geo":"rect"}"#,
                ExportTier::Native,
            ),
            (
                r#"{"id":"c","type":"connector","from":{"at":[0,0]},"to":{"at":[1,1]}}"#,
                ExportTier::Native,
            ),
            (
                r#"{"id":"tb","type":"table","rows":[["a","b"]]}"#,
                ExportTier::Native,
            ),
            (
                r#"{"id":"ch","type":"chart","data":{"origin":"stated-by-user"}}"#,
                ExportTier::Grouped,
            ),
            (
                r#"{"id":"d","type":"diagram","nodes":[{"id":"n","label":"n"}]}"#,
                ExportTier::Grouped,
            ),
            (
                r#"{"id":"iv","type":"image","src":"assets/fig.svg"}"#,
                ExportTier::Vector,
            ),
            (
                r#"{"id":"ir","type":"image","src":"assets/shot.png"}"#,
                ExportTier::Raster,
            ),
            (
                r#"{"id":"eq","type":"equation","tex":"x^2","alt":"x^2"}"#,
                ExportTier::Raster, // picture; the reason names the OMML arm
            ),
            (
                r#"{"id":"u","type":"hologram"}"#,
                ExportTier::Raster, // skipped
            ),
        ];
        for (json, want) in cases {
            let (tier, reason) = tier_of(&obj(json));
            assert_eq!(tier, want, "{json}");
            assert!(!reason.is_empty());
        }
        let (tier, reason) = tier_of(&obj(r#"{"id":"u","type":"hologram"}"#));
        assert_eq!(tier, ExportTier::Raster);
        assert!(reason.starts_with("skipped:"), "{reason}");
    }

    #[test]
    fn a_group_takes_its_lowest_childs_tier() {
        let g = obj(r#"{"id":"g","type":"group","objects":[
                 {"id":"t","type":"text","text":["hi"]},
                 {"id":"i","type":"image","src":"a.png"}]}"#);
        let (tier, reason) = tier_of(&g);
        assert_eq!(tier, ExportTier::Raster);
        assert_eq!(reason, "raster pixels at placed size");
        let empty = obj(r#"{"id":"g","type":"group","objects":[]}"#);
        assert_eq!(tier_of(&empty).0, ExportTier::Native);
    }
}
