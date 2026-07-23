//! `normalize()` — sugar expansion and the constraints that make ugly output
//! unrepresentable.
//!
//! This is where roughly half of "beauty" lives, and it lives here rather than
//! in a linter on purpose: a constraint that makes bad output *unrepresentable*
//! costs the agent nothing, while a rule that reports bad output after the fact
//! costs it a round trip and is ignorable. `normalize` is a **pure function**
//! and **idempotent** — normalizing twice must not move a byte, or every save
//! churns the file and `git status` stops being honest.
//!
//! Two sugars deliberately do *not* live here, though the plan filed all four
//! together: defaulting a nominal axis to descending sort, and flipping to
//! horizontal bars. Both silently reorder or transpose a chart, which is the
//! right editorial call for a throwaway `board show` and a surprise in a board
//! a human wrote and placed by hand. They live in [`crate::show`].

use serde_json::Value;

use crate::schema::{
    Board, Channel, ChannelType, ChartObject, Mark, MarkKind, Object, Page, Paragraph,
    RichParagraph, FORMAT, FORMAT_VERSION,
};

/// The design grid. Every position and extent snaps to a multiple of this, so
/// three boxes cannot end up at gaps of 20/22/20 — the single most common
/// tell of machine-placed layout.
pub const GRID_PT: f64 = 8.0;

/// The smallest object Board will represent, in points. Below this, snapping
/// could round an extent to zero and silently delete an object from the page.
pub const MIN_EXTENT_PT: f64 = 8.0;

/// Inline data caps. An inline 50k series is an unwritable file and it poisons
/// the id-anchored sparse-`Edit` contract that lets an agent adjust one object
/// without rewriting the board.
pub const MAX_INLINE_ROWS: usize = 500;
pub const MAX_INLINE_BYTES: usize = 32 * 1024;

/// `data.trace` cap: how the values were produced fits in a paragraph; a
/// pipeline log does not belong in a board file.
pub const MAX_TRACE_BYTES: usize = 2 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    pub fn label(&self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }
}

/// One finding. Every finding names the object, the field, the measured value
/// and the expected value — the reason string *is* the entire UX, so a
/// diagnostic that says only "layout problem" has failed at its job.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub page: Option<String>,
    pub object: Option<String>,
    pub field: Option<String>,
    pub message: String,
}

impl Diagnostic {
    pub fn new(severity: Severity, message: impl Into<String>) -> Self {
        Diagnostic {
            severity,
            page: None,
            object: None,
            field: None,
            message: message.into(),
        }
    }

    pub fn at(mut self, page: &str, object: &str) -> Self {
        self.page = Some(page.to_string());
        self.object = Some(object.to_string());
        self
    }

    pub fn field(mut self, field: &str) -> Self {
        self.field = Some(field.to_string());
        self
    }

    /// A one-line rendering for the CLI and the pane.
    pub fn render(&self) -> String {
        let mut s = String::new();
        s.push_str(self.severity.label());
        s.push_str(": ");
        if let Some(o) = &self.object {
            s.push_str(o);
            if let Some(f) = &self.field {
                s.push('.');
                s.push_str(f);
            }
            s.push_str(" — ");
        }
        s.push_str(&self.message);
        s
    }
}

/// Normalize a board in place, returning what it had to say about it.
pub fn normalize(board: &mut Board) -> Vec<Diagnostic> {
    let mut diags = Vec::new();

    board.format = FORMAT.to_string();
    if board.format_version == 0 {
        board.format_version = FORMAT_VERSION;
    }

    // A canvas that is zero, negative or NaN in either axis renders nothing at
    // all; fall back rather than emit an empty image with no explanation.
    let usable = |v: f64| v.is_finite() && v > 0.0;
    if !usable(board.canvas.size[0]) || !usable(board.canvas.size[1]) {
        diags.push(
            Diagnostic::new(
                Severity::Warning,
                format!("canvas.size is {:?}; using 960 × 540", board.canvas.size),
            )
            .field("canvas.size"),
        );
        board.canvas.size = [960.0, 540.0];
    }

    // `canvas.background` is a color *reference* — an `@token` or a `#rrggbb`
    // literal. Only the form is checkable here (which token exists is the
    // resolved theme's business, and normalize is a pure function of the
    // board alone); anything that is neither form is dropped so the ground
    // falls back to the theme rather than silently painting nothing.
    if let Some(bg) = &board.canvas.background {
        let token = bg.strip_prefix('@').map(|t| !t.is_empty()).unwrap_or(false);
        if !token && crate::theme::parse_hex(bg).is_none() {
            diags.push(
                Diagnostic::new(
                    Severity::Warning,
                    format!(
                        "canvas.background {bg:?} is neither an @token nor a #rrggbb literal; \
                         using the theme's ground"
                    ),
                )
                .field("canvas.background"),
            );
            board.canvas.background = None;
        }
    }

    for (pi, page) in board.pages.iter_mut().enumerate() {
        if page.id.is_empty() {
            page.id = format!("page-{}", pi + 1);
        }
        let page_id = page.id.clone();
        normalize_objects(&mut page.objects, &page_id, &mut diags);
    }

    diags.extend(duplicate_ids(board));
    diags
}

fn normalize_objects(objects: &mut [Object], page: &str, diags: &mut Vec<Diagnostic>) {
    for (oi, obj) in objects.iter_mut().enumerate() {
        ensure_id(obj, page, oi);
        snap_frame(obj);
        match obj {
            Object::Text(t) => canonicalize_paragraphs(&mut t.text),
            Object::Shape(s) => canonicalize_paragraphs(&mut s.text),
            Object::Connector(c) => canonicalize_paragraphs(&mut c.text),
            Object::Chart(c) => {
                let id = c.id.clone();
                normalize_chart(c, page, &id, diags);
            }
            Object::Group(g) => {
                let gid = g.id.clone();
                normalize_objects(&mut g.objects, page, diags);
                // A group's own box is the union of its children, always —
                // it is a selection envelope, not a coordinate system, so it
                // has no independent geometry to preserve.
                if let Some(f) = union_frame(&g.objects) {
                    g.at = Some([f.0, f.1]);
                    g.size = Some([f.2, f.3]);
                } else {
                    diags.push(
                        Diagnostic::new(Severity::Warning, "group has no positioned children")
                            .at(page, &gid),
                    );
                }
            }
            // A callout's bound text is real paragraphs, so it canonicalizes
            // exactly like a shape's.
            Object::Callout(c) => canonicalize_paragraphs(&mut c.text),
            // Table cells are the same text model, so each row canonicalizes
            // exactly like a text object's paragraphs.
            Object::Table(t) => {
                for row in &mut t.rows {
                    canonicalize_paragraphs(row);
                }
            }
            // Composite children are computed at render, never stored, so
            // there is nothing to canonicalize beyond the snapped frame.
            // An equation's `tex` is source, not sugar — it stays verbatim.
            Object::Image(_)
            | Object::Diagram(_)
            | Object::Equation(_)
            | Object::Icon(_)
            | Object::PanelLabel(_)
            | Object::Scalebar(_)
            | Object::SigBracket(_)
            | Object::Legend(_)
            | Object::Colorbar(_)
            | Object::Inset(_)
            | Object::Unknown(_) => {}
        }
    }
}

fn ensure_id(obj: &mut Object, page: &str, index: usize) {
    let blank = obj.id().is_empty();
    if !blank {
        return;
    }
    let generated = format!("{page}-{}-{}", obj.kind(), index + 1);
    match obj {
        Object::Text(o) => o.id = generated,
        Object::Shape(o) => o.id = generated,
        Object::Connector(o) => o.id = generated,
        Object::Image(o) => o.id = generated,
        Object::Group(o) => o.id = generated,
        Object::Table(o) => o.id = generated,
        Object::Chart(o) => o.id = generated,
        Object::Diagram(o) => o.id = generated,
        Object::Equation(o) => o.id = generated,
        Object::Icon(o) => o.id = generated,
        Object::PanelLabel(o) => o.id = generated,
        Object::Scalebar(o) => o.id = generated,
        Object::SigBracket(o) => o.id = generated,
        Object::Legend(o) => o.id = generated,
        Object::Colorbar(o) => o.id = generated,
        Object::Callout(o) => o.id = generated,
        Object::Inset(o) => o.id = generated,
        Object::Unknown(o) => o.id = generated,
    }
}

/// Snap geometry to the 8 pt grid.
///
/// `round` (not floor) so an object never drifts consistently up-left across
/// repeated saves, and extents are floored at [`MIN_EXTENT_PT`] so snapping a
/// thin rule cannot round it out of existence.
fn snap_frame(obj: &mut Object) {
    if let Some(f) = obj.frame() {
        obj.set_at([snap(f.x), snap(f.y)]);
        obj.set_size([snap(f.w).max(MIN_EXTENT_PT), snap(f.h).max(MIN_EXTENT_PT)]);
    }
}

fn snap(v: f64) -> f64 {
    if !v.is_finite() {
        return 0.0;
    }
    (v / GRID_PT).round() * GRID_PT
}

fn union_frame(objects: &[Object]) -> Option<(f64, f64, f64, f64)> {
    let mut acc: Option<(f64, f64, f64, f64)> = None;
    for o in objects {
        let Some(f) = o.frame() else { continue };
        acc = Some(match acc {
            None => (f.x, f.y, f.right(), f.bottom()),
            Some((x0, y0, x1, y1)) => (
                x0.min(f.x),
                y0.min(f.y),
                x1.max(f.right()),
                y1.max(f.bottom()),
            ),
        });
    }
    acc.map(|(x0, y0, x1, y1)| {
        (
            x0,
            y0,
            (x1 - x0).max(MIN_EXTENT_PT),
            (y1 - y0).max(MIN_EXTENT_PT),
        )
    })
}

/// Collapse a rich paragraph that carries no styling back to its bare-string
/// form.
///
/// The direction matters. Expanding `"hello"` into `{"runs":[{"t":"hello"}]}`
/// would destroy the sugar on first save and triple the size of an ordinary
/// deck; collapsing toward the terse spelling gives the format one canonical
/// representation per meaning — which is what byte-stability actually requires
/// — while keeping the file readable.
fn canonicalize_paragraphs(paras: &mut [Paragraph]) {
    for p in paras.iter_mut() {
        let Paragraph::Rich(rich) = p else { continue };
        if is_bare(rich) {
            *p = Paragraph::Plain(rich.runs[0].t.clone());
        }
    }
}

fn is_bare(p: &RichParagraph) -> bool {
    p.align.is_none()
        && p.space_before.is_none()
        && p.space_after.is_none()
        && p.bullet.is_none()
        && p.extra.is_empty()
        && p.runs.len() == 1
        && {
            let r = &p.runs[0];
            r.b.is_none()
                && r.i.is_none()
                && r.u.is_none()
                && r.color.is_none()
                && r.size.is_none()
                && r.family.is_none()
                && r.link.is_none()
                && r.extra.is_empty()
        }
}

// ---------------------------------------------------------------------------
// Chart
// ---------------------------------------------------------------------------

fn normalize_chart(c: &mut ChartObject, page: &str, id: &str, diags: &mut Vec<Diagnostic>) {
    // Channel types are inferred from **inline JSON only**, which is a named
    // exception to "types are declared, never inferred". The ban exists
    // because CSV is untyped text, where an integer-coded category silently
    // lands on a linear axis; inline JSON is typed, so a number really is a
    // number and the inference is sound.
    let rows = c.data.values.clone();
    for (name, ch) in [
        ("x", c.x.as_mut()),
        ("y", c.y.as_mut()),
        ("color", c.color.as_mut()),
    ] {
        let Some(ch) = ch else { continue };
        if ch.kind.is_none() {
            ch.kind = Some(infer_type(&rows, &ch.field));
            if rows.is_empty() {
                diags.push(
                    Diagnostic::new(
                        Severity::Warning,
                        format!(
                            "channel {name:?} has no declared type and no inline rows to infer \
                             one from; assuming nominal"
                        ),
                    )
                    .at(page, id)
                    .field(name),
                );
            }
        }
        if ch.scale.is_none() {
            ch.scale = Some(default_scale(ch.kind.unwrap_or(ChannelType::Nominal)));
        }
    }

    if c.marks.is_empty() {
        match infer_marks(c.x.as_ref(), c.y.as_ref()) {
            Some(marks) => c.marks = marks,
            None => diags.push(
                Diagnostic::new(
                    Severity::Error,
                    "cannot infer a mark: give the chart both an x and a y channel, or state \
                     `marks` explicitly",
                )
                .at(page, id)
                .field("marks"),
            ),
        }
    }

    if c.data.values.len() > MAX_INLINE_ROWS {
        diags.push(
            Diagnostic::new(
                Severity::Error,
                format!(
                    "{} inline rows exceeds the {MAX_INLINE_ROWS}-row cap; bind a `source` file \
                     instead of inlining, or aggregate upstream",
                    c.data.values.len()
                ),
            )
            .at(page, id)
            .field("data.values"),
        );
    }
    let bytes = serde_json::to_string(&c.data.values)
        .map(|s| s.len())
        .unwrap_or(0);
    if bytes > MAX_INLINE_BYTES {
        diags.push(
            Diagnostic::new(
                Severity::Error,
                format!(
                    "inline data is {} KiB, over the {} KiB cap; bind a `source` file instead",
                    bytes / 1024,
                    MAX_INLINE_BYTES / 1024
                ),
            )
            .at(page, id)
            .field("data.values"),
        );
    }

    // `trace` is a provenance note, not a document: clamp to the cap on a
    // char boundary (truncation, not rejection — losing the tail of a trace
    // beats losing the trace). Idempotent, so normalize stays a fixed point.
    if let Some(trace) = &mut c.data.trace {
        if trace.len() > MAX_TRACE_BYTES {
            let mut cut = MAX_TRACE_BYTES;
            while !trace.is_char_boundary(cut) {
                cut -= 1;
            }
            let full = trace.len();
            trace.truncate(cut);
            diags.push(
                Diagnostic::new(
                    Severity::Warning,
                    format!(
                        "trace is {full} bytes; clamped to {MAX_TRACE_BYTES} — keep it a summary \
                         (method, command, seed), not a transcript"
                    ),
                )
                .at(page, id)
                .field("data.trace"),
            );
        }
    }
}

/// Number → quantitative, ISO-8601 → temporal, anything else → nominal.
fn infer_type(rows: &[Value], field: &str) -> ChannelType {
    let mut saw = None;
    for row in rows {
        let Some(v) = row.get(field) else { continue };
        let t = match v {
            Value::Number(_) => ChannelType::Quantitative,
            Value::String(s) if crate::chart::parse_temporal(s).is_some() => ChannelType::Temporal,
            Value::Null => continue,
            _ => ChannelType::Nominal,
        };
        match saw {
            None => saw = Some(t),
            // A column that is numbers *and* strings is not a number column.
            Some(prev) if prev != t => return ChannelType::Nominal,
            _ => {}
        }
    }
    saw.unwrap_or(ChannelType::Nominal)
}

fn default_scale(t: ChannelType) -> crate::schema::ScaleKind {
    use crate::schema::ScaleKind;
    match t {
        ChannelType::Quantitative => ScaleKind::Linear,
        ChannelType::Temporal => ScaleKind::Temporal,
        ChannelType::Ordinal | ChannelType::Nominal => ScaleKind::Ordinal,
    }
}

/// Infer a mark from the declared channel types. A pure total function of two
/// enums — which is what lets `marks` be omitted without giving up
/// determinism.
pub fn infer_marks(x: Option<&Channel>, y: Option<&Channel>) -> Option<Vec<Mark>> {
    use ChannelType::*;
    let xt = x?.kind.unwrap_or(Nominal);
    let yt = y?.kind.unwrap_or(Quantitative);
    let kind = match (xt, yt) {
        (Nominal, Quantitative) => MarkKind::Bar,
        (Ordinal, Quantitative) | (Temporal, Quantitative) => MarkKind::Line,
        (Quantitative, Quantitative) => MarkKind::Point,
        // A quantitative x against a categorical y is a horizontal bar.
        (Quantitative, Nominal) | (Quantitative, Ordinal) => MarkKind::Bar,
        _ => MarkKind::Point,
    };
    Some(vec![Mark::new(kind)])
}

fn duplicate_ids(board: &Board) -> Vec<Diagnostic> {
    let mut seen: std::collections::BTreeMap<&str, usize> = Default::default();
    for (_, obj) in board.objects() {
        *seen.entry(obj.id()).or_default() += 1;
    }
    seen.into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(id, n)| {
            // Never *rewrite* a duplicate: the id is simultaneously the diff
            // anchor, the agent's Edit anchor, the journal's subject and the
            // merge key, so renaming one silently breaks all four.
            Diagnostic::new(
                Severity::Error,
                format!(
                    "id {id:?} is used by {n} objects; ids are the merge and journal key and \
                     must be unique"
                ),
            )
        })
        .collect()
}

/// Every object on a page with *explicit* geometry, keyed by id.
///
/// This sees only stated `at`/`size` — slot- and anchor-placed objects are
/// absent. Where slots or anchors may be in play (render, describe, export),
/// use [`crate::slots::resolve_page_frames`] instead: resolution happens
/// there, at read time, never here — writing resolved geometry back would
/// churn the file and break byte-stability.
pub fn index_page(page: &Page) -> std::collections::BTreeMap<String, crate::schema::Frame> {
    page.walk()
        .filter_map(|o| o.frame().map(|f| (o.id().to_string(), f)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    fn board_with(objects: &str) -> Board {
        parse(&format!(
            r#"{{"format":"chimaera.board","formatVersion":1,
                "canvas":{{"size":[960,540]}},
                "pages":[{{"id":"p1","objects":[{objects}]}}]}}"#
        ))
        .unwrap()
    }

    #[test]
    fn normalize_is_idempotent() {
        let mut b = board_with(
            r#"{"id":"t","type":"text","at":[81,131],"size":[300,50],
                "text":[{"runs":[{"t":"hi"}]}]}"#,
        );
        normalize(&mut b);
        let once = crate::to_string(&b).unwrap();
        normalize(&mut b);
        let twice = crate::to_string(&b).unwrap();
        assert_eq!(once, twice, "normalize must be a fixed point");
    }

    #[test]
    fn an_oversized_trace_clamps_on_a_char_boundary_and_warns() {
        let chart = |trace: &str| {
            board_with(&format!(
                r#"{{"id":"c","type":"chart","at":[80,80],"size":[480,320],
                    "data":{{"origin":"command","trace":"{trace}",
                             "values":[{{"f":"a","v":1}}]}},
                    "x":{{"field":"f"}},"y":{{"field":"v"}}}}"#
            ))
        };
        // Multi-byte tail so a naive byte truncate would split a char.
        let mut long = "x".repeat(MAX_TRACE_BYTES - 1);
        long.push_str("ééé");
        let mut b = chart(&long);
        let diags = normalize(&mut b);
        let crate::Object::Chart(c) = &b.pages[0].objects[0] else {
            panic!()
        };
        let clamped = c.data.trace.as_deref().unwrap();
        assert!(clamped.len() <= MAX_TRACE_BYTES);
        assert!(clamped.is_char_boundary(clamped.len()));
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Warning && d.message.contains("clamped")),
            "{diags:?}"
        );
        // Idempotent: the clamped board re-normalizes without motion.
        let once = crate::to_string(&b).unwrap();
        let diags = normalize(&mut b);
        assert!(!diags.iter().any(|d| d.message.contains("clamped")));
        assert_eq!(once, crate::to_string(&b).unwrap());

        // Under the cap: untouched, no warning.
        let mut ok = chart("quartiles via numpy, seed 42");
        let diags = normalize(&mut ok);
        assert!(!diags.iter().any(|d| d.message.contains("clamped")));
    }

    #[test]
    fn a_canvas_background_round_trips_byte_stably_in_canonical_position() {
        // The field sits between `size` and any preserved extras — canonical
        // key order is part of the format, and a save must not move a byte.
        let src = "{\n  \"format\": \"chimaera.board\",\n  \"formatVersion\": 1,\n  \
                   \"canvas\": { \"size\": [960, 540], \"background\": \"@surface\" },\n  \
                   \"pages\": [\n    { \"id\": \"p1\", \"objects\": [] }\n  ]\n}\n";
        let mut b = parse(src).unwrap();
        let diags = normalize(&mut b);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(b.canvas.background.as_deref(), Some("@surface"));
        assert_eq!(crate::to_string(&b).unwrap(), src);
        // A literal is as valid as a token.
        b.canvas.background = Some("#20242c".to_string());
        assert!(normalize(&mut b).is_empty());
        assert_eq!(b.canvas.background.as_deref(), Some("#20242c"));
    }

    #[test]
    fn a_malformed_canvas_background_is_dropped_with_a_diagnostic() {
        let mut b = board_with(r#"{"id":"t","type":"text","text":["hi"]}"#);
        b.canvas.background = Some("cornflower blue".to_string());
        let diags = normalize(&mut b);
        assert!(
            diags.iter().any(|d| d.severity == Severity::Warning
                && d.field.as_deref() == Some("canvas.background")),
            "{diags:?}"
        );
        assert_eq!(b.canvas.background, None, "invalid reference must drop");
        // Idempotent: the dropped field re-normalizes without a finding.
        assert!(normalize(&mut b).is_empty());
    }

    #[test]
    fn geometry_snaps_to_the_grid() {
        let mut b =
            board_with(r#"{"id":"t","type":"text","at":[81,131],"size":[301,49],"text":["hi"]}"#);
        normalize(&mut b);
        let f = b.pages[0].objects[0].frame().unwrap();
        assert_eq!((f.x, f.y, f.w, f.h), (80.0, 128.0, 304.0, 48.0));
    }

    #[test]
    fn snapping_never_rounds_an_object_away() {
        let mut b =
            board_with(r#"{"id":"r","type":"shape","geo":"line","at":[0,0],"size":[200,1]}"#);
        normalize(&mut b);
        let f = b.pages[0].objects[0].frame().unwrap();
        assert!(f.h >= MIN_EXTENT_PT, "a 1 pt rule must survive snapping");
    }

    #[test]
    fn a_bare_run_collapses_to_its_string_form() {
        let mut b = board_with(r#"{"id":"t","type":"text","text":[{"runs":[{"t":"hello"}]}]}"#);
        normalize(&mut b);
        let out = crate::to_string(&b).unwrap();
        assert!(out.contains(r#""text": ["hello"]"#), "{out}");
    }

    #[test]
    fn a_styled_run_is_left_alone() {
        let mut b =
            board_with(r#"{"id":"t","type":"text","text":[{"runs":[{"t":"hello","b":true}]}]}"#);
        normalize(&mut b);
        let out = crate::to_string(&b).unwrap();
        assert!(out.contains(r#""b": true"#), "{out}");
    }

    #[test]
    fn a_table_save_is_byte_stable_and_cells_canonicalize() {
        let mut b = board_with(
            r#"{"id":"tb","type":"table","at":[80,80],"size":[320,160],"header":true,
                "columns":[2,1,1],
                "rows":[["Fixture","Before","After"],
                        [{"runs":[{"t":"large.json"}]},"812","244"]]}"#,
        );
        normalize(&mut b);
        let once = crate::to_string(&b).unwrap();
        let mut again = crate::parse(&once).unwrap();
        normalize(&mut again);
        let twice = crate::to_string(&again).unwrap();
        assert_eq!(once, twice, "a table save must be a fixed point");
        // A bare rich cell collapses to its string form, exactly like text.
        assert!(once.contains(r#""large.json""#), "{once}");
        assert!(!once.contains(r#""runs""#), "{once}");
    }

    #[test]
    fn channel_types_are_inferred_from_inline_json() {
        let mut b = board_with(
            r#"{"id":"c","type":"chart","at":[0,0],"size":[400,300],
                "data":{"origin":"command","values":[{"f":"a","ms":1},{"f":"b","ms":2}]},
                "x":{"field":"f"},"y":{"field":"ms"}}"#,
        );
        normalize(&mut b);
        let Object::Chart(c) = &b.pages[0].objects[0] else {
            panic!()
        };
        assert_eq!(c.x.as_ref().unwrap().kind, Some(ChannelType::Nominal));
        assert_eq!(c.y.as_ref().unwrap().kind, Some(ChannelType::Quantitative));
        // nominal × quantitative → bar, without the author saying so.
        assert_eq!(c.marks[0].mark, MarkKind::Bar);
    }

    #[test]
    fn a_mixed_column_is_not_a_number_column() {
        let rows: Vec<Value> = serde_json::from_str(r#"[{"v":1},{"v":"n/a"}]"#).unwrap();
        assert_eq!(infer_type(&rows, "v"), ChannelType::Nominal);
    }

    #[test]
    fn duplicate_ids_are_an_error_not_a_rename() {
        let mut b = board_with(
            r#"{"id":"dup","type":"text","text":["a"]},{"id":"dup","type":"text","text":["b"]}"#,
        );
        let diags = normalize(&mut b);
        assert!(diags
            .iter()
            .any(|d| d.severity == Severity::Error && d.message.contains("dup")));
        // Both objects keep their id — renaming would break the journal.
        assert_eq!(b.pages[0].objects[1].id(), "dup");
    }

    #[test]
    fn a_group_box_is_the_union_of_its_children() {
        let mut b = board_with(
            r#"{"id":"g","type":"group","objects":[
                 {"id":"a","type":"shape","geo":"rect","at":[0,0],"size":[80,80]},
                 {"id":"b","type":"shape","geo":"rect","at":[160,80],"size":[80,80]}]}"#,
        );
        normalize(&mut b);
        let f = b.pages[0].objects[0].frame().unwrap();
        assert_eq!((f.x, f.y, f.w, f.h), (0.0, 0.0, 240.0, 160.0));
    }
}
