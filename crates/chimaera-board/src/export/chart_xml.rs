//! The native `c:chart` writer: an **opt-in** exporter optimization
//! (`ChartFidelity::Native`) that emits a real DrawingML chart part plus a
//! minimal embedded workbook, instead of the default grouped vector shapes.
//!
//! Scope is deliberately narrow — a chart maps natively only when its marks
//! are exclusively plain/grouped/stacked bars, a line, or points, on an
//! ordinal-category or linear axis. Everything else (rect/area/errorbar/
//! rule/tick/text marks, interval x2/y2, temporal or log scales, `box`
//! sugar, per-mark overrides) returns `Err(reason)` and the caller degrades
//! that one chart to today's grouped shapes with the reason in its fate.
//!
//! The mode stays opt-in because the plan gates default-on behind a
//! hand-verified "double-click → Edit Data opens" pass in desktop PowerPoint
//! (docs/board-plan.md §11) that has not run yet. Google Slides is known to
//! flatten `c:chart` to a non-editable object; grouped shapes remain the
//! universally editable default.
//!
//! Data resolution is shared with the grouped path — the caller resolves
//! source-bound rows through the workspace exactly as `emit_chart` does, and
//! series colours come from the same `chart::series_color_for` the renderer
//! uses, so the two export paths cannot disagree about numbers or colours.
//!
//! Determinism matches the package writer: fixed part layout, fixed zip
//! mtime, no wall clock, and floats printed with Rust's shortest round-trip
//! `Display`.

use std::fmt::Write as _;
use std::io::Write as _;

use serde_json::Value;

use super::pptx::{emu, esc, srgb, NS_A, NS_C, NS_R, XML_DECL};
use crate::chart::{
    category_of, category_order, in_series, number, series_color_for, series_values,
};
use crate::schema::{Channel, ChannelType, ChartObject, Mark, MarkKind, ScaleKind, Stack};
use crate::theme::Theme;

/// One native chart, ready for the package writer: the `chartN.xml`
/// chartSpace and the `dataN.xlsx` workbook its "Edit Data" opens.
pub(super) struct NativeChartPart {
    pub xml: String,
    pub xlsx: Vec<u8>,
}

/// Fixed axis ids, scoped to one chart part (each native chart is its own
/// part, so reuse across charts is fine).
const AX_CAT: u32 = 100;
const AX_VAL: u32 = 200;

/// Try to express `chart` as a native `c:chart`. `Err` carries the exact
/// reason the caller puts in the fate string; it must read as a feature name,
/// never a stack trace.
pub(super) fn build_native(
    chart: &ChartObject,
    preloaded: Option<&[Value]>,
    theme: &Theme,
) -> Result<NativeChartPart, String> {
    let rows: &[Value] = match preloaded {
        Some(r) => r,
        None => &chart.data.values,
    };
    if rows.is_empty() {
        return Err("no rows resolved".to_string());
    }
    let (Some(xch), Some(ych)) = (chart.x.as_ref(), chart.y.as_ref()) else {
        return Err("chart needs both an x and a y channel".to_string());
    };
    let mark = match chart.marks.as_slice() {
        [m] => m,
        [] => return Err("no marks".to_string()),
        _ => return Err("multiple mark layers".to_string()),
    };
    match mark.mark {
        MarkKind::Bar | MarkKind::Line | MarkKind::Point => {}
        MarkKind::Box => return Err("box sugar".to_string()),
        MarkKind::Rect => return Err("rect (heatmap) marks".to_string()),
        other => return Err(format!("{other:?} marks").to_lowercase()),
    }
    if mark.fields.contains_key("x2") || mark.fields.contains_key("y2") {
        return Err("interval marks (x2/y2 fields)".to_string());
    }
    if !mark.fields.is_empty() {
        return Err("per-mark field overrides".to_string());
    }
    if !mark.values.is_empty() {
        return Err("per-mark row subsets".to_string());
    }
    if mark.step.as_deref() == Some("post") {
        return Err("step interpolation".to_string());
    }
    if mark.dash.is_some() {
        // The native line writer states no dash; dropping one silently would
        // be a fidelity lie, so a dashed series keeps the grouped path.
        return Err("dashed series".to_string());
    }

    // Declared-or-default channel types, exactly as `chart::build` reads them.
    let xt = xch.kind.unwrap_or(ChannelType::Nominal);
    let yt = ych.kind.unwrap_or(ChannelType::Quantitative);
    for (ch, t) in [(xch, xt), (ych, yt)] {
        if matches!(t, ChannelType::Temporal) || ch.scale == Some(ScaleKind::Temporal) {
            return Err("a temporal scale".to_string());
        }
        if ch.scale == Some(ScaleKind::Log) {
            return Err("a log scale".to_string());
        }
    }

    let series = series_values(rows, chart.color.as_ref());
    match mark.mark {
        MarkKind::Point => {
            if mark.stack == Some(Stack::Stack) {
                return Err("stacking on a point mark".to_string());
            }
            if !matches!(xt, ChannelType::Quantitative) || !matches!(yt, ChannelType::Quantitative)
            {
                return Err("points on a category axis".to_string());
            }
            build_scatter(chart, mark, rows, &series, xch, ych, theme)
        }
        MarkKind::Bar | MarkKind::Line => {
            // The same orientation rule as `chart::build` (temporal is
            // already excluded above): categories down y when x is the
            // magnitude.
            let horizontal = matches!(xt, ChannelType::Quantitative)
                && matches!(yt, ChannelType::Nominal | ChannelType::Ordinal);
            let (cat_ch, mag_ch, cat_t, mag_t) = if horizontal {
                (ych, xch, yt, xt)
            } else {
                (xch, ych, xt, yt)
            };
            if !matches!(cat_t, ChannelType::Nominal | ChannelType::Ordinal) {
                return Err(match mark.mark {
                    MarkKind::Bar => "bars over a continuous axis".to_string(),
                    _ => "line marks over a continuous axis".to_string(),
                });
            }
            if !matches!(mag_t, ChannelType::Quantitative) {
                return Err("a non-quantitative value axis".to_string());
            }
            let stacked = mark.stack == Some(Stack::Stack);
            if stacked && mark.mark == MarkKind::Line {
                return Err("stacking on a line mark".to_string());
            }
            build_cat_chart(
                chart, mark, rows, &series, cat_ch, mag_ch, horizontal, stacked, theme,
            )
        }
        _ => unreachable!("gated above"),
    }
}

// ---------------------------------------------------------------------------
// Category charts: c:barChart / c:lineChart
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn build_cat_chart(
    chart: &ChartObject,
    mark: &Mark,
    rows: &[Value],
    series: &[String],
    cat_ch: &Channel,
    mag_ch: &Channel,
    horizontal: bool,
    stacked: bool,
    theme: &Theme,
) -> Result<NativeChartPart, String> {
    let categories = category_order(rows, &cat_ch.field, &mag_ch.field, cat_ch.sort.as_deref());
    if categories.is_empty() {
        return Err("no categories found".to_string());
    }
    let color_field = chart.color.as_ref().map(|c| c.field.as_str());

    // values[si][ci]: at most one value per (series, category) — a category
    // chart cannot state two, and the grouped path would overdraw them.
    let mut table: Vec<Vec<Option<f64>>> = vec![vec![None; categories.len()]; series.len()];
    for (si, s) in series.iter().enumerate() {
        for row in rows {
            if !in_series(row, color_field, s) {
                continue;
            }
            let (Some(c), Some(v)) = (category_of(row, &cat_ch.field), number(row, &mag_ch.field))
            else {
                continue;
            };
            let Some(ci) = categories.iter().position(|x| *x == c) else {
                continue;
            };
            if table[si][ci].is_some() {
                return Err("duplicate rows for one series and category".to_string());
            }
            table[si][ci] = Some(v);
        }
    }

    // --- The workbook grid: A = categories, one column per series ---------
    let ncat = categories.len();
    let mut grid: Vec<Vec<Option<Cell>>> = vec![vec![None; 1 + series.len()]; 1 + ncat];
    for (si, s) in series.iter().enumerate() {
        grid[0][1 + si] = Some(Cell::S(display_name(s, mag_ch)));
    }
    for (ci, c) in categories.iter().enumerate() {
        grid[1 + ci][0] = Some(Cell::S(c.clone()));
        for si in 0..series.len() {
            grid[1 + ci][1 + si] = table[si][ci].map(Cell::N);
        }
    }

    // --- Series XML -------------------------------------------------------
    let cat_ref = format!("Sheet1!$A$2:$A${}", 1 + ncat);
    let cat_cache = str_cache(&categories);
    let mut sers = String::new();
    for (si, s) in series.iter().enumerate() {
        let col = col_letter(1 + si);
        let color = series_color_for(theme, chart, mark, si, series.len());
        let sp = match mark.mark {
            MarkKind::Bar => format!(
                "<c:spPr><a:solidFill>{}</a:solidFill><a:ln><a:noFill/></a:ln></c:spPr>",
                srgb(color, mark.opacity)
            ),
            _ => {
                let w = mark.width.unwrap_or(theme.chart.series_width);
                format!(
                    r#"<c:spPr><a:ln w="{}" cap="rnd"><a:solidFill>{}</a:solidFill><a:round/></a:ln></c:spPr><c:marker><c:symbol val="none"/></c:marker>"#,
                    emu(w).max(1),
                    srgb(color, None)
                )
            }
        };
        let smooth = match mark.mark {
            MarkKind::Line => r#"<c:smooth val="0"/>"#,
            _ => "",
        };
        let _ = write!(
            sers,
            concat!(
                r#"<c:ser><c:idx val="{si}"/><c:order val="{si}"/>"#,
                "<c:tx><c:strRef><c:f>Sheet1!${col}$1</c:f>{name_cache}</c:strRef></c:tx>",
                "{sp}",
                "<c:cat><c:strRef><c:f>{cat_ref}</c:f>{cat_cache}</c:strRef></c:cat>",
                "<c:val><c:numRef><c:f>Sheet1!${col}$2:${col}${last}</c:f>{val_cache}</c:numRef></c:val>",
                "{smooth}</c:ser>"
            ),
            si = si,
            col = col,
            name_cache = str_cache(std::slice::from_ref(&display_name(s, mag_ch))),
            sp = sp,
            cat_ref = cat_ref,
            cat_cache = cat_cache,
            last = 1 + ncat,
            val_cache = num_cache(&table[si]),
            smooth = smooth,
        );
    }

    let plot = match mark.mark {
        MarkKind::Bar => {
            let dir = if horizontal { "bar" } else { "col" };
            let grouping = if stacked { "stacked" } else { "clustered" };
            // Stacked segments must overlap fully or they draw side by side.
            let extra = if stacked {
                r#"<c:gapWidth val="150"/><c:overlap val="100"/>"#
            } else {
                ""
            };
            format!(
                r#"<c:barChart><c:barDir val="{dir}"/><c:grouping val="{grouping}"/><c:varyColors val="0"/>{sers}{extra}<c:axId val="{AX_CAT}"/><c:axId val="{AX_VAL}"/></c:barChart>"#
            )
        }
        _ => format!(
            r#"<c:lineChart><c:grouping val="standard"/><c:varyColors val="0"/>{sers}<c:marker val="1"/><c:axId val="{AX_CAT}"/><c:axId val="{AX_VAL}"/></c:lineChart>"#
        ),
    };

    // Category order in the caches matches the pane; a consumer's default
    // display order for horizontal bars (first category at the bottom) is
    // its own convention — the data order is what we pin.
    let (cat_pos, val_pos) = if horizontal { ("l", "b") } else { ("b", "l") };
    let axes = format!(
        concat!(
            r#"<c:catAx><c:axId val="{cat_id}"/><c:scaling><c:orientation val="minMax"/></c:scaling>"#,
            r#"<c:delete val="0"/><c:axPos val="{cat_pos}"/>{cat_title}<c:crossAx val="{val_id}"/></c:catAx>"#,
            r#"<c:valAx><c:axId val="{val_id}"/><c:scaling><c:orientation val="minMax"/></c:scaling>"#,
            r#"<c:delete val="0"/><c:axPos val="{val_pos}"/><c:majorGridlines/>{val_title}<c:crossAx val="{cat_id}"/></c:valAx>"#
        ),
        cat_id = AX_CAT,
        val_id = AX_VAL,
        cat_pos = cat_pos,
        val_pos = val_pos,
        cat_title = title_frag(cat_ch.title.as_deref()),
        val_title = title_frag(mag_ch.title.as_deref()),
    );

    Ok(NativeChartPart {
        xml: chart_space(&plot, &axes, series.len() > 1),
        xlsx: xlsx(&grid)?,
    })
}

// ---------------------------------------------------------------------------
// Scatter: c:scatterChart
// ---------------------------------------------------------------------------

fn build_scatter(
    chart: &ChartObject,
    mark: &Mark,
    rows: &[Value],
    series: &[String],
    xch: &Channel,
    ych: &Channel,
    theme: &Theme,
) -> Result<NativeChartPart, String> {
    let color_field = chart.color.as_ref().map(|c| c.field.as_str());
    // Per-series point lists; each series gets its own (x, y) column pair in
    // the workbook because series need not share an x set.
    let mut pts: Vec<Vec<(f64, f64)>> = vec![Vec::new(); series.len()];
    for (si, s) in series.iter().enumerate() {
        for row in rows {
            if !in_series(row, color_field, s) {
                continue;
            }
            let (Some(x), Some(y)) = (number(row, &xch.field), number(row, &ych.field)) else {
                continue;
            };
            pts[si].push((x, y));
        }
    }
    if pts.iter().all(|p| p.is_empty()) {
        return Err(format!(
            "no numeric values found in fields {:?} × {:?}",
            xch.field, ych.field
        ));
    }

    let max_pts = pts.iter().map(Vec::len).max().unwrap_or(0);
    let mut grid: Vec<Vec<Option<Cell>>> = vec![vec![None; 2 * series.len()]; 1 + max_pts];
    for (si, s) in series.iter().enumerate() {
        grid[0][2 * si] = Some(Cell::S(
            xch.title.clone().unwrap_or_else(|| xch.field.clone()),
        ));
        grid[0][2 * si + 1] = Some(Cell::S(display_name(s, ych)));
        for (pi, (x, y)) in pts[si].iter().enumerate() {
            grid[1 + pi][2 * si] = Some(Cell::N(*x));
            grid[1 + pi][2 * si + 1] = Some(Cell::N(*y));
        }
    }

    // Marker size is a diameter in whole points (2..72); the mark's `size`
    // is a radius, matching the grouped path's discs.
    let diameter = ((mark.size.unwrap_or(3.0) * 2.0).round() as i64).clamp(2, 72);
    let mut sers = String::new();
    for (si, s) in series.iter().enumerate() {
        let xcol = col_letter(2 * si);
        let ycol = col_letter(2 * si + 1);
        let n = pts[si].len();
        let color = series_color_for(theme, chart, mark, si, series.len());
        let xs: Vec<Option<f64>> = pts[si].iter().map(|(x, _)| Some(*x)).collect();
        let ys: Vec<Option<f64>> = pts[si].iter().map(|(_, y)| Some(*y)).collect();
        let _ = write!(
            sers,
            concat!(
                r#"<c:ser><c:idx val="{si}"/><c:order val="{si}"/>"#,
                "<c:tx><c:strRef><c:f>Sheet1!${ycol}$1</c:f>{name_cache}</c:strRef></c:tx>",
                r#"<c:spPr><a:ln w="19050"><a:noFill/></a:ln></c:spPr>"#,
                r#"<c:marker><c:symbol val="circle"/><c:size val="{size}"/>"#,
                "<c:spPr><a:solidFill>{fill}</a:solidFill><a:ln><a:noFill/></a:ln></c:spPr></c:marker>",
                "<c:xVal><c:numRef><c:f>Sheet1!${xcol}$2:${xcol}${last}</c:f>{x_cache}</c:numRef></c:xVal>",
                "<c:yVal><c:numRef><c:f>Sheet1!${ycol}$2:${ycol}${last}</c:f>{y_cache}</c:numRef></c:yVal>",
                r#"<c:smooth val="0"/></c:ser>"#
            ),
            si = si,
            ycol = ycol,
            name_cache = str_cache(std::slice::from_ref(&display_name(s, ych))),
            size = diameter,
            fill = srgb(color, mark.opacity),
            xcol = xcol,
            last = 1 + n.max(1),
            x_cache = num_cache(&xs),
            y_cache = num_cache(&ys),
        );
    }

    let plot = format!(
        r#"<c:scatterChart><c:scatterStyle val="lineMarker"/><c:varyColors val="0"/>{sers}<c:axId val="{AX_CAT}"/><c:axId val="{AX_VAL}"/></c:scatterChart>"#
    );
    let axes = format!(
        concat!(
            r#"<c:valAx><c:axId val="{x_id}"/><c:scaling><c:orientation val="minMax"/></c:scaling>"#,
            r#"<c:delete val="0"/><c:axPos val="b"/><c:majorGridlines/>{x_title}<c:crossAx val="{y_id}"/></c:valAx>"#,
            r#"<c:valAx><c:axId val="{y_id}"/><c:scaling><c:orientation val="minMax"/></c:scaling>"#,
            r#"<c:delete val="0"/><c:axPos val="l"/><c:majorGridlines/>{y_title}<c:crossAx val="{x_id}"/></c:valAx>"#
        ),
        x_id = AX_CAT,
        y_id = AX_VAL,
        x_title = title_frag(xch.title.as_deref()),
        y_title = title_frag(ych.title.as_deref()),
    );

    Ok(NativeChartPart {
        xml: chart_space(&plot, &axes, series.len() > 1),
        xlsx: xlsx(&grid)?,
    })
}

// ---------------------------------------------------------------------------
// Shared chartSpace + cache fragments
// ---------------------------------------------------------------------------

fn chart_space(plot: &str, axes: &str, legend: bool) -> String {
    // Multi-series native charts carry a legend: the grouped path
    // direct-labels series, and without either the names would be lost.
    let legend = if legend {
        r#"<c:legend><c:legendPos val="b"/><c:overlay val="0"/></c:legend>"#
    } else {
        ""
    };
    format!(
        concat!(
            r#"{decl}<c:chartSpace xmlns:c="{c}" xmlns:a="{a}" xmlns:r="{r}">"#,
            r#"<c:chart><c:autoTitleDeleted val="1"/>"#,
            "<c:plotArea><c:layout/>{plot}{axes}</c:plotArea>",
            "{legend}",
            r#"<c:plotVisOnly val="1"/></c:chart>"#,
            r#"<c:externalData r:id="rId1"><c:autoUpdate val="0"/></c:externalData>"#,
            "</c:chartSpace>"
        ),
        decl = XML_DECL,
        c = NS_C,
        a = NS_A,
        r = NS_R,
        plot = plot,
        axes = axes,
        legend = legend,
    )
}

/// An unnamed single series still needs a name in the deck; the magnitude
/// channel's title (or field) is the honest one.
fn display_name(series: &str, mag_ch: &Channel) -> String {
    if series.is_empty() {
        mag_ch.title.clone().unwrap_or_else(|| mag_ch.field.clone())
    } else {
        series.to_string()
    }
}

fn title_frag(t: Option<&str>) -> String {
    match t {
        Some(s) => format!(
            concat!(
                "<c:title><c:tx><c:rich><a:bodyPr/><a:lstStyle/>",
                "<a:p><a:r><a:t>{}</a:t></a:r></a:p>",
                r#"</c:rich></c:tx><c:overlay val="0"/></c:title>"#
            ),
            esc(s)
        ),
        None => String::new(),
    }
}

fn str_cache(values: &[String]) -> String {
    let mut s = format!(r#"<c:strCache><c:ptCount val="{}"/>"#, values.len());
    for (i, v) in values.iter().enumerate() {
        let _ = write!(s, r#"<c:pt idx="{i}"><c:v>{}</c:v></c:pt>"#, esc(v));
    }
    s.push_str("</c:strCache>");
    s
}

fn num_cache(values: &[Option<f64>]) -> String {
    let mut s = format!(
        r#"<c:numCache><c:formatCode>General</c:formatCode><c:ptCount val="{}"/>"#,
        values.len()
    );
    for (i, v) in values.iter().enumerate() {
        if let Some(v) = v {
            let _ = write!(s, r#"<c:pt idx="{i}"><c:v>{}</c:v></c:pt>"#, fmt_num(*v));
        }
    }
    s.push_str("</c:numCache>");
    s
}

/// Rust's shortest round-trip `Display` — deterministic, exact, and never
/// scientific for the magnitudes charts state.
fn fmt_num(v: f64) -> String {
    format!("{v}")
}

// ---------------------------------------------------------------------------
// The embedded workbook: the verified 5-part minimal xlsx
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum Cell {
    S(String),
    N(f64),
}

fn col_letter(mut i: usize) -> String {
    let mut s = String::new();
    loop {
        s.insert(0, (b'A' + (i % 26) as u8) as char);
        if i < 26 {
            return s;
        }
        i = i / 26 - 1;
    }
}

fn sheet_xml(grid: &[Vec<Option<Cell>>]) -> String {
    let mut s = format!(
        concat!(
            "{decl}",
            r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" "#,
            r#"xmlns:r="{r}"><sheetData>"#
        ),
        decl = XML_DECL,
        r = NS_R,
    );
    for (ri, row) in grid.iter().enumerate() {
        if row.iter().all(Option::is_none) {
            continue;
        }
        let _ = write!(s, r#"<row r="{}">"#, ri + 1);
        for (ci, cell) in row.iter().enumerate() {
            let Some(cell) = cell else { continue };
            let cref = format!("{}{}", col_letter(ci), ri + 1);
            match cell {
                Cell::S(text) => {
                    let _ = write!(
                        s,
                        r#"<c r="{cref}" t="inlineStr"><is><t>{}</t></is></c>"#,
                        esc(text)
                    );
                }
                Cell::N(v) => {
                    let _ = write!(s, r#"<c r="{cref}"><v>{}</v></c>"#, fmt_num(*v));
                }
            }
        }
        s.push_str("</row>");
    }
    s.push_str("</sheetData></worksheet>");
    s
}

/// Assemble the minimal workbook — [Content_Types], root rels, workbook,
/// workbook rels, one sheet with the actual values — zipped with the same
/// fixed mtime as the outer package, so native exports stay byte-stable.
fn xlsx(grid: &[Vec<Option<Cell>>]) -> Result<Vec<u8>, String> {
    let ct = format!(
        concat!(
            "{decl}",
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">"#,
            r#"<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>"#,
            r#"<Default Extension="xml" ContentType="application/xml"/>"#,
            r#"<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>"#,
            r#"<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#,
            "</Types>"
        ),
        decl = XML_DECL,
    );
    let root_rels = format!(
        concat!(
            "{decl}",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
            r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>"#,
            "</Relationships>"
        ),
        decl = XML_DECL,
    );
    let workbook = format!(
        concat!(
            "{decl}",
            r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" "#,
            r#"xmlns:r="{r}"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#
        ),
        decl = XML_DECL,
        r = NS_R,
    );
    let wb_rels = format!(
        concat!(
            "{decl}",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
            r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>"#,
            "</Relationships>"
        ),
        decl = XML_DECL,
    );
    let parts: [(&str, String); 5] = [
        ("[Content_Types].xml", ct),
        ("_rels/.rels", root_rels),
        ("xl/workbook.xml", workbook),
        ("xl/_rels/workbook.xml.rels", wb_rels),
        ("xl/worksheets/sheet1.xml", sheet_xml(grid)),
    ];

    let cursor = std::io::Cursor::new(Vec::new());
    let mut zw = zip::ZipWriter::new(cursor);
    let mtime = zip::DateTime::from_date_and_time(2000, 1, 1, 0, 0, 0)
        .expect("a constant, valid zip datetime");
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(mtime);
    for (name, body) in &parts {
        zw.start_file(name.to_string(), opts)
            .map_err(|e| format!("workbook assembly failed at {name}: {e}"))?;
        zw.write_all(body.as_bytes())
            .map_err(|e| format!("workbook assembly failed at {name}: {e}"))?;
    }
    let cursor = zw
        .finish()
        .map_err(|e| format!("workbook assembly failed: {e}"))?;
    Ok(cursor.into_inner())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::{write_pptx, write_pptx_with, ChartFidelity, ExportTier, PptxOptions};
    use super::*;
    use crate::layout::FontStack;
    use crate::schema::Board;
    use std::io::Read as _;

    fn board(json: &str) -> Board {
        let mut b = crate::parse(json).unwrap();
        crate::normalize(&mut b);
        b
    }

    fn write_native(b: &Board) -> (Vec<u8>, super::super::ExportReport) {
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let opts = PptxOptions {
            chart_fidelity: ChartFidelity::Native,
        };
        let mut out = Vec::new();
        let report = write_pptx_with(b, &theme, &fonts, None, &opts, &mut out).unwrap();
        (out, report)
    }

    fn names_of(bytes: &[u8]) -> Vec<String> {
        let mut ar = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
        (0..ar.len())
            .map(|i| ar.by_index(i).unwrap().name().to_string())
            .collect()
    }

    fn read_part(bytes: &[u8], name: &str) -> Vec<u8> {
        let mut ar = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
        let mut f = ar
            .by_name(name)
            .unwrap_or_else(|_| panic!("missing {name}"));
        let mut v = Vec::new();
        f.read_to_end(&mut v).unwrap();
        v
    }

    fn read_text(bytes: &[u8], name: &str) -> String {
        String::from_utf8(read_part(bytes, name)).unwrap()
    }

    /// The same tripwire the pptx tests use: balanced tags, balanced
    /// attribute quotes, no raw ampersands. Not a validator.
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

    /// Bar (grouped), line (two series), scatter, and a heatmap that must
    /// fall back — the coverage matrix in one deck.
    const NATIVE_DECK: &str = r#"{
      "format": "chimaera.board", "formatVersion": 1,
      "title": "Native charts",
      "canvas": { "size": [960, 540] },
      "pages": [
        { "id": "p", "objects": [
          { "id": "bars", "type": "chart", "at": [8, 8], "size": [400, 240],
            "data": { "origin": "command", "values": [
              { "fixture": "large.json", "ms": 812, "build": "before" },
              { "fixture": "large.json", "ms": 244, "build": "after" },
              { "fixture": "small.json", "ms": 91, "build": "before" },
              { "fixture": "small.json", "ms": 30, "build": "after" } ] },
            "x": { "field": "fixture", "type": "nominal" },
            "y": { "field": "ms", "type": "quantitative", "title": "Parse time (ms)" },
            "color": { "field": "build" },
            "marks": [ { "mark": "bar", "stack": "group" } ] },
          { "id": "lines", "type": "chart", "at": [420, 8], "size": [400, 240],
            "data": { "origin": "command", "values": [
              { "q": "Q1", "v": 1.5, "team": "a" }, { "q": "Q2", "v": 3, "team": "a" },
              { "q": "Q1", "v": 2, "team": "b" }, { "q": "Q2", "v": 2.5, "team": "b" } ] },
            "x": { "field": "q", "type": "nominal" },
            "y": { "field": "v", "type": "quantitative" },
            "color": { "field": "team" },
            "marks": [ { "mark": "line" } ] },
          { "id": "dots", "type": "chart", "at": [8, 260], "size": [400, 240],
            "data": { "origin": "command", "values": [
              { "x": 0.5, "y": 2.25 }, { "x": 1, "y": 4 }, { "x": 2, "y": 8 } ] },
            "x": { "field": "x", "type": "quantitative" },
            "y": { "field": "y", "type": "quantitative" },
            "marks": [ { "mark": "point" } ] },
          { "id": "heat", "type": "chart", "at": [420, 260], "size": [400, 240],
            "data": { "origin": "command", "values": [
              { "r": "a", "c": "x", "v": 1 }, { "r": "a", "c": "y", "v": 2 },
              { "r": "b", "c": "x", "v": 3 }, { "r": "b", "c": "y", "v": 4 } ] },
            "x": { "field": "c", "type": "nominal" },
            "y": { "field": "r", "type": "nominal" },
            "color": { "field": "v" },
            "marks": [ { "mark": "rect" } ] }
        ] }
      ]
    }"#;

    #[test]
    fn native_mode_exports_real_chart_parts() {
        let b = board(NATIVE_DECK);
        let (bytes, report) = write_native(&b);
        let names = names_of(&bytes);

        // Three native charts and their workbooks; the heatmap adds none.
        for required in [
            "ppt/charts/chart1.xml",
            "ppt/charts/chart2.xml",
            "ppt/charts/chart3.xml",
            "ppt/charts/_rels/chart1.xml.rels",
            "ppt/embeddings/data1.xlsx",
            "ppt/embeddings/data2.xlsx",
            "ppt/embeddings/data3.xlsx",
        ] {
            assert!(names.iter().any(|n| n == required), "missing {required}");
        }
        assert!(!names.iter().any(|n| n.contains("chart4")), "{names:?}");

        // The bar chart: clustered columns, exact caches, series colours.
        let c1 = read_text(&bytes, "ppt/charts/chart1.xml");
        assert_well_formed(&c1);
        assert!(c1.contains(r#"<c:barDir val="col"/>"#), "{c1}");
        assert!(c1.contains(r#"<c:grouping val="clustered"/>"#), "{c1}");
        assert!(c1.contains("<c:v>large.json</c:v>"), "{c1}");
        for v in ["812", "244", "91", "30"] {
            assert!(c1.contains(&format!("<c:v>{v}</c:v>")), "missing {v}: {c1}");
        }
        assert!(c1.contains("<a:srgbClr"), "series colour resolved: {c1}");
        assert!(c1.contains("Parse time (ms)"), "axis title: {c1}");
        // Two series → a legend so names survive without direct labels.
        assert!(c1.contains("<c:legend>"), "{c1}");
        assert!(c1.contains(r#"<c:externalData r:id="rId1">"#), "{c1}");

        // The line chart keeps its float values exactly.
        let c2 = read_text(&bytes, "ppt/charts/chart2.xml");
        assert_well_formed(&c2);
        assert!(c2.contains("<c:lineChart>"), "{c2}");
        assert!(c2.contains("<c:v>1.5</c:v>"), "{c2}");
        assert!(c2.contains("<c:v>2.5</c:v>"), "{c2}");

        // The scatter chart carries x and y caches.
        let c3 = read_text(&bytes, "ppt/charts/chart3.xml");
        assert_well_formed(&c3);
        assert!(c3.contains("<c:scatterChart>"), "{c3}");
        assert!(c3.contains("<c:xVal>"), "{c3}");
        assert!(c3.contains("<c:v>0.5</c:v>"), "{c3}");
        assert!(c3.contains("<c:v>2.25</c:v>"), "{c3}");

        // The embedded workbook is a real zip whose sheet holds the values.
        let xlsx = read_part(&bytes, "ppt/embeddings/data1.xlsx");
        let sheet = read_text(&xlsx, "xl/worksheets/sheet1.xml");
        assert_well_formed(&sheet);
        assert!(sheet.contains("<v>812</v>"), "{sheet}");
        assert!(sheet.contains("<is><t>large.json</t></is>"), "{sheet}");
        assert!(sheet.contains("<is><t>before</t></is>"), "{sheet}");

        // The slide hosts graphicFrames for the native three, a grouped
        // shape tree for the heatmap.
        let slide = read_text(&bytes, "ppt/slides/slide1.xml");
        assert_eq!(slide.matches("<p:graphicFrame>").count(), 3, "{slide}");
        assert!(slide.contains("<p:grpSp>"), "heatmap fallback: {slide}");
        let rels = read_text(&bytes, "ppt/slides/_rels/slide1.xml.rels");
        assert!(rels.contains("../charts/chart1.xml"), "{rels}");

        // Content types declare the chart parts and the workbook default.
        let ct = read_text(&bytes, "[Content_Types].xml");
        assert!(ct.contains(r#"PartName="/ppt/charts/chart1.xml""#), "{ct}");
        assert!(ct.contains(r#"Extension="xlsx""#), "{ct}");

        // Fates: native says native; the heatmap says exactly why not.
        let fate = |id: &str| report.objects.iter().find(|f| f.id == id).unwrap();
        for id in ["bars", "lines", "dots"] {
            assert_eq!(fate(id).tier, ExportTier::Native, "{id}");
            assert!(fate(id).reason.contains("native c:chart"), "{id}");
        }
        assert_eq!(fate("heat").tier, ExportTier::Grouped);
        assert_eq!(
            fate("heat").reason,
            "native chart unsupported: rect (heatmap) marks; exported as grouped shapes"
        );
    }

    #[test]
    fn stacked_and_horizontal_bars_map_grouping_and_direction() {
        let b = board(
            r#"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[960,540]},
                "pages":[{"id":"p","objects":[
                  {"id":"stacked","type":"chart","at":[8,8],"size":[400,240],
                   "data":{"origin":"command","values":[
                     {"k":"a","v":1,"s":"x"},{"k":"a","v":2,"s":"y"},
                     {"k":"b","v":3,"s":"x"},{"k":"b","v":4,"s":"y"}]},
                   "x":{"field":"k","type":"nominal"},
                   "y":{"field":"v","type":"quantitative"},
                   "color":{"field":"s"},
                   "marks":[{"mark":"bar","stack":"stack"}]},
                  {"id":"sideways","type":"chart","at":[8,260],"size":[400,240],
                   "data":{"origin":"command","values":[
                     {"name":"alpha","score":5},{"name":"beta","score":7}]},
                   "x":{"field":"score","type":"quantitative"},
                   "y":{"field":"name","type":"nominal"},
                   "marks":[{"mark":"bar"}]}]}]}"#,
        );
        let (bytes, report) = write_native(&b);
        let c1 = read_text(&bytes, "ppt/charts/chart1.xml");
        assert!(c1.contains(r#"<c:grouping val="stacked"/>"#), "{c1}");
        assert!(c1.contains(r#"<c:overlap val="100"/>"#), "{c1}");
        let c2 = read_text(&bytes, "ppt/charts/chart2.xml");
        assert!(c2.contains(r#"<c:barDir val="bar"/>"#), "{c2}");
        assert!(c2.contains("<c:v>alpha</c:v>"), "{c2}");
        for f in &report.objects {
            assert_eq!(f.tier, ExportTier::Native, "{}: {}", f.id, f.reason);
        }
    }

    #[test]
    fn unsupported_features_fall_back_with_stated_reasons() {
        let b = board(
            r#"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[960,540]},
                "pages":[{"id":"p","objects":[
                  {"id":"logline","type":"chart","at":[8,8],"size":[300,200],
                   "data":{"origin":"command","values":[
                     {"q":"a","v":1},{"q":"b","v":100}]},
                   "x":{"field":"q","type":"nominal"},
                   "y":{"field":"v","type":"quantitative","scale":"log"},
                   "marks":[{"mark":"point"}]},
                  {"id":"band","type":"chart","at":[8,220],"size":[300,200],
                   "data":{"origin":"command","values":[
                     {"lane":"a","from":2,"to":3},{"lane":"b","from":3,"to":4}]},
                   "x":{"field":"lane","type":"nominal"},
                   "y":{"field":"from","type":"quantitative"},
                   "marks":[{"mark":"bar","fields":{"y2":"to"}}]},
                  {"id":"ribbon","type":"chart","at":[640,8],"size":[300,200],
                   "data":{"origin":"command","values":[
                     {"x":1,"y":2,"y2":3},{"x":2,"y":3,"y2":4}]},
                   "x":{"field":"x","type":"quantitative"},
                   "y":{"field":"y","type":"quantitative"},
                   "marks":[{"mark":"area","fields":{"y2":"y2"}}]},
                  {"id":"layered","type":"chart","at":[320,8],"size":[300,200],
                   "data":{"origin":"command","values":[
                     {"k":"a","v":1},{"k":"b","v":2}]},
                   "x":{"field":"k","type":"nominal"},
                   "y":{"field":"v","type":"quantitative"},
                   "marks":[{"mark":"bar"},{"mark":"errorbar","fields":{"err":"e"}}]},
                  {"id":"dated","type":"chart","at":[320,220],"size":[300,200],
                   "data":{"origin":"command","values":[
                     {"d":"2024-01-01","v":1},{"d":"2024-02-01","v":2}]},
                   "x":{"field":"d","type":"temporal"},
                   "y":{"field":"v","type":"quantitative"},
                   "marks":[{"mark":"line"}]}]}]}"#,
        );
        let (bytes, report) = write_native(&b);
        assert!(
            !names_of(&bytes)
                .iter()
                .any(|n| n.starts_with("ppt/charts/")),
            "everything fell back"
        );
        let reason = |id: &str| {
            report
                .objects
                .iter()
                .find(|f| f.id == id)
                .unwrap()
                .reason
                .clone()
        };
        assert_eq!(
            reason("logline"),
            "native chart unsupported: a log scale; exported as grouped shapes"
        );
        assert!(
            reason("band").contains("interval marks (x2/y2 fields)"),
            "{}",
            reason("band")
        );
        assert!(
            reason("ribbon").contains("area marks"),
            "{}",
            reason("ribbon")
        );
        assert!(
            reason("layered").contains("multiple mark layers"),
            "{}",
            reason("layered")
        );
        assert!(
            reason("dated").contains("a temporal scale"),
            "{}",
            reason("dated")
        );
        for id in ["logline", "band", "ribbon", "layered", "dated"] {
            assert_eq!(
                report.objects.iter().find(|f| f.id == id).unwrap().tier,
                ExportTier::Grouped,
                "{id}"
            );
        }
    }

    #[test]
    fn native_mode_writes_identical_bytes() {
        let b = board(NATIVE_DECK);
        let (a, _) = write_native(&b);
        let (c, _) = write_native(&b);
        assert_eq!(a, c, "the native export must be deterministic");
    }

    #[test]
    fn default_export_is_unchanged_by_the_native_writer() {
        let b = board(NATIVE_DECK);
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let mut out = Vec::new();
        let report = write_pptx(&b, &theme, &fonts, None, &mut out).unwrap();
        assert!(
            !names_of(&out).iter().any(|n| n.starts_with("ppt/charts/")),
            "grouped default must not emit chart parts"
        );
        let bars = report.objects.iter().find(|f| f.id == "bars").unwrap();
        assert_eq!(bars.tier, ExportTier::Grouped);
        assert!(
            bars.reason
                .contains("native c:chart is a later optimization"),
            "{}",
            bars.reason
        );
    }

    #[test]
    fn column_letters_cover_the_alphabet_rollover() {
        assert_eq!(col_letter(0), "A");
        assert_eq!(col_letter(1), "B");
        assert_eq!(col_letter(25), "Z");
        assert_eq!(col_letter(26), "AA");
        assert_eq!(col_letter(27), "AB");
        assert_eq!(col_letter(52), "BA");
    }
}
