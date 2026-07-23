//! The `.board.json` scene graph.
//!
//! Four properties are load-bearing and every type here is shaped by them:
//!
//! 1. **Points, only points.** No px, no EMU, no affine matrices. A 16:9 slide
//!    is 960 × 540 pt, origin top-left, y down. One unit kills a class of agent
//!    arithmetic errors that a human-only tool could afford.
//! 2. **Byte-stable serialization.** Struct field order *is* the key order in
//!    the file (serde_json's struct serializer preserves declaration order),
//!    so a semantically identical save is byte-identical and `git status`
//!    stays honest. Do not reorder fields casually — it rewrites every board.
//! 3. **Lenient parsing that never bricks.** Unknown fields are preserved
//!    verbatim in `extra`, and an object whose `type` is unknown — or whose
//!    known type fails to parse — is kept as [`Object::Unknown`] rather than
//!    dropped. A newer board opened by an older daemon round-trips without
//!    losing data.
//! 4. **No churn fields.** No nonces, no `updated` timestamps, no selection or
//!    zoom state. Excalidraw's dirty-on-open is the anti-pattern that makes a
//!    format unmergeable.

use std::collections::BTreeMap;

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

/// The `format` discriminator every board carries.
pub const FORMAT: &str = "chimaera.board";

/// The current `formatVersion`. Bumping this requires a migration in
/// [`crate::migrate`]; readers accept anything ≤ this and preserve anything
/// greater rather than refusing it.
pub const FORMAT_VERSION: u32 = 1;

/// Unknown-key catch-all. `BTreeMap` (not `HashMap`) because the order must be
/// deterministic — it is serialized straight back out.
pub type Extra = BTreeMap<String, Value>;

fn is_false(b: &bool) -> bool {
    !*b
}

/// A whole board: the file at `*.board.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Board {
    pub format: String,
    pub format_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Workspace-relative path to a `.theme.json`. Referenced, never inlined —
    /// the theme is git-tracked in the same repo, so determinism is already
    /// guaranteed and inlining would churn every board diff.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    pub canvas: Canvas,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brief: Option<Brief>,
    pub pages: Vec<Page>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Board {
    /// A blank board at the given preset, with one empty page.
    pub fn new(title: impl Into<String>, canvas: Canvas) -> Self {
        Board {
            format: FORMAT.to_string(),
            format_version: FORMAT_VERSION,
            title: Some(title.into()),
            theme: None,
            canvas,
            brief: None,
            pages: vec![Page::new("page-1")],
            extra: Extra::new(),
        }
    }

    /// Every object on every page, in page then z order, with its page id.
    /// Group children are yielded after their group.
    pub fn objects(&self) -> impl Iterator<Item = (&str, &Object)> {
        self.pages
            .iter()
            .flat_map(|p| p.walk().map(move |o| (p.id.as_str(), o)))
    }
}

/// The canvas: size in points, plus the preset and target that supply the
/// geometry, floors, page furniture, and rules (the four preset axes).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Canvas {
    /// Named geometry preset, e.g. `talk-16x9`. Advisory in v0 — `size` is the
    /// truth — but carried so a later slice can remap all four axes at once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    /// The export target whose floors and refusals apply, e.g. `design-review`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// `[width, height]` in points.
    pub size: [f64; 2],
    /// The board's own ground: a theme token (`@bg`, `@surface`, …) or a
    /// `#rrggbb` literal painted under every page instead of the resolved
    /// theme's ground. Absent — the default — means "the theme's ground",
    /// which is what lets an `auto`-themed board follow the viewer's
    /// light/dark mode. A page's `background.fill` still wins over this: the
    /// more specific statement is the stronger one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Canvas {
    pub fn width(&self) -> f64 {
        self.size[0]
    }
    pub fn height(&self) -> f64 {
        self.size[1]
    }
}

impl Default for Canvas {
    fn default() -> Self {
        Canvas {
            preset: Some("talk-16x9".to_string()),
            target: None,
            size: [960.0, 540.0],
            background: None,
            extra: Extra::new(),
        }
    }
}

/// What the deck as a whole is arguing. Parsed and preserved in slice 0;
/// resolved into layout selection in slice 1.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Brief {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thesis: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minutes: Option<f64>,
    /// The human's own words, verbatim. Never regenerated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asked: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// One page. Z-order is array order within `objects`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub id: String,
    /// What this page is *doing*. Not derivable from its objects — that is
    /// precisely why it is stored. Never drawn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<Intent>,
    /// Named slot layout. Parsed and preserved in slice 0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<Background>,
    pub objects: Vec<Object>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Page {
    pub fn new(id: impl Into<String>) -> Self {
        Page {
            id: id.into(),
            intent: None,
            layout: None,
            background: None,
            objects: Vec::new(),
            notes: None,
            caption: None,
            extra: Extra::new(),
        }
    }

    /// Every object on the page including group children, in z order.
    pub fn walk(&self) -> impl Iterator<Item = &Object> {
        fn rec<'a>(objs: &'a [Object], out: &mut Vec<&'a Object>) {
            for o in objs {
                out.push(o);
                if let Object::Group(g) = o {
                    rec(&g.objects, out);
                }
            }
        }
        let mut out = Vec::new();
        rec(&self.objects, &mut out);
        out.into_iter()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Intent {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Background {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

// ---------------------------------------------------------------------------
// Objects
// ---------------------------------------------------------------------------

/// The five primitives, `chart`, `diagram`, and the preservation fallback.
///
/// Serialized with a `type` discriminator. Deserialization is hand-written
/// rather than `#[serde(tag = "type")]` for one reason that matters: an
/// unrecognized *or* malformed object must survive round-trip as
/// [`Object::Unknown`] instead of failing the whole parse. A board that
/// half-loads is worse than useless; a board that loses an object silently is
/// worse still.
// A board holds at most dozens of objects, so paying the largest variant's
// size per element is nothing; boxing the big variants would put a deref in
// every match arm in the workspace for no measurable win.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum Object {
    Text(TextObject),
    Shape(ShapeObject),
    Connector(ConnectorObject),
    Image(ImageObject),
    Group(GroupObject),
    Table(TableObject),
    Chart(ChartObject),
    Diagram(DiagramObject),
    Equation(EquationObject),
    Icon(IconObject),
    PanelLabel(PanelLabelObject),
    Scalebar(ScalebarObject),
    SigBracket(SigBracketObject),
    Legend(LegendObject),
    Colorbar(ColorbarObject),
    Callout(CalloutObject),
    Inset(InsetObject),
    /// Preserved verbatim, skipped in render. Carries the reason so `describe`
    /// and the repair banner can say *why* rather than just dropping it.
    Unknown(UnknownObject),
}

#[derive(Debug, Clone)]
pub struct UnknownObject {
    pub id: String,
    pub kind: String,
    pub raw: Value,
    /// `None` when the type is simply not known to this build; `Some` when a
    /// known type failed to parse.
    pub error: Option<String>,
}

impl Object {
    pub fn id(&self) -> &str {
        match self {
            Object::Text(o) => &o.id,
            Object::Shape(o) => &o.id,
            Object::Connector(o) => &o.id,
            Object::Image(o) => &o.id,
            Object::Group(o) => &o.id,
            Object::Table(o) => &o.id,
            Object::Chart(o) => &o.id,
            Object::Diagram(o) => &o.id,
            Object::Equation(o) => &o.id,
            Object::Icon(o) => &o.id,
            Object::PanelLabel(o) => &o.id,
            Object::Scalebar(o) => &o.id,
            Object::SigBracket(o) => &o.id,
            Object::Legend(o) => &o.id,
            Object::Colorbar(o) => &o.id,
            Object::Callout(o) => &o.id,
            Object::Inset(o) => &o.id,
            Object::Unknown(o) => &o.id,
        }
    }

    /// The `type` string as it appears in the file.
    pub fn kind(&self) -> &str {
        match self {
            Object::Text(_) => "text",
            Object::Shape(_) => "shape",
            Object::Connector(_) => "connector",
            Object::Image(_) => "image",
            Object::Group(_) => "group",
            Object::Table(_) => "table",
            Object::Chart(_) => "chart",
            Object::Diagram(_) => "diagram",
            Object::Equation(_) => "equation",
            Object::Icon(_) => "icon",
            Object::PanelLabel(_) => "panelLabel",
            Object::Scalebar(_) => "scalebar",
            Object::SigBracket(_) => "sigBracket",
            Object::Legend(_) => "legend",
            Object::Colorbar(_) => "colorbar",
            Object::Callout(_) => "callout",
            Object::Inset(_) => "inset",
            Object::Unknown(o) => &o.kind,
        }
    }

    /// The object's page-space box, where it has one. A connector's box is
    /// derived from its endpoints at render time, so it has none here — and
    /// the same holds for a `sigBracket` (endpoint-bound) and a `scalebar`
    /// (its extent derives from `lengthPt` and the theme at expansion).
    pub fn frame(&self) -> Option<Frame> {
        let (at, size) = match self {
            Object::Text(o) => (o.at, o.size),
            Object::Shape(o) => (o.at, o.size),
            Object::Image(o) => (o.at, o.size),
            Object::Group(o) => (o.at, o.size),
            Object::Table(o) => (o.at, o.size),
            Object::Chart(o) => (o.at, o.size),
            Object::Diagram(o) => (o.at, o.size),
            Object::Equation(o) => (o.at, o.size),
            Object::Icon(o) => (o.at, o.size),
            Object::PanelLabel(o) => (o.at, o.size),
            Object::Legend(o) => (o.at, o.size),
            Object::Colorbar(o) => (o.at, o.size),
            Object::Callout(o) => (o.at, o.size),
            Object::Inset(o) => (o.at, o.size),
            Object::Scalebar(o) => (o.at, None),
            Object::Connector(_) | Object::SigBracket(_) | Object::Unknown(_) => (None, None),
        };
        match (at, size) {
            (Some(at), Some(size)) => Some(Frame {
                x: at[0],
                y: at[1],
                w: size[0],
                h: size[1],
            }),
            _ => None,
        }
    }

    pub fn set_at(&mut self, at: [f64; 2]) {
        match self {
            Object::Text(o) => o.at = Some(at),
            Object::Shape(o) => o.at = Some(at),
            Object::Image(o) => o.at = Some(at),
            Object::Group(o) => o.at = Some(at),
            Object::Table(o) => o.at = Some(at),
            Object::Chart(o) => o.at = Some(at),
            Object::Diagram(o) => o.at = Some(at),
            Object::Equation(o) => o.at = Some(at),
            Object::Icon(o) => o.at = Some(at),
            Object::PanelLabel(o) => o.at = Some(at),
            Object::Scalebar(o) => o.at = Some(at),
            Object::Legend(o) => o.at = Some(at),
            Object::Colorbar(o) => o.at = Some(at),
            Object::Callout(o) => o.at = Some(at),
            Object::Inset(o) => o.at = Some(at),
            Object::Connector(_) | Object::SigBracket(_) | Object::Unknown(_) => {}
        }
    }

    pub fn set_size(&mut self, size: [f64; 2]) {
        match self {
            Object::Text(o) => o.size = Some(size),
            Object::Shape(o) => o.size = Some(size),
            Object::Image(o) => o.size = Some(size),
            Object::Group(o) => o.size = Some(size),
            Object::Table(o) => o.size = Some(size),
            Object::Chart(o) => o.size = Some(size),
            Object::Diagram(o) => o.size = Some(size),
            Object::Equation(o) => o.size = Some(size),
            Object::Icon(o) => o.size = Some(size),
            Object::PanelLabel(o) => o.size = Some(size),
            Object::Legend(o) => o.size = Some(size),
            Object::Colorbar(o) => o.size = Some(size),
            Object::Callout(o) => o.size = Some(size),
            Object::Inset(o) => o.size = Some(size),
            // A scalebar's extent is `lengthPt`, never a stored box.
            Object::Connector(_)
            | Object::Scalebar(_)
            | Object::SigBracket(_)
            | Object::Unknown(_) => {}
        }
    }

    /// The declared slot, where the object has one. Annotation composites sit
    /// *over* slot content, so they never occupy a slot themselves.
    pub fn slot(&self) -> Option<&str> {
        match self {
            Object::Text(o) => o.slot.as_deref(),
            Object::Shape(o) => o.slot.as_deref(),
            Object::Image(o) => o.slot.as_deref(),
            Object::Table(o) => o.slot.as_deref(),
            Object::Chart(o) => o.slot.as_deref(),
            Object::Diagram(o) => o.slot.as_deref(),
            Object::Equation(o) => o.slot.as_deref(),
            Object::Icon(o) => o.slot.as_deref(),
            Object::Group(_)
            | Object::Connector(_)
            | Object::PanelLabel(_)
            | Object::Scalebar(_)
            | Object::SigBracket(_)
            | Object::Legend(_)
            | Object::Colorbar(_)
            | Object::Callout(_)
            | Object::Inset(_)
            | Object::Unknown(_) => None,
        }
    }
}

/// A page-space rectangle in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Frame {
    pub fn right(&self) -> f64 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
    pub fn cx(&self) -> f64 {
        self.x + self.w / 2.0
    }
    pub fn cy(&self) -> f64 {
        self.y + self.h / 2.0
    }
}

impl Serialize for Object {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // Each variant owns its own key order; `type` is written by the
        // variant struct itself (a `kind` field pinned to the discriminator)
        // so declaration order — and therefore byte order — stays visible in
        // one place per type.
        match self {
            Object::Text(o) => o.serialize(s),
            Object::Shape(o) => o.serialize(s),
            Object::Connector(o) => o.serialize(s),
            Object::Image(o) => o.serialize(s),
            Object::Group(o) => o.serialize(s),
            Object::Table(o) => o.serialize(s),
            Object::Chart(o) => o.serialize(s),
            Object::Diagram(o) => o.serialize(s),
            Object::Equation(o) => o.serialize(s),
            Object::Icon(o) => o.serialize(s),
            Object::PanelLabel(o) => o.serialize(s),
            Object::Scalebar(o) => o.serialize(s),
            Object::SigBracket(o) => o.serialize(s),
            Object::Legend(o) => o.serialize(s),
            Object::Colorbar(o) => o.serialize(s),
            Object::Callout(o) => o.serialize(s),
            Object::Inset(o) => o.serialize(s),
            Object::Unknown(o) => o.raw.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for Object {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = Value::deserialize(d)?;
        let kind = raw
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let id = raw
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        // A known type that fails to parse is preserved, not fatal. The board
        // still opens; `describe` and the pane report the object as
        // unrenderable with its reason.
        macro_rules! try_variant {
            ($ty:ty, $wrap:expr) => {
                match serde_json::from_value::<$ty>(raw.clone()) {
                    Ok(v) => return Ok($wrap(v)),
                    Err(e) => {
                        return Ok(Object::Unknown(UnknownObject {
                            id,
                            kind,
                            raw,
                            error: Some(e.to_string()),
                        }))
                    }
                }
            };
        }

        match kind.as_str() {
            "text" => try_variant!(TextObject, Object::Text),
            "shape" => try_variant!(ShapeObject, Object::Shape),
            "connector" => try_variant!(ConnectorObject, Object::Connector),
            "image" => try_variant!(ImageObject, Object::Image),
            "group" => try_variant!(GroupObject, Object::Group),
            "table" => try_variant!(TableObject, Object::Table),
            "chart" => try_variant!(ChartObject, Object::Chart),
            "diagram" => try_variant!(DiagramObject, Object::Diagram),
            "equation" => try_variant!(EquationObject, Object::Equation),
            "icon" => try_variant!(IconObject, Object::Icon),
            "panelLabel" => try_variant!(PanelLabelObject, Object::PanelLabel),
            "scalebar" => try_variant!(ScalebarObject, Object::Scalebar),
            "sigBracket" => try_variant!(SigBracketObject, Object::SigBracket),
            "legend" => try_variant!(LegendObject, Object::Legend),
            "colorbar" => try_variant!(ColorbarObject, Object::Colorbar),
            "callout" => try_variant!(CalloutObject, Object::Callout),
            "inset" => try_variant!(InsetObject, Object::Inset),
            _ => Ok(Object::Unknown(UnknownObject {
                id,
                kind,
                raw,
                error: None,
            })),
        }
    }
}

/// Emitted by every object struct so the `type` key round-trips in the right
/// position without each variant hand-rolling `Serialize`.
macro_rules! kind_field {
    ($name:ident, $lit:literal) => {
        #[derive(Debug, Clone, Copy, Default, PartialEq)]
        pub struct $name;

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str($lit)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                if s == $lit {
                    Ok($name)
                } else {
                    Err(de::Error::custom(format!(
                        "expected type {:?}, found {:?}",
                        $lit, s
                    )))
                }
            }
        }
    };
}

kind_field!(TextKind, "text");
kind_field!(ShapeKind, "shape");
kind_field!(ConnectorKind, "connector");
kind_field!(ImageKind, "image");
kind_field!(GroupKind, "group");
kind_field!(TableKind, "table");
kind_field!(ChartKind, "chart");
kind_field!(DiagramKind, "diagram");
kind_field!(EquationKind, "equation");
kind_field!(IconKind, "icon");
kind_field!(PanelLabelKind, "panelLabel");
kind_field!(ScalebarKind, "scalebar");
kind_field!(SigBracketKind, "sigBracket");
kind_field!(LegendKind, "legend");
kind_field!(ColorbarKind, "colorbar");
kind_field!(CalloutKind, "callout");
kind_field!(InsetKind, "inset");

/// A box of paragraphs. The only object that owns glyph layout.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextObject {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: TextKind,
    /// Resolves family/size/weight/color from the theme's type scale. Sizes
    /// are *derived* — there is deliberately no `fontSize` field for an agent
    /// to reach for; per-run overrides exist but carry a lint budget.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<[f64; 2]>,
    pub text: Vec<Paragraph>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub align: Option<Align>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valign: Option<VAlign>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<f64>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VAlign {
    Top,
    Middle,
    Bottom,
}

/// A paragraph. Sugar: a paragraph that is one unstyled run may be written as
/// a bare string, which [`crate::normalize`] expands. Markdown is authoring
/// sugar on the skill side only — two stored styling representations create
/// normalization ambiguity and diff churn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Paragraph {
    Plain(String),
    Rich(RichParagraph),
}

impl Paragraph {
    /// The paragraph's text with styling flattened away.
    pub fn plain_text(&self) -> String {
        match self {
            Paragraph::Plain(s) => s.clone(),
            Paragraph::Rich(p) => p.runs.iter().map(|r| r.t.as_str()).collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RichParagraph {
    pub runs: Vec<Run>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub align: Option<Align>,
    /// Exact pt (`spcPts`), never a percentage — percentages resolve
    /// differently across PowerPoint, Keynote and LibreOffice.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space_before: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space_after: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bullet: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A styled span. Runs carry **only overrides**; everything unset inherits
/// from the object's `role` through the theme.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub t: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub i: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub u: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Plain pt in the file; the PPTX writer multiplies by 100.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Run {
    pub fn plain(t: impl Into<String>) -> Self {
        Run {
            t: t.into(),
            b: None,
            i: None,
            u: None,
            color: None,
            size: None,
            family: None,
            link: None,
            extra: Extra::new(),
        }
    }
}

/// A geometry — named preset **or** arbitrary path — with fill/stroke and
/// optional bound child text. Absorbs `line`: an unbound straight line is a
/// shape with `geo: "line"`, because a connector's irreducible property is
/// *binding*, not thinness.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShapeObject {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: ShapeKind,
    /// A named preset (`rect`, `roundRect`, `ellipse`, `line`, …) or `"path"`.
    /// Board never *infers* a preset from a path — inference produces
    /// near-misses that ship as visibly wrong corner radii.
    pub geo: String,
    /// SVG-syntax path data, required when `geo == "path"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub d: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,
    /// 0..=1. There is no Venn overlap, highlight band, or legend swatch
    /// without it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill_opacity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stroke: Option<Stroke>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radius: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text: Vec<Paragraph>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub align: Option<Align>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<f64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub flip_h: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub flip_v: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stroke {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dash: Option<Vec<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cap: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub join: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A stroked two-endpoint geometry binding to other objects by box edge.
///
/// Carries bound text exactly as `shape` does — an edge label is a run on the
/// connector at a fraction along its path, not a free-floating `text` with a
/// manual `at`. Without that, every diagram edge label detaches the moment a
/// node moves.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorObject {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: ConnectorKind,
    /// `straight` or `bent`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geo: Option<String>,
    pub from: EndPoint,
    pub to: EndPoint,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stroke: Option<Stroke>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_end: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail_end: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text: Vec<Paragraph>,
    /// Where bound text sits along the path, 0..=1. Defaults to the midpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_at: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A connector endpoint: bound to another object's box edge, or a free point.
///
/// `side` names an edge of the target's bounding box — never an OOXML
/// `a:cxnLst` index, whose numbering is geometry-specific (a rect has four
/// connection sites, a hexagon six, and "left" has no stable index across
/// them).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndPoint {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side: Option<Side>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Top,
    Right,
    Bottom,
    Left,
    Center,
}

/// Placed pixels or SVG. Absorbs the former `plot` type: `provenance`,
/// `pixelSize` and `frame` are fields here, so the stale badge, the regenerate
/// action, the panel lint and the `p:pic` writer are one code path — and a
/// pasted screenshot can carry provenance too.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageObject {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: ImageKind,
    /// Workspace-relative path.
    pub src: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<[f64; 2]>,
    /// Fractional crop `[l, t, r, b]`, matching PPTX `a:srcRect`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src_rect: Option<[f64; 4]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    /// Natural pixel size, for effective-DPI lint at placed size.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pixel_size: Option<[f64; 2]>,
    /// Recolors a monochrome SVG to a theme token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<f64>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub regen: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A z-order and selection envelope. **Not** a coordinate system: children
/// carry page-absolute `at`/`size` exactly like ungrouped objects, so ids,
/// `describe`, journal move events, off-canvas lint and per-object merge stay
/// uniform whether or not an object is grouped.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupObject {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: GroupKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<[f64; 2]>,
    pub objects: Vec<Object>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

// ---------------------------------------------------------------------------
// Table
// ---------------------------------------------------------------------------

/// A grid of cells — the tier-1 element that lands as a native, editable
/// `a:tbl` at every export target. A cell is one [`Paragraph`]: the same text
/// model text objects use (bare-string sugar, rich runs, run-level overrides),
/// because a second cell text model would split canonicalization, measurement
/// and the PPTX run writer in three places.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableObject {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: TableKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<[f64; 2]>,
    /// Relative column widths — pt widths keep their ratio and fill the box.
    /// Absent means an equal split; see [`TableObject::column_widths`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub columns: Option<Vec<f64>>,
    /// First row styled as a header: surface fill, bold runs, `firstRow` on
    /// the exported `a:tblPr`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub header: bool,
    /// One paragraph per cell, row-major. Ragged rows are legal; missing
    /// cells draw empty.
    pub rows: Vec<Vec<Paragraph>>,
    /// Resolves cell family/size/weight/color from the theme's type scale,
    /// exactly as bound shape text does. Defaults to `body`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl TableObject {
    /// The grid's column count: the widest row, or `columns` when it names
    /// more.
    pub fn column_count(&self) -> usize {
        let widest = self.rows.iter().map(Vec::len).max().unwrap_or(0);
        widest.max(self.columns.as_ref().map_or(0, Vec::len))
    }

    /// Column widths in points across `total`. Stated `columns` are relative
    /// weights; the fallback to an equal split — when `columns` is absent,
    /// does not match the grid, or carries an unusable weight — is deliberate:
    /// a half-stated grid must still resolve deterministically (lint reports
    /// the mismatch).
    pub fn column_widths(&self, total: f64) -> Vec<f64> {
        let n = self.column_count();
        if n == 0 {
            return Vec::new();
        }
        if let Some(cols) = &self.columns {
            if cols.len() == n && cols.iter().all(|w| w.is_finite() && *w > 0.0) {
                let sum: f64 = cols.iter().sum();
                return cols.iter().map(|w| w / sum * total).collect();
            }
        }
        vec![total / n as f64; n]
    }

    /// A header cell's paragraph: every run bold unless it states otherwise.
    /// Transient — computed at render/export, never written back.
    pub fn header_cell(cell: &Paragraph) -> Paragraph {
        match cell {
            Paragraph::Plain(s) => {
                let mut run = Run::plain(s.clone());
                run.b = Some(true);
                Paragraph::Rich(RichParagraph {
                    runs: vec![run],
                    align: None,
                    space_before: None,
                    space_after: None,
                    bullet: None,
                    extra: Extra::new(),
                })
            }
            Paragraph::Rich(rich) => {
                let mut rich = rich.clone();
                for r in &mut rich.runs {
                    if r.b.is_none() {
                        r.b = Some(true);
                    }
                }
                Paragraph::Rich(rich)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Equation
// ---------------------------------------------------------------------------

/// LaTeX math typeset to a picture (docs/board-plan.md: the one named C6
/// exception — an equation is *notation*, not prose). `alt` is **required**:
/// the carve-out only holds because the LaTeX source always travels with the
/// picture, so an equation without `alt` fails parse into [`Object::Unknown`]
/// exactly like any other malformed known type. The native OMML export arm is
/// deliberately not built in v1; every target gets the picture.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EquationObject {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: EquationKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<[f64; 2]>,
    /// The LaTeX source (math mode, display style; no surrounding `$`).
    pub tex: String,
    /// Typeset size in points, independent of the placed `size` (which only
    /// bounds the fitted picture). Defaults to the theme's body role size.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub em_size: Option<f64>,
    /// Required alt text carrying the LaTeX source — the C6 condition.
    pub alt: String,
    #[serde(flatten)]
    pub extra: Extra,
}

// ---------------------------------------------------------------------------
// Icon
// ---------------------------------------------------------------------------

/// A named glyph from the bundled Tabler outline set ([`crate::icons`]) —
/// close kin to a `shape` with `geo:"path"`, but the drawing lives in the
/// engine keyed by `name`, not inline in the board. The renderer fits its
/// 24×24 paths into the object box (aspect-preserving, centered) and strokes
/// them with `color`; recoloring is just a different token, and a resize is
/// free because the geometry is spec-only. An unknown `name` renders a visible
/// placeholder and lints — never a silent blank. Icons compose with imported
/// SVG/PNG and shapes into real figures, all editable after a PPTX export.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IconObject {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: IconKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<[f64; 2]>,
    /// The Tabler outline icon name, e.g. `"flask"`. Find one with
    /// `chimaera board icons <query>`.
    pub name: String,
    /// A theme `@token` or literal for the stroke/fill; defaults to `@fg`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Stroke weight in the 24-unit viewBox space (Tabler's own scale, so
    /// `2` is the designed look); scales with the placed size. Defaults to
    /// [`crate::icons::DEFAULT_STROKE_WIDTH`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stroke_width: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

// ---------------------------------------------------------------------------
// Anchors
// ---------------------------------------------------------------------------

/// Positional binding. `at` and `object`+`rel` resolve at render via
/// [`crate::slots::resolve_page_frames`]; `px`/`data` need the target's
/// pixel/data frame and resolve with the figures pack.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Anchor {
    /// Absolute page points — the default spelling of a position.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
    /// The object this one is positioned relative to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    /// e.g. `above`, `below`, `left-of`, `right-of`, `inside-top-left`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<[f64; 2]>,
    /// Pixel coordinates within the target image's natural pixel space.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub px: Option<[f64; 2]>,
    /// Data coordinates within the target chart's scales.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<[f64; 2]>,
    #[serde(flatten)]
    pub extra: Extra,
}

// ---------------------------------------------------------------------------
// Chart
// ---------------------------------------------------------------------------

/// A native chart: marks over a plot-ready table, with zero transforms.
///
/// Rejecting Vega-Lite's `transform` block is the inclusion principle
/// expressed as a schema, not scope triage — nineteen transform types is
/// precisely where "we are writing a plotting library" begins. Faceting is
/// likewise absent: small multiples are N chart objects placed by the layout
/// engine, which is only possible because Board *is* the layout engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartObject {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: ChartKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub data: ChartData,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x: Option<Channel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y: Option<Channel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<Channel>,
    /// Omittable — `normalize()` infers a mark from the channel types. A pure
    /// function of declared types, so determinism holds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marks: Vec<Mark>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub axes: Option<Axes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Where the numbers came from — a required field, not a paragraph.
///
/// The skill says *"a confident chart of numbers you inferred is the one way
/// this feature does harm"* and then left it to prose, while a merely *stale*
/// digest got a badge, a lint, an export block and a describe line. That
/// asymmetry was backwards. `origin` surfaces in `describe`, lint, and the
/// chat card's data disclosure — never painted on the canvas itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartData {
    pub origin: DataOrigin,
    /// Inline rows. Capped at 500 plotted points and 32 KiB serialized — an
    /// inline 50k series is an unwritable file and it poisons the id-anchored
    /// sparse-`Edit` contract.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<Value>,
    /// Slice 4b: a file the chart binds to, with digest staleness.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<u64>,
    /// Free text: where the command/file came from, for the card's chip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// How the plotted values were produced — command/script, method, seed —
    /// so a later session can answer "how did you calculate this" from the
    /// file alone. Covers computed-from-files cases (quartiles, aggregations)
    /// where `source` cannot bind the derived rows. Clamped to 2 KiB in
    /// `normalize()`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<String>,
    /// Workspace-relative paths the computation read. The trace's evidence:
    /// `source` remains the first-class binding for rows plotted verbatim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inputs: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DataOrigin {
    /// Read from a file in the workspace.
    File,
    /// Captured from a command the agent ran.
    Command,
    /// The human supplied these numbers.
    StatedByUser,
    /// The agent produced them without running anything. The one that needs
    /// the loudest chip.
    DerivedByAgent,
}

impl DataOrigin {
    pub fn label(&self) -> &'static str {
        match self {
            DataOrigin::File => "from file",
            DataOrigin::Command => "from command",
            DataOrigin::StatedByUser => "stated by user",
            DataOrigin::DerivedByAgent => "derived by agent",
        }
    }
}

/// An encoding channel. **Types are declared, not inferred** over CSV: that is
/// where an integer-coded category silently lands on a linear axis and a date
/// parses as a number — plausible-looking, wrong, and invisible. `normalize()`
/// may infer from *inline JSON only*, which is typed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub field: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<ChannelType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<ScaleKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticks: Option<Vec<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nice: Option<bool>,
    /// `"-y"` sorts descending by the quantitative channel — the most common
    /// single request about a bar chart, and a transform if it isn't a scale
    /// property.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<TickFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub palette: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelType {
    Quantitative,
    Ordinal,
    Nominal,
    Temporal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScaleKind {
    Linear,
    Log,
    Ordinal,
    Temporal,
}

/// Tick formatting is specified, not left to `format!`. Unspecified, this
/// ships `0.30000000000000004` on an axis in week one.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TickFormat {
    /// Significant figures. The default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sig: Option<u32>,
    /// Fixed decimal places, when the axis wants alignment over brevity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decimals: Option<u32>,
    /// SI prefixes (`k`, `M`, `G`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<bool>,
    /// Thousands separator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sep: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// One layer. Marks may carry their own `fields` override and a `from` naming
/// another dataset — without it a `text` mark cannot label only the nine genes
/// of a volcano or only the end of one series, and since `transform` is
/// rightly rejected there is no `filter` to fall back on. Binding is not
/// computing: the subset is a table the agent supplied.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mark {
    pub mark: MarkKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<Stack>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
    /// Errorbar cap width in points.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cap_pt: Option<f64>,
    /// `none` or `post` — `post` is a Kaplan–Meier step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stroke: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dash: Option<Vec<f64>>,
    /// Constant position for a `rule`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x: Option<f64>,
    /// Per-mark field overrides, e.g. `{"text": "label", "err": "stderr"}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, String>,
    /// Rows for this mark only, when it labels a subset.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Mark {
    pub fn new(mark: MarkKind) -> Self {
        Mark {
            mark,
            stack: None,
            width: None,
            size: None,
            cap_pt: None,
            step: None,
            stroke: None,
            fill: None,
            opacity: None,
            dash: None,
            y: None,
            x: None,
            fields: BTreeMap::new(),
            values: Vec::new(),
            label: None,
            extra: Extra::new(),
        }
    }
}

/// The mark set. A strict SUBSET of the full vocabulary, never a
/// differently-spelled simplification of it — missing capability must be
/// *absent*, so nothing written in week one needs migrating.
///
/// `bar` with an `x2`/`y2` field in `fields` is an interval — v to v2 rather
/// than zero to v — which is what makes a timeline or trace-span expressible.
/// `area` takes explicit `y`/`y2` (a CI ribbon) or `stack: "stack"`. `rect`
/// is a heatmap cell over a matrix. `tick` is a degenerate rule for strips
/// and rugs. `box` is sugar, not a real mark: it expands at build time into
/// interval + whisker + median marks from a five-number summary the rows
/// supply — Board never computes quartiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MarkKind {
    Bar,
    Line,
    Point,
    Area,
    Rect,
    Tick,
    Rule,
    Errorbar,
    Text,
    Box,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stack {
    None,
    Stack,
    Group,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Axes {
    /// Which spines to draw, e.g. `["left", "bottom"]`. Minimal chrome is the
    /// default; top/right off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spines: Option<Vec<String>>,
    /// `none`, `x`, `y`, or `both`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grid: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

// ---------------------------------------------------------------------------
// Diagram
// ---------------------------------------------------------------------------

/// A composite: nodes + edges + lanes under a deterministic layered layout.
///
/// The file stores the *intent* — which nodes exist, what connects to what,
/// which lane each belongs to — and [`crate::diagram::expand`] computes the
/// geometry at render time. The expansion is never written back: storing it
/// would be a second representation, and spec-only is what makes retheme and
/// resize free.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagramObject {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: DiagramKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<[f64; 2]>,
    /// Layer flow: `down` (the default) or `right`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<DiagramDirection>,
    pub nodes: Vec<DiagramNode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<DiagramEdge>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lanes: Vec<DiagramLane>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl DiagramObject {
    pub fn direction(&self) -> DiagramDirection {
        self.direction.unwrap_or(DiagramDirection::Down)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagramDirection {
    #[default]
    Down,
    Right,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagramNode {
    pub id: String,
    pub label: String,
    /// Defaults to `roundRect` when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shape: Option<NodeShape>,
    /// A bundled icon name ([`crate::icons`]) drawn leading the label, sized to
    /// the node height — the cheap fix for a "too boring" flow. Layout widens
    /// the node for it; an unknown name renders a placeholder and lints.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Pinned top-left position in page points. Absent (the norm) means the
    /// layered layout places the node; present means the layout honors the
    /// pin — a human dragged this node and it stays where its diagram flows
    /// around it. Size stays layout-derived either way (it follows the label).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,
    /// Names a lane this node belongs to; the lane's container rect is drawn
    /// behind its members.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NodeShape {
    Rect,
    RoundRect,
    Ellipse,
    Diamond,
}

impl NodeShape {
    /// The shape geometry this node expands to.
    pub fn geo(&self) -> &'static str {
        match self {
            NodeShape::Rect => "rect",
            NodeShape::RoundRect => "roundRect",
            NodeShape::Ellipse => "ellipse",
            NodeShape::Diamond => "diamond",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagramEdge {
    pub from: String,
    pub to: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<EdgeStyle>,
    /// Arrowhead at the destination; absent means `true`. An `Option` rather
    /// than a defaulted `bool` so an explicit `"arrow": true` round-trips
    /// byte-identically instead of being canonicalized away.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arrow: Option<bool>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EdgeStyle {
    Solid,
    Dashed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagramLane {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

// ---------------------------------------------------------------------------
// Annotation composites
// ---------------------------------------------------------------------------
//
// The figures pack's vocabulary: each of these sits *above* an already-placed
// panel — which is exactly what no upstream plotting library can do for
// itself. Like `diagram`, the file stores intent and
// [`crate::composites`] expands it into primitives at render; the expansion is
// never written back, so retheme and resize stay free. Every composite works
// identically over an imported panel and over a native chart.

/// A panel letter — `a`, `b`, `c` over a figure's panels.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelLabelObject {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: PanelLabelKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
    /// Optional; the expansion measures the letter when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<[f64; 2]>,
    /// Typically `{ object: <panel>, rel: "inside-top-left" }`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
    /// `nature` (bold lowercase a/b/c, the default) or `capital` (A/B/C) —
    /// a venue retarget flips the case without touching the stored letter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<PanelLabelStyle>,
    pub label: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PanelLabelStyle {
    #[default]
    Nature,
    Capital,
}

/// A drawn length with its caption — `100 µm` under a micrograph. `at` is
/// the left end of the bar; `lengthPt` is the drawn length in page points.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScalebarObject {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: ScalebarKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
    pub length_pt: f64,
    /// `"100 µm"` — Board draws the stated caption, it never converts units.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Bar weight and color; width defaults to 2 pt, color to `@fg`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stroke: Option<Stroke>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A significance bracket spanning two objects' top edges, with its label —
/// `**` or `p = 0.03`. Board draws the stated label; Board never tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SigBracketObject {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: SigBracketKind,
    /// `{ object, side? }` — the bracket's endpoints bind like a connector's,
    /// so the bracket survives any move of what it spans.
    pub from: EndPoint,
    pub to: EndPoint,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// How far the crossbar rises above the taller target's top edge, in
    /// points. Defaults to 12.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drop_pt: Option<f64>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A swatch/line/point key. The plan prefers direct labels — lint says so at
/// ≤3 entries — but a legend stays expressible for the panels that need one.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegendObject {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: LegendKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
    /// Optional; the expansion measures the widest label when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<[f64; 2]>,
    pub entries: Vec<LegendEntry>,
    /// Defaults to 1 — a vertical stack.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub columns: Option<u32>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegendEntry {
    pub label: String,
    /// `@cat1` or a literal; defaults to the theme's categorical ramp in
    /// entry order, which is what the chart itself would have used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<LegendMarker>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LegendMarker {
    #[default]
    Swatch,
    Line,
    Point,
}

/// A continuous color scale strip: a bundled colormap sampled across `domain`,
/// with end tick labels. Vertical or horizontal by the box's aspect.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColorbarObject {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: ColorbarKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<[f64; 2]>,
    /// A bundled name ([`crate::colormap::NAMES`]) — never a literal ramp.
    pub colormap: String,
    /// `[lo, hi]` in data units; the ends label through the chart's own tick
    /// formatter so a colorbar and an axis can never disagree on rounding.
    pub domain: [f64; 2],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The roundRect-with-a-tail the demo deck hand-builds, as one object:
/// a `@surface`/`@accent1` box with bound text and a connector to its target.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalloutObject {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: CalloutKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text: Vec<Paragraph>,
    /// `{ object, side? }` — where the tail points.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail: Option<EndPoint>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A magnified crop of an image: the same `src` re-placed with a computed
/// `srcRect`, plus a border at the inset and a dashed region mark on the
/// target. Needs the target's `pixelSize` — a missing one is reported, never
/// guessed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InsetObject {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: InsetKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<[f64; 2]>,
    pub of: InsetSource,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InsetSource {
    /// The id of an `image` on the same page.
    pub object: String,
    /// `[x, y, w, h]` in the target's natural pixel space.
    pub px: [f64; 4],
    #[serde(flatten)]
    pub extra: Extra,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_object_type_is_preserved_not_dropped() {
        let raw = r#"{"id":"x","type":"hologram","fancy":{"a":1}}"#;
        let obj: Object = serde_json::from_str(raw).unwrap();
        match &obj {
            Object::Unknown(u) => {
                assert_eq!(u.kind, "hologram");
                assert_eq!(u.id, "x");
                assert!(u.error.is_none(), "unknown type is not an error");
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
        // Round-trips verbatim.
        let back = serde_json::to_string(&obj).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&back).unwrap(),
            serde_json::from_str::<Value>(raw).unwrap()
        );
    }

    #[test]
    fn malformed_known_type_is_preserved_with_a_reason() {
        // `text` requires `text`; this one has none.
        let raw = r#"{"id":"t1","type":"text","role":"title"}"#;
        let obj: Object = serde_json::from_str(raw).unwrap();
        match &obj {
            Object::Unknown(u) => {
                assert_eq!(u.kind, "text");
                assert!(u.error.is_some(), "a malformed known type carries a reason");
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn unknown_fields_survive_round_trip() {
        let raw = r#"{"id":"t1","type":"text","text":["hi"],"futureField":42}"#;
        let obj: Object = serde_json::from_str(raw).unwrap();
        let Object::Text(t) = &obj else {
            panic!("expected text, got {obj:?}")
        };
        assert_eq!(t.extra.get("futureField").unwrap(), &Value::from(42));
        let back = serde_json::to_value(&obj).unwrap();
        assert_eq!(back.get("futureField").unwrap(), &Value::from(42));
    }

    #[test]
    fn bare_string_paragraph_is_sugar() {
        let p: Paragraph = serde_json::from_str(r#""hello""#).unwrap();
        assert_eq!(p.plain_text(), "hello");
        let p: Paragraph = serde_json::from_str(r#"{"runs":[{"t":"a"},{"t":"b"}]}"#).unwrap();
        assert_eq!(p.plain_text(), "ab");
    }

    #[test]
    fn a_table_cell_is_the_text_model_and_unknown_fields_survive() {
        let raw = r#"{"id":"tb","type":"table","at":[0,0],"size":[300,100],
            "header":true,
            "rows":[["Fixture","ms"],["large.json",{"runs":[{"t":"244","b":true}]}]],
            "futureKnob":7}"#;
        let obj: Object = serde_json::from_str(raw).unwrap();
        let Object::Table(t) = &obj else {
            panic!("expected table, got {obj:?}")
        };
        assert!(t.header);
        assert_eq!(t.rows[0][0].plain_text(), "Fixture");
        assert_eq!(t.rows[1][1].plain_text(), "244");
        assert_eq!(t.column_count(), 2);
        assert_eq!(t.extra.get("futureKnob").unwrap(), &Value::from(7));
        let back = serde_json::to_value(&obj).unwrap();
        assert_eq!(back.get("futureKnob").unwrap(), &Value::from(7));
    }

    #[test]
    fn table_column_widths_are_relative_with_an_equal_fallback() {
        let t: TableObject = serde_json::from_str(
            r#"{"id":"tb","type":"table","columns":[2,1,1],
                "rows":[["a","b","c"]]}"#,
        )
        .unwrap();
        // Weights 2:1:1 over 400 pt — pt widths keep their ratio.
        assert_eq!(t.column_widths(400.0), vec![200.0, 100.0, 100.0]);
        // A mismatched `columns` falls back to the equal split.
        let ragged: TableObject = serde_json::from_str(
            r#"{"id":"tb","type":"table","columns":[2,1],
                "rows":[["a","b","c"]]}"#,
        )
        .unwrap();
        assert_eq!(ragged.column_widths(300.0), vec![100.0, 100.0, 100.0]);
    }

    #[test]
    fn a_header_cell_bolds_runs_that_do_not_state_otherwise() {
        let bolded = TableObject::header_cell(&Paragraph::Plain("hi".into()));
        let Paragraph::Rich(rich) = &bolded else {
            panic!("expected rich, got {bolded:?}")
        };
        assert_eq!(rich.runs[0].b, Some(true));
        // An explicit b:false survives — the author's word wins.
        let styled: Paragraph =
            serde_json::from_str(r#"{"runs":[{"t":"a","b":false},{"t":"b"}]}"#).unwrap();
        let Paragraph::Rich(rich) = TableObject::header_cell(&styled) else {
            panic!()
        };
        assert_eq!(rich.runs[0].b, Some(false));
        assert_eq!(rich.runs[1].b, Some(true));
    }

    #[test]
    fn an_equation_round_trips_byte_stably_with_canonical_key_order() {
        let raw = r#"{"id":"eq1","type":"equation","at":[80,80],"size":[240,80],"tex":"E = mc^2","emSize":14,"alt":"E = mc^2","futureKnob":7}"#;
        let obj: Object = serde_json::from_str(raw).unwrap();
        let Object::Equation(eq) = &obj else {
            panic!("expected equation, got {obj:?}")
        };
        assert_eq!(eq.tex, "E = mc^2");
        assert_eq!(eq.em_size, Some(14.0));
        assert_eq!(eq.alt, "E = mc^2");
        assert_eq!(eq.extra.get("futureKnob").unwrap(), &Value::from(7));
        // Canonical key order is declaration order — byte-stable round-trip.
        let back = serde_json::to_string(&obj).unwrap();
        assert_eq!(
            back,
            r#"{"id":"eq1","type":"equation","at":[80.0,80.0],"size":[240.0,80.0],"tex":"E = mc^2","emSize":14.0,"alt":"E = mc^2","futureKnob":7}"#
        );
    }

    #[test]
    fn an_icon_round_trips_byte_stably_with_canonical_key_order() {
        let raw = r#"{"id":"ic1","type":"icon","at":[80,80],"size":[48,48],"name":"flask","color":"@accent1","strokeWidth":1.5,"alt":"flask","futureKnob":7}"#;
        let obj: Object = serde_json::from_str(raw).unwrap();
        let Object::Icon(ic) = &obj else {
            panic!("expected icon, got {obj:?}")
        };
        assert_eq!(ic.name, "flask");
        assert_eq!(ic.color.as_deref(), Some("@accent1"));
        assert_eq!(ic.stroke_width, Some(1.5));
        assert_eq!(ic.extra.get("futureKnob").unwrap(), &Value::from(7));
        // Canonical key order is declaration order — byte-stable round-trip.
        let back = serde_json::to_string(&obj).unwrap();
        assert_eq!(
            back,
            r#"{"id":"ic1","type":"icon","at":[80.0,80.0],"size":[48.0,48.0],"name":"flask","color":"@accent1","strokeWidth":1.5,"alt":"flask","futureKnob":7}"#
        );
    }

    #[test]
    fn an_icon_without_a_name_fails_parse_into_unknown() {
        // `name` is required — a nameless icon is malformed and preserved as
        // Unknown with a reason, exactly like any other broken known type.
        let raw = r#"{"id":"ic1","type":"icon","at":[0,0],"size":[24,24]}"#;
        let obj: Object = serde_json::from_str(raw).unwrap();
        assert!(matches!(&obj, Object::Unknown(u) if u.kind == "icon" && u.error.is_some()));
    }

    #[test]
    fn a_pinned_diagram_node_round_trips_byte_stably() {
        // Canonical key order is declaration order — `at` sits between the
        // node's shape and its styling, and an unpinned node writes nothing.
        let raw = r#"{"id":"d1","type":"diagram","at":[48.0,48.0],"size":[400.0,300.0],"nodes":[{"id":"a","label":"Start","shape":"rect","at":[64.0,80.0],"fill":"@cat1"},{"id":"b","label":"End"}],"edges":[{"from":"a","to":"b"}]}"#;
        let obj: Object = serde_json::from_str(raw).unwrap();
        let Object::Diagram(d) = &obj else {
            panic!("expected diagram, got {obj:?}")
        };
        assert_eq!(d.nodes[0].at, Some([64.0, 80.0]));
        assert_eq!(d.nodes[1].at, None);
        assert_eq!(serde_json::to_string(&obj).unwrap(), raw);
    }

    #[test]
    fn a_malformed_node_pin_fails_parse_into_unknown_with_a_reason() {
        // The lenient Object deserialize preserves the diagram rather than
        // dropping it — and the reason is what the edit route's `set` op
        // reports when refusing an invalid pin atomically.
        let raw = r#"{"id":"d1","type":"diagram",
            "nodes":[{"id":"a","label":"A","at":[1,2,3]}]}"#;
        let obj: Object = serde_json::from_str(raw).unwrap();
        match &obj {
            Object::Unknown(u) => {
                assert_eq!(u.kind, "diagram");
                assert!(u.error.is_some(), "a malformed pin carries a reason");
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn an_equation_without_alt_fails_parse_into_unknown_with_a_reason() {
        // The C6 carve-out: an equation must carry its LaTeX as alt text.
        let raw = r#"{"id":"eq1","type":"equation","at":[0,0],"size":[10,10],"tex":"x"}"#;
        let obj: Object = serde_json::from_str(raw).unwrap();
        match &obj {
            Object::Unknown(u) => {
                assert_eq!(u.kind, "equation");
                assert!(
                    u.error.as_deref().is_some_and(|e| e.contains("alt")),
                    "the reason names the missing field: {:?}",
                    u.error
                );
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
        // And `tex` is just as required.
        let raw = r#"{"id":"eq1","type":"equation","alt":"x"}"#;
        let obj: Object = serde_json::from_str(raw).unwrap();
        assert!(matches!(&obj, Object::Unknown(u) if u.error.is_some()));
    }

    #[test]
    fn group_children_are_walked_in_z_order() {
        let page: Page = serde_json::from_str(
            r#"{"id":"p","objects":[
                 {"id":"a","type":"text","text":["a"]},
                 {"id":"g","type":"group","objects":[
                   {"id":"b","type":"text","text":["b"]}]}]}"#,
        )
        .unwrap();
        let ids: Vec<_> = page.walk().map(|o| o.id().to_string()).collect();
        assert_eq!(ids, ["a", "g", "b"]);
    }
}
