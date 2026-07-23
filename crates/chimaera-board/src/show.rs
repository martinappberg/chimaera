//! `board show` — the agent showing you something mid-work.
//!
//! An agent debugging, benchmarking or running a QC pass reaches for a picture
//! instead of a paragraph. The spec arrives on stdin as one tool call, and the
//! result is a real one-page `.board.json` — **not** a second schema: the
//! `chart` value *is* the schema's chart object minus `id`/`at`/`size`, and
//! `show` normalizes it into a board and runs the identical render path. The
//! `emitted board rendered again is byte-identical` test in this module is the
//! whole defense against `show` quietly becoming a second product.
//!
//! Two pieces of sugar live here and NOT in `normalize()`, because they
//! silently reorder or transpose a chart — the right editorial call for a
//! throwaway, a surprise in a board a human placed by hand: descending sort by
//! default on a nominal axis, and flipping to horizontal bars when categories
//! are many or labels long.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::schema::{
    Board, Canvas, ChannelType, ChartObject, Extra, Object, Page, Paragraph, TextObject,
};
use crate::theme::Theme;

/// The `show` spec: `title`, `note`, and exactly one of `chart` / `table` /
/// `text` / `mermaid`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShowSpec {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub chart: Option<Value>,
    #[serde(default)]
    pub table: Option<TableSpec>,
    #[serde(default)]
    pub text: Option<Vec<String>>,
    /// Mermaid flowchart source, converted to a `diagram` object — the
    /// `show --mermaid` stdin path.
    #[serde(default)]
    pub mermaid: Option<String>,
}

/// A table to show: column order plus rows. The most common thing to show
/// mid-work — test results, a config diff, a comparison matrix — is a table,
/// and a chart-only `show` would send the agent back to prose for it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TableSpec {
    pub columns: Vec<String>,
    pub rows: Vec<Value>,
}

/// Canvas presets for a shown card. 720×450 by default: a full 16:9 slide is
/// too wide for a 600–700 px transcript column.
pub fn preset_size(preset: &str) -> Option<[f64; 2]> {
    match preset {
        "default" => Some([720.0, 450.0]),
        "wide" => Some([960.0, 400.0]),
        "square" => Some([560.0, 560.0]),
        "tall" => Some([560.0, 720.0]),
        _ => None,
    }
}

/// Margins inside a shown card, in points.
const M: f64 = 40.0;
const TITLE_H: f64 = 48.0;
const NOTE_H: f64 = 32.0;

/// Build the one-page board a `show` spec means.
///
/// Pure — no filesystem, no clock — so the emit/render-equality test can hold
/// byte-for-byte.
pub fn build_board(spec: &ShowSpec, size: [f64; 2], theme_id: &str) -> Result<Board> {
    let n_bodies = [
        spec.chart.is_some(),
        spec.table.is_some(),
        spec.text.is_some(),
        spec.mermaid.is_some(),
    ]
    .iter()
    .filter(|b| **b)
    .count();
    if n_bodies != 1 {
        bail!("a show spec carries exactly one of `chart`, `table`, `text` or `mermaid`");
    }

    let canvas = Canvas {
        preset: None,
        target: None,
        size,
        extra: Extra::new(),
    };
    let mut board = Board::new(spec.title.clone().unwrap_or_default(), canvas);
    if spec.title.is_none() {
        board.title = None;
    }
    board.theme = Some(theme_id.to_string());

    let mut page = Page::new("shown");
    let mut y = M;

    if let Some(title) = &spec.title {
        page.objects.push(Object::Text(TextObject {
            id: "title".into(),
            kind: Default::default(),
            role: Some("heading".into()),
            slot: None,
            at: Some([M, y]),
            size: Some([size[0] - M * 2.0, TITLE_H]),
            text: vec![Paragraph::Plain(title.clone())],
            align: None,
            valign: None,
            anchor: None,
            alt: None,
            link: None,
            rotation: None,
            extra: Extra::new(),
        }));
        y += TITLE_H + 8.0;
    }

    let note_h = if spec.note.is_some() {
        NOTE_H + 8.0
    } else {
        0.0
    };
    let body = [
        M,
        y,
        size[0] - M * 2.0,
        (size[1] - y - M - note_h).max(64.0),
    ];

    if let Some(chart) = &spec.chart {
        page.objects.push(chart_object(chart, body)?);
    } else if let Some(table) = &spec.table {
        table_objects(table, body, &mut page)?;
    } else if let Some(mermaid) = &spec.mermaid {
        page.objects.push(diagram_object(mermaid, body)?);
    } else if let Some(text) = &spec.text {
        page.objects.push(Object::Text(TextObject {
            id: "body".into(),
            kind: Default::default(),
            role: Some("body".into()),
            slot: None,
            at: Some([body[0], body[1]]),
            size: Some([body[2], body[3]]),
            text: text.iter().map(|l| Paragraph::Plain(l.clone())).collect(),
            align: None,
            valign: None,
            anchor: None,
            alt: None,
            link: None,
            rotation: None,
            extra: Extra::new(),
        }));
    }

    if let Some(note) = &spec.note {
        page.objects.push(Object::Text(TextObject {
            id: "note".into(),
            kind: Default::default(),
            role: Some("caption".into()),
            slot: None,
            at: Some([M, size[1] - M - NOTE_H + 8.0]),
            size: Some([size[0] - M * 2.0, NOTE_H]),
            text: vec![Paragraph::Plain(note.clone())],
            align: None,
            valign: None,
            anchor: None,
            alt: None,
            link: None,
            rotation: None,
            extra: Extra::new(),
        }));
    }

    board.pages = vec![page];
    let diags = crate::normalize(&mut board);
    for d in &diags {
        if d.severity == crate::Severity::Error {
            bail!("{}", d.render());
        }
    }
    Ok(board)
}

/// Interpret the spec's `chart` value as a schema chart object, filling in
/// what `show` owns: id, geometry, origin defaulting, and the two editorial
/// sugars. The value is the WHOLE chart vocabulary, not an x/y/values subset:
/// `marks` (including `box` over precomputed five-number rows), per-mark
/// `fields`, a `color` channel, axes, sort, and scale types all pass through
/// into the same normalize/render path a hand-placed chart takes. A spec that
/// states `marks` is a deliberate composition, so the editorial sugars stand
/// down for it.
fn chart_object(chart: &Value, body: [f64; 4]) -> Result<Object> {
    let mut v = chart.clone();
    let obj = v.as_object_mut().context("`chart` must be an object")?;
    obj.insert("id".into(), Value::from("chart"));
    obj.insert("type".into(), Value::from("chart"));
    obj.insert("at".into(), serde_json::json!([body[0], body[1]]));
    obj.insert("size".into(), serde_json::json!([body[2], body[3]]));

    // Sugar: bare-string channels. `"x": "file"` means `{"field": "file"}`.
    for ch in ["x", "y", "color"] {
        if let Some(Value::String(f)) = obj.get(ch) {
            let f = f.clone();
            obj.insert(ch.into(), serde_json::json!({ "field": f }));
        }
    }
    // Sugar: a singular `mark` means a one-layer `marks`. Without this the
    // vega-familiar `"mark": "box"` fell into the lenient extras and the
    // inferred bar drew instead — the silent-wrong outcome. A string names
    // the kind; an object is the layer itself.
    if !obj.contains_key("marks") {
        if let Some(mark) = obj.remove("mark") {
            let layer = match mark {
                Value::String(kind) => serde_json::json!({ "mark": kind }),
                other => other,
            };
            obj.insert("marks".into(), Value::Array(vec![layer]));
        }
    }
    // Provenance sugar: top-level `trace`/`inputs` belong inside `data`,
    // beside the top-level `values` they annotate.
    let trace = obj.remove("trace");
    let inputs = obj.remove("inputs");
    // A throwaway's numbers came from what the agent just ran, so `command`
    // is the honest default — but only a default; a stated origin wins.
    let values = obj.remove("values");
    if let Some(data) = obj.get_mut("data").and_then(Value::as_object_mut) {
        data.entry("origin").or_insert(Value::from("command"));
        // An explicit `data` object beside top-level `values` is a common
        // agent spelling; folding beats silently rendering zero rows.
        if let Some(values) = values {
            data.entry("values").or_insert(values);
        }
    } else if let Some(values) = values {
        obj.insert(
            "data".into(),
            serde_json::json!({ "origin": "command", "values": values }),
        );
    }
    if let Some(data) = obj.get_mut("data").and_then(Value::as_object_mut) {
        if let Some(t) = trace {
            data.entry("trace").or_insert(t);
        }
        if let Some(i) = inputs {
            data.entry("inputs").or_insert(i);
        }
    }

    let mut chart: ChartObject =
        serde_json::from_value(v).context("the `chart` spec does not parse as a chart")?;

    // Editorial sugar, applied before normalize so inference sees the final
    // shape — but only when the mark is OURS to infer. Stated `marks` mean a
    // composed chart (a box, a layered line+area, an interval bar), where a
    // silent reorder or transpose breaks stated geometry. Descending by value
    // on a nominal axis…
    if !chart.marks.is_empty() {
        return Ok(Object::Chart(chart));
    }
    let rows = chart.data.values.clone();
    if let (Some(x), Some(y)) = (chart.x.as_mut(), chart.y.as_ref()) {
        let x_nominal = x
            .kind
            .map(|k| k == ChannelType::Nominal)
            .unwrap_or_else(|| {
                !rows
                    .iter()
                    .any(|r| r.get(&x.field).map(|v| v.is_number()).unwrap_or(false))
            });
        if x_nominal && x.sort.is_none() {
            x.sort = Some(format!("-{}", y.field));
        }
        // …and horizontal bars when labels would collide: over 7 categories,
        // or any label over 12 characters.
        if x_nominal {
            let cats: std::collections::BTreeSet<String> = rows
                .iter()
                .filter_map(|r| r.get(&x.field))
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            let long = cats.iter().any(|c| c.chars().count() > 12);
            if cats.len() > 7 || long {
                std::mem::swap(&mut chart.x, &mut chart.y);
            }
        }
    }

    Ok(Object::Chart(chart))
}

/// Convert mermaid source to a diagram filling the body. `show` owns only the
/// id and geometry; the parse itself is [`crate::diagram::from_mermaid`].
fn diagram_object(src: &str, body: [f64; 4]) -> Result<Object> {
    let mut d = crate::diagram::from_mermaid(src)?;
    d.id = "diagram".to_string();
    d.at = Some([body[0], body[1]]);
    d.size = Some([body[2], body[3]]);
    Ok(Object::Diagram(d))
}

/// The card size a `show --mermaid` auto-fits to its flowchart: the layout's
/// natural size plus the card chrome, clamped to a sane card range — a
/// 7-node flow gets a card its shape fills instead of floating in a preset.
/// Only the default preset auto-sizes; an explicit `--size`/`--preset` wins.
pub fn mermaid_card_size(
    src: &str,
    has_title: bool,
    has_note: bool,
    theme: &Theme,
    fonts: &crate::layout::FontStack,
) -> Result<[f64; 2]> {
    let d = crate::diagram::from_mermaid(src)?;
    let nat = crate::diagram::natural_size(&d, theme, fonts);
    let mut h = nat[1] + M * 2.0;
    if has_title {
        h += TITLE_H + 8.0;
    }
    if has_note {
        h += NOTE_H + 8.0;
    }
    Ok([
        (nat[0] + M * 2.0).clamp(420.0, 1440.0).ceil(),
        h.clamp(280.0, 1100.0).ceil(),
    ])
}

/// Lay a table out as text rows. A real `table` composite arrives in slice 3;
/// this is the honest slice-0 rendering — monospace-free, role-driven, and
/// readable — not a second table implementation to migrate away from.
fn table_objects(table: &TableSpec, body: [f64; 4], page: &mut Page) -> Result<()> {
    if table.columns.is_empty() {
        bail!("a table needs at least one column");
    }
    let n_rows = table.rows.len() + 1; // header
    let row_h = (body[3] / n_rows as f64).clamp(24.0, 44.0);
    let col_w = body[2] / table.columns.len() as f64;

    let cell = |text: String, col: usize, row: usize, header: bool| -> Object {
        Object::Text(TextObject {
            id: format!("r{row}c{col}"),
            kind: Default::default(),
            role: Some(if header { "label" } else { "body" }.into()),
            slot: None,
            at: Some([body[0] + col_w * col as f64, body[1] + row_h * row as f64]),
            size: Some([col_w - 8.0, row_h]),
            text: vec![Paragraph::Plain(text)],
            align: None,
            valign: Some(crate::schema::VAlign::Middle),
            anchor: None,
            alt: None,
            link: None,
            rotation: None,
            extra: Extra::new(),
        })
    };

    for (ci, c) in table.columns.iter().enumerate() {
        page.objects.push(cell(c.clone(), ci, 0, true));
    }
    for (ri, row) in table.rows.iter().enumerate() {
        for (ci, c) in table.columns.iter().enumerate() {
            let text = match row.get(c) {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Number(n)) => n.to_string(),
                Some(Value::Bool(b)) => b.to_string(),
                Some(Value::Null) | None => String::new(),
                Some(other) => other.to_string(),
            };
            page.objects.push(cell(text, ci, ri + 1, false));
        }
    }
    Ok(())
}

/// A short content-derived id for the shown files: `a3f1`-style, stable for
/// the same spec so a re-shown identical result overwrites rather than
/// multiplying.
pub fn spec_id(spec_json: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(spec_json.as_bytes());
    let d = h.finalize();
    format!("{:02x}{:02x}", d[0], d[1])
}

/// The one-line summary `show` prints: `shown chart · 9 bars · talk-dark ·
/// 720×450`.
pub fn summary(board: &Board, theme_id: &str) -> String {
    let page = &board.pages[0];
    let kind = page
        .objects
        .iter()
        .find_map(|o| match o {
            Object::Chart(c) => Some(format!("chart · {} rows", c.data.values.len())),
            Object::Diagram(d) => Some(format!("diagram · {} nodes", d.nodes.len())),
            _ => None,
        })
        .unwrap_or_else(|| {
            if page.objects.iter().any(|o| o.id().starts_with('r')) {
                "table".to_string()
            } else {
                "text".to_string()
            }
        });
    format!(
        "shown {kind} · {theme_id} · {}×{}",
        board.canvas.width(),
        board.canvas.height()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::FontStack;
    use crate::render::{render_page, RasterParams};

    const SPEC: &str = r#"{
      "title": "Test failures by file",
      "note": "after the parser rewrite; 3 runs",
      "chart": {
        "x": "file", "y": "failures",
        "values": [
          {"file": "parser.rs", "failures": 12},
          {"file": "lexer.rs", "failures": 3},
          {"file": "ast.rs", "failures": 1}
        ]
      }
    }"#;

    #[test]
    fn a_minimal_spec_becomes_a_board_that_renders() {
        let spec: ShowSpec = serde_json::from_str(SPEC).unwrap();
        let board = build_board(&spec, [720.0, 450.0], "talk-dark").unwrap();
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let out = render_page(&board, 0, &theme, &fonts, RasterParams::default()).unwrap();
        assert!(out.png.len() > 1000);
    }

    #[test]
    fn the_emitted_board_rendered_again_is_byte_identical() {
        // THE test: --emit-board must yield a file whose render equals show's
        // own render, or `show` has become a second format.
        let spec: ShowSpec = serde_json::from_str(SPEC).unwrap();
        let board = build_board(&spec, [720.0, 450.0], "talk-dark").unwrap();
        let emitted = crate::to_string(&board).unwrap();
        let reparsed = crate::parse(&emitted).unwrap();
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let a = render_page(&board, 0, &theme, &fonts, RasterParams::default()).unwrap();
        let b = render_page(&reparsed, 0, &theme, &fonts, RasterParams::default()).unwrap();
        assert_eq!(a.png, b.png);
    }

    #[test]
    fn shown_charts_sort_descending_by_default() {
        let spec: ShowSpec = serde_json::from_str(
            r#"{"chart": {"x": "f", "y": "v",
                "values": [{"f":"low","v":1},{"f":"high","v":9},{"f":"mid","v":5}]}}"#,
        )
        .unwrap();
        let board = build_board(&spec, [720.0, 450.0], "talk-dark").unwrap();
        let Object::Chart(c) = &board.pages[0].objects[0] else {
            panic!()
        };
        assert_eq!(c.x.as_ref().unwrap().sort.as_deref(), Some("-v"));
    }

    #[test]
    fn long_labels_flip_the_chart_horizontal() {
        let spec: ShowSpec = serde_json::from_str(
            r#"{"chart": {"x": "file", "y": "n",
                "values": [{"file":"crates/chimaera-server/src/router.rs","n":4},
                           {"file":"web-ui/src/App.svelte","n":2}]}}"#,
        )
        .unwrap();
        let board = build_board(&spec, [720.0, 450.0], "talk-dark").unwrap();
        let Object::Chart(c) = &board.pages[0].objects[0] else {
            panic!()
        };
        // After the flip, y carries the categories.
        assert_eq!(c.y.as_ref().unwrap().field, "file");
        assert_eq!(c.x.as_ref().unwrap().field, "n");
    }

    #[test]
    fn a_stated_origin_survives_the_default() {
        let spec: ShowSpec = serde_json::from_str(
            r#"{"chart": {"x": "f", "y": "v",
                "data": {"origin": "stated-by-user",
                         "values": [{"f":"a","v":1},{"f":"b","v":2}]}}}"#,
        )
        .unwrap();
        let board = build_board(&spec, [720.0, 450.0], "talk-dark").unwrap();
        let Object::Chart(c) = &board.pages[0].objects[0] else {
            panic!()
        };
        assert_eq!(c.data.origin, crate::schema::DataOrigin::StatedByUser);
    }

    #[test]
    fn zero_or_two_bodies_are_refused() {
        let none: ShowSpec = serde_json::from_str(r#"{"title": "empty"}"#).unwrap();
        assert!(build_board(&none, [720.0, 450.0], "talk-dark").is_err());
        let two: ShowSpec =
            serde_json::from_str(r#"{"text": ["a"], "table": {"columns": ["c"], "rows": []}}"#)
                .unwrap();
        assert!(build_board(&two, [720.0, 450.0], "talk-dark").is_err());
    }

    #[test]
    fn a_table_spec_renders() {
        let spec: ShowSpec = serde_json::from_str(
            r#"{"title": "Failures",
                "table": {"columns": ["file", "failures"],
                          "rows": [{"file": "parser.rs", "failures": 12},
                                   {"file": "lexer.rs", "failures": 3}]}}"#,
        )
        .unwrap();
        let board = build_board(&spec, [720.0, 450.0], "talk-light").unwrap();
        let theme = crate::theme::default_for(false);
        let fonts = FontStack::new(&[]);
        let out = render_page(&board, 0, &theme, &fonts, RasterParams::default()).unwrap();
        assert!(out.png.len() > 1000);
        // Header + 2 rows × 2 cols + title.
        assert!(board.pages[0].objects.len() >= 7);
    }

    #[test]
    fn a_mermaid_spec_becomes_a_diagram_that_renders() {
        let spec: ShowSpec = serde_json::from_str(
            r#"{"title": "Ingestion",
                "mermaid": "flowchart TD\nA[Reader] --> B{Valid?}\nB -->|yes| C((Store))"}"#,
        )
        .unwrap();
        let board = build_board(&spec, [720.0, 450.0], "talk-dark").unwrap();
        let Object::Diagram(d) = &board.pages[0].objects[1] else {
            panic!("expected a diagram body, got {:?}", board.pages[0].objects)
        };
        assert_eq!(d.nodes.len(), 3);
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let out = render_page(&board, 0, &theme, &fonts, RasterParams::default()).unwrap();
        assert!(out.png.len() > 1000);
        assert!(
            !out.diagnostics
                .iter()
                .any(|d| d.severity == crate::Severity::Error),
            "{:?}",
            out.diagnostics
        );
    }

    #[test]
    fn a_mermaid_card_auto_sizes_to_its_flowchart() {
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let src = "flowchart TD\nA[Lift cup] --> B[Bring to lips]\nB --> C{Too hot?}\n\
                   C -->|Yes| D[Blow and wait]\nD --> B\nC -->|No| E[Sip and swallow]\n\
                   E --> F{Coffee left?}\nF -->|Yes| A\nF -->|No| G[Set cup down]\n";
        let size = mermaid_card_size(src, false, false, &theme, &fonts).unwrap();
        // The card wraps the natural layout plus margins, inside the clamp.
        let d = crate::diagram::from_mermaid(src).unwrap();
        let nat = crate::diagram::natural_size(&d, &theme, &fonts);
        assert!(
            size[0] >= nat[0] && size[1] >= nat[1],
            "{size:?} vs {nat:?}"
        );
        assert!((280.0..=1100.0).contains(&size[1]), "{size:?}");
        assert!((420.0..=1440.0).contains(&size[0]), "{size:?}");
        // Title and note buy their chrome back.
        let titled = mermaid_card_size(src, true, true, &theme, &fonts).unwrap();
        assert!(titled[1] > size[1], "{titled:?} vs {size:?}");
        // A tiny graph still gets a readable card, not a stamp.
        let tiny =
            mermaid_card_size("flowchart TD\nA --> B\n", false, false, &theme, &fonts).unwrap();
        assert_eq!(tiny, [420.0, 280.0]);
        // And the sized board renders without errors.
        let spec = ShowSpec {
            title: None,
            note: None,
            chart: None,
            table: None,
            text: None,
            mermaid: Some(src.to_string()),
        };
        let board = build_board(&spec, size, "talk-dark").unwrap();
        let out = render_page(
            &board,
            0,
            &theme,
            &FontStack::new(&[]),
            RasterParams::default(),
        )
        .unwrap();
        assert!(
            !out.diagnostics
                .iter()
                .any(|d| d.severity == crate::Severity::Error),
            "{:?}",
            out.diagnostics
        );
    }

    #[test]
    fn unreadable_mermaid_is_refused_loudly() {
        let spec: ShowSpec =
            serde_json::from_str(r#"{"mermaid": "flowchart TD\n%% nothing"}"#).unwrap();
        assert!(build_board(&spec, [720.0, 450.0], "talk-dark").is_err());
    }

    #[test]
    fn spec_ids_are_stable_and_content_derived() {
        assert_eq!(spec_id("abc"), spec_id("abc"));
        assert_ne!(spec_id("abc"), spec_id("abd"));
        assert_eq!(spec_id("abc").len(), 4);
    }

    #[test]
    fn unknown_spec_fields_are_refused_loudly() {
        // deny_unknown_fields: a typo like "vals" must not silently produce an
        // empty chart.
        let r: Result<ShowSpec, _> = serde_json::from_str(r#"{"chrat": {}}"#);
        assert!(r.is_err());
    }

    /// One `board show` pipe with `marks: [{"mark": "box"}]` and precomputed
    /// five-number rows — the transcript failure this passthrough exists for.
    const BOX_SPEC: &str = r#"{
      "title": "Latency by day",
      "chart": {
        "x": "day", "y": "med", "marks": [{"mark": "box"}],
        "values": [
          {"day": "Mon", "lo": 1, "q1": 2, "med": 3, "q3": 4, "hi": 5},
          {"day": "Tue", "lo": 2, "q1": 3, "med": 4, "q3": 5, "hi": 6}
        ]
      }
    }"#;

    #[test]
    fn a_rich_spec_passes_marks_through_and_skips_the_editorial_sugars() {
        let spec: ShowSpec = serde_json::from_str(BOX_SPEC).unwrap();
        let board = build_board(&spec, [720.0, 450.0], "talk-dark").unwrap();
        let Object::Chart(c) = &board.pages[0].objects[1] else {
            panic!()
        };
        assert_eq!(c.marks.len(), 1);
        assert_eq!(c.marks[0].mark, crate::schema::MarkKind::Box);
        // Stated marks are a composition: no injected sort, no transpose.
        assert!(c.x.as_ref().unwrap().sort.is_none());
        assert_eq!(c.x.as_ref().unwrap().field, "day");
    }

    #[test]
    fn a_box_spec_draws_box_geometry_not_inferred_bars() {
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let render = |json: &str| {
            let spec: ShowSpec = serde_json::from_str(json).unwrap();
            let board = build_board(&spec, [720.0, 450.0], "talk-dark").unwrap();
            let Object::Chart(c) = &board.pages[0].objects[1] else {
                panic!()
            };
            let frame = crate::schema::Frame {
                x: 0.0,
                y: 0.0,
                w: 640.0,
                h: 312.0,
            };
            let scene = crate::chart::build(c, frame, &theme, &fonts);
            let png = render_page(&board, 0, &theme, &fonts, RasterParams::default())
                .unwrap()
                .png;
            (scene, png)
        };
        let (box_scene, box_png) = render(BOX_SPEC);
        // The same rows with no marks infer a bar chart of `med` alone.
        let bar_spec = BOX_SPEC.replace(r#""marks": [{"mark": "box"}],"#, "");
        let (bar_scene, bar_png) = render(&bar_spec);

        let rects = |s: &crate::chart::ChartScene| {
            s.items
                .iter()
                .filter(|i| matches!(i, crate::chart::ChartItem::Rect { .. }))
                .count()
        };
        let paths = |s: &crate::chart::ChartScene| {
            s.items
                .iter()
                .filter(|i| matches!(i, crate::chart::ChartItem::Path { .. }))
                .count()
        };
        // One IQR rect per category either way, but the box adds whisker
        // spines, caps, and median ticks — visible extra path geometry.
        assert_eq!(rects(&box_scene), 2, "{:?}", box_scene.problems);
        assert!(
            paths(&box_scene) > paths(&bar_scene),
            "box whiskers/medians must draw: {} vs {}",
            paths(&box_scene),
            paths(&bar_scene)
        );
        // And the pixels differ — the mark was honored, not silently dropped.
        assert_ne!(box_png, bar_png);
    }

    #[test]
    fn a_singular_mark_key_is_sugar_for_one_layer() {
        // The vega-familiar spelling: `"mark": "box"` used to fall into the
        // lenient extras while an inferred bar drew — silent and wrong.
        let spec: ShowSpec = serde_json::from_str(
            r#"{"chart": {"x": "day", "y": "med", "mark": "box",
                "values": [{"day":"Mon","lo":1,"q1":2,"med":3,"q3":4,"hi":5}]}}"#,
        )
        .unwrap();
        let board = build_board(&spec, [720.0, 450.0], "talk-dark").unwrap();
        let Object::Chart(c) = &board.pages[0].objects[0] else {
            panic!()
        };
        assert_eq!(c.marks.len(), 1);
        assert_eq!(c.marks[0].mark, crate::schema::MarkKind::Box);
        // An object form carries per-layer fields through.
        let spec: ShowSpec = serde_json::from_str(
            r#"{"chart": {"x": "d", "y": "v", "mark": {"mark": "line", "step": "post"},
                "values": [{"d":1,"v":2},{"d":2,"v":3}]}}"#,
        )
        .unwrap();
        let board = build_board(&spec, [720.0, 450.0], "talk-dark").unwrap();
        let Object::Chart(c) = &board.pages[0].objects[0] else {
            panic!()
        };
        assert_eq!(c.marks[0].mark, crate::schema::MarkKind::Line);
        assert_eq!(c.marks[0].step.as_deref(), Some("post"));
    }

    #[test]
    fn sugar_specs_keep_the_editorial_sugars_byte_identically() {
        // The plain x/y/values spec is untouched by the passthrough: sort and
        // flip still apply exactly as before.
        let spec: ShowSpec = serde_json::from_str(SPEC).unwrap();
        let board = build_board(&spec, [720.0, 450.0], "talk-dark").unwrap();
        let Object::Chart(c) = &board.pages[0].objects[1] else {
            panic!()
        };
        assert_eq!(c.x.as_ref().unwrap().sort.as_deref(), Some("-failures"));
        assert_eq!(c.marks[0].mark, crate::schema::MarkKind::Bar);
    }

    #[test]
    fn unknown_chart_keys_stay_lenient() {
        // The chart value rides the schema's lenient Extra: an unknown key is
        // preserved, never a parse refusal — same contract as a board file.
        let spec: ShowSpec = serde_json::from_str(
            r#"{"chart": {"x": "f", "y": "v", "legendPosition": "top",
                "values": [{"f":"a","v":1}]}}"#,
        )
        .unwrap();
        let board = build_board(&spec, [720.0, 450.0], "talk-dark").unwrap();
        let Object::Chart(c) = &board.pages[0].objects[0] else {
            panic!()
        };
        assert!(c.extra.contains_key("legendPosition"));
    }

    #[test]
    fn trace_and_inputs_ride_into_data_from_the_sugar_form() {
        let spec: ShowSpec = serde_json::from_str(
            r#"{"chart": {"x": "day", "y": "med", "mark": "box",
                "trace": "quartiles via numpy.percentile over latency_ms, seed 42",
                "inputs": ["results/latency.csv"],
                "values": [{"day":"Mon","lo":1,"q1":2,"med":3,"q3":4,"hi":5}]}}"#,
        )
        .unwrap();
        let board = build_board(&spec, [720.0, 450.0], "talk-dark").unwrap();
        let Object::Chart(c) = &board.pages[0].objects[0] else {
            panic!()
        };
        assert_eq!(
            c.data.trace.as_deref(),
            Some("quartiles via numpy.percentile over latency_ms, seed 42")
        );
        assert_eq!(
            c.data.inputs.as_deref(),
            Some(&["results/latency.csv".to_string()][..])
        );
    }

    #[test]
    fn top_level_values_fold_into_an_explicit_data_object() {
        // The trap a live agent hit: stating `data` (for origin/trace) while
        // keeping `values` top-level silently rendered zero rows.
        let spec: ShowSpec = serde_json::from_str(
            r#"{"chart": {"x": "f", "y": "v",
                "data": {"origin": "derived-by-agent", "trace": "sums per file"},
                "values": [{"f":"a","v":1},{"f":"b","v":2}]}}"#,
        )
        .unwrap();
        let board = build_board(&spec, [720.0, 450.0], "talk-dark").unwrap();
        let Object::Chart(c) = &board.pages[0].objects[0] else {
            panic!()
        };
        assert_eq!(c.data.values.len(), 2, "values fold into the stated data");
        assert_eq!(format!("{:?}", c.data.origin), "DerivedByAgent");
        assert_eq!(c.data.trace.as_deref(), Some("sums per file"));
    }
}
