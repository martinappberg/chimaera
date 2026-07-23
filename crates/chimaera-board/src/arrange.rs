//! `board arrange` — the multi-object geometry verbs.
//!
//! Align, distribute and grid over named objects, as one pure function the
//! CLI (and later the pane) wraps. The vocabulary is deliberately the
//! smallest one every design tool shares; anything cleverer (packing,
//! auto-layout) belongs to the slot engine, which places by construction
//! rather than by correction.
//!
//! Only objects with **explicit** geometry can be arranged: a slot-placed
//! object's frame is derived at read time, so writing an `at` to it would
//! silently convert it to hand-placed — that conversion is the author's call,
//! never a side effect of an align. Unknown ids and slot-placed targets are
//! errors naming the object, not skips.

use anyhow::{bail, Result};

use crate::schema::{Board, Frame, Object};

/// A frame measurement (an edge or an extent), named so tuple types carrying
/// one stay readable.
type EdgeOf = fn(&Frame) -> f64;

/// The verbs. `align-*` snaps every later object to the FIRST id's edge or
/// center; `distribute-*` equalizes the gaps between the two spatial extremes;
/// `grid` flows the objects into `cols` columns inside their union bbox.
pub const OPS: &[&str] = &[
    "align-left",
    "align-right",
    "align-top",
    "align-bottom",
    "align-center-h",
    "align-center-v",
    "distribute-h",
    "distribute-v",
    "grid",
];

/// Apply `op` to `ids` (in the order given), returning one line per moved
/// object — `moved b to x=80` — so the CLI can print exactly what happened.
///
/// `gap` applies to `grid` (defaulting to the caller's theme gap); `cols`
/// picks the grid's column count (default: ceil(sqrt(n)), the squarest fit).
/// Positions are written as stated; the caller's save pipeline (normalize)
/// owns the grid snap, exactly as it does for a pane drag.
pub fn arrange(
    board: &mut Board,
    op: &str,
    ids: &[&str],
    gap: Option<f64>,
    cols: Option<usize>,
) -> Result<Vec<String>> {
    if ids.len() < 2 {
        bail!("arrange wants at least two ids; got {}", ids.len());
    }

    // Collect every target's current frame first, so the op computes over a
    // consistent snapshot and every error is raised before anything moves.
    let mut frames: Vec<Frame> = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(obj) = find_object(board, id) else {
            bail!("no object {id:?} in this board");
        };
        if obj.slot().is_some() && obj.frame().is_none() {
            bail!(
                "{id:?} is slot-placed; its geometry is derived, so arranging it would silently \
                 convert it to hand-placed — give it an explicit at/size first"
            );
        }
        let Some(f) = obj.frame() else {
            bail!("{id:?} has no explicit at/size to arrange");
        };
        frames.push(f);
    }

    let mut moves: Vec<(usize, f64, f64)> = Vec::new(); // (ids index, new x, new y)
    match op {
        "align-left" | "align-right" | "align-top" | "align-bottom" | "align-center-h"
        | "align-center-v" => {
            let a = frames[0];
            for (i, f) in frames.iter().enumerate().skip(1) {
                let (x, y) = match op {
                    "align-left" => (a.x, f.y),
                    "align-right" => (a.right() - f.w, f.y),
                    "align-top" => (f.x, a.y),
                    "align-bottom" => (f.x, a.bottom() - f.h),
                    // center-h aligns horizontal centers (moves in x);
                    // center-v aligns vertical centers (moves in y).
                    "align-center-h" => (a.cx() - f.w / 2.0, f.y),
                    _ => (f.x, a.cy() - f.h / 2.0),
                };
                moves.push((i, x, y));
            }
        }
        "distribute-h" | "distribute-v" => {
            let horizontal = op == "distribute-h";
            if ids.len() < 3 {
                bail!("{op} wants at least three ids; two objects have only one gap");
            }
            // Spatial order, not argument order: the two extremes stay put
            // and the middle spreads between them with equal gaps.
            let mut order: Vec<usize> = (0..frames.len()).collect();
            order.sort_by(|&a, &b| {
                let (pa, pb) = if horizontal {
                    (frames[a].x, frames[b].x)
                } else {
                    (frames[a].y, frames[b].y)
                };
                pa.partial_cmp(&pb)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.cmp(&b))
            });
            let first = frames[order[0]];
            let last = frames[*order.last().expect("len >= 3")];
            let (span, extent): (f64, EdgeOf) = if horizontal {
                (last.right() - first.x, |f: &Frame| f.w)
            } else {
                (last.bottom() - first.y, |f: &Frame| f.h)
            };
            let total: f64 = order.iter().map(|&i| extent(&frames[i])).sum();
            let step = (span - total) / (order.len() - 1) as f64;
            let mut cursor = if horizontal { first.x } else { first.y };
            for &i in &order {
                let f = frames[i];
                let (x, y) = if horizontal {
                    (cursor, f.y)
                } else {
                    (f.x, cursor)
                };
                if i != order[0] {
                    moves.push((i, x, y));
                }
                cursor += extent(&f) + step;
            }
        }
        "grid" => {
            let n = ids.len();
            let cols = cols
                .unwrap_or_else(|| (n as f64).sqrt().ceil() as usize)
                .max(1);
            let rows = n.div_ceil(cols);
            let gap = gap.unwrap_or(0.0).max(0.0);
            // The union bbox of the current frames is the canvas the grid
            // fills — arrange reflows what the author already claimed.
            let bbox = union(&frames);
            let cell_w = ((bbox.w - gap * (cols - 1) as f64) / cols as f64).max(1.0);
            let cell_h = ((bbox.h - gap * (rows - 1) as f64) / rows as f64).max(1.0);
            for i in 0..n {
                let (r, c) = (i / cols, i % cols);
                let x = bbox.x + c as f64 * (cell_w + gap);
                let y = bbox.y + r as f64 * (cell_h + gap);
                moves.push((i, x, y));
            }
        }
        other => bail!("unknown arrange op {other:?}; ops are {}", OPS.join(", ")),
    }

    let mut lines = Vec::new();
    for (i, x, y) in moves {
        let f = frames[i];
        if (f.x - x).abs() < 1e-9 && (f.y - y).abs() < 1e-9 {
            continue; // already in place — no move, no journal noise
        }
        let obj = find_object_mut(board, ids[i]).expect("resolved above");
        // Translate by the delta rather than writing `at` directly: a group's
        // frame is its children's union, so aligning a group must carry its
        // children (page-absolute) with it. For a leaf object this is exactly
        // `set_at([x, y])` (old at + delta = the new at).
        crate::schema::translate_object(obj, x - f.x, y - f.y);
        lines.push(match (f.x != x, f.y != y) {
            (true, false) => format!("moved {} to x={x}", ids[i]),
            (false, true) => format!("moved {} to y={y}", ids[i]),
            _ => format!("moved {} to [{x}, {y}]", ids[i]),
        });
    }
    Ok(lines)
}

/// The server-facing verb dispatch: one op over an id-set, no gap/cols knobs
/// (the daemon's arrange gesture is a selection align/distribute or a grid
/// snap, never the reflow's column count). `snap-grid` snaps each object's
/// `at` to the board's `canvas.grid`; every other verb is [`arrange`] with the
/// pane's defaults. Returns one moved-object line per object, like `arrange`.
pub fn arrange_ids(board: &mut Board, op: &str, ids: &[&str]) -> Result<Vec<String>> {
    match op {
        "snap-grid" => snap_to_grid(board, ids),
        op if OPS.contains(&op) => arrange(board, op, ids, None, None),
        other => bail!(
            "unknown arrange op {other:?}; ops are {}, snap-grid",
            OPS.join(", ")
        ),
    }
}

/// Snap each object's top-left `at` to the nearest [`crate::schema::grid_lines`]
/// column-start (and row-start, when the grid has rows). Groups snap as a unit
/// (their children ride the delta). Refuses a board with no `canvas.grid` and,
/// like [`arrange`], a slot-placed target whose geometry is derived. Unlike the
/// alignment verbs it takes any number of ids (snapping one object is a valid
/// gesture) and moves nothing already on a line.
fn snap_to_grid(board: &mut Board, ids: &[&str]) -> Result<Vec<String>> {
    if board.canvas.grid.is_none() {
        bail!("snap-grid needs a canvas.grid; this board declares none");
    }
    let (xs, ys) = crate::schema::grid_lines(&board.canvas).expect("grid present");
    let nearest = |lines: &[f64], v: f64| -> Option<f64> {
        lines.iter().copied().min_by(|a, b| {
            (a - v)
                .abs()
                .partial_cmp(&(b - v).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    };

    let mut lines = Vec::new();
    for id in ids {
        let Some(obj) = find_object(board, id) else {
            bail!("no object {id:?} in this board");
        };
        if obj.slot().is_some() && obj.frame().is_none() {
            bail!(
                "{id:?} is slot-placed; its geometry is derived, so snapping it would silently \
                 convert it to hand-placed — give it an explicit at/size first"
            );
        }
        let Some(f) = obj.frame() else {
            bail!("{id:?} has no explicit at/size to snap");
        };
        let nx = nearest(&xs, f.x).unwrap_or(f.x);
        let ny = if ys.is_empty() {
            f.y
        } else {
            nearest(&ys, f.y).unwrap_or(f.y)
        };
        if (nx - f.x).abs() < 1e-9 && (ny - f.y).abs() < 1e-9 {
            continue;
        }
        let obj = find_object_mut(board, id).expect("resolved above");
        crate::schema::translate_object(obj, nx - f.x, ny - f.y);
        lines.push(format!("snapped {id} to [{nx}, {ny}]"));
    }
    Ok(lines)
}

fn union(frames: &[Frame]) -> Frame {
    let mut it = frames.iter();
    let first = it.next().copied().unwrap_or(Frame {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    });
    let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.right(), first.bottom());
    for f in it {
        x0 = x0.min(f.x);
        y0 = y0.min(f.y);
        x1 = x1.max(f.right());
        y1 = y1.max(f.bottom());
    }
    Frame {
        x: x0,
        y: y0,
        w: (x1 - x0).max(1.0),
        h: (y1 - y0).max(1.0),
    }
}

fn find_object<'a>(board: &'a Board, id: &str) -> Option<&'a Object> {
    board.objects().map(|(_, o)| o).find(|o| o.id() == id)
}

fn find_object_mut<'a>(board: &'a mut Board, id: &str) -> Option<&'a mut Object> {
    for page in &mut board.pages {
        for obj in &mut page.objects {
            if obj.id() == id {
                return Some(obj);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board(objects: &str) -> Board {
        crate::parse(&format!(
            r#"{{"format":"chimaera.board","formatVersion":1,
                "canvas":{{"size":[960,540]}},
                "pages":[{{"id":"p","objects":[{objects}]}}]}}"#
        ))
        .unwrap()
    }

    fn rect(id: &str, at: [f64; 2], size: [f64; 2]) -> String {
        format!(
            r#"{{"id":"{id}","type":"shape","geo":"rect","at":[{},{}],"size":[{},{}]}}"#,
            at[0], at[1], size[0], size[1]
        )
    }

    fn frame_of(b: &Board, id: &str) -> Frame {
        find_object(b, id).unwrap().frame().unwrap()
    }

    #[test]
    fn align_left_snaps_later_objects_to_the_first_ids_edge() {
        let mut b = board(
            &[
                rect("a", [80.0, 64.0], [160.0, 80.0]),
                rect("b", [96.0, 200.0], [120.0, 80.0]),
                rect("c", [80.0, 320.0], [200.0, 80.0]),
            ]
            .join(","),
        );
        let lines = arrange(&mut b, "align-left", &["a", "b", "c"], None, None).unwrap();
        assert_eq!(frame_of(&b, "b").x, 80.0);
        assert_eq!(frame_of(&b, "b").y, 200.0, "align-left never moves y");
        assert_eq!(frame_of(&b, "a").x, 80.0, "the first id is the anchor");
        // c was already at 80: no move reported for it.
        assert_eq!(lines, ["moved b to x=80"]);
    }

    #[test]
    fn align_right_bottom_and_centers_use_exact_arithmetic() {
        let mut b = board(
            &[
                rect("a", [80.0, 64.0], [160.0, 80.0]), // right 240, bottom 144, cx 160, cy 104
                rect("b", [400.0, 200.0], [120.0, 40.0]),
            ]
            .join(","),
        );
        arrange(&mut b, "align-right", &["a", "b"], None, None).unwrap();
        assert_eq!(frame_of(&b, "b").x, 120.0); // 240 - 120
        arrange(&mut b, "align-bottom", &["a", "b"], None, None).unwrap();
        assert_eq!(frame_of(&b, "b").y, 104.0); // 144 - 40
        arrange(&mut b, "align-center-h", &["a", "b"], None, None).unwrap();
        assert_eq!(frame_of(&b, "b").x, 100.0); // 160 - 60
        arrange(&mut b, "align-center-v", &["a", "b"], None, None).unwrap();
        assert_eq!(frame_of(&b, "b").y, 84.0); // 104 - 20
    }

    #[test]
    fn distribute_h_equalizes_the_gaps_between_the_extremes() {
        // a [0..80], c [200..280], b [500..580]: span 580, widths 240,
        // so each gap is (580 - 240) / 2 = 170.
        let mut b = board(
            &[
                rect("a", [0.0, 0.0], [80.0, 40.0]),
                rect("b", [500.0, 0.0], [80.0, 40.0]),
                rect("c", [200.0, 0.0], [80.0, 40.0]),
            ]
            .join(","),
        );
        let lines = arrange(&mut b, "distribute-h", &["a", "b", "c"], None, None).unwrap();
        assert_eq!(frame_of(&b, "a").x, 0.0, "the left extreme stays put");
        assert_eq!(frame_of(&b, "c").x, 250.0); // 0 + 80 + 170
        assert_eq!(frame_of(&b, "b").x, 500.0, "the right extreme stays put");
        assert_eq!(lines, ["moved c to x=250"]);
    }

    #[test]
    fn distribute_v_works_down_the_column() {
        // a [0..40], c [100..140], b [300..340]: span 340, heights 120,
        // gaps (340 - 120) / 2 = 110.
        let mut b = board(
            &[
                rect("a", [0.0, 0.0], [80.0, 40.0]),
                rect("b", [0.0, 300.0], [80.0, 40.0]),
                rect("c", [0.0, 100.0], [80.0, 40.0]),
            ]
            .join(","),
        );
        arrange(&mut b, "distribute-v", &["a", "b", "c"], None, None).unwrap();
        assert_eq!(frame_of(&b, "c").y, 150.0); // 0 + 40 + 110
        assert_eq!(frame_of(&b, "b").y, 300.0);
    }

    #[test]
    fn grid_flows_ids_in_order_within_the_union_bbox() {
        // Union bbox [0,0]..[400,300]; 2 cols × 2 rows, gap 20 →
        // cells 190×140 at x 0/210, y 0/160.
        let mut b = board(
            &[
                rect("a", [0.0, 0.0], [100.0, 100.0]),
                rect("b", [300.0, 0.0], [100.0, 100.0]),
                rect("c", [0.0, 200.0], [100.0, 100.0]),
                rect("d", [300.0, 200.0], [100.0, 100.0]),
            ]
            .join(","),
        );
        arrange(&mut b, "grid", &["a", "b", "c", "d"], Some(20.0), Some(2)).unwrap();
        assert_eq!(
            (frame_of(&b, "a").x, frame_of(&b, "a").y),
            (0.0, 0.0),
            "cell 1"
        );
        assert_eq!((frame_of(&b, "b").x, frame_of(&b, "b").y), (210.0, 0.0));
        assert_eq!((frame_of(&b, "c").x, frame_of(&b, "c").y), (0.0, 160.0));
        assert_eq!((frame_of(&b, "d").x, frame_of(&b, "d").y), (210.0, 160.0));
        // Sizes are never touched — arrange moves, it does not resize.
        assert_eq!(frame_of(&b, "a").w, 100.0);
    }

    #[test]
    fn an_unknown_id_is_an_error_naming_it() {
        let mut b = board(&rect("a", [0.0, 0.0], [80.0, 40.0]));
        let err = arrange(&mut b, "align-left", &["a", "ghost"], None, None).unwrap_err();
        assert!(err.to_string().contains("ghost"), "{err}");
    }

    #[test]
    fn a_slot_placed_object_is_refused_not_converted() {
        let mut b = board(
            &[
                rect("a", [0.0, 0.0], [80.0, 40.0]),
                r#"{"id":"s","type":"shape","geo":"rect","slot":"body"}"#.to_string(),
            ]
            .join(","),
        );
        let err = arrange(&mut b, "align-left", &["a", "s"], None, None).unwrap_err();
        assert!(err.to_string().contains("slot-placed"), "{err}");
        // Nothing moved: the error is raised before any mutation.
        assert_eq!(frame_of(&b, "a").x, 0.0);
    }

    #[test]
    fn an_unknown_op_lists_the_vocabulary() {
        let mut b = board(
            &[
                rect("a", [0.0, 0.0], [8.0, 8.0]),
                rect("b", [16.0, 0.0], [8.0, 8.0]),
            ]
            .join(","),
        );
        let err = arrange(&mut b, "tidy-up", &["a", "b"], None, None).unwrap_err();
        assert!(err.to_string().contains("distribute-h"), "{err}");
    }

    #[test]
    fn aligning_a_group_carries_its_children() {
        // The group's frame is its children's union [100,100]..[300,180];
        // align-left to `anchor` (x=0) slides the whole group by −100 x.
        let mut b = board(
            &[
                rect("anchor", [0.0, 64.0], [80.0, 80.0]),
                r#"{"id":"g","type":"group","at":[100,100],"size":[200,80],"objects":[
                    {"id":"c1","type":"shape","geo":"rect","at":[100,100],"size":[80,80]},
                    {"id":"c2","type":"shape","geo":"rect","at":[220,100],"size":[80,80]}]}"#
                    .to_string(),
            ]
            .join(","),
        );
        let lines = arrange(&mut b, "align-left", &["anchor", "g"], None, None).unwrap();
        assert_eq!(lines, ["moved g to x=0"]);
        assert_eq!(frame_of(&b, "g").x, 0.0, "the envelope moved");
        assert_eq!(find_object(&b, "c1").unwrap().at(), Some([0.0, 100.0]));
        assert_eq!(find_object(&b, "c2").unwrap().at(), Some([120.0, 100.0]));
    }

    #[test]
    fn snap_grid_snaps_at_to_the_nearest_cell() {
        // 960 × 540, 12 columns → 80 pt column starts. `box` sits at x=83 (a
        // near-miss of the 80 line) and y=205 (nearest row start is 240 in a
        // 6-row grid? no rows here → y untouched).
        let mut b = crate::parse(
            r#"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[960,540],"grid":{"cols":12}},
                "pages":[{"id":"p","objects":[
                  {"id":"box","type":"shape","geo":"rect","at":[83,205],"size":[80,80]}]}]}"#,
        )
        .unwrap();
        let lines = arrange_ids(&mut b, "snap-grid", &["box"]).unwrap();
        assert_eq!(frame_of(&b, "box").x, 80.0, "left edge snaps to the column");
        assert_eq!(frame_of(&b, "box").y, 205.0, "a column-only grid leaves y");
        assert_eq!(lines, ["snapped box to [80, 205]"]);
    }

    #[test]
    fn snap_grid_needs_a_grid() {
        let mut b = board(&rect("a", [3.0, 3.0], [80.0, 80.0]));
        let err = arrange_ids(&mut b, "snap-grid", &["a"]).unwrap_err();
        assert!(err.to_string().contains("canvas.grid"), "{err}");
    }

    #[test]
    fn arrange_ids_reports_the_unknown_op() {
        let mut b = board(&rect("a", [0.0, 0.0], [8.0, 8.0]));
        let err = arrange_ids(&mut b, "tidy-up", &["a"]).unwrap_err();
        assert!(err.to_string().contains("snap-grid"), "{err}");
    }
}
