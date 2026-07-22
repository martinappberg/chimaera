//! The native chart: marks over a plot-ready table, with zero transforms.
//!
//! Board computes scales, layout and typography. **Board draws numbers you
//! state; it never derives numbers.** That one line settles most questions
//! that come up here — no binning (so no histogram), no quartiles (so `box`
//! needs a five-number summary), no aggregation, no regression. What arrives
//! in `values` is what gets drawn.
//!
//! The payoff for keeping charts native rather than importing pictures is that
//! they stay lintable *through*: the exporter owns the tick font size, so
//! "this label is 4.6 pt after placement scaling" is computable rather than
//! locked inside a rasterized panel.
//!
//! This module produces a flat list of [`ChartItem`]s in page space. It does
//! no drawing, which is what makes tick selection, gutter arithmetic and
//! stacking testable without a font or a rasterizer in the loop.

use serde_json::Value;

use crate::layout::FontStack;
use crate::schema::{Axes, Channel, ChannelType, ChartObject, Frame, Mark, MarkKind, Stack};
use crate::theme::{Rgb, Theme};

/// A primitive the renderer knows how to draw. Deliberately tiny: everything
/// a chart needs is a rectangle, a polyline, a disc or a string.
#[derive(Debug, Clone)]
pub enum ChartItem {
    Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        fill: Rgb,
        opacity: f64,
    },
    Path {
        points: Vec<(f64, f64)>,
        stroke: Rgb,
        width: f64,
        dash: Option<Vec<f64>>,
    },
    Circle {
        cx: f64,
        cy: f64,
        r: f64,
        fill: Rgb,
    },
    Text {
        x: f64,
        /// Baseline.
        y: f64,
        text: String,
        size: f64,
        weight: u16,
        color: Rgb,
        anchor: TextAnchor,
        families: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAnchor {
    Start,
    Middle,
    End,
}

/// A built chart: what to draw, and what went wrong doing it.
#[derive(Debug, Default)]
pub struct ChartScene {
    pub items: Vec<ChartItem>,
    pub problems: Vec<String>,
}

/// Which axis carries the categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Orient {
    /// Categories along x, magnitudes up y — an ordinary column chart.
    Vertical,
    /// Categories down y, magnitudes along x. Long category labels are
    /// readable here and unreadable rotated 45°, which is why this exists.
    Horizontal,
}

// ---------------------------------------------------------------------------
// Scales
// ---------------------------------------------------------------------------

/// A continuous scale over a numeric domain.
#[derive(Debug, Clone, Copy)]
pub struct LinearScale {
    pub d0: f64,
    pub d1: f64,
    pub r0: f64,
    pub r1: f64,
}

impl LinearScale {
    pub fn map(&self, v: f64) -> f64 {
        if (self.d1 - self.d0).abs() < f64::EPSILON {
            return (self.r0 + self.r1) / 2.0;
        }
        self.r0 + (v - self.d0) / (self.d1 - self.d0) * (self.r1 - self.r0)
    }
}

/// A band scale over ordered categories.
#[derive(Debug, Clone)]
pub struct BandScale {
    pub categories: Vec<String>,
    pub r0: f64,
    pub r1: f64,
    /// Fraction of each band the marks occupy.
    pub ratio: f64,
}

impl BandScale {
    pub fn step(&self) -> f64 {
        if self.categories.is_empty() {
            return 0.0;
        }
        (self.r1 - self.r0) / self.categories.len() as f64
    }

    /// Centre of the band for `category`.
    pub fn center(&self, category: &str) -> Option<f64> {
        let i = self.categories.iter().position(|c| c == category)?;
        Some(self.r0 + self.step() * (i as f64 + 0.5))
    }

    pub fn band_width(&self) -> f64 {
        self.step() * self.ratio
    }
}

/// Nice round tick values covering `[min, max]`.
///
/// The rounding rule is fixed rather than adaptive so the same data yields the
/// same axis on every platform and every run — an axis that quietly shifts
/// between renders makes every diff of a rendered board meaningless.
///
/// Returns the ticks together with the (possibly widened) domain, because
/// widening the domain to the outer ticks is what stops a bar ending exactly
/// on the axis maximum with no headroom.
pub fn nice_ticks(min: f64, max: f64, target: usize) -> (Vec<f64>, f64, f64) {
    let target = target.max(2);
    if !min.is_finite() || !max.is_finite() {
        return (vec![0.0, 1.0], 0.0, 1.0);
    }
    // A degenerate domain still needs a readable axis: pad around the value
    // rather than dividing by zero.
    if (max - min).abs() < f64::EPSILON {
        let v = min;
        let pad = if v.abs() < f64::EPSILON {
            1.0
        } else {
            v.abs() * 0.5
        };
        return nice_ticks(v - pad, v + pad, target);
    }

    // The d3 step rule: round to the nearest of {1, 2, 5, 10}×10ⁿ rather than
    // the next one up. Rounding up gave 0–12 a step of 2.5 — decimals on an
    // axis of integer counts. Nearest gives step 2 there, at the cost of a
    // tick or two over target, which is the right trade.
    let raw = (max - min) / target as f64;
    let mag = 10f64.powf(raw.log10().floor());
    let norm = raw / mag;
    let step = mag
        * if norm < 1.5 {
            1.0
        } else if norm < 3.0 {
            2.0
        } else if norm < 7.0 {
            5.0
        } else {
            10.0
        };

    let d0 = (min / step).floor() * step;
    let d1 = (max / step).ceil() * step;
    let mut ticks = Vec::new();
    let n = ((d1 - d0) / step).round() as i64;
    for i in 0..=n {
        // Multiply rather than accumulate: repeated addition of 0.1 drifts,
        // and a drifted tick prints as 0.30000000000000004.
        ticks.push(d0 + step * i as f64);
    }
    (ticks, d0, d1)
}

/// Format a tick for display.
///
/// Decimal places come from the *step*, not from the value, so an axis reads
/// `0.0 0.5 1.0` rather than `0 0.5 1`. The count is derived by finding the
/// smallest exact decimal representation of the step, which handles 2.5 (one
/// place) as correctly as 0.001 (three).
pub fn format_tick(v: f64, step: f64, fmt: Option<&crate::schema::TickFormat>) -> String {
    if let Some(f) = fmt {
        if let Some(d) = f.decimals {
            return with_separator(&format!("{v:.*}", d as usize), f.sep.unwrap_or(false));
        }
        if f.prefix.unwrap_or(false) {
            return si_prefix(v);
        }
        if let Some(sig) = f.sig {
            return with_separator(&sig_figs(v, sig), f.sep.unwrap_or(false));
        }
    }
    let d = decimals_for(step);
    let s = format!("{v:.*}", d);
    // `-0` is never what anyone means.
    let s = if s
        .trim_start_matches('-')
        .chars()
        .all(|c| c == '0' || c == '.')
    {
        s.trim_start_matches('-').to_string()
    } else {
        s
    };
    with_separator(&s, fmt.and_then(|f| f.sep).unwrap_or(false))
}

fn decimals_for(step: f64) -> usize {
    let step = step.abs();
    if step <= 0.0 || !step.is_finite() {
        return 0;
    }
    for d in 0..=6u32 {
        let scaled = step * 10f64.powi(d as i32);
        if (scaled - scaled.round()).abs() < 1e-9 {
            return d as usize;
        }
    }
    6
}

fn sig_figs(v: f64, sig: u32) -> String {
    if v == 0.0 || !v.is_finite() {
        return "0".to_string();
    }
    let d = (sig as i32 - 1 - v.abs().log10().floor() as i32).max(0) as usize;
    format!("{v:.*}", d)
}

fn si_prefix(v: f64) -> String {
    const UNITS: [(f64, &str); 4] = [(1e9, "G"), (1e6, "M"), (1e3, "k"), (1.0, "")];
    for (scale, suffix) in UNITS {
        if v.abs() >= scale {
            let scaled = v / scale;
            let d = if scaled.abs() < 10.0 && scaled.fract().abs() > 1e-9 {
                1
            } else {
                0
            };
            return format!("{scaled:.*}{suffix}", d);
        }
    }
    format!("{v}")
}

fn with_separator(s: &str, on: bool) -> String {
    if !on {
        return s.to_string();
    }
    let (sign, rest) = match s.strip_prefix('-') {
        Some(r) => ("-", r),
        None => ("", s),
    };
    let (int, frac) = match rest.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (rest, None),
    };
    let mut grouped = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    match frac {
        Some(f) => format!("{sign}{grouped}.{f}"),
        None => format!("{sign}{grouped}"),
    }
}

// ---------------------------------------------------------------------------
// Temporal
// ---------------------------------------------------------------------------

/// Parse an ISO-8601 date or datetime into days since 1970-01-01.
///
/// Accepts `YYYY-MM`, `YYYY-MM-DD` and `YYYY-MM-DDTHH:MM[:SS]`, with a
/// trailing zone designator ignored. Requires at least a month, so the bare
/// string `"2024"` stays nominal — a four-digit year and a category code are
/// indistinguishable, and guessing wrong silently puts categories on a time
/// axis.
pub fn parse_temporal(s: &str) -> Option<f64> {
    let s = s.trim();
    let date = s.split(['T', ' ']).next()?;
    let mut parts = date.split('-');
    let y: i32 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&m) {
        return None;
    }
    let d: u32 = match parts.next() {
        Some(d) => d.parse().ok()?,
        None => 1,
    };
    if !(1..=31).contains(&d) {
        return None;
    }
    if parts.next().is_some() {
        return None;
    }
    let mut days = days_from_civil(y, m, d) as f64;
    if let Some(time) = s.split(['T', ' ']).nth(1) {
        let t = time.trim_end_matches('Z');
        let t = t.split(['+', 'Z']).next().unwrap_or(t);
        let mut tp = t.split(':');
        let hh: f64 = tp.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
        let mm: f64 = tp.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
        let ss: f64 = tp.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
        days += (hh * 3600.0 + mm * 60.0 + ss) / 86_400.0;
    }
    Some(days)
}

/// Days from 1970-01-01 for a civil date. Howard Hinnant's `days_from_civil`,
/// exact for the whole `i32` year range and free of any calendar library.
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = ((m as i64) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse of [`days_from_civil`].
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((if m <= 2 { y + 1 } else { y }) as i32, m, d)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Tick positions and labels for a time span, chosen by span rather than by a
/// nice-number rule — nice numbers over epoch days produce ticks on arbitrary
/// Tuesdays, which is the specific reason a temporal scale cannot just reuse
/// [`nice_ticks`].
pub fn temporal_ticks(d0: f64, d1: f64, target: usize) -> Vec<(f64, String)> {
    let span = (d1 - d0).abs().max(1.0);
    let (y0, m0, _) = civil_from_days(d0.floor() as i64);
    let (y1, m1, _) = civil_from_days(d1.ceil() as i64);
    let mut out = Vec::new();

    if span <= 21.0 {
        // Days.
        let stride = ((span / target as f64).ceil() as i64).max(1);
        let mut day = d0.floor() as i64;
        while (day as f64) <= d1 {
            let (_, m, d) = civil_from_days(day);
            out.push((day as f64, format!("{d} {}", MONTHS[(m - 1) as usize])));
            day += stride;
        }
    } else if span <= 730.0 {
        // Months.
        let months = (y1 - y0) * 12 + (m1 as i32 - m0 as i32);
        let stride = ((months as f64 / target as f64).ceil() as i32).max(1);
        let mut y = y0;
        let mut m = m0 as i32;
        while (y, m) <= (y1, m1 as i32) {
            let day = days_from_civil(y, m as u32, 1) as f64;
            if day >= d0 - 0.5 {
                let label = if m == 1 || out.is_empty() {
                    format!("{} {y}", MONTHS[(m - 1) as usize])
                } else {
                    MONTHS[(m - 1) as usize].to_string()
                };
                out.push((day, label));
            }
            m += stride;
            while m > 12 {
                m -= 12;
                y += 1;
            }
        }
    } else {
        // Years.
        let years = (y1 - y0).max(1);
        let stride = ((years as f64 / target as f64).ceil() as i32).max(1);
        let mut y = y0;
        while y <= y1 {
            let day = days_from_civil(y, 1, 1) as f64;
            if day >= d0 - 0.5 {
                out.push((day, y.to_string()));
            }
            y += stride;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Building
// ---------------------------------------------------------------------------

/// How much room the axis furniture gets, in points.
const TICK_LEN: f64 = 4.0;
const GAP: f64 = 6.0;
/// Above this many bars, value labels are noise rather than help.
const MAX_VALUE_LABELS: usize = 12;

struct Ctx<'a> {
    theme: &'a Theme,
    fonts: &'a FontStack,
    label_family: Vec<String>,
    label_size: f64,
    label_color: Rgb,
    label_weight: u16,
}

/// Build a chart into page-space primitives.
pub fn build(chart: &ChartObject, frame: Frame, theme: &Theme, fonts: &FontStack) -> ChartScene {
    let mut scene = ChartScene::default();
    let label_role = theme.role("label").unwrap_or_else(|| theme.body());
    let ctx = Ctx {
        theme,
        fonts,
        label_family: label_role.family.clone(),
        label_size: label_role.size,
        label_color: theme.color_or_fg(Some(&label_role.color)),
        label_weight: label_role.weight,
    };

    let rows = &chart.data.values;
    if rows.is_empty() {
        scene.problems.push("chart has no rows to draw".to_string());
        return scene;
    }
    let (Some(xch), Some(ych)) = (chart.x.as_ref(), chart.y.as_ref()) else {
        scene
            .problems
            .push("chart needs both an x and a y channel".to_string());
        return scene;
    };

    let xt = xch.kind.unwrap_or(ChannelType::Nominal);
    let yt = ych.kind.unwrap_or(ChannelType::Quantitative);
    let orient = if matches!(xt, ChannelType::Quantitative | ChannelType::Temporal)
        && matches!(yt, ChannelType::Nominal | ChannelType::Ordinal)
    {
        Orient::Horizontal
    } else {
        Orient::Vertical
    };

    // The categorical channel and the magnitude channel, whichever way round.
    let (cat_ch, mag_ch) = match orient {
        Orient::Vertical => (xch, ych),
        Orient::Horizontal => (ych, xch),
    };
    let cat_type = match orient {
        Orient::Vertical => xt,
        Orient::Horizontal => yt,
    };

    let series = series_values(rows, chart.color.as_ref());
    let has_bar = chart.marks.iter().any(|m| m.mark == MarkKind::Bar);

    // Magnitude domain. A bar whose axis does not include zero misstates the
    // ratio it exists to communicate, so zero is forced in rather than left to
    // the author — one of the few places Board overrides an explicit-looking
    // default, and it is worth it.
    let mut mag_min = f64::INFINITY;
    let mut mag_max = f64::NEG_INFINITY;
    for row in rows {
        if let Some(v) = number(row, &mag_ch.field) {
            mag_min = mag_min.min(v);
            mag_max = mag_max.max(v);
        }
        for m in &chart.marks {
            if let Some(err) = m.fields.get("err").and_then(|f| number(row, f)) {
                let base = number(row, &mag_ch.field).unwrap_or(0.0);
                mag_min = mag_min.min(base - err);
                mag_max = mag_max.max(base + err);
            }
        }
    }
    for m in &chart.marks {
        if m.mark == MarkKind::Rule {
            if let Some(v) = m.y.or(m.x) {
                mag_min = mag_min.min(v);
                mag_max = mag_max.max(v);
            }
        }
    }
    if !mag_min.is_finite() || !mag_max.is_finite() {
        scene.problems.push(format!(
            "no numeric values found in field {:?}",
            mag_ch.field
        ));
        return scene;
    }
    if has_bar {
        mag_min = mag_min.min(0.0);
        mag_max = mag_max.max(0.0);
    }

    let (mag_ticks, mag_d0, mag_d1) = match mag_ch.domain {
        Some([a, b]) => {
            let (t, _, _) = nice_ticks(a, b, 5);
            (t.into_iter().filter(|v| *v >= a && *v <= b).collect(), a, b)
        }
        None => nice_ticks(mag_min, mag_max, 5),
    };
    let mag_step = if mag_ticks.len() > 1 {
        mag_ticks[1] - mag_ticks[0]
    } else {
        1.0
    };

    // Categorical or continuous domain for the other axis.
    let categories = if matches!(cat_type, ChannelType::Nominal | ChannelType::Ordinal) {
        category_order(rows, &cat_ch.field, &mag_ch.field, cat_ch.sort.as_deref())
    } else {
        Vec::new()
    };
    let cat_continuous: Option<(f64, f64)> = if categories.is_empty() {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for row in rows {
            if let Some(v) = coord(row, &cat_ch.field, cat_type) {
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        if lo.is_finite() && hi.is_finite() {
            Some((lo, hi))
        } else {
            None
        }
    } else {
        None
    };

    // ---- Gutters -------------------------------------------------------
    // Measured, not guessed: the left gutter is exactly as wide as the widest
    // tick label plus the tick and a gap, which is why the plot area lines up
    // between two charts of different magnitudes placed side by side.
    let cat_labels: Vec<String> = if !categories.is_empty() {
        categories.clone()
    } else if let Some((lo, hi)) = cat_continuous {
        match cat_type {
            ChannelType::Temporal => temporal_ticks(lo, hi, 5)
                .into_iter()
                .map(|(_, l)| l)
                .collect(),
            _ => {
                let (t, _, _) = nice_ticks(lo, hi, 5);
                let st = if t.len() > 1 { t[1] - t[0] } else { 1.0 };
                t.iter()
                    .map(|v| format_tick(*v, st, cat_ch.format.as_ref()))
                    .collect()
            }
        }
    } else {
        Vec::new()
    };

    let mag_label_w = mag_ticks
        .iter()
        .map(|v| {
            ctx.fonts.measure(
                &format_tick(*v, mag_step, mag_ch.format.as_ref()),
                &ctx.label_family,
                ctx.label_size,
                ctx.label_weight,
            )
        })
        .fold(0.0f64, f64::max);
    let cat_label_w = cat_labels
        .iter()
        .map(|l| {
            ctx.fonts
                .measure(l, &ctx.label_family, ctx.label_size, ctx.label_weight)
        })
        .fold(0.0f64, f64::max);
    let line_h = ctx
        .fonts
        .metrics(&ctx.label_family, ctx.label_size, ctx.label_weight);

    let axis_title_h = |ch: &Channel| -> f64 {
        if ch.title.is_some() {
            line_h.height + GAP
        } else {
            0.0
        }
    };

    // A vertical chart's magnitude title reads horizontally at the top-left
    // (never rotated 90° — sideways text is the single most-skipped part of a
    // chart), so it costs top height, not left width.
    let mag_title_on_top = orient == Orient::Vertical && ych.title.is_some();
    let (left, bottom) = match orient {
        Orient::Vertical => (
            mag_label_w + TICK_LEN + GAP,
            line_h.height + TICK_LEN + GAP + axis_title_h(xch),
        ),
        Orient::Horizontal => (
            cat_label_w + TICK_LEN + GAP + axis_title_h(ych),
            line_h.height + TICK_LEN + GAP + axis_title_h(xch),
        ),
    };

    // Series names are direct-labelled, never boxed in a legend. Line and
    // point series get their name at the end of the run; bars get a compact
    // coloured row above the plot, which is the only honest option when bars
    // in a group all end at different heights. The title line and the series
    // row stack — sharing a baseline is how they collide.
    let bar_series_row = has_bar && series.len() > 1;
    let mut top = line_h.height * 0.5;
    if mag_title_on_top {
        top += line_h.height;
    }
    if bar_series_row {
        top += line_h.height + GAP * 0.5;
    }
    let right = if !has_bar && series.len() > 1 {
        series
            .iter()
            .map(|s| {
                ctx.fonts
                    .measure(s, &ctx.label_family, ctx.label_size, ctx.label_weight)
            })
            .fold(0.0f64, f64::max)
            + GAP * 1.5
    } else {
        line_h.height * 0.5
    };

    let plot = Frame {
        x: frame.x + left,
        y: frame.y + top,
        w: (frame.w - left - right).max(8.0),
        h: (frame.h - top - bottom).max(8.0),
    };

    // ---- Scales --------------------------------------------------------
    let mag_scale = match orient {
        Orient::Vertical => LinearScale {
            d0: mag_d0,
            d1: mag_d1,
            r0: plot.bottom(),
            r1: plot.y,
        },
        Orient::Horizontal => LinearScale {
            d0: mag_d0,
            d1: mag_d1,
            r0: plot.x,
            r1: plot.right(),
        },
    };
    let band = BandScale {
        categories: categories.clone(),
        r0: match orient {
            Orient::Vertical => plot.x,
            Orient::Horizontal => plot.y,
        },
        r1: match orient {
            Orient::Vertical => plot.right(),
            Orient::Horizontal => plot.bottom(),
        },
        ratio: theme.chart.bar_ratio,
    };
    let cat_linear = cat_continuous.map(|(lo, hi)| {
        let (d0, d1) = if matches!(cat_type, ChannelType::Temporal) {
            (lo, hi)
        } else {
            let (_, a, b) = nice_ticks(lo, hi, 5);
            (a, b)
        };
        match orient {
            Orient::Vertical => LinearScale {
                d0,
                d1,
                r0: plot.x,
                r1: plot.right(),
            },
            Orient::Horizontal => LinearScale {
                d0,
                d1,
                r0: plot.bottom(),
                r1: plot.y,
            },
        }
    });

    let axes = chart.axes.clone().unwrap_or(Axes {
        spines: None,
        grid: None,
        extra: Default::default(),
    });

    draw_axes(
        &mut scene,
        &ctx,
        &axes,
        plot,
        orient,
        &mag_ticks,
        mag_step,
        mag_scale,
        mag_ch,
        &band,
        cat_linear,
        cat_type,
        cat_ch,
        &cat_labels,
    );

    draw_marks(
        &mut scene, &ctx, chart, plot, orient, mag_scale, &band, cat_linear, cat_type, &series,
        mag_step, mag_ch,
    );

    // The stacked top furniture: the magnitude title on the first line, the
    // series row on the next, both left-aligned with the plot.
    if mag_title_on_top {
        if let Some(t) = &ych.title {
            scene.items.push(ChartItem::Text {
                x: plot.x,
                y: frame.y + line_h.ascent,
                text: t.clone(),
                size: ctx.label_size,
                weight: ctx.label_weight,
                color: ctx.label_color,
                anchor: TextAnchor::Start,
                families: ctx.label_family.clone(),
            });
        }
    }
    if bar_series_row {
        let baseline = if mag_title_on_top {
            frame.y + line_h.height + line_h.ascent
        } else {
            frame.y + line_h.ascent
        };
        draw_series_row(&mut scene, &ctx, plot, &series, baseline);
    }

    scene
}

#[allow(clippy::too_many_arguments)]
fn draw_axes(
    scene: &mut ChartScene,
    ctx: &Ctx,
    axes: &Axes,
    plot: Frame,
    orient: Orient,
    mag_ticks: &[f64],
    mag_step: f64,
    mag_scale: LinearScale,
    mag_ch: &Channel,
    band: &BandScale,
    cat_linear: Option<LinearScale>,
    cat_type: ChannelType,
    cat_ch: &Channel,
    cat_labels: &[String],
) {
    let axis_c = ctx.theme.color_or_fg(Some(&ctx.theme.chart.axis));
    let grid_c = ctx.theme.color_or_fg(Some(&ctx.theme.chart.grid));
    let aw = ctx.theme.chart.axis_width;
    let grid = axes.grid.as_deref().unwrap_or("magnitude");
    let spines = axes
        .spines
        .clone()
        .unwrap_or_else(|| vec!["left".into(), "bottom".into()]);

    // Grid + magnitude ticks.
    for t in mag_ticks {
        let p = mag_scale.map(*t);
        let label = format_tick(*t, mag_step, mag_ch.format.as_ref());
        match orient {
            Orient::Vertical => {
                if grid != "none" {
                    scene.items.push(ChartItem::Path {
                        points: vec![(plot.x, p), (plot.right(), p)],
                        stroke: grid_c,
                        width: aw,
                        dash: None,
                    });
                }
                scene.items.push(ChartItem::Text {
                    x: plot.x - TICK_LEN - GAP * 0.5,
                    y: p + ctx.label_size * 0.36,
                    text: label,
                    size: ctx.label_size,
                    weight: ctx.label_weight,
                    color: ctx.label_color,
                    anchor: TextAnchor::End,
                    families: ctx.label_family.clone(),
                });
            }
            Orient::Horizontal => {
                if grid != "none" {
                    scene.items.push(ChartItem::Path {
                        points: vec![(p, plot.y), (p, plot.bottom())],
                        stroke: grid_c,
                        width: aw,
                        dash: None,
                    });
                }
                scene.items.push(ChartItem::Text {
                    x: p,
                    y: plot.bottom() + TICK_LEN + ctx.label_size,
                    text: label,
                    size: ctx.label_size,
                    weight: ctx.label_weight,
                    color: ctx.label_color,
                    anchor: TextAnchor::Middle,
                    families: ctx.label_family.clone(),
                });
            }
        }
    }

    // Categorical ticks.
    if !band.categories.is_empty() {
        for c in &band.categories {
            let Some(p) = band.center(c) else { continue };
            match orient {
                Orient::Vertical => scene.items.push(ChartItem::Text {
                    x: p,
                    y: plot.bottom() + TICK_LEN + ctx.label_size,
                    text: c.clone(),
                    size: ctx.label_size,
                    weight: ctx.label_weight,
                    color: ctx.label_color,
                    anchor: TextAnchor::Middle,
                    families: ctx.label_family.clone(),
                }),
                Orient::Horizontal => scene.items.push(ChartItem::Text {
                    x: plot.x - TICK_LEN - GAP * 0.5,
                    y: p + ctx.label_size * 0.36,
                    text: c.clone(),
                    size: ctx.label_size,
                    weight: ctx.label_weight,
                    color: ctx.label_color,
                    anchor: TextAnchor::End,
                    families: ctx.label_family.clone(),
                }),
            }
        }
    } else if let Some(scale) = cat_linear {
        let ticks: Vec<(f64, String)> = match cat_type {
            ChannelType::Temporal => temporal_ticks(scale.d0, scale.d1, 5),
            _ => {
                let (t, _, _) = nice_ticks(scale.d0, scale.d1, 5);
                let st = if t.len() > 1 { t[1] - t[0] } else { 1.0 };
                t.into_iter()
                    .map(|v| (v, format_tick(v, st, cat_ch.format.as_ref())))
                    .collect()
            }
        };
        let _ = cat_labels;
        for (v, label) in ticks {
            if v < scale.d0 - 1e-9 || v > scale.d1 + 1e-9 {
                continue;
            }
            let p = scale.map(v);
            match orient {
                Orient::Vertical => scene.items.push(ChartItem::Text {
                    x: p,
                    y: plot.bottom() + TICK_LEN + ctx.label_size,
                    text: label,
                    size: ctx.label_size,
                    weight: ctx.label_weight,
                    color: ctx.label_color,
                    anchor: TextAnchor::Middle,
                    families: ctx.label_family.clone(),
                }),
                Orient::Horizontal => scene.items.push(ChartItem::Text {
                    x: plot.x - TICK_LEN - GAP * 0.5,
                    y: p + ctx.label_size * 0.36,
                    text: label,
                    size: ctx.label_size,
                    weight: ctx.label_weight,
                    color: ctx.label_color,
                    anchor: TextAnchor::End,
                    families: ctx.label_family.clone(),
                }),
            }
        }
    }

    // Spines last, so they sit over the grid.
    for s in &spines {
        let pts = match s.as_str() {
            "left" => Some(vec![(plot.x, plot.y), (plot.x, plot.bottom())]),
            "bottom" => Some(vec![(plot.x, plot.bottom()), (plot.right(), plot.bottom())]),
            "right" => Some(vec![(plot.right(), plot.y), (plot.right(), plot.bottom())]),
            "top" => Some(vec![(plot.x, plot.y), (plot.right(), plot.y)]),
            _ => None,
        };
        if let Some(points) = pts {
            scene.items.push(ChartItem::Path {
                points,
                stroke: axis_c,
                width: aw,
                dash: None,
            });
        }
    }

    // Axis titles. A vertical chart's magnitude title is drawn by `build` as
    // top furniture, so only the horizontal case renders here.
    if let Some(t) = &mag_ch.title {
        if orient == Orient::Horizontal {
            scene.items.push(ChartItem::Text {
                x: plot.cx(),
                y: plot.bottom() + TICK_LEN + ctx.label_size * 2.4,
                text: t.clone(),
                size: ctx.label_size,
                weight: ctx.label_weight,
                color: ctx.label_color,
                anchor: TextAnchor::Middle,
                families: ctx.label_family.clone(),
            });
        }
    }
    if let Some(t) = &cat_ch.title {
        if orient == Orient::Vertical {
            scene.items.push(ChartItem::Text {
                x: plot.cx(),
                y: plot.bottom() + TICK_LEN + ctx.label_size * 2.4,
                text: t.clone(),
                size: ctx.label_size,
                weight: ctx.label_weight,
                color: ctx.label_color,
                anchor: TextAnchor::Middle,
                families: ctx.label_family.clone(),
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_marks(
    scene: &mut ChartScene,
    ctx: &Ctx,
    chart: &ChartObject,
    plot: Frame,
    orient: Orient,
    mag: LinearScale,
    band: &BandScale,
    cat_linear: Option<LinearScale>,
    cat_type: ChannelType,
    series: &[String],
    mag_step: f64,
    mag_ch: &Channel,
) {
    let rows = &chart.data.values;
    let (cat_field, mag_field) = match orient {
        Orient::Vertical => (
            chart.x.as_ref().unwrap().field.clone(),
            chart.y.as_ref().unwrap().field.clone(),
        ),
        Orient::Horizontal => (
            chart.y.as_ref().unwrap().field.clone(),
            chart.x.as_ref().unwrap().field.clone(),
        ),
    };
    let color_field = chart.color.as_ref().map(|c| c.field.clone());

    let bar_marks = chart
        .marks
        .iter()
        .filter(|m| m.mark == MarkKind::Bar)
        .count();
    let total_bars = rows.len();

    for mark in &chart.marks {
        let mark_rows: Vec<&Value> = if mark.values.is_empty() {
            rows.iter().collect()
        } else {
            mark.values.iter().collect()
        };

        match mark.mark {
            MarkKind::Rule => {
                let stroke = mark
                    .stroke
                    .as_deref()
                    .and_then(|c| ctx.theme.color(c))
                    .unwrap_or_else(|| ctx.theme.color_or_fg(Some(&ctx.theme.chart.axis)));
                let dash = mark.dash.clone().or(Some(vec![3.0, 3.0]));
                if let Some(v) = mark.y {
                    let p = mag.map(v);
                    let points = match orient {
                        Orient::Vertical => vec![(plot.x, p), (plot.right(), p)],
                        Orient::Horizontal => vec![(p, plot.y), (p, plot.bottom())],
                    };
                    scene.items.push(ChartItem::Path {
                        points,
                        stroke,
                        width: mark.width.unwrap_or(1.0),
                        dash,
                    });
                    if let Some(label) = &mark.label {
                        scene.items.push(ChartItem::Text {
                            x: plot.right(),
                            y: p - GAP * 0.5,
                            text: label.clone(),
                            size: ctx.label_size,
                            weight: ctx.label_weight,
                            color: stroke,
                            anchor: TextAnchor::End,
                            families: ctx.label_family.clone(),
                        });
                    }
                }
            }
            MarkKind::Bar => {
                let stacked = mark.stack == Some(Stack::Stack);
                let n = if stacked { 1 } else { series.len().max(1) };
                let slot = band.band_width() / n as f64;
                let zero = mag.map(0.0);
                let mut running: std::collections::BTreeMap<String, f64> = Default::default();

                for (si, s) in series.iter().enumerate() {
                    let color = series_color(ctx, chart, mark, si, series.len());
                    for row in &mark_rows {
                        if !in_series(row, color_field.as_deref(), s) {
                            continue;
                        }
                        let Some(cat) = category_of(row, &cat_field) else {
                            continue;
                        };
                        let Some(v) = number(row, &mag_field) else {
                            continue;
                        };
                        let Some(center) = band.center(&cat) else {
                            continue;
                        };
                        let (base, top) = if stacked {
                            let acc = running.entry(cat.clone()).or_insert(0.0);
                            let b = *acc;
                            *acc += v;
                            (mag.map(b), mag.map(*acc))
                        } else {
                            (zero, mag.map(v))
                        };
                        let offset = if stacked {
                            0.0
                        } else {
                            -band.band_width() / 2.0 + slot * (si as f64 + 0.5)
                        };
                        let (x, y, w, h) = match orient {
                            Orient::Vertical => {
                                let bx =
                                    center + offset - slot * 0.5 * if stacked { 0.0 } else { 1.0 };
                                let bw = if stacked { band.band_width() } else { slot };
                                let bx = if stacked {
                                    center - band.band_width() / 2.0
                                } else {
                                    bx
                                };
                                (bx, top.min(base), bw, (base - top).abs())
                            }
                            Orient::Horizontal => {
                                let by =
                                    center + offset - slot * 0.5 * if stacked { 0.0 } else { 1.0 };
                                let bh = if stacked { band.band_width() } else { slot };
                                let by = if stacked {
                                    center - band.band_width() / 2.0
                                } else {
                                    by
                                };
                                (base.min(top), by, (top - base).abs(), bh)
                            }
                        };
                        scene.items.push(ChartItem::Rect {
                            x,
                            y,
                            w: w.max(0.5),
                            h: h.max(0.5),
                            fill: color,
                            opacity: mark.opacity.unwrap_or(1.0),
                        });

                        // Value labels: a small chart is far more readable
                        // with the number on the bar than with the reader
                        // tracking back to an axis. Suppressed once there are
                        // enough bars that the labels become the noise.
                        if total_bars <= MAX_VALUE_LABELS
                            && bar_marks == 1
                            && !chart.marks.iter().any(|m| m.mark == MarkKind::Errorbar)
                        {
                            let text = format_tick(v, mag_step, mag_ch.format.as_ref());
                            let (lx, ly, anchor) = match orient {
                                Orient::Vertical => {
                                    (x + w / 2.0, y - GAP * 0.5, TextAnchor::Middle)
                                }
                                Orient::Horizontal => (
                                    x + w + GAP * 0.5,
                                    y + h / 2.0 + ctx.label_size * 0.36,
                                    TextAnchor::Start,
                                ),
                            };
                            scene.items.push(ChartItem::Text {
                                x: lx,
                                y: ly,
                                text,
                                size: ctx.label_size,
                                weight: ctx.label_weight,
                                color: ctx.label_color,
                                anchor,
                                families: ctx.label_family.clone(),
                            });
                        }
                    }
                }
            }
            MarkKind::Line | MarkKind::Point => {
                for (si, s) in series.iter().enumerate() {
                    let color = series_color(ctx, chart, mark, si, series.len());
                    let mut points: Vec<(f64, f64)> = Vec::new();
                    for row in &mark_rows {
                        if !in_series(row, color_field.as_deref(), s) {
                            continue;
                        }
                        let Some(v) = number(row, &mag_field) else {
                            continue;
                        };
                        let cat_pos = if !band.categories.is_empty() {
                            category_of(row, &cat_field).and_then(|c| band.center(&c))
                        } else {
                            cat_linear
                                .and_then(|sc| coord(row, &cat_field, cat_type).map(|v| sc.map(v)))
                        };
                        let Some(cp) = cat_pos else { continue };
                        let mp = mag.map(v);
                        points.push(match orient {
                            Orient::Vertical => (cp, mp),
                            Orient::Horizontal => (mp, cp),
                        });
                    }
                    if points.is_empty() {
                        continue;
                    }
                    if mark.mark == MarkKind::Line {
                        let pts = if mark.step.as_deref() == Some("post") {
                            step_post(&points, orient)
                        } else {
                            points.clone()
                        };
                        scene.items.push(ChartItem::Path {
                            points: pts,
                            stroke: color,
                            width: mark.width.unwrap_or(ctx.theme.chart.series_width),
                            dash: mark.dash.clone(),
                        });
                    } else {
                        for (px, py) in &points {
                            scene.items.push(ChartItem::Circle {
                                cx: *px,
                                cy: *py,
                                r: mark.size.unwrap_or(3.0),
                                fill: color,
                            });
                        }
                    }
                    // Direct label at the end of the run — the reason there is
                    // no legend.
                    if series.len() > 1 && mark.mark == MarkKind::Line {
                        if let Some((lx, ly)) = points.last() {
                            scene.items.push(ChartItem::Text {
                                x: lx + GAP * 0.75,
                                y: ly + ctx.label_size * 0.36,
                                text: s.clone(),
                                size: ctx.label_size,
                                weight: ctx.label_weight,
                                color,
                                anchor: TextAnchor::Start,
                                families: ctx.label_family.clone(),
                            });
                        }
                    }
                }
            }
            MarkKind::Errorbar => {
                let cap = mark.cap_pt.unwrap_or(3.0);
                let err_field = mark.fields.get("err").cloned();
                // Over a bar, an error bar in the series colour is invisible
                // against its own bar — the lower half disappears entirely.
                // Foreground by default there; an explicit stroke still wins.
                let over_bars = chart.marks.iter().any(|m| m.mark == MarkKind::Bar);
                for (si, s) in series.iter().enumerate() {
                    let color = if mark.stroke.is_none() && over_bars {
                        ctx.theme.color_or_fg(Some("@fg"))
                    } else {
                        series_color(ctx, chart, mark, si, series.len())
                    };
                    let n = series.len().max(1);
                    let slot = band.band_width() / n as f64;
                    for row in &mark_rows {
                        if !in_series(row, color_field.as_deref(), s) {
                            continue;
                        }
                        let Some(v) = number(row, &mag_field) else {
                            continue;
                        };
                        let Some(e) = err_field.as_ref().and_then(|f| number(row, f)) else {
                            continue;
                        };
                        let cat_pos = if !band.categories.is_empty() {
                            category_of(row, &cat_field).and_then(|c| {
                                band.center(&c).map(|center| {
                                    if series.len() > 1 {
                                        center - band.band_width() / 2.0 + slot * (si as f64 + 0.5)
                                    } else {
                                        center
                                    }
                                })
                            })
                        } else {
                            cat_linear
                                .and_then(|sc| coord(row, &cat_field, cat_type).map(|v| sc.map(v)))
                        };
                        let Some(cp) = cat_pos else { continue };
                        let lo = mag.map(v - e);
                        let hi = mag.map(v + e);
                        let w = mark.width.unwrap_or(1.0);
                        let (spine, cap_lo, cap_hi) = match orient {
                            Orient::Vertical => (
                                vec![(cp, lo), (cp, hi)],
                                vec![(cp - cap, lo), (cp + cap, lo)],
                                vec![(cp - cap, hi), (cp + cap, hi)],
                            ),
                            Orient::Horizontal => (
                                vec![(lo, cp), (hi, cp)],
                                vec![(lo, cp - cap), (lo, cp + cap)],
                                vec![(hi, cp - cap), (hi, cp + cap)],
                            ),
                        };
                        for points in [spine, cap_lo, cap_hi] {
                            scene.items.push(ChartItem::Path {
                                points,
                                stroke: color,
                                width: w,
                                dash: None,
                            });
                        }
                    }
                }
            }
            MarkKind::Text => {
                let text_field = mark
                    .fields
                    .get("text")
                    .cloned()
                    .unwrap_or_else(|| "label".to_string());
                for row in &mark_rows {
                    let Some(v) = number(row, &mag_field) else {
                        continue;
                    };
                    let Some(label) = row.get(&text_field).and_then(as_text) else {
                        continue;
                    };
                    let cat_pos = if !band.categories.is_empty() {
                        category_of(row, &cat_field).and_then(|c| band.center(&c))
                    } else {
                        cat_linear
                            .and_then(|sc| coord(row, &cat_field, cat_type).map(|v| sc.map(v)))
                    };
                    let Some(cp) = cat_pos else { continue };
                    let mp = mag.map(v);
                    let (tx, ty) = match orient {
                        Orient::Vertical => (cp, mp - GAP * 0.5),
                        Orient::Horizontal => (mp + GAP * 0.5, cp),
                    };
                    scene.items.push(ChartItem::Text {
                        x: tx,
                        y: ty,
                        text: label,
                        size: ctx.label_size,
                        weight: ctx.label_weight,
                        color: mark
                            .stroke
                            .as_deref()
                            .and_then(|c| ctx.theme.color(c))
                            .unwrap_or(ctx.label_color),
                        anchor: TextAnchor::Middle,
                        families: ctx.label_family.clone(),
                    });
                }
            }
        }
    }
}

fn draw_series_row(
    scene: &mut ChartScene,
    ctx: &Ctx,
    plot: Frame,
    series: &[String],
    baseline: f64,
) {
    let mut x = plot.x;
    let y = baseline;
    for (i, s) in series.iter().enumerate() {
        let color = ctx.theme.categorical(i);
        scene.items.push(ChartItem::Text {
            x,
            y,
            text: s.clone(),
            size: ctx.label_size,
            weight: 600,
            color,
            anchor: TextAnchor::Start,
            families: ctx.label_family.clone(),
        });
        x += ctx.fonts.measure(s, &ctx.label_family, ctx.label_size, 600) + GAP * 2.0;
    }
}

fn step_post(points: &[(f64, f64)], orient: Orient) -> Vec<(f64, f64)> {
    let mut out = Vec::with_capacity(points.len() * 2);
    for (i, p) in points.iter().enumerate() {
        if i == 0 {
            out.push(*p);
            continue;
        }
        let prev = points[i - 1];
        match orient {
            Orient::Vertical => out.push((p.0, prev.1)),
            Orient::Horizontal => out.push((prev.0, p.1)),
        }
        out.push(*p);
    }
    out
}

fn series_color(ctx: &Ctx, chart: &ChartObject, mark: &Mark, i: usize, n: usize) -> Rgb {
    if let Some(c) = mark.fill.as_deref().or(mark.stroke.as_deref()) {
        if let Some(rgb) = ctx.theme.color(c) {
            return rgb;
        }
    }
    if n <= 1 && chart.color.is_none() {
        return ctx.theme.categorical(0);
    }
    ctx.theme.categorical(i)
}

// ---------------------------------------------------------------------------
// Row helpers
// ---------------------------------------------------------------------------

fn number(row: &Value, field: &str) -> Option<f64> {
    row.get(field)?.as_f64()
}

fn as_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn category_of(row: &Value, field: &str) -> Option<String> {
    row.get(field).and_then(as_text)
}

/// A coordinate on a continuous axis, parsing dates where the channel is
/// temporal.
fn coord(row: &Value, field: &str, kind: ChannelType) -> Option<f64> {
    let v = row.get(field)?;
    match kind {
        ChannelType::Temporal => match v {
            Value::String(s) => parse_temporal(s),
            _ => v.as_f64(),
        },
        _ => v.as_f64(),
    }
}

fn in_series(row: &Value, color_field: Option<&str>, series: &str) -> bool {
    match color_field {
        None => true,
        Some(f) => category_of(row, f).as_deref() == Some(series),
    }
}

/// Distinct series values in first-seen order — stable, so a re-render does
/// not reshuffle colours.
fn series_values(rows: &[Value], color: Option<&Channel>) -> Vec<String> {
    let Some(c) = color else {
        return vec![String::new()];
    };
    let mut out: Vec<String> = Vec::new();
    for row in rows {
        if let Some(v) = category_of(row, &c.field) {
            if !out.contains(&v) {
                out.push(v);
            }
        }
    }
    if out.is_empty() {
        vec![String::new()]
    } else {
        out
    }
}

/// Category order: data order by default, or by the declared `sort`.
pub fn category_order(
    rows: &[Value],
    cat_field: &str,
    mag_field: &str,
    sort: Option<&str>,
) -> Vec<String> {
    let mut order: Vec<String> = Vec::new();
    for row in rows {
        if let Some(c) = category_of(row, cat_field) {
            if !order.contains(&c) {
                order.push(c);
            }
        }
    }
    let Some(sort) = sort else { return order };
    let desc = sort.starts_with('-');
    let key = sort.trim_start_matches('-');

    if key == "x" || key == cat_field {
        order.sort();
    } else {
        // Sum the magnitude per category, so a grouped chart sorts by total
        // rather than by whichever series happened to come first.
        let mut totals: Vec<(String, f64)> = order
            .iter()
            .map(|c| {
                let sum = rows
                    .iter()
                    .filter(|r| category_of(r, cat_field).as_deref() == Some(c.as_str()))
                    .filter_map(|r| number(r, mag_field))
                    .sum();
                (c.clone(), sum)
            })
            .collect();
        totals.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        order = totals.into_iter().map(|(c, _)| c).collect();
        if !desc {
            order.reverse();
        }
        return order;
    }
    if desc {
        order.reverse();
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_ticks_are_round_numbers() {
        let (ticks, d0, d1) = nice_ticks(0.0, 812.0, 5);
        assert_eq!(d0, 0.0);
        assert!(d1 >= 812.0);
        assert!(ticks.windows(2).all(|w| w[1] > w[0]));
        // Every tick is a multiple of the step, exactly.
        let step = ticks[1] - ticks[0];
        for t in &ticks {
            assert!((t / step - (t / step).round()).abs() < 1e-9, "{t}");
        }
    }

    #[test]
    fn ticks_never_print_float_noise() {
        // The specific failure this exists to prevent: 0.1 + 0.1 + 0.1.
        let (ticks, _, _) = nice_ticks(0.0, 1.0, 10);
        let step = ticks[1] - ticks[0];
        let rendered: Vec<String> = ticks.iter().map(|t| format_tick(*t, step, None)).collect();
        assert!(
            !rendered.iter().any(|s| s.len() > 6),
            "float noise in {rendered:?}"
        );
        assert!(rendered.contains(&"0.3".to_string()), "{rendered:?}");
    }

    #[test]
    fn decimals_follow_the_step_not_the_value() {
        assert_eq!(format_tick(1.0, 0.5, None), "1.0");
        assert_eq!(format_tick(1.0, 1.0, None), "1");
        assert_eq!(format_tick(2.5, 2.5, None), "2.5");
        assert_eq!(format_tick(0.0, 0.25, None), "0.00");
    }

    #[test]
    fn negative_zero_never_reaches_an_axis() {
        assert_eq!(format_tick(-0.0, 1.0, None), "0");
    }

    #[test]
    fn a_degenerate_domain_still_gets_an_axis() {
        let (ticks, d0, d1) = nice_ticks(5.0, 5.0, 5);
        assert!(ticks.len() >= 2, "{ticks:?}");
        assert!(d0 < 5.0 && d1 > 5.0);
        // And a zero-valued one, which would otherwise pad by zero.
        let (ticks, d0, d1) = nice_ticks(0.0, 0.0, 5);
        assert!(ticks.len() >= 2 && d0 < d1);
    }

    #[test]
    fn thousands_and_si_formatting() {
        let f = crate::schema::TickFormat {
            sig: None,
            decimals: Some(0),
            prefix: None,
            sep: Some(true),
            suffix: None,
            extra: Default::default(),
        };
        assert_eq!(format_tick(1234567.0, 1.0, Some(&f)), "1,234,567");
        let g = crate::schema::TickFormat {
            sig: None,
            decimals: None,
            prefix: Some(true),
            sep: None,
            suffix: None,
            extra: Default::default(),
        };
        assert_eq!(format_tick(1500.0, 1.0, Some(&g)), "1.5k");
        assert_eq!(format_tick(2_000_000.0, 1.0, Some(&g)), "2M");
    }

    #[test]
    fn civil_dates_round_trip() {
        for (y, m, d) in [(1970, 1, 1), (2000, 2, 29), (2026, 7, 22), (1899, 12, 31)] {
            let days = days_from_civil(y, m, d);
            assert_eq!(civil_from_days(days), (y, m, d), "{y}-{m}-{d}");
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn temporal_parsing_requires_at_least_a_month() {
        assert!(parse_temporal("2024").is_none(), "a bare year is ambiguous");
        assert_eq!(parse_temporal("1970-01-01"), Some(0.0));
        assert_eq!(parse_temporal("1970-01"), Some(0.0));
        assert_eq!(parse_temporal("1970-01-02T12:00:00Z"), Some(1.5));
        assert!(parse_temporal("not a date").is_none());
        assert!(parse_temporal("2024-13-01").is_none());
    }

    #[test]
    fn temporal_ticks_land_on_calendar_boundaries() {
        let d0 = days_from_civil(2024, 1, 15) as f64;
        let d1 = days_from_civil(2024, 12, 20) as f64;
        let ticks = temporal_ticks(d0, d1, 5);
        assert!(!ticks.is_empty());
        for (v, _) in &ticks {
            let (_, _, day) = civil_from_days(*v as i64);
            assert_eq!(day, 1, "month ticks must land on the first");
        }
    }

    #[test]
    fn band_scale_centres_are_evenly_spaced() {
        let b = BandScale {
            categories: vec!["a".into(), "b".into(), "c".into()],
            r0: 0.0,
            r1: 300.0,
            ratio: 0.68,
        };
        assert_eq!(b.center("a"), Some(50.0));
        assert_eq!(b.center("b"), Some(150.0));
        assert_eq!(b.center("c"), Some(250.0));
        assert!(b.center("nope").is_none());
    }

    #[test]
    fn sorting_by_magnitude_sums_across_series() {
        let rows: Vec<Value> = serde_json::from_str(
            r#"[{"f":"a","v":1,"s":"x"},{"f":"a","v":1,"s":"y"},
                {"f":"b","v":5,"s":"x"}]"#,
        )
        .unwrap();
        // b (5) outranks a (1+1), which a first-row comparison would miss.
        assert_eq!(category_order(&rows, "f", "v", Some("-v")), ["b", "a"]);
        assert_eq!(category_order(&rows, "f", "v", Some("v")), ["a", "b"]);
        assert_eq!(category_order(&rows, "f", "v", None), ["a", "b"]);
    }

    fn chart_from(json: &str) -> ChartObject {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn a_bar_chart_axis_always_includes_zero() {
        // A bar axis that starts at 800 exaggerates a 5% difference into a
        // visual 10x. Forced, not advised.
        let c = chart_from(
            r#"{"id":"c","type":"chart","at":[0,0],"size":[400,300],
                "data":{"origin":"command","values":[{"f":"a","v":810},{"f":"b","v":812}]},
                "x":{"field":"f","type":"nominal"},
                "y":{"field":"v","type":"quantitative"},
                "marks":[{"mark":"bar"}]}"#,
        );
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let scene = build(
            &c,
            Frame {
                x: 0.0,
                y: 0.0,
                w: 400.0,
                h: 300.0,
            },
            &theme,
            &fonts,
        );
        assert!(scene.problems.is_empty(), "{:?}", scene.problems);
        // The lowest bar must reach the baseline, i.e. a rect touching y=0.
        let rects: Vec<_> = scene
            .items
            .iter()
            .filter_map(|i| match i {
                ChartItem::Rect { y, h, .. } => Some(y + h),
                _ => None,
            })
            .collect();
        assert!(rects.len() >= 2, "expected two bars");
        assert!(
            (rects[0] - rects[1]).abs() < 0.01,
            "bars must share a baseline: {rects:?}"
        );
    }

    #[test]
    fn an_empty_chart_reports_rather_than_drawing_nothing() {
        let c = chart_from(
            r#"{"id":"c","type":"chart","at":[0,0],"size":[400,300],
                "data":{"origin":"command"},
                "x":{"field":"f","type":"nominal"},"y":{"field":"v","type":"quantitative"}}"#,
        );
        let scene = build(
            &c,
            Frame {
                x: 0.0,
                y: 0.0,
                w: 400.0,
                h: 300.0,
            },
            &crate::theme::default_for(true),
            &FontStack::new(&[]),
        );
        assert!(!scene.problems.is_empty());
        assert!(scene.items.is_empty());
    }

    #[test]
    fn grouped_bars_do_not_overlap() {
        let c = chart_from(
            r#"{"id":"c","type":"chart","at":[0,0],"size":[480,320],
                "data":{"origin":"command","values":[
                  {"f":"large.json","ms":812,"build":"before"},
                  {"f":"large.json","ms":244,"build":"after"},
                  {"f":"small.json","ms":91,"build":"before"},
                  {"f":"small.json","ms":30,"build":"after"}]},
                "x":{"field":"f","type":"nominal"},
                "y":{"field":"ms","type":"quantitative"},
                "color":{"field":"build"},
                "marks":[{"mark":"bar","stack":"group"}]}"#,
        );
        let scene = build(
            &c,
            Frame {
                x: 0.0,
                y: 0.0,
                w: 480.0,
                h: 320.0,
            },
            &crate::theme::default_for(true),
            &FontStack::new(&[]),
        );
        assert!(scene.problems.is_empty(), "{:?}", scene.problems);
        let mut spans: Vec<(f64, f64)> = scene
            .items
            .iter()
            .filter_map(|i| match i {
                ChartItem::Rect { x, w, .. } => Some((*x, x + w)),
                _ => None,
            })
            .collect();
        assert_eq!(spans.len(), 4, "four bars");
        spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        for pair in spans.windows(2) {
            assert!(
                pair[0].1 <= pair[1].0 + 0.01,
                "bars overlap: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn stacked_bars_accumulate_rather_than_overwrite() {
        let c = chart_from(
            r#"{"id":"c","type":"chart","at":[0,0],"size":[400,300],
                "data":{"origin":"command","values":[
                  {"f":"a","v":10,"s":"x"},{"f":"a","v":30,"s":"y"}]},
                "x":{"field":"f","type":"nominal"},
                "y":{"field":"v","type":"quantitative"},
                "color":{"field":"s"},
                "marks":[{"mark":"bar","stack":"stack"}]}"#,
        );
        let scene = build(
            &c,
            Frame {
                x: 0.0,
                y: 0.0,
                w: 400.0,
                h: 300.0,
            },
            &crate::theme::default_for(true),
            &FontStack::new(&[]),
        );
        let rects: Vec<_> = scene
            .items
            .iter()
            .filter_map(|i| match i {
                ChartItem::Rect { y, h, .. } => Some((*y, *h)),
                _ => None,
            })
            .collect();
        assert_eq!(rects.len(), 2);
        // The two segments stack: one sits directly on top of the other.
        let (y1, h1) = rects[0];
        let (y2, h2) = rects[1];
        let touching = ((y1 - (y2 + h2)).abs() < 0.01) || ((y2 - (y1 + h1)).abs() < 0.01);
        assert!(touching, "segments should abut: {rects:?}");
    }

    #[test]
    fn a_step_line_only_moves_one_axis_at_a_time() {
        let pts = vec![(0.0, 10.0), (10.0, 20.0)];
        let out = step_post(&pts, Orient::Vertical);
        assert_eq!(out, vec![(0.0, 10.0), (10.0, 10.0), (10.0, 20.0)]);
    }

    #[test]
    fn long_categories_go_horizontal_and_stay_readable() {
        let c = chart_from(
            r#"{"id":"c","type":"chart","at":[0,0],"size":[400,300],
                "data":{"origin":"command","values":[
                  {"n":12,"f":"crates/chimaera-server/src/router.rs"},
                  {"n":3,"f":"crates/chimaera-pty/src/lib.rs"}]},
                "x":{"field":"n","type":"quantitative"},
                "y":{"field":"f","type":"nominal"},
                "marks":[{"mark":"bar"}]}"#,
        );
        let scene = build(
            &c,
            Frame {
                x: 0.0,
                y: 0.0,
                w: 400.0,
                h: 300.0,
            },
            &crate::theme::default_for(true),
            &FontStack::new(&[]),
        );
        assert!(scene.problems.is_empty(), "{:?}", scene.problems);
        // Horizontal bars share a left edge rather than a bottom edge.
        let lefts: Vec<f64> = scene
            .items
            .iter()
            .filter_map(|i| match i {
                ChartItem::Rect { x, .. } => Some(*x),
                _ => None,
            })
            .collect();
        assert_eq!(lefts.len(), 2);
        assert!((lefts[0] - lefts[1]).abs() < 0.01, "{lefts:?}");
    }
}
