//! `board arrange` — the multi-object geometry verbs.
//!
//! Align, distribute and grid over named objects, as one pure function the
//! CLI (and later the pane) wraps. The vocabulary is deliberately the
//! smallest one every design tool shares; anything cleverer (packing,
//! auto-layout) belongs to the slot engine, which places by construction
//! rather than by correction.
//!
//! Beside them live the two **structural** verbs, `group` and `ungroup`
//! ([`structural`]): the same refuse-before-mutate contract, but they change
//! page membership rather than geometry, so they answer with the group's
//! identity instead of a list of moves.
//!
//! Only objects with **explicit** geometry can be arranged: a slot-placed
//! object's frame is derived at read time, so writing an `at` to it would
//! silently convert it to hand-placed — that conversion is the author's call,
//! never a side effect of an align. Unknown ids and slot-placed targets are
//! errors naming the object, not skips.

use anyhow::{bail, Result};

use crate::schema::{Board, Extra, Frame, GroupKind, GroupObject, Object};

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
        // `effective_frame`, not `frame`: a group's box is derived from its
        // children, and a hand-authored one stores none at all — reading its
        // stored pair would refuse (or mis-anchor) every agent-written group.
        let Some(f) = crate::normalize::effective_frame(obj) else {
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
            "unknown arrange op {other:?}; ops are {}, snap-grid, {}",
            OPS.join(", "),
            STRUCTURAL_OPS.join(", ")
        ),
    }
}

/// The structural verbs. They change page **membership**, not geometry, so
/// they answer with the group's identity rather than a list of moves — which
/// is why they sit beside [`arrange_ids`] instead of inside it.
pub const STRUCTURAL_OPS: &[&str] = &["group", "ungroup"];

/// What a structural verb did: the group it created or dissolved, the page it
/// lives on, and its members in z-order (array order).
#[derive(Debug, Clone)]
pub struct Structural {
    /// The verb, echoed as the crate's own spelling.
    pub op: &'static str,
    pub group: String,
    pub page: String,
    pub members: Vec<String>,
}

/// Apply a [`STRUCTURAL_OPS`] verb to `ids`. Every refusal is raised before a
/// single object is touched, exactly like [`arrange`], so a rejected gesture
/// leaves the board byte-identical.
pub fn structural(board: &mut Board, op: &str, ids: &[&str]) -> Result<Structural> {
    match op {
        "group" => group_ids(board, ids),
        "ungroup" => ungroup_id(board, ids),
        other => bail!(
            "unknown structural op {other:?}; ops are {}",
            STRUCTURAL_OPS.join(", ")
        ),
    }
}

/// Wrap the named top-level objects in a new group, preserving their relative
/// z-order and landing the group where the topmost member sat.
///
/// The members must be top-level on ONE page: a group's children are the
/// page's own array entries moved wholesale, so an id that is already inside
/// another group, or on a different page, has no well-defined slice to lift.
/// The new group states no `at`/`size` — the envelope is
/// [`crate::normalize`]'s to mint from the children, and writing a second
/// copy here would be a source of truth that drifts on the first child move.
fn group_ids(board: &mut Board, ids: &[&str]) -> Result<Structural> {
    if ids.len() < 2 {
        bail!("group wants at least two ids; got {}", ids.len());
    }
    let mut named = std::collections::BTreeSet::new();
    for id in ids {
        if !named.insert(*id) {
            bail!("{id:?} is named twice; a group's members are a set");
        }
    }

    // Resolve every member to its page slot first, so the gesture is
    // all-or-nothing and the z-order below computes over a consistent
    // snapshot.
    let mut page: Option<usize> = None;
    let mut picks: Vec<usize> = Vec::with_capacity(ids.len());
    for id in ids {
        let (pi, oi) = top_level_position(board, id).ok_or_else(|| not_top_level(board, id))?;
        match page {
            None => page = Some(pi),
            Some(p) if p == pi => {}
            Some(_) => bail!("{id:?} is on another page; a group's members must share one page"),
        }
        let obj = &board.pages[pi].objects[oi];
        if obj.slot().is_some() && obj.frame().is_none() {
            bail!(
                "{id:?} is slot-placed; its geometry is derived, so grouping it would silently \
                 convert it to hand-placed — give it an explicit at/size first"
            );
        }
        picks.push(oi);
    }
    let pi = page.expect("at least two ids resolved");
    picks.sort_unstable();

    // The group takes the topmost member's place once the members are lifted
    // out, so the selection keeps its depth in the page instead of jumping to
    // the front of the z-order.
    let insert_at = picks[picks.len() - 1] + 1 - picks.len();
    let page_id = board.pages[pi].id.clone();
    let group_id = fresh_group_id(board, &page_id, insert_at);

    // Removed high index first, so the lower indices stay valid.
    let mut objects: Vec<Object> = Vec::with_capacity(picks.len());
    for &oi in picks.iter().rev() {
        objects.push(board.pages[pi].objects.remove(oi));
    }
    objects.reverse();
    let members: Vec<String> = objects.iter().map(|o| o.id().to_string()).collect();
    board.pages[pi].objects.insert(
        insert_at,
        Object::Group(GroupObject {
            id: group_id.clone(),
            kind: GroupKind,
            at: None,
            size: None,
            objects,
            alt: None,
            extra: Extra::new(),
        }),
    );
    Ok(Structural {
        op: "group",
        group: group_id,
        page: page_id,
        members,
    })
}

/// Dissolve a group: its children take its own index on the page, in order.
/// They are already page-absolute, so nothing moves — the page renders
/// identically, minus one level of selection.
fn ungroup_id(board: &mut Board, ids: &[&str]) -> Result<Structural> {
    let [id] = ids else {
        bail!("ungroup takes exactly one group id; got {}", ids.len());
    };
    let (pi, oi) = top_level_position(board, id).ok_or_else(|| not_top_level(board, id))?;
    if !matches!(board.pages[pi].objects[oi], Object::Group(_)) {
        bail!(
            "{id:?} is a {}, not a group",
            board.pages[pi].objects[oi].kind()
        );
    }
    let Object::Group(group) = board.pages[pi].objects.remove(oi) else {
        unreachable!("checked above");
    };
    let members: Vec<String> = group.objects.iter().map(|o| o.id().to_string()).collect();
    for (k, child) in group.objects.into_iter().enumerate() {
        board.pages[pi].objects.insert(oi + k, child);
    }
    Ok(Structural {
        op: "ungroup",
        group: group.id,
        page: board.pages[pi].id.clone(),
        members,
    })
}

/// The refusal for an id that is not a page-level object, naming which of the
/// two reasons applies — an id nested in another group is a different (and
/// unsupported) gesture from a typo, and saying so saves a round trip.
fn not_top_level(board: &Board, id: &str) -> anyhow::Error {
    if board.objects().any(|(_, o)| o.id() == id) {
        anyhow::anyhow!(
            "{id:?} is not a top-level object; group/ungroup take objects that sit directly on a \
             page"
        )
    } else {
        anyhow::anyhow!("no object {id:?} in this board")
    }
}

fn top_level_position(board: &Board, id: &str) -> Option<(usize, usize)> {
    board.pages.iter().enumerate().find_map(|(pi, page)| {
        page.objects
            .iter()
            .position(|o| o.id() == id)
            .map(|oi| (pi, oi))
    })
}

/// A new group's id in the crate's own generated-id shape (`<page>-group-<n>`
/// — exactly what [`crate::normalize`] mints for an id-less object), advanced
/// past any collision: an id is the diff anchor, the agent's Edit anchor, the
/// journal subject and the merge key at once, so a duplicate is an error the
/// format never auto-renames away.
fn fresh_group_id(board: &Board, page: &str, index: usize) -> String {
    let taken: std::collections::BTreeSet<&str> = board.objects().map(|(_, o)| o.id()).collect();
    let mut n = index + 1;
    loop {
        let candidate = format!("{page}-group-{n}");
        if !taken.contains(candidate.as_str()) {
            return candidate;
        }
        n += 1;
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
        let Some(f) = crate::normalize::effective_frame(obj) else {
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

    /// The shape an agent actually writes: a group with children and no
    /// stored envelope. Aligning it must work off the child union, not refuse
    /// for "no explicit at/size".
    #[test]
    fn aligning_a_group_with_no_stored_envelope_uses_the_child_union() {
        let mut b = board(
            &[
                rect("anchor", [0.0, 64.0], [80.0, 80.0]),
                r#"{"id":"g","type":"group","objects":[
                    {"id":"c1","type":"shape","geo":"rect","at":[100,100],"size":[80,80]},
                    {"id":"c2","type":"shape","geo":"rect","at":[220,100],"size":[80,80]}]}"#
                    .to_string(),
            ]
            .join(","),
        );
        let lines = arrange(&mut b, "align-left", &["anchor", "g"], None, None).unwrap();
        assert_eq!(lines, ["moved g to x=0"]);
        assert_eq!(find_object(&b, "c1").unwrap().at(), Some([0.0, 100.0]));
        assert_eq!(find_object(&b, "c2").unwrap().at(), Some([120.0, 100.0]));
    }

    fn ids_of(b: &Board) -> Vec<&str> {
        b.pages[0].objects.iter().map(|o| o.id()).collect()
    }

    #[test]
    fn group_wraps_three_objects_keeping_z_order_at_the_topmost_index() {
        let mut b = board(
            &[
                rect("a", [0.0, 0.0], [80.0, 40.0]),
                rect("b", [100.0, 0.0], [80.0, 40.0]),
                rect("c", [200.0, 0.0], [80.0, 40.0]),
                rect("d", [300.0, 0.0], [80.0, 40.0]),
            ]
            .join(","),
        );
        // Named out of z-order on purpose: membership order follows the page.
        let out = structural(&mut b, "group", &["c", "a", "b"]).unwrap();
        assert_eq!(out.op, "group");
        assert_eq!(out.members, ["a", "b", "c"], "relative z-order preserved");
        assert_eq!(out.page, "p");
        // Topmost member was index 2; after lifting three members the group
        // takes index 0 — still below d, which was above all of them.
        assert_eq!(ids_of(&b), [out.group.as_str(), "d"]);
        let Object::Group(g) = &b.pages[0].objects[0] else {
            panic!("a group landed on the page");
        };
        assert_eq!(
            g.objects.iter().map(|o| o.id()).collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        assert_eq!(g.at, None, "the envelope is normalize's to mint");
        assert_eq!(g.size, None);
        // The children kept their page-absolute geometry verbatim.
        assert_eq!(find_object(&b, "b").unwrap().at(), Some([100.0, 0.0]));
        // Generated in the crate's own id shape, off the group's page + index.
        assert_eq!(out.group, "p-group-1");
    }

    #[test]
    fn group_lands_under_the_objects_that_were_above_it() {
        let mut b = board(
            &[
                rect("bottom", [0.0, 0.0], [80.0, 40.0]),
                rect("a", [100.0, 0.0], [80.0, 40.0]),
                rect("b", [200.0, 0.0], [80.0, 40.0]),
                rect("top", [300.0, 0.0], [80.0, 40.0]),
            ]
            .join(","),
        );
        let out = structural(&mut b, "group", &["a", "b"]).unwrap();
        assert_eq!(ids_of(&b), ["bottom", out.group.as_str(), "top"]);
    }

    #[test]
    fn a_generated_group_id_never_collides() {
        let mut b = board(
            &[
                rect("p-group-1", [0.0, 0.0], [80.0, 40.0]),
                rect("a", [100.0, 0.0], [80.0, 40.0]),
                rect("b", [200.0, 0.0], [80.0, 40.0]),
            ]
            .join(","),
        );
        let out = structural(&mut b, "group", &["a", "b"]).unwrap();
        assert_eq!(out.group, "p-group-2", "advanced past the taken id");
    }

    #[test]
    fn ungroup_restores_the_children_at_the_groups_index() {
        let mut b = board(
            &[
                rect("bottom", [0.0, 0.0], [80.0, 40.0]),
                r#"{"id":"g","type":"group","at":[100,100],"size":[200,80],"objects":[
                    {"id":"c1","type":"shape","geo":"rect","at":[100,100],"size":[80,80]},
                    {"id":"c2","type":"shape","geo":"rect","at":[220,100],"size":[80,80]}]}"#
                    .to_string(),
                rect("top", [400.0, 0.0], [80.0, 40.0]),
            ]
            .join(","),
        );
        let out = structural(&mut b, "ungroup", &["g"]).unwrap();
        assert_eq!(out.op, "ungroup");
        assert_eq!(out.group, "g");
        assert_eq!(out.members, ["c1", "c2"]);
        assert_eq!(ids_of(&b), ["bottom", "c1", "c2", "top"]);
        // Children are page-absolute, so dissolving the group moves nothing.
        assert_eq!(find_object(&b, "c1").unwrap().at(), Some([100.0, 100.0]));
        assert_eq!(find_object(&b, "c2").unwrap().at(), Some([220.0, 100.0]));
    }

    #[test]
    fn group_then_ungroup_round_trips_the_page() {
        let mut b = board(
            &[
                rect("a", [0.0, 0.0], [80.0, 40.0]),
                rect("b", [100.0, 0.0], [80.0, 40.0]),
                rect("c", [200.0, 0.0], [80.0, 40.0]),
            ]
            .join(","),
        );
        let before = crate::to_string(&b).unwrap();
        let out = structural(&mut b, "group", &["a", "b"]).unwrap();
        structural(&mut b, "ungroup", &[&out.group]).unwrap();
        assert_eq!(crate::to_string(&b).unwrap(), before, "byte-identical");
    }

    #[test]
    fn group_and_ungroup_refuse_before_mutating() {
        let mut b = board(
            &[
                rect("a", [0.0, 0.0], [80.0, 40.0]),
                rect("b", [100.0, 0.0], [80.0, 40.0]),
                r#"{"id":"s","type":"shape","geo":"rect","slot":"body"}"#.to_string(),
                r#"{"id":"g","type":"group","objects":[
                    {"id":"c1","type":"shape","geo":"rect","at":[300,0],"size":[80,40]}]}"#
                    .to_string(),
            ]
            .join(","),
        );
        let before = crate::to_string(&b).unwrap();
        let cases: &[(&str, &[&str], &str)] = &[
            ("group", &["a"], "at least two"),
            ("group", &["a", "a"], "named twice"),
            ("group", &["a", "ghost"], "no object \"ghost\""),
            ("group", &["a", "s"], "slot-placed"),
            ("group", &["a", "c1"], "not a top-level object"),
            ("ungroup", &["a", "b"], "exactly one"),
            ("ungroup", &["a"], "not a group"),
            ("ungroup", &["ghost"], "no object \"ghost\""),
            ("ungroup", &["c1"], "not a top-level object"),
            ("regroup", &["a", "b"], "unknown structural op"),
        ];
        for (op, ids, needle) in cases {
            let err = structural(&mut b, op, ids).unwrap_err();
            assert!(
                err.to_string().contains(needle),
                "{op} {ids:?}: expected {needle:?}, got {err}"
            );
        }
        assert_eq!(crate::to_string(&b).unwrap(), before, "nothing mutated");
    }

    #[test]
    fn group_refuses_members_on_different_pages() {
        let mut b = crate::parse(
            r#"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[960,540]},
                "pages":[
                  {"id":"p1","objects":[{"id":"a","type":"shape","geo":"rect","at":[0,0],"size":[8,8]}]},
                  {"id":"p2","objects":[{"id":"b","type":"shape","geo":"rect","at":[0,0],"size":[8,8]}]}]}"#,
        )
        .unwrap();
        let err = structural(&mut b, "group", &["a", "b"]).unwrap_err();
        assert!(err.to_string().contains("share one page"), "{err}");
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
