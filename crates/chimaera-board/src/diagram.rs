//! `diagram` — nodes + edges + lanes under a deterministic layered layout.
//!
//! The first composite after `chart`: the file stores ~30 lines of intent and
//! [`expand`] computes the five primitives at render time — shapes for nodes,
//! stroked paths with explicit arrowheads for edges, container rects for
//! lanes. The expansion is never stored, so retheme and resize are free and
//! `git diff` reads the intent.
//!
//! Layout is a minimal layered pass (Sugiyama-lite): longest-path layering
//! after deterministic cycle-breaking, two barycenter ordering sweeps, then
//! neighbor-mean coordinate refinement so chains run straight. The plan names
//! a vendored `dagre-rs` here, but no maintained crate exists — these lines
//! are the deliberate deviation, and they are enough for the architecture
//! diagrams agents actually draw. A node carrying an explicit `at` pin keeps
//! that spot verbatim; the rest of the layout is computed as if it were in
//! its slot, and edges route to wherever the nodes actually are.
//!
//! Edges route **orthogonally** (rounded corners, one idiom everywhere):
//! forward edges drop through the inter-layer channel on their own horizontal
//! track, rank-skipping and loop-back edges swing around the node columns on
//! stacked side lanes, and every edge owns its ports on a node's border — so
//! no two edges ever share a segment and nothing cuts across a node. Labels
//! ride their own edge on a surface-colored chip. All of it is pure
//! arithmetic over the same measured text the renderer draws.
//!
//! [`from_mermaid`] converts the flowchart subset agents already emit
//! unprompted. Converted once, at import — the mermaid source is kept in the
//! object's `extra.provenance`, never re-parsed at render.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use anyhow::{bail, Result};

use crate::layout::FontStack;
use crate::schema::{
    Align, DiagramDirection, DiagramEdge, DiagramLane, DiagramNode, DiagramObject, EdgeStyle,
    Extra, Frame, IconObject, NodeShape, Object, Paragraph, RichParagraph, Run, ShapeObject,
    Stroke, TextObject, VAlign,
};
use crate::theme::{Rgb, Theme};

/// The smallest node box, in points.
const MIN_NODE_W: f64 = 96.0;
const MIN_NODE_H: f64 = 40.0;
/// Horizontal padding around a node label inside its box.
const NODE_PAD_X: f64 = 14.0;
/// Gap between nodes within a layer.
const NODE_GAP: f64 = 24.0;
/// Gap between layers — also the routing channel edges cross through, so it
/// clears an edge-label chip with room to spare.
const LAYER_GAP: f64 = 56.0;
/// Padding a lane container adds around its members' bounding box.
const LANE_PAD: f64 = 12.0;
/// The uniform-scale floor. Below it the diagram overflows its box and the
/// expansion says so instead of shrinking nodes into illegibility.
const MIN_SCALE: f64 = 0.6;
/// Clearance between the node columns and the first side lane loop-backs and
/// rank-skips route through.
const LOOP_MARGIN: f64 = 16.0;
/// Spacing between stacked side lanes (inner lanes carry the shorter spans).
const LOOP_STEP: f64 = 26.0;
/// Corner radius of the rounded orthogonal bends.
const CORNER_R: f64 = 7.0;
/// Arrowhead length, and the daylight between its tip and the node border.
const ARROW_LEN: f64 = 7.5;
const ARROW_GAP: f64 = 1.5;
/// Padding inside an edge-label chip.
const CHIP_PAD_X: f64 = 5.0;
const CHIP_PAD_Y: f64 = 2.5;
/// Inset of a node's leading icon within its square cell (the cell is as wide
/// as the node is tall), and the gap between that cell and the label.
const NODE_ICON_INSET: f64 = 6.0;
const NODE_ICON_GAP: f64 = 6.0;

/// Node id → node index, first declaration winning on a duplicate.
type NodeIndex<'a> = BTreeMap<&'a str, usize>;
/// An edge resolved to node indices: `(from, to, declaration index)`.
type EdgeIx = (usize, usize, usize);
/// Border ports pooled per `(node, side)`: `(sort key, edge index, role)`.
type PortPools = BTreeMap<(usize, PSide), Vec<(f64, usize, u8)>>;

/// Expand a diagram into primitives, page-absolute inside its `at`/`size` box.
///
/// Pure and deterministic: same diagram, theme and fonts → byte-identical
/// children. Problems (unknown edge targets, an overflowing layout, lane
/// hulls that overlap) come back as strings the renderer turns into
/// warnings — a diagram with a bad edge still draws everything else.
///
/// Child z-order (and id grammar): lane rects + labels
/// (`<id>/lane.<lane>[.label]`), then edge paths and arrowheads
/// (`<id>/edge[<i>]`, `<id>/edge[<i>].arrow`), then node shapes
/// (`<id>/<node>`), then edge-label chips and text
/// (`<id>/edge[<i>].chip`, `<id>/edge[<i>].label`).
pub fn expand(d: &DiagramObject, theme: &Theme, fonts: &FontStack) -> (Vec<Object>, Vec<String>) {
    let mut problems = Vec::new();
    let (Some(at), Some(size)) = (d.at, d.size) else {
        problems.push("diagram has no at/size; nothing to expand".to_string());
        return (Vec::new(), problems);
    };
    if d.nodes.is_empty() {
        problems.push("diagram has no nodes".to_string());
        return (Vec::new(), problems);
    }

    let (by_id, edges) = resolve_edges(d, &mut problems);
    let meas = measure(d, &edges, theme, fonts);
    let placed = place(d, &meas, at, size, &mut problems);
    let xf = Xf {
        swap: d.direction() == DiagramDirection::Right,
    };
    let real: Vec<Frame> = placed.frames.iter().map(|&f| xf.frame(f)).collect();
    let routes = route_edges(d, &edges, &meas, &placed);

    let mut children = Vec::new();
    emit_lanes(d, &real, theme, placed.scale, &mut children, &mut problems);
    let mut labels = Vec::new();
    emit_edges(
        d,
        &routes,
        xf,
        &real,
        theme,
        fonts,
        placed.scale,
        &mut children,
        &mut labels,
    );
    emit_nodes(d, &by_id, &real, theme, placed.scale, &mut children);
    children.extend(labels);
    (children, problems)
}

/// The size the layout wants at scale 1, in page points — `[w, h]` in the
/// diagram's own direction. What `show --mermaid` auto-sizes its card from.
pub fn natural_size(d: &DiagramObject, theme: &Theme, fonts: &FontStack) -> [f64; 2] {
    if d.nodes.is_empty() {
        return [0.0, 0.0];
    }
    let mut sink = Vec::new();
    let (_, edges) = resolve_edges(d, &mut sink);
    let meas = measure(d, &edges, theme, fonts);
    if d.direction() == DiagramDirection::Right {
        [meas.natural_main, meas.natural_cross]
    } else {
        [meas.natural_cross, meas.natural_main]
    }
}

/// Nodes by id (first declaration winning on a duplicate) and edges resolved
/// to node indices; an edge naming an unknown node is reported and skipped
/// rather than emitting a dangling line.
fn resolve_edges<'a>(
    d: &'a DiagramObject,
    problems: &mut Vec<String>,
) -> (NodeIndex<'a>, Vec<EdgeIx>) {
    let mut by_id: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, n) in d.nodes.iter().enumerate() {
        if by_id.contains_key(n.id.as_str()) {
            problems.push(format!(
                "duplicate node id {:?}; the first declaration wins",
                n.id
            ));
        } else {
            by_id.insert(&n.id, i);
        }
    }
    let mut edges: Vec<(usize, usize, usize)> = Vec::new(); // (from, to, decl index)
    for (ei, e) in d.edges.iter().enumerate() {
        match (by_id.get(e.from.as_str()), by_id.get(e.to.as_str())) {
            (Some(&f), Some(&t)) => edges.push((f, t, ei)),
            (from, _) => {
                let missing = if from.is_none() { &e.from } else { &e.to };
                problems.push(format!(
                    "edge {:?} → {:?} names unknown node {:?}; skipped",
                    e.from, e.to, missing
                ));
            }
        }
    }
    (by_id, edges)
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// Longest-path layering after deterministic cycle-breaking: a DFS in
/// declaration order marks edges into the active stack as back-edges, and
/// layering ignores them.
fn layer_nodes(n: usize, edges: &[(usize, usize, usize)]) -> Vec<usize> {
    let fw = forward_edges(n, edges);
    let mut layer = vec![0usize; n];
    // Bellman-Ford-style relaxation: a DAG's longest path settles within n
    // passes, and the loop is trivially deterministic.
    for _ in 0..n {
        let mut changed = false;
        for &(u, v) in &fw {
            if layer[v] < layer[u] + 1 {
                layer[v] = layer[u] + 1;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    layer
}

/// The edge set minus back-edges and self-loops — the DAG layout runs on.
fn forward_edges(n: usize, edges: &[(usize, usize, usize)]) -> Vec<(usize, usize)> {
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(f, t, _) in edges {
        adj[f].push(t);
    }
    // Iterative gray/black DFS in declaration order; an edge into a gray node
    // is a back-edge.
    let mut color = vec![0u8; n]; // 0 white, 1 gray, 2 black
    let mut back: BTreeSet<(usize, usize)> = BTreeSet::new();
    for start in 0..n {
        if color[start] != 0 {
            continue;
        }
        color[start] = 1;
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        while let Some(&mut (u, ref mut next)) = stack.last_mut() {
            if *next < adj[u].len() {
                let v = adj[u][*next];
                *next += 1;
                match color[v] {
                    0 => {
                        color[v] = 1;
                        stack.push((v, 0));
                    }
                    1 => {
                        back.insert((u, v));
                    }
                    _ => {}
                }
            } else {
                color[u] = 2;
                stack.pop();
            }
        }
    }
    edges
        .iter()
        .filter(|&&(f, t, _)| f != t && !back.contains(&(f, t)))
        .map(|&(f, t, _)| (f, t))
        .collect()
}

/// Group nodes into ordered layers: declaration order first, then two
/// barycenter sweeps (down by predecessors, up by successors), ties broken by
/// declaration order.
fn order_layers(layer: &[usize], n: usize, edges: &[(usize, usize, usize)]) -> Vec<Vec<usize>> {
    let fw = forward_edges(n, edges);
    let n_layers = layer.iter().copied().max().unwrap_or(0) + 1;
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); n_layers];
    for (node, &l) in layer.iter().enumerate() {
        layers[l].push(node);
    }

    // A node's position normalized within its layer, so barycenters compare
    // across layers of different sizes.
    let positions = |layers: &[Vec<usize>]| -> Vec<f64> {
        let mut pos = vec![0.0; n];
        for lay in layers {
            for (i, &node) in lay.iter().enumerate() {
                pos[node] = (i as f64 + 0.5) / lay.len() as f64;
            }
        }
        pos
    };
    let sort_by_barycenter =
        |lay: &mut Vec<usize>, bary: &dyn Fn(usize, &[f64]) -> f64, pos: &[f64]| {
            let keys: BTreeMap<usize, f64> = lay.iter().map(|&v| (v, bary(v, pos))).collect();
            lay.sort_by(|&a, &b| keys[&a].total_cmp(&keys[&b]).then(a.cmp(&b)));
        };

    // Downward: order each layer by the mean position of its predecessors.
    let pos = positions(&layers);
    let down_bary = |v: usize, pos: &[f64]| -> f64 {
        let preds: Vec<f64> = fw
            .iter()
            .filter(|&&(_, t)| t == v)
            .map(|&(u, _)| pos[u])
            .collect();
        if preds.is_empty() {
            pos[v]
        } else {
            preds.iter().sum::<f64>() / preds.len() as f64
        }
    };
    for lay in layers.iter_mut().skip(1) {
        sort_by_barycenter(lay, &down_bary, &pos);
    }
    // Upward: and again by successors, so long chains straighten.
    let pos = positions(&layers);
    let up_bary = |v: usize, pos: &[f64]| -> f64 {
        let succs: Vec<f64> = fw
            .iter()
            .filter(|&&(f, _)| f == v)
            .map(|&(_, t)| pos[t])
            .collect();
        if succs.is_empty() {
            pos[v]
        } else {
            succs.iter().sum::<f64>() / succs.len() as f64
        }
    };
    for lay in layers.iter_mut().rev().skip(1) {
        sort_by_barycenter(lay, &up_bary, &pos);
    }
    layers
}

/// Working-space transform. Layout and routing always think flow-down
/// (`x` = cross axis, `y` = main axis); a `right` diagram transposes on the
/// way in and back out. The swap is an involution, so one function serves
/// both directions; diamonds and ellipses are symmetric under it, so border
/// math holds either way.
#[derive(Clone, Copy)]
struct Xf {
    swap: bool,
}

impl Xf {
    fn pt(&self, p: (f64, f64)) -> (f64, f64) {
        if self.swap {
            (p.1, p.0)
        } else {
            p
        }
    }
    fn frame(&self, f: Frame) -> Frame {
        if self.swap {
            Frame {
                x: f.y,
                y: f.x,
                w: f.h,
                h: f.w,
            }
        } else {
            f
        }
    }
}

/// Everything measurement decides before geometry: sizes, layers, gaps, and
/// the reserves routing will need (side lanes, above/below channels), so the
/// scale-to-fit accounts for the whole drawing and loops never leave the box.
struct Measured {
    layer: Vec<usize>,
    layers: Vec<Vec<usize>>,
    fw: Vec<(usize, usize)>,
    /// Working-space node boxes at scale 1.
    boxes: Vec<(f64, f64)>,
    layer_main: Vec<f64>,
    layer_cross: Vec<f64>,
    node_extent_cross: f64,
    layer_gap: f64,
    /// Lane label headroom above the first layer (type — does not scale).
    top_headroom: f64,
    /// Routing strip reserved above the node columns, when a loop-back
    /// targets the first layer. (The matching below-strip reserve folds into
    /// `natural_main` only — nothing after placement needs its height.)
    above_route: f64,
    natural_main: f64,
    natural_cross: f64,
    /// Side-routed edges (loop-backs, rank-skips, self-loops) → lane index,
    /// shorter spans on the inner lanes.
    lane_rank: BTreeMap<usize, usize>,
    has_lanes: bool,
}

fn measure(
    d: &DiagramObject,
    edges: &[(usize, usize, usize)],
    theme: &Theme,
    fonts: &FontStack,
) -> Measured {
    let n = d.nodes.len();
    let layer = layer_nodes(n, edges);
    let layers = order_layers(&layer, n, edges);
    let fw = forward_edges(n, edges);
    let down = d.direction() == DiagramDirection::Down;

    let role = theme.role("label").unwrap_or_else(|| theme.body());
    let boxes: Vec<(f64, f64)> = d
        .nodes
        .iter()
        .map(|nd| {
            let label_w = fonts.measure(&nd.label, &role.family, role.size, role.weight);
            // Ellipses and diamonds inscribe less of their box, so the label
            // needs more room to stay inside the geometry.
            let (factor, min_h) = match nd.shape.unwrap_or(NodeShape::RoundRect) {
                NodeShape::Diamond => (1.6, 56.0),
                NodeShape::Ellipse => (1.3, 48.0),
                NodeShape::Rect | NodeShape::RoundRect => (1.0, MIN_NODE_H),
            };
            // A leading icon claims a square cell as wide as the node is tall,
            // plus a gap — the label keeps its measured room to its right.
            let icon_col = if nd.icon.is_some() {
                min_h + NODE_ICON_GAP
            } else {
                0.0
            };
            let w = ((label_w + NODE_PAD_X * 2.0) * factor + icon_col).max(MIN_NODE_W);
            if down {
                (w, min_h)
            } else {
                (min_h, w)
            }
        })
        .collect();

    let has_lanes = !d.lanes.is_empty() || d.nodes.iter().any(|nd| nd.lane.is_some());
    let label_h = role.size * role.line_height;
    // Lane containers add label headroom above their members, and that
    // headroom is type — it does not scale with geometry. The gap between
    // layers must clear it even at the scale floor, or a shrunk diagram puts
    // the lane label inside the layer above.
    let layer_gap = if has_lanes {
        LAYER_GAP.max((LANE_PAD + label_h + 8.0) / MIN_SCALE)
    } else {
        LAYER_GAP
    };
    let top_headroom = if has_lanes {
        LANE_PAD + label_h + 6.0
    } else {
        0.0
    };

    let layer_main: Vec<f64> = layers
        .iter()
        .map(|lay| lay.iter().map(|&v| boxes[v].1).fold(0.0, f64::max))
        .collect();
    let layer_cross: Vec<f64> = layers
        .iter()
        .map(|lay| {
            let sum: f64 = lay.iter().map(|&v| boxes[v].0).sum();
            sum + NODE_GAP * lay.len().saturating_sub(1) as f64
        })
        .collect();
    let node_extent_cross = layer_cross.iter().copied().fold(0.0, f64::max);

    // Side-routed edges, inner lanes to the shorter spans so loops nest.
    let mut side: Vec<(usize, usize)> = Vec::new(); // (span, edges-vec idx)
    for (i, &(f, t, _)) in edges.iter().enumerate() {
        if f == t || layer[t] < layer[f] || layer[t] > layer[f] + 1 {
            side.push((layer[f].abs_diff(layer[t]), i));
        }
    }
    side.sort_unstable();
    let lane_rank: BTreeMap<usize, usize> =
        side.iter().enumerate().map(|(k, &(_, i))| (i, k)).collect();

    let above_route = if edges
        .iter()
        .any(|&(f, t, _)| f != t && layer[t] < layer[f] && layer[t] == 0)
    {
        layer_gap * 0.75
    } else {
        0.0
    };
    let n_layers = layers.len();
    let below_route = if edges
        .iter()
        .any(|&(f, t, _)| f != t && layer[t] == layer[f] && layer[f] + 1 == n_layers)
    {
        layer_gap * 0.75
    } else {
        0.0
    };

    let natural_main = layer_main.iter().sum::<f64>()
        + layer_gap * n_layers.saturating_sub(1) as f64
        + top_headroom
        + above_route
        + below_route
        + if has_lanes { LANE_PAD } else { 0.0 };
    let side_reserve = if side.is_empty() {
        0.0
    } else {
        LOOP_MARGIN + LOOP_STEP * side.len() as f64
    };
    let natural_cross =
        node_extent_cross + if has_lanes { LANE_PAD * 2.0 } else { 0.0 } + side_reserve;

    Measured {
        layer,
        layers,
        fw,
        boxes,
        layer_main,
        layer_cross,
        node_extent_cross,
        layer_gap,
        top_headroom,
        above_route,
        natural_main,
        natural_cross,
        lane_rank,
        has_lanes,
    }
}

struct Placed {
    /// Working-space node frames, pins already applied.
    frames: Vec<Frame>,
    scale: f64,
}

/// Fit the measured layout into the box: uniform scale (never below
/// [`MIN_SCALE`], past which it overflows and says so), extra main-axis room
/// spent on wider channels rather than dead margins, the whole block
/// centered. Then neighbor-mean refinement straightens chains, and explicit
/// pins override their nodes verbatim.
fn place(
    d: &DiagramObject,
    meas: &Measured,
    at: [f64; 2],
    size: [f64; 2],
    problems: &mut Vec<String>,
) -> Placed {
    let xf = Xf {
        swap: d.direction() == DiagramDirection::Right,
    };
    let (wx0, wy0) = xf.pt((at[0], at[1]));
    let (box_cross, box_main) = {
        let f = xf.frame(Frame {
            x: 0.0,
            y: 0.0,
            w: size[0],
            h: size[1],
        });
        (f.w, f.h)
    };

    let mut scale: f64 = 1.0;
    if meas.natural_main > 0.0 && meas.natural_cross > 0.0 {
        scale = (box_main / meas.natural_main)
            .min(box_cross / meas.natural_cross)
            .min(1.0);
    }
    if scale < MIN_SCALE {
        let (nw, nh) = if xf.swap {
            (meas.natural_main, meas.natural_cross)
        } else {
            (meas.natural_cross, meas.natural_main)
        };
        problems.push(format!(
            "layout wants {nw:.0}×{nh:.0} pt but the box is {}×{} pt; scaled to the 60% floor \
             and overflowing — enlarge the diagram or split it",
            size[0], size[1]
        ));
        scale = MIN_SCALE;
    }

    // Spare main-axis room widens the channels (up to 60% extra) instead of
    // pooling at the margins — a 7-node flow in a big frame reads composed.
    let n_gaps = meas.layers.len().saturating_sub(1);
    let mut gap_eff = meas.layer_gap * scale;
    let mut used_main = meas.natural_main * scale;
    if n_gaps > 0 && box_main > used_main {
        let add = ((box_main - used_main) / n_gaps as f64).min(gap_eff * 0.6);
        gap_eff += add;
        used_main += add * n_gaps as f64;
    }
    let mut m = wy0
        + ((box_main - used_main) / 2.0).max(0.0)
        + (meas.top_headroom + meas.above_route) * scale;

    let content_w = meas.node_extent_cross * scale;
    let c0 = wx0
        + ((box_cross - meas.natural_cross * scale) / 2.0).max(0.0)
        + if meas.has_lanes {
            LANE_PAD * scale
        } else {
            0.0
        };

    // Initial cross packing: each layer centered on the content column.
    let n = d.nodes.len();
    let mut cx = vec![0.0; n];
    for (li, lay) in meas.layers.iter().enumerate() {
        let lw = meas.layer_cross[li] * scale;
        let mut c = c0 + (content_w - lw) / 2.0;
        for &v in lay {
            let w = meas.boxes[v].0 * scale;
            cx[v] = c + w / 2.0;
            c += w + NODE_GAP * scale;
        }
    }
    refine(&mut cx, meas, scale, c0, content_w);

    let mut frames = vec![
        Frame {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        };
        n
    ];
    for (li, lay) in meas.layers.iter().enumerate() {
        let band = meas.layer_main[li] * scale;
        for &v in lay {
            let (bw, bh) = (meas.boxes[v].0 * scale, meas.boxes[v].1 * scale);
            frames[v] = Frame {
                x: cx[v] - bw / 2.0,
                y: m + (band - bh) / 2.0,
                w: bw,
                h: bh,
            };
        }
        m += band + gap_eff;
    }

    // Pins win verbatim: the node keeps its spot, the rest keeps the layout,
    // and routing follows the frames wherever they are.
    for (i, nd) in d.nodes.iter().enumerate() {
        if let Some(p) = nd.at {
            let (px, py) = xf.pt((p[0], p[1]));
            frames[i].x = px;
            frames[i].y = py;
        }
    }

    Placed { frames, scale }
}

/// Neighbor-mean coordinate refinement: three alternating sweeps pull each
/// node toward the mean of its placed neighbors, keeping layer order, node
/// separation, and the content column. This is what makes a chain a straight
/// line instead of a staircase.
fn refine(cx: &mut [f64], meas: &Measured, scale: f64, c0: f64, content_w: f64) {
    let n = meas.boxes.len();
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut succs: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(u, v) in &meas.fw {
        preds[v].push(u);
        succs[u].push(v);
    }
    let gap = NODE_GAP * scale;
    let half = |v: usize| meas.boxes[v].0 * scale / 2.0;

    for pass in 0..3 {
        let down = pass % 2 == 0;
        let order: Vec<usize> = if down {
            (0..meas.layers.len()).collect()
        } else {
            (0..meas.layers.len()).rev().collect()
        };
        for li in order {
            let lay = &meas.layers[li];
            if lay.is_empty() {
                continue;
            }
            let desired: Vec<f64> = lay
                .iter()
                .map(|&v| {
                    let nb = if down { &preds[v] } else { &succs[v] };
                    if nb.is_empty() {
                        cx[v]
                    } else {
                        nb.iter().map(|&u| cx[u]).sum::<f64>() / nb.len() as f64
                    }
                })
                .collect();
            // Enforce order + separation left-to-right, undo the resulting
            // drift as a block, then clamp the block into the content column
            // — uniform shifts preserve the separations just enforced.
            let mut xs = desired.clone();
            for i in 1..xs.len() {
                let minx = xs[i - 1] + half(lay[i - 1]) + gap + half(lay[i]);
                if xs[i] < minx {
                    xs[i] = minx;
                }
            }
            let drift = xs.iter().zip(&desired).map(|(a, b)| a - b).sum::<f64>() / xs.len() as f64;
            let lo = xs[0] - half(lay[0]) - drift;
            let hi = xs[xs.len() - 1] + half(lay[lay.len() - 1]) - drift;
            let clamp = if lo < c0 {
                c0 - lo
            } else if hi > c0 + content_w {
                c0 + content_w - hi
            } else {
                0.0
            };
            for (i, &v) in lay.iter().enumerate() {
                cx[v] = xs[i] - drift + clamp;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

/// A node border side, in working space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PSide {
    Top,
    Bottom,
    Left,
    Right,
}

#[derive(Clone, Copy)]
enum EKind {
    /// One layer down.
    Fwd,
    /// Two or more layers down — side lane.
    Skip,
    /// Upward (a broken cycle) — side lane.
    Back,
    SelfLoop,
    /// Same layer, adjacent columns — direct.
    SameAdj,
    /// Same layer, columns apart — under the layer through a channel.
    SameFar,
}

struct Arrow {
    tip: (f64, f64),
    /// Unit direction of the final approach.
    dir: (f64, f64),
}

struct Route {
    /// Declaration index into `d.edges`.
    ei: usize,
    /// Working-space polyline, source border → head-shortened end.
    pts: Vec<(f64, f64)>,
    head: Option<Arrow>,
    dashed: bool,
    label: Option<String>,
}

/// The exact point on a node's border where a port at fraction `t` of the
/// side sits. Diamonds and ellipses get true border intersections, so an
/// arrow tip lands on the drawn outline, not the bounding box.
fn port_on(f: &Frame, sh: NodeShape, side: PSide, t: f64) -> (f64, f64) {
    let nx = (2.0 * t - 1.0).abs();
    match side {
        PSide::Top | PSide::Bottom => {
            let x = f.x + f.w * t;
            let inset = match sh {
                NodeShape::Diamond => f.h / 2.0 * nx,
                NodeShape::Ellipse => f.h / 2.0 * (1.0 - (1.0 - nx * nx).max(0.0).sqrt()),
                _ => 0.0,
            };
            let y = if side == PSide::Bottom {
                f.bottom() - inset
            } else {
                f.y + inset
            };
            (x, y)
        }
        PSide::Left | PSide::Right => {
            let y = f.y + f.h * t;
            let inset = match sh {
                NodeShape::Diamond => f.w / 2.0 * nx,
                NodeShape::Ellipse => f.w / 2.0 * (1.0 - (1.0 - nx * nx).max(0.0).sqrt()),
                _ => 0.0,
            };
            let x = if side == PSide::Right {
                f.right() - inset
            } else {
                f.x + inset
            };
            (x, y)
        }
    }
}

/// Compute every edge's orthogonal polyline. The invariant this function
/// exists for: **no two edges share a segment, and no edge crosses a node**.
/// Ports fan out along a node's border (each edge owns its exit/entry point),
/// every horizontal run in an inter-layer channel owns its own track, and
/// side-routed edges own stacked lanes outside the node columns.
fn route_edges(
    d: &DiagramObject,
    edges: &[(usize, usize, usize)],
    meas: &Measured,
    placed: &Placed,
) -> Vec<Route> {
    if edges.is_empty() {
        return Vec::new();
    }
    let frames = &placed.frames;
    let scale = placed.scale;
    let layer = &meas.layer;
    let n_layers = meas.layers.len();
    let shape_of = |v: usize| d.nodes[v].shape.unwrap_or(NodeShape::RoundRect);

    // Content bounds (all nodes) — side lanes stack outside them.
    let mut bb = frames[0];
    for f in &frames[1..] {
        let r = bb.right().max(f.right());
        let b = bb.bottom().max(f.bottom());
        bb.x = bb.x.min(f.x);
        bb.y = bb.y.min(f.y);
        bb.w = r - bb.x;
        bb.h = b - bb.y;
    }
    let lane_off = if meas.has_lanes {
        LANE_PAD * scale
    } else {
        0.0
    };
    let lane_x = |k: usize| bb.right() + lane_off + (LOOP_MARGIN + LOOP_STEP * k as f64) * scale;

    // Cross-order within each layer, for same-layer adjacency.
    let mut xrank: BTreeMap<usize, usize> = BTreeMap::new();
    for lay in &meas.layers {
        let mut by_x: Vec<usize> = lay.clone();
        by_x.sort_by(|&a, &b| frames[a].cx().total_cmp(&frames[b].cx()).then(a.cmp(&b)));
        for (i, &v) in by_x.iter().enumerate() {
            xrank.insert(v, i);
        }
    }

    let kinds: Vec<EKind> = edges
        .iter()
        .map(|&(f, t, _)| {
            if f == t {
                EKind::SelfLoop
            } else if layer[t] == layer[f] {
                if xrank[&f].abs_diff(xrank[&t]) == 1 {
                    EKind::SameAdj
                } else {
                    EKind::SameFar
                }
            } else if layer[t] == layer[f] + 1 {
                EKind::Fwd
            } else if layer[t] > layer[f] {
                EKind::Skip
            } else {
                EKind::Back
            }
        })
        .collect();

    // Ports: every (node, side) pool sorts its edges by where they head and
    // spreads them along the border — two edges leaving one node separate at
    // the source, which is what keeps a branch's labels apart too.
    let mut pools: PortPools = BTreeMap::new();
    let register =
        |pools: &mut PortPools, node: usize, side: PSide, key: f64, eidx: usize, role: u8| {
            pools
                .entry((node, side))
                .or_default()
                .push((key, eidx, role));
        };
    for (i, &(f, t, _)) in edges.iter().enumerate() {
        match kinds[i] {
            EKind::Fwd => {
                register(&mut pools, f, PSide::Bottom, frames[t].cx(), i, 0);
                register(&mut pools, t, PSide::Top, frames[f].cx(), i, 1);
            }
            EKind::Skip | EKind::Back => {
                let lx = lane_x(meas.lane_rank[&i]);
                let (out_side, in_side) = if matches!(kinds[i], EKind::Skip) {
                    (PSide::Bottom, PSide::Top)
                } else {
                    (PSide::Top, PSide::Top)
                };
                register(&mut pools, f, out_side, lx, i, 0);
                register(&mut pools, t, in_side, lx, i, 1);
            }
            EKind::SelfLoop => {
                register(&mut pools, f, PSide::Right, frames[f].cy() - 1.0, i, 0);
                register(&mut pools, f, PSide::Right, frames[f].cy() + 1.0, i, 1);
            }
            EKind::SameAdj => {
                if frames[f].cx() <= frames[t].cx() {
                    register(&mut pools, f, PSide::Right, frames[t].cy(), i, 0);
                    register(&mut pools, t, PSide::Left, frames[f].cy(), i, 1);
                } else {
                    register(&mut pools, f, PSide::Left, frames[t].cy(), i, 0);
                    register(&mut pools, t, PSide::Right, frames[f].cy(), i, 1);
                }
            }
            EKind::SameFar => {
                register(&mut pools, f, PSide::Bottom, frames[t].cx(), i, 0);
                register(&mut pools, t, PSide::Bottom, frames[f].cx(), i, 1);
            }
        }
    }
    let mut ports: BTreeMap<(usize, u8), (usize, PSide, f64)> = BTreeMap::new();
    for ((node, side), mut list) in pools {
        list.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        let k = list.len() as f64;
        for (i, (_, eidx, role)) in list.into_iter().enumerate() {
            ports.insert((eidx, role), (node, side, (i as f64 + 1.0) / (k + 1.0)));
        }
    }
    let port_pt = |eidx: usize, role: u8| -> (f64, f64) {
        let (node, side, t) = ports[&(eidx, role)];
        port_on(&frames[node], shape_of(node), side, t)
    };

    // Channels: the node-free strip between adjacent layers (plus a virtual
    // strip above/below the content for edges that need one). Every
    // horizontal run through a channel gets its own track.
    let strip_h = meas.layer_gap * 0.75 * scale;
    let strip_bounds = |c: i64| -> (f64, f64) {
        if c < 0 {
            return (bb.y - strip_h, bb.y - 3.0 * scale);
        }
        if c as usize + 1 >= n_layers {
            return (bb.bottom() + 3.0 * scale, bb.bottom() + strip_h);
        }
        let y0 = meas.layers[c as usize]
            .iter()
            .map(|&v| frames[v].bottom())
            .fold(f64::MIN, f64::max);
        let y1 = meas.layers[c as usize + 1]
            .iter()
            .map(|&v| frames[v].y)
            .fold(f64::MAX, f64::min);
        let pad = ((y1 - y0) * 0.15).clamp(0.0, 4.0 * scale);
        (y0 + pad, y1 - pad)
    };
    let mut demand: BTreeMap<i64, Vec<(usize, u8)>> = BTreeMap::new();
    for (i, &(f, t, _)) in edges.iter().enumerate() {
        match kinds[i] {
            EKind::Fwd => {
                let p = port_pt(i, 0);
                let q = port_pt(i, 1);
                if (p.0 - q.0).abs() > 0.75 {
                    demand.entry(layer[f] as i64).or_default().push((i, 0));
                }
            }
            EKind::Skip => {
                demand.entry(layer[f] as i64).or_default().push((i, 0));
                demand.entry(layer[t] as i64 - 1).or_default().push((i, 1));
            }
            EKind::Back => {
                demand.entry(layer[f] as i64 - 1).or_default().push((i, 0));
                demand.entry(layer[t] as i64 - 1).or_default().push((i, 1));
            }
            EKind::SameFar => {
                demand.entry(layer[f] as i64).or_default().push((i, 0));
            }
            _ => {}
        }
    }
    let mut track: BTreeMap<(usize, u8), f64> = BTreeMap::new();
    for (c, list) in &demand {
        let (y0, y1) = strip_bounds(*c);
        let k = list.len() as f64;
        for (i, &(e, slot)) in list.iter().enumerate() {
            track.insert((e, slot), y0 + (y1 - y0) * (i as f64 + 1.0) / (k + 1.0));
        }
    }

    let mut routes = Vec::with_capacity(edges.len());
    for (i, &(f, t, ei)) in edges.iter().enumerate() {
        let e = &d.edges[ei];
        let arrow = e.arrow.unwrap_or(true);
        let mut pts: Vec<(f64, f64)> = match kinds[i] {
            EKind::Fwd => {
                let p = port_pt(i, 0);
                let q = port_pt(i, 1);
                if (p.0 - q.0).abs() <= 0.75 {
                    // Straight: snap the entry to the exit's x so the line is
                    // exactly vertical, re-intersected with the true border.
                    let tf = &frames[t];
                    let tt = ((p.0 - tf.x) / tf.w.max(f64::EPSILON)).clamp(0.05, 0.95);
                    let q2 = port_on(tf, shape_of(t), PSide::Top, tt);
                    vec![p, (p.0, q2.1)]
                } else {
                    let ty = track[&(i, 0)];
                    vec![p, (p.0, ty), (q.0, ty), q]
                }
            }
            EKind::Skip | EKind::Back => {
                let p = port_pt(i, 0);
                let q = port_pt(i, 1);
                let (t1, t2) = (track[&(i, 0)], track[&(i, 1)]);
                let lx = lane_x(meas.lane_rank[&i]);
                vec![p, (p.0, t1), (lx, t1), (lx, t2), (q.0, t2), q]
            }
            EKind::SelfLoop => {
                let p = port_pt(i, 0);
                let q = port_pt(i, 1);
                let lx = frames[f].right() + LOOP_MARGIN * scale;
                vec![p, (lx, p.1), (lx, q.1), q]
            }
            EKind::SameAdj => {
                let p = port_pt(i, 0);
                let q = port_pt(i, 1);
                if (p.1 - q.1).abs() <= 0.75 {
                    let tf = &frames[t];
                    let tt = ((p.1 - tf.y) / tf.h.max(f64::EPSILON)).clamp(0.05, 0.95);
                    let (_, side, _) = ports[&(i, 1)];
                    let q2 = port_on(tf, shape_of(t), side, tt);
                    vec![p, (q2.0, p.1)]
                } else {
                    let mx = (p.0 + q.0) / 2.0;
                    vec![p, (mx, p.1), (mx, q.1), q]
                }
            }
            EKind::SameFar => {
                let p = port_pt(i, 0);
                let q = port_pt(i, 1);
                let ty = track[&(i, 0)];
                vec![p, (p.0, ty), (q.0, ty), q]
            }
        };
        let head = shorten_for_head(&mut pts, arrow, scale);
        routes.push(Route {
            ei,
            pts,
            head,
            dashed: e.style == Some(EdgeStyle::Dashed),
            label: e.label.clone(),
        });
    }
    routes
}

/// Pull the shaft back from the target border so the arrowhead (and a hair of
/// daylight) sits between line and node — arrows never pierce a border.
fn shorten_for_head(pts: &mut [(f64, f64)], arrow: bool, scale: f64) -> Option<Arrow> {
    let s = scale.clamp(0.7, 1.0);
    let n = pts.len();
    let (lx, ly) = pts[n - 1];
    let (px, py) = pts[n - 2];
    let (dx, dy) = (lx - px, ly - py);
    let el = (dx * dx + dy * dy).sqrt();
    if el < 1e-6 {
        return None;
    }
    let dir = (dx / el, dy / el);
    let cut = (if arrow {
        ARROW_GAP + ARROW_LEN * s
    } else {
        ARROW_GAP
    })
    .min(el * 0.5);
    pts[n - 1] = (lx - dir.0 * cut, ly - dir.1 * cut);
    arrow.then_some(Arrow {
        tip: (lx - dir.0 * ARROW_GAP, ly - dir.1 * ARROW_GAP),
        dir,
    })
}

// ---------------------------------------------------------------------------
// Emission
// ---------------------------------------------------------------------------

/// Lane containers, drawn first so they sit behind their nodes. A lane is the
/// hull of its members plus padding; lanes whose members interleave across
/// layers can overlap, and v1 draws the hulls anyway and says so.
fn emit_lanes(
    d: &DiagramObject,
    frames: &[Frame],
    theme: &Theme,
    scale: f64,
    out: &mut Vec<Object>,
    problems: &mut Vec<String>,
) {
    // Declared lanes in declaration order, then lane ids only ever named on a
    // node, in node order — deterministic either way.
    let mut lanes: Vec<(&str, Option<&str>)> = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for lane in &d.lanes {
        if seen.insert(&lane.id) {
            lanes.push((&lane.id, lane.label.as_deref()));
        }
    }
    for n in &d.nodes {
        if let Some(lane) = n.lane.as_deref() {
            if seen.insert(lane) {
                lanes.push((lane, None));
            }
        }
    }

    let role = theme.role("label").unwrap_or_else(|| theme.body());
    let pt = (role.size * scale).max(role.min_pt);
    let label_h = pt * role.line_height;
    let pad = LANE_PAD * scale;
    let mut rects: Vec<(&str, Frame)> = Vec::new();
    for (lane_id, label) in lanes {
        let members: Vec<&Frame> = d
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.lane.as_deref() == Some(lane_id))
            .map(|(i, _)| &frames[i])
            .collect();
        let Some(first) = members.first() else {
            problems.push(format!("lane {lane_id:?} has no nodes; skipped"));
            continue;
        };
        let mut hull = **first;
        for f in &members[1..] {
            let x1 = hull.right().max(f.right());
            let y1 = hull.bottom().max(f.bottom());
            hull.x = hull.x.min(f.x);
            hull.y = hull.y.min(f.y);
            hull.w = x1 - hull.x;
            hull.h = y1 - hull.y;
        }
        // Padding all round, plus headroom for the label at the top-left.
        let rect = Frame {
            x: hull.x - pad,
            y: hull.y - pad - label_h - 6.0 * scale,
            w: hull.w + pad * 2.0,
            h: hull.h + pad * 2.0 + label_h + 6.0 * scale,
        };
        for (other_id, other) in &rects {
            let disjoint = rect.x >= other.right()
                || other.x >= rect.right()
                || rect.y >= other.bottom()
                || other.y >= rect.bottom();
            if !disjoint {
                problems.push(format!(
                    "lanes {other_id:?} and {lane_id:?} overlap; their members interleave \
                     across layers and v1 draws the hulls anyway"
                ));
            }
        }
        rects.push((lane_id, rect));

        // A quiet tinted panel: translucent surface + hairline, so the raised
        // node boxes on top stay the loudest thing in the container.
        let mut shape = base_shape(format!("{}/lane.{lane_id}", d.id), "rect");
        shape.at = Some([rect.x, rect.y]);
        shape.size = Some([rect.w, rect.h]);
        shape.fill = Some("@surface".to_string());
        shape.fill_opacity = Some(0.55);
        shape.stroke = Some(stroke("@edge", 1.0));
        out.push(Object::Shape(shape));

        let mut run = Run::plain(label.unwrap_or(lane_id).to_string());
        run.size = Some(pt);
        out.push(Object::Text(TextObject {
            id: format!("{}/lane.{lane_id}.label", d.id),
            kind: Default::default(),
            role: Some("label".to_string()),
            slot: None,
            at: Some([rect.x + pad, rect.y + 4.0 * scale]),
            size: Some([(rect.w - pad * 2.0).max(1.0), label_h]),
            text: vec![Paragraph::Rich(rich(run))],
            align: None,
            valign: None,
            anchor: None,
            alt: None,
            link: None,
            rotation: None,
            extra: Extra::new(),
        }));
    }
}

fn emit_nodes(
    d: &DiagramObject,
    by_id: &BTreeMap<&str, usize>,
    frames: &[Frame],
    theme: &Theme,
    scale: f64,
    out: &mut Vec<Object>,
) {
    let role = theme.role("label").unwrap_or_else(|| theme.body());
    let pt = (role.size * scale).max(role.min_pt);
    // Theme-derived paint with real contrast on every ground: process boxes
    // on a raised surface tone with a visible @axis border, decision diamonds
    // accent-tinted with an @accent1 stroke — never another invisible gray.
    let surface = theme.color("@surface").unwrap_or_else(|| theme.bg());
    let accent = theme
        .color("@accent1")
        .unwrap_or_else(|| theme.color_or_fg(None));
    let fg = theme.color_or_fg(Some("@fg"));
    let raised = blend(surface, fg, 0.05).hex();
    let diamond_fill = blend(surface, accent, 0.18).hex();

    for (i, n) in d.nodes.iter().enumerate() {
        // A duplicate declaration was already reported; emitting it too would
        // draw two shapes under one derived id.
        if by_id.get(n.id.as_str()) != Some(&i) {
            continue;
        }
        let f = frames[i];
        let shape = n.shape.unwrap_or(NodeShape::RoundRect);
        let (fill, stroke_color, stroke_w) = match shape {
            NodeShape::Diamond => (
                n.fill.clone().unwrap_or_else(|| diamond_fill.clone()),
                "@accent1",
                1.4,
            ),
            _ => (
                n.fill.clone().unwrap_or_else(|| raised.clone()),
                "@axis",
                1.25,
            ),
        };
        let mut run = Run::plain(n.label.clone());
        run.color = Some("@fg".to_string());
        run.size = Some(pt);

        let mut sh = base_shape(format!("{}/{}", d.id, n.id), shape.geo());
        sh.at = Some([f.x, f.y]);
        sh.size = Some([f.w, f.h]);
        sh.fill = Some(fill);
        sh.stroke = Some(stroke(stroke_color, stroke_w));
        sh.role = Some("label".to_string());

        match n.icon.as_deref() {
            Some(icon_name) if !icon_name.trim().is_empty() => {
                // A leading icon rides in its own square cell (as wide as the
                // node is tall) with the label centered in the room to its
                // right — the box was widened for exactly this in `measure`.
                // Bare shape first, icon + label on top.
                out.push(Object::Shape(sh));
                let inset = NODE_ICON_INSET * scale;
                let cell = f.h;
                let icon_size = (f.h - inset * 2.0).max(8.0);
                out.push(Object::Icon(IconObject {
                    id: format!("{}/{}.icon", d.id, n.id),
                    kind: Default::default(),
                    slot: None,
                    at: Some([f.x + inset, f.y + (f.h - icon_size) / 2.0]),
                    size: Some([icon_size, icon_size]),
                    name: icon_name.to_string(),
                    color: Some("@fg".to_string()),
                    stroke_width: None,
                    alt: None,
                    extra: Extra::new(),
                }));
                let lx = f.x + cell + NODE_ICON_GAP * scale;
                let lw = (f.right() - lx - NODE_PAD_X * scale).max(1.0);
                out.push(Object::Text(TextObject {
                    id: format!("{}/{}.label", d.id, n.id),
                    kind: Default::default(),
                    role: Some("label".to_string()),
                    slot: None,
                    at: Some([lx, f.y]),
                    size: Some([lw, f.h]),
                    text: vec![Paragraph::Rich(rich(run))],
                    align: Some(Align::Center),
                    valign: Some(VAlign::Middle),
                    anchor: None,
                    alt: None,
                    link: None,
                    rotation: None,
                    extra: Extra::new(),
                }));
            }
            _ => {
                sh.text = vec![Paragraph::Rich(rich(run))];
                out.push(Object::Shape(sh));
            }
        }
    }
}

/// Edges as rounded orthogonal paths with explicit arrowhead polygons, and
/// labels as surface-chip + text pairs placed on their own edge's clearest
/// run. Lines and arrows go under the nodes; chips and text go on top of
/// everything, nudged along their segment until they collide with nothing.
#[allow(clippy::too_many_arguments)]
fn emit_edges(
    d: &DiagramObject,
    routes: &[Route],
    xf: Xf,
    node_frames: &[Frame],
    theme: &Theme,
    fonts: &FontStack,
    scale: f64,
    out: &mut Vec<Object>,
    labels_out: &mut Vec<Object>,
) {
    let role = theme.role("label").unwrap_or_else(|| theme.body());
    let pt = (role.size * scale).max(role.min_pt);
    let s = scale.clamp(0.7, 1.0);
    // Legible mid-tone lines: @axis is every theme's designed line color
    // against its ground (@edge is a hairline tint, invisible on talk-dark).
    let edge_w = (theme.chart.axis_width * 2.0).clamp(1.1, 1.8);
    let mut chips: Vec<Frame> = Vec::new();

    for r in routes {
        let real: Vec<(f64, f64)> = r.pts.iter().map(|&p| xf.pt(p)).collect();
        let dstr = rounded_path(&real, CORNER_R * s);
        let bbox = bbox_of(&real);
        let mut sh = base_shape(format!("{}/edge[{}]", d.id, r.ei), "path");
        sh.d = Some(dstr);
        sh.at = Some([bbox.x, bbox.y]);
        sh.size = Some([bbox.w.max(1.0), bbox.h.max(1.0)]);
        let mut st = stroke("@axis", edge_w);
        if r.dashed {
            st.dash = Some(vec![4.0, 3.0]);
        }
        sh.stroke = Some(st);
        out.push(Object::Shape(sh));

        if let Some(a) = &r.head {
            let tip = xf.pt(a.tip);
            let dv = xf.pt(a.dir);
            let hl = ARROW_LEN * s;
            let half = hl * 0.45;
            let base = (tip.0 - dv.0 * hl, tip.1 - dv.1 * hl);
            let (ox, oy) = (-dv.1, dv.0);
            let p1 = (base.0 + ox * half, base.1 + oy * half);
            let p2 = (base.0 - ox * half, base.1 - oy * half);
            let tri = [tip, p1, p2];
            let bbox = bbox_of(&tri);
            let mut sh = base_shape(format!("{}/edge[{}].arrow", d.id, r.ei), "path");
            sh.d = Some(format!(
                "M{} {} L{} {} L{} {} Z",
                fx(tip.0),
                fx(tip.1),
                fx(p1.0),
                fx(p1.1),
                fx(p2.0),
                fx(p2.1)
            ));
            sh.at = Some([bbox.x, bbox.y]);
            sh.size = Some([bbox.w.max(1.0), bbox.h.max(1.0)]);
            sh.fill = Some("@axis".to_string());
            out.push(Object::Shape(sh));
        }

        let Some(label) = r.label.as_deref().filter(|l| !l.trim().is_empty()) else {
            continue;
        };
        // The clearest run: prefer a horizontal segment (labels read along
        // them), else the longest. The chip covers its own line — that IS
        // the separation from the ink under it.
        let segs: Vec<((f64, f64), (f64, f64))> = real.windows(2).map(|w| (w[0], w[1])).collect();
        let len = |sg: &((f64, f64), (f64, f64))| {
            let (a, b) = sg;
            ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt()
        };
        let best = segs
            .iter()
            .fold(None::<((f64, f64), (f64, f64))>, |acc, sg| match acc {
                Some(cur) if len(&cur) >= len(sg) => Some(cur),
                _ => Some(*sg),
            })
            .unwrap_or((real[0], real[real.len() - 1]));
        let best_h = segs
            .iter()
            .filter(|sg| (sg.1 .1 - sg.0 .1).abs() < 0.01)
            .fold(None::<((f64, f64), (f64, f64))>, |acc, sg| match acc {
                Some(cur) if len(&cur) >= len(sg) => Some(cur),
                _ => Some(*sg),
            });
        let seg = match best_h {
            Some(h) if len(&h) >= (len(&best) * 0.45).max(30.0 * s) => h,
            _ => best,
        };
        let (a, b) = seg;
        let seg_len = len(&seg);
        let mid = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
        let ul = if seg_len > 1e-6 {
            ((b.0 - a.0) / seg_len, (b.1 - a.1) / seg_len)
        } else {
            (1.0, 0.0)
        };

        let tw = fonts.measure(label, &role.family, pt, role.weight);
        let cw = tw + CHIP_PAD_X * 2.0;
        let ch = pt * role.line_height + CHIP_PAD_Y * 2.0;
        let rect_at = |c: (f64, f64)| Frame {
            x: c.0 - cw / 2.0,
            y: c.1 - ch / 2.0,
            w: cw,
            h: ch,
        };
        // Slide along the segment until the chip clears nodes and the chips
        // already placed; the midpoint stands if nothing clears.
        let collides = |rect: &Frame| {
            let hit = |o: &Frame| {
                rect.x < o.right() + 2.0
                    && o.x < rect.right() + 2.0
                    && rect.y < o.bottom() + 2.0
                    && o.y < rect.bottom() + 2.0
            };
            node_frames.iter().any(&hit) || chips.iter().any(&hit)
        };
        let half_span = (seg_len / 2.0 - 6.0).max(0.0);
        let mut chosen = rect_at(mid);
        for off in [0.0, 14.0, -14.0, 28.0, -28.0, 42.0, -42.0] {
            let o = off * s;
            if o.abs() > half_span {
                continue;
            }
            let rect = rect_at((mid.0 + ul.0 * o, mid.1 + ul.1 * o));
            if !collides(&rect) {
                chosen = rect;
                break;
            }
        }
        chips.push(chosen);

        let mut chip = base_shape(format!("{}/edge[{}].chip", d.id, r.ei), "roundRect");
        chip.at = Some([chosen.x, chosen.y]);
        chip.size = Some([chosen.w, chosen.h]);
        chip.fill = Some("@surface".to_string());
        chip.radius = Some(3.0);
        labels_out.push(Object::Shape(chip));

        let mut run = Run::plain(label.to_string());
        run.color = Some("@body".to_string());
        run.size = Some(pt);
        labels_out.push(Object::Text(TextObject {
            id: format!("{}/edge[{}].label", d.id, r.ei),
            kind: Default::default(),
            role: Some("label".to_string()),
            slot: None,
            at: Some([chosen.x, chosen.y]),
            size: Some([chosen.w, chosen.h]),
            text: vec![Paragraph::Rich(rich(run))],
            align: Some(Align::Center),
            valign: Some(VAlign::Middle),
            anchor: None,
            alt: None,
            link: None,
            rotation: None,
            extra: Extra::new(),
        }));
    }
}

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

fn rich(run: Run) -> RichParagraph {
    RichParagraph {
        runs: vec![run],
        align: None,
        space_before: None,
        space_after: None,
        bullet: None,
        extra: Extra::new(),
    }
}

/// A polyline as SVG path data with quadratic-rounded corners — the one bend
/// idiom every edge uses. Corner radius shrinks to fit short segments.
fn rounded_path(pts: &[(f64, f64)], r: f64) -> String {
    let mut out = String::new();
    let _ = write!(out, "M{} {}", fx(pts[0].0), fx(pts[0].1));
    for i in 1..pts.len().saturating_sub(1) {
        let (a, p, b) = (pts[i - 1], pts[i], pts[i + 1]);
        let l1 = ((p.0 - a.0).powi(2) + (p.1 - a.1).powi(2)).sqrt();
        let l2 = ((b.0 - p.0).powi(2) + (b.1 - p.1).powi(2)).sqrt();
        if l1 < 1e-6 || l2 < 1e-6 {
            continue;
        }
        let rr = r.min(l1 * 0.5).min(l2 * 0.5);
        if rr < 0.75 {
            let _ = write!(out, " L{} {}", fx(p.0), fx(p.1));
            continue;
        }
        let v1 = ((p.0 - a.0) / l1, (p.1 - a.1) / l1);
        let v2 = ((b.0 - p.0) / l2, (b.1 - p.1) / l2);
        let pa = (p.0 - v1.0 * rr, p.1 - v1.1 * rr);
        let pb = (p.0 + v2.0 * rr, p.1 + v2.1 * rr);
        let _ = write!(
            out,
            " L{} {} Q{} {} {} {}",
            fx(pa.0),
            fx(pa.1),
            fx(p.0),
            fx(p.1),
            fx(pb.0),
            fx(pb.1)
        );
    }
    let l = pts[pts.len() - 1];
    let _ = write!(out, " L{} {}", fx(l.0), fx(l.1));
    out
}

fn bbox_of(pts: &[(f64, f64)]) -> Frame {
    let mut x0 = f64::MAX;
    let mut y0 = f64::MAX;
    let mut x1 = f64::MIN;
    let mut y1 = f64::MIN;
    for &(x, y) in pts {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    Frame {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    }
}

/// Per-channel mix of two theme colors — how the diamond tint and the raised
/// node tone derive from tokens without the theme growing new entries.
fn blend(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let m = |x: u8, y: u8| ((x as f64) + ((y as f64) - (x as f64)) * t).round() as u8;
    Rgb {
        r: m(a.r, b.r),
        g: m(a.g, b.g),
        b: m(a.b, b.b),
    }
}

/// Path coordinates rounded to 0.01 pt — compact `d` strings, unchanged
/// geometry at any raster scale.
fn fx(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

// ---------------------------------------------------------------------------
// Mermaid import
// ---------------------------------------------------------------------------

/// Parse the mermaid flowchart subset agents actually emit. See
/// [`from_mermaid_with_notes`] for what happens to lines it cannot read.
pub fn from_mermaid(src: &str) -> Result<DiagramObject> {
    from_mermaid_with_notes(src).map(|(d, _)| d)
}

/// [`from_mermaid`], returning the notes: ignored directives (`classDef`,
/// `style`, `click`, …) and lines that did not parse. Lenient by design —
/// the parse fails only when *no* nodes came out of it, because a diagram
/// missing one exotic line is useful and an error is not.
///
/// Covered: `flowchart`/`graph` headers with `TD`/`TB`/`LR` (plus `BT`/`RL`
/// mapped to the nearest supported flow), node shapes `[rect]` `(round)`
/// `((ellipse))` `{diamond}` and the common doubled variants, bare ids, edges
/// `-->` `---` `-.->` `==>` with `|label|` or inline `-- label -->` labels,
/// chains `A --> B --> C`, and `subgraph id[Label] … end` as lanes.
pub fn from_mermaid_with_notes(src: &str) -> Result<(DiagramObject, Vec<String>)> {
    let mut p = MermaidState::default();
    for (ln, raw) in src.lines().enumerate() {
        let line = raw.trim().trim_end_matches(';').trim();
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }
        if let Some(rest) =
            strip_keyword(line, "flowchart").or_else(|| strip_keyword(line, "graph"))
        {
            p.header(rest.trim());
            continue;
        }
        if let Some(rest) = strip_keyword(line, "subgraph") {
            p.open_subgraph(rest.trim());
            continue;
        }
        if line == "end" {
            if p.lane_stack.pop().is_none() {
                p.notes
                    .push(format!("line {}: `end` without a subgraph", ln + 1));
            }
            continue;
        }
        const DIRECTIVES: &[&str] = &[
            "classDef",
            "class",
            "style",
            "linkStyle",
            "click",
            "direction",
            "accTitle",
            "accDescr",
        ];
        if let Some(word) = DIRECTIVES.iter().find(|w| strip_keyword(line, w).is_some()) {
            p.notes.push(format!("ignored `{word}` directive"));
            continue;
        }
        if !p.chain(line) {
            p.notes.push(format!("line {}: unparsed: {line}", ln + 1));
        }
    }
    if p.nodes.is_empty() {
        bail!(
            "no nodes parsed from mermaid{}",
            if p.notes.is_empty() {
                String::new()
            } else {
                format!(" — {}", p.notes.join("; "))
            }
        );
    }

    let mut extra = Extra::new();
    // Converted once: the source rides along as provenance, never re-parsed.
    extra.insert(
        "provenance".to_string(),
        serde_json::json!({ "mermaid": src }),
    );
    let diagram = DiagramObject {
        id: "diagram".to_string(),
        kind: Default::default(),
        slot: None,
        at: None,
        size: None,
        // `down` is the schema default, so only `right` needs stating.
        direction: (p.direction == DiagramDirection::Right).then_some(DiagramDirection::Right),
        nodes: p.nodes,
        edges: p.edges,
        lanes: p.lanes,
        anchor: None,
        alt: None,
        extra,
    };
    Ok((diagram, p.notes))
}

/// `line` minus a leading keyword, only when the keyword ends at a word
/// boundary — `classDef` must not swallow a node named `classDefault`.
fn strip_keyword<'a>(line: &'a str, word: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(word)?;
    match rest.chars().next() {
        None => Some(rest),
        Some(c) if c.is_whitespace() => Some(rest),
        _ => None,
    }
}

#[derive(Default)]
struct MermaidState {
    nodes: Vec<DiagramNode>,
    by_id: BTreeMap<String, usize>,
    /// Node ids that already had a bracketed (label-carrying) definition.
    defined: BTreeSet<String>,
    edges: Vec<DiagramEdge>,
    lanes: Vec<DiagramLane>,
    lane_stack: Vec<String>,
    direction: DiagramDirection,
    notes: Vec<String>,
}

impl MermaidState {
    fn header(&mut self, dir: &str) {
        self.direction = match dir {
            "TD" | "TB" | "" => DiagramDirection::Down,
            "LR" => DiagramDirection::Right,
            "BT" => {
                self.notes
                    .push("direction BT is drawn top-down".to_string());
                DiagramDirection::Down
            }
            "RL" => {
                self.notes
                    .push("direction RL is drawn left-to-right".to_string());
                DiagramDirection::Right
            }
            other => {
                self.notes
                    .push(format!("unknown direction {other:?}; drawing top-down"));
                DiagramDirection::Down
            }
        };
    }

    fn open_subgraph(&mut self, rest: &str) {
        // `subgraph id[Label]` or `subgraph Title` (the title is the id).
        let (id, label) = match rest.split_once('[') {
            Some((id, tail)) => {
                let label = tail.trim_end_matches(']');
                (id.trim().to_string(), Some(unquote(label).to_string()))
            }
            None => (unquote(rest).to_string(), None),
        };
        if !self.lanes.iter().any(|l| l.id == id) {
            self.lanes.push(DiagramLane {
                id: id.clone(),
                label,
                extra: Extra::new(),
            });
        }
        self.lane_stack.push(id);
    }

    /// Parse `A[Label] --> B -.->|x| C` as nodes and edges; `false` when the
    /// line does not read as a chain, so the caller can note it.
    fn chain(&mut self, line: &str) -> bool {
        let Some((first, mut rest)) = parse_node_ref(line) else {
            return false;
        };
        let mut refs = vec![first];
        let mut ops = Vec::new();
        loop {
            rest = rest.trim_start();
            if rest.is_empty() {
                break;
            }
            let Some((op, after_op)) = parse_edge_op(rest) else {
                return false;
            };
            let Some((node, after_node)) = parse_node_ref(after_op.trim_start()) else {
                return false;
            };
            ops.push(op);
            refs.push(node);
            rest = after_node;
        }
        // Only mutate state once the whole line parsed, so a half-read line
        // never registers phantom nodes.
        let ids: Vec<String> = refs.into_iter().map(|r| self.register(r)).collect();
        for (i, op) in ops.into_iter().enumerate() {
            self.edges.push(DiagramEdge {
                from: ids[i].clone(),
                to: ids[i + 1].clone(),
                label: op.label,
                style: (op.style == EdgeStyle::Dashed).then_some(EdgeStyle::Dashed),
                arrow: (!op.arrow).then_some(false),
                extra: Extra::new(),
            });
        }
        true
    }

    fn register(&mut self, r: NodeRef) -> String {
        match self.by_id.get(&r.id) {
            Some(&ix) => {
                // The first bracketed definition wins the label and shape; a
                // bare mention inside a subgraph still claims lane membership.
                if let Some(label) = r.label {
                    if self.defined.insert(r.id.clone()) {
                        self.nodes[ix].label = label;
                        self.nodes[ix].shape = r.shape;
                    }
                }
                if self.nodes[ix].lane.is_none() {
                    self.nodes[ix].lane = self.lane_stack.last().cloned();
                }
            }
            None => {
                if r.label.is_some() {
                    self.defined.insert(r.id.clone());
                }
                self.by_id.insert(r.id.clone(), self.nodes.len());
                self.nodes.push(DiagramNode {
                    id: r.id.clone(),
                    label: r.label.unwrap_or_else(|| r.id.clone()),
                    shape: r.shape,
                    // Mermaid's flowchart subset has no clean, universal icon
                    // slot, so imported nodes stay iconless (add `icon` to the
                    // node afterward) rather than overreaching the parser.
                    icon: None,
                    at: None,
                    fill: None,
                    lane: self.lane_stack.last().cloned(),
                    extra: Extra::new(),
                });
            }
        }
        r.id
    }
}

struct NodeRef {
    id: String,
    label: Option<String>,
    shape: Option<NodeShape>,
}

struct EdgeOp {
    style: EdgeStyle,
    arrow: bool,
    label: Option<String>,
}

/// `A`, `A[Label]`, `A(Label)`, `A((Label))`, `A{Label}` and the doubled
/// variants, returning the ref and the unconsumed remainder.
fn parse_node_ref(s: &str) -> Option<(NodeRef, &str)> {
    let bytes = s.as_bytes();
    let mut end = 0;
    while end < bytes.len() {
        let c = bytes[end] as char;
        let id_char = c.is_ascii_alphanumeric() || c == '_';
        // `-` stays in the id (`api-server`) unless it starts an edge token.
        let joining_dash = c == '-'
            && end > 0
            && matches!(bytes.get(end + 1).map(|&b| b as char), Some(n) if n.is_ascii_alphanumeric() || n == '_');
        if id_char || joining_dash {
            end += 1;
        } else {
            break;
        }
    }
    if end == 0 {
        return None;
    }
    let id = s[..end].to_string();
    let rest = &s[end..];

    // Longest bracket prefixes first, or `((x))` would read as `(` + `(x`.
    const BRACKETS: &[(&str, &str, NodeShape)] = &[
        ("((", "))", NodeShape::Ellipse),
        ("([", "])", NodeShape::RoundRect),
        ("[[", "]]", NodeShape::Rect),
        ("[(", ")]", NodeShape::Ellipse),
        ("{{", "}}", NodeShape::Diamond),
        ("[", "]", NodeShape::Rect),
        ("(", ")", NodeShape::RoundRect),
        ("{", "}", NodeShape::Diamond),
    ];
    for &(open, close, shape) in BRACKETS {
        if let Some(inner) = rest.strip_prefix(open) {
            let end = inner.find(close)?;
            let label = unquote(inner[..end].trim()).to_string();
            return Some((
                NodeRef {
                    id,
                    label: Some(label),
                    shape: Some(shape),
                },
                &inner[end + close.len()..],
            ));
        }
    }
    Some((
        NodeRef {
            id,
            label: None,
            shape: None,
        },
        rest,
    ))
}

/// One edge token at the head of `s`: `-->`, `---`, `-.->`, `==>`, with an
/// optional `|label|` after it or a `-- label -->` inline form.
fn parse_edge_op(s: &str) -> Option<(EdgeOp, &str)> {
    const OPS: &[(&str, EdgeStyle, bool)] = &[
        ("-.->", EdgeStyle::Dashed, true),
        ("-.-", EdgeStyle::Dashed, false),
        ("==>", EdgeStyle::Solid, true),
        ("===", EdgeStyle::Solid, false),
        ("-->", EdgeStyle::Solid, true),
        ("---", EdgeStyle::Solid, false),
    ];
    for &(op, style, mut arrow) in OPS {
        if let Some(mut rest) = s.strip_prefix(op) {
            // `--->` and friends: extra dashes, maybe a late arrowhead.
            while let Some(r) = rest.strip_prefix('-') {
                rest = r;
            }
            if !arrow {
                if let Some(r) = rest.strip_prefix('>') {
                    rest = r;
                    arrow = true;
                }
            }
            let (label, rest) = parse_pipe_label(rest);
            return Some((
                EdgeOp {
                    style,
                    arrow,
                    label,
                },
                rest,
            ));
        }
    }
    // Inline labels: `-- text -->` and `-. text .->`.
    for &(open, close, style) in &[
        ("--", "-->", EdgeStyle::Solid),
        ("-.", ".->", EdgeStyle::Dashed),
    ] {
        if let Some(rest) = s.strip_prefix(open) {
            let end = rest.find(close)?;
            let label = unquote(rest[..end].trim()).to_string();
            return Some((
                EdgeOp {
                    style,
                    arrow: true,
                    label: (!label.is_empty()).then_some(label),
                },
                &rest[end + close.len()..],
            ));
        }
    }
    None
}

fn parse_pipe_label(s: &str) -> (Option<String>, &str) {
    let trimmed = s.trim_start();
    if let Some(inner) = trimmed.strip_prefix('|') {
        if let Some(end) = inner.find('|') {
            let label = unquote(inner[..end].trim()).to_string();
            return ((!label.is_empty()).then_some(label), &inner[end + 1..]);
        }
    }
    (None, s)
}

fn unquote(s: &str) -> &str {
    s.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Object;

    fn diagram(json: &str) -> DiagramObject {
        serde_json::from_str(json).unwrap()
    }

    const FIVE_NODE_TWO_LANE: &str = r#"{
      "id": "arch", "type": "diagram", "at": [48, 48], "size": [864, 444],
      "nodes": [
        {"id": "ui", "label": "Web UI", "lane": "front"},
        {"id": "api", "label": "API server", "lane": "back"},
        {"id": "queue", "label": "Queue", "shape": "ellipse", "lane": "back"},
        {"id": "worker", "label": "Worker", "lane": "back"},
        {"id": "db", "label": "Database", "shape": "rect", "lane": "back"}
      ],
      "edges": [
        {"from": "ui", "to": "api", "label": "HTTP"},
        {"from": "api", "to": "queue"},
        {"from": "queue", "to": "worker", "style": "dashed"},
        {"from": "worker", "to": "db", "label": "SQL"},
        {"from": "api", "to": "db"}
      ],
      "lanes": [
        {"id": "front", "label": "Frontend"},
        {"id": "back", "label": "Backend"}
      ]
    }"#;

    /// The flowchart from the user's screenshot: two labeled branches off a
    /// diamond, a short loop-back, and a full-height loop-back to the start.
    const COFFEE: &str = "flowchart TD\n\
        A[Lift cup] --> B[Bring to lips]\n\
        B --> C{Too hot?}\n\
        C -->|Yes| D[Blow and wait]\n\
        D --> B\n\
        C -->|No| E[Sip and swallow]\n\
        E --> F{Coffee left?}\n\
        F -->|Yes| A\n\
        F -->|No| G[Set cup down]\n";

    fn expand_five() -> (Vec<Object>, Vec<String>) {
        let d = diagram(FIVE_NODE_TWO_LANE);
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        expand(&d, &theme, &fonts)
    }

    fn expand_coffee() -> (DiagramObject, Vec<Object>, Vec<String>) {
        let mut d = from_mermaid(COFFEE).unwrap();
        d.id = "flow".to_string();
        d.at = Some([40.0, 40.0]);
        d.size = Some([640.0, 640.0]);
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let (children, problems) = expand(&d, &theme, &fonts);
        (d, children, problems)
    }

    /// Reconstruct the polyline this module's own `rounded_path` emitted:
    /// `M p (L a Q corner b)* L end` — the corners are the Q control points.
    fn polyline(d: &str) -> Vec<(f64, f64)> {
        let mut pts = Vec::new();
        let mut rest = d;
        assert!(rest.starts_with('M'), "{d}");
        rest = &rest[1..];
        let end = rest.find(['L', 'Q']).unwrap_or(rest.len());
        let start: Vec<f64> = rest[..end]
            .split_whitespace()
            .map(|t| t.parse().unwrap())
            .collect();
        pts.push((start[0], start[1]));
        rest = &rest[end..];
        let mut cmds: Vec<(char, Vec<f64>)> = Vec::new();
        while let Some(pos) = rest.find(['L', 'Q']) {
            let cmd = rest.as_bytes()[pos] as char;
            rest = &rest[pos + 1..];
            let end = rest.find(['L', 'Q', 'Z']).unwrap_or(rest.len());
            let vals: Vec<f64> = rest[..end]
                .split_whitespace()
                .map(|t| t.parse().unwrap())
                .collect();
            cmds.push((cmd, vals));
            rest = &rest[end..];
        }
        let n = cmds.len();
        for (i, (cmd, vals)) in cmds.into_iter().enumerate() {
            match cmd {
                // A corner rides in as the Q control point; the L before it
                // and the Q endpoint are on-segment approach points.
                'Q' => pts.push((vals[0], vals[1])),
                'L' if i + 1 == n => pts.push((vals[0], vals[1])),
                _ => {}
            }
        }
        pts
    }

    fn edge_paths(children: &[Object], id_prefix: &str) -> Vec<(usize, Vec<(f64, f64)>)> {
        children
            .iter()
            .filter_map(|o| match o {
                Object::Shape(s) if s.geo == "path" && s.id.starts_with(id_prefix) => {
                    let tail = &s.id[id_prefix.len()..];
                    if !tail.starts_with("edge[") || !tail.ends_with(']') {
                        return None;
                    }
                    let ei: usize = tail[5..tail.len() - 1].parse().ok()?;
                    Some((ei, polyline(s.d.as_deref()?)))
                }
                _ => None,
            })
            .collect()
    }

    fn node_frames(children: &[Object], d: &DiagramObject) -> BTreeMap<String, Frame> {
        d.nodes
            .iter()
            .filter_map(|n| {
                let id = format!("{}/{}", d.id, n.id);
                children.iter().find_map(|o| match o {
                    Object::Shape(s) if s.id == id => o.frame().map(|f| (n.id.clone(), f)),
                    _ => None,
                })
            })
            .collect()
    }

    #[test]
    fn expansion_is_deterministic() {
        let (a, pa) = expand_five();
        let (b, pb) = expand_five();
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap(),
            "same diagram, same expansion"
        );
        assert_eq!(pa, pb);
    }

    #[test]
    fn nodes_never_overlap_and_lanes_sit_behind() {
        let (children, problems) = expand_five();
        assert!(problems.is_empty(), "{problems:?}");
        let ids = [
            "arch/ui",
            "arch/api",
            "arch/queue",
            "arch/worker",
            "arch/db",
        ];
        let node_frames: Vec<(String, Frame)> = children
            .iter()
            .filter_map(|o| match o {
                Object::Shape(s) if ids.contains(&s.id.as_str()) => {
                    o.frame().map(|f| (s.id.clone(), f))
                }
                _ => None,
            })
            .collect();
        assert_eq!(node_frames.len(), 5);
        for (i, (ia, a)) in node_frames.iter().enumerate() {
            for (ib, b) in node_frames.iter().skip(i + 1) {
                let disjoint =
                    a.right() <= b.x || b.right() <= a.x || a.bottom() <= b.y || b.bottom() <= a.y;
                assert!(disjoint, "{ia} and {ib} overlap: {a:?} vs {b:?}");
            }
        }
        // Lane rects come first, so they draw behind their members.
        assert!(matches!(&children[0], Object::Shape(s) if s.id == "arch/lane.front"));
        // Edges are routed paths with a legible theme stroke.
        let edge = children
            .iter()
            .find_map(|o| match o {
                Object::Shape(s) if s.id == "arch/edge[0]" => Some(s),
                _ => None,
            })
            .expect("edge 0 draws");
        assert_eq!(edge.geo, "path");
        assert_eq!(
            edge.stroke.as_ref().and_then(|s| s.color.as_deref()),
            Some("@axis")
        );
    }

    #[test]
    fn an_edge_to_an_unknown_node_is_reported_and_skipped() {
        let d = diagram(
            r#"{"id": "d", "type": "diagram", "at": [0, 0], "size": [400, 300],
                "nodes": [{"id": "a", "label": "A"}],
                "edges": [{"from": "a", "to": "ghost"}]}"#,
        );
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let (children, problems) = expand(&d, &theme, &fonts);
        assert!(problems.iter().any(|p| p.contains("ghost")), "{problems:?}");
        assert!(
            !children.iter().any(|o| o.id().contains("/edge[")),
            "no dangling edge geometry"
        );
    }

    #[test]
    fn a_cycle_terminates_and_places_every_node() {
        let d = diagram(
            r#"{"id": "d", "type": "diagram", "at": [0, 0], "size": [600, 400],
                "nodes": [{"id": "a", "label": "A"}, {"id": "b", "label": "B"},
                          {"id": "c", "label": "C"}],
                "edges": [{"from": "a", "to": "b"}, {"from": "b", "to": "c"},
                          {"from": "c", "to": "a"}]}"#,
        );
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let (children, _) = expand(&d, &theme, &fonts);
        let nodes = ["d/a", "d/b", "d/c"];
        let node_shapes = children
            .iter()
            .filter(|o| matches!(o, Object::Shape(s) if nodes.contains(&s.id.as_str())))
            .count();
        let edges = edge_paths(&children, "d/");
        assert_eq!(node_shapes, 3, "every node placed despite the cycle");
        assert_eq!(edges.len(), 3, "the back-edge still draws");
    }

    #[test]
    fn a_too_small_box_reports_the_scale_floor() {
        let d = diagram(
            r#"{"id": "d", "type": "diagram", "at": [0, 0], "size": [80, 60],
                "nodes": [{"id": "a", "label": "A long node label"},
                          {"id": "b", "label": "Another long label"},
                          {"id": "c", "label": "And a third one"}],
                "edges": [{"from": "a", "to": "b"}, {"from": "b", "to": "c"}]}"#,
        );
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let (_, problems) = expand(&d, &theme, &fonts);
        assert!(
            problems.iter().any(|p| p.contains("60% floor")),
            "{problems:?}"
        );
    }

    #[test]
    fn coffee_edges_are_orthogonal_disjoint_and_clear_of_nodes() {
        let (d, children, problems) = expand_coffee();
        assert!(problems.is_empty(), "{problems:?}");
        let paths = edge_paths(&children, "flow/");
        assert_eq!(paths.len(), d.edges.len(), "every edge draws");
        let frames = node_frames(&children, &d);

        // Every segment is axis-aligned: one routing idiom, no raw diagonals.
        for (ei, pts) in &paths {
            for w in pts.windows(2) {
                let (a, b) = (w[0], w[1]);
                assert!(
                    (a.0 - b.0).abs() < 0.02 || (a.1 - b.1).abs() < 0.02,
                    "edge[{ei}] has a diagonal segment {a:?} → {b:?}"
                );
            }
        }

        // No two edges share a segment: colinear runs from different edges
        // never overlap in range.
        type Seg = (usize, (f64, f64), (f64, f64));
        let segs: Vec<Seg> = paths
            .iter()
            .flat_map(|(ei, pts)| {
                pts.windows(2)
                    .map(|w| (*ei, w[0], w[1]))
                    .collect::<Vec<_>>()
            })
            .collect();
        for (i, &(ea, a1, a2)) in segs.iter().enumerate() {
            for &(eb, b1, b2) in segs.iter().skip(i + 1) {
                if ea == eb {
                    continue;
                }
                let a_h = (a1.1 - a2.1).abs() < 0.02;
                let b_h = (b1.1 - b2.1).abs() < 0.02;
                if a_h != b_h {
                    continue; // perpendicular: crossings are fine, overlap impossible
                }
                let (fixed_a, lo_a, hi_a, fixed_b, lo_b, hi_b) = if a_h {
                    (
                        a1.1,
                        a1.0.min(a2.0),
                        a1.0.max(a2.0),
                        b1.1,
                        b1.0.min(b2.0),
                        b1.0.max(b2.0),
                    )
                } else {
                    (
                        a1.0,
                        a1.1.min(a2.1),
                        a1.1.max(a2.1),
                        b1.0,
                        b1.1.min(b2.1),
                        b1.1.max(b2.1),
                    )
                };
                if (fixed_a - fixed_b).abs() > 0.3 {
                    continue; // not colinear
                }
                let overlap = hi_a.min(hi_b) - lo_a.max(lo_b);
                assert!(
                    overlap <= 0.5,
                    "edge[{ea}] and edge[{eb}] overlap on a colinear run \
                     ({a1:?}→{a2:?} vs {b1:?}→{b2:?})"
                );
            }
        }

        // No segment cuts through a node it is not attached to.
        for (ei, pts) in &paths {
            let e = &d.edges[*ei];
            for w in pts.windows(2) {
                let (a, b) = (w[0], w[1]);
                for (nid, f) in &frames {
                    if *nid == e.from || *nid == e.to {
                        continue;
                    }
                    let (x0, x1) = (a.0.min(b.0), a.0.max(b.0));
                    let (y0, y1) = (a.1.min(b.1), a.1.max(b.1));
                    let hit = x1 > f.x + 0.5
                        && x0 < f.right() - 0.5
                        && y1 > f.y + 0.5
                        && y0 < f.bottom() - 0.5;
                    assert!(
                        !hit,
                        "edge[{ei}] ({} → {}) crosses node {nid}: {a:?}→{b:?} vs {f:?}",
                        e.from, e.to
                    );
                }
            }
        }

        // Loop-backs route around the columns: their outermost run sits
        // outside every node, and still inside the diagram's box.
        let max_node_right = frames.values().map(|f| f.right()).fold(f64::MIN, f64::max);
        for ei in [3usize, 6] {
            let (_, pts) = paths.iter().find(|(e, _)| *e == ei).unwrap();
            let max_x = pts.iter().map(|p| p.0).fold(f64::MIN, f64::max);
            assert!(
                max_x > max_node_right,
                "edge[{ei}] should swing around the node columns"
            );
        }
        for (ei, pts) in &paths {
            for p in pts {
                assert!(
                    p.0 >= 39.0 && p.0 <= 681.0 && p.1 >= 39.0 && p.1 <= 681.0,
                    "edge[{ei}] leaves the diagram box at {p:?}"
                );
            }
        }
    }

    #[test]
    fn labeled_edges_get_separated_surface_chips() {
        let (d, children, _) = expand_coffee();
        let labeled: Vec<usize> = d
            .edges
            .iter()
            .enumerate()
            .filter(|(_, e)| e.label.is_some())
            .map(|(i, _)| i)
            .collect();
        assert_eq!(labeled.len(), 4, "Yes/No twice");
        let chips: Vec<(String, Frame)> = children
            .iter()
            .filter_map(|o| match o {
                Object::Shape(s) if s.id.ends_with(".chip") => {
                    assert_eq!(s.fill.as_deref(), Some("@surface"), "{}", s.id);
                    o.frame().map(|f| (s.id.clone(), f))
                }
                _ => None,
            })
            .collect();
        assert_eq!(chips.len(), labeled.len(), "one chip per labeled edge");
        for &ei in &labeled {
            assert!(
                children
                    .iter()
                    .any(|o| o.id() == format!("flow/edge[{ei}].label")),
                "edge[{ei}] label text draws"
            );
        }
        for (i, (ia, a)) in chips.iter().enumerate() {
            for (ib, b) in chips.iter().skip(i + 1) {
                let disjoint = a.right() <= b.x + 0.5
                    || b.right() <= a.x + 0.5
                    || a.bottom() <= b.y + 0.5
                    || b.bottom() <= a.y + 0.5;
                assert!(disjoint, "{ia} and {ib} collide: {a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn a_pinned_node_keeps_its_spot_and_the_rest_lays_out() {
        let d = diagram(
            r#"{"id": "d", "type": "diagram", "at": [0, 0], "size": [500, 400],
                "nodes": [{"id": "a", "label": "A"},
                          {"id": "b", "label": "B", "at": [340, 24]},
                          {"id": "c", "label": "C"}],
                "edges": [{"from": "a", "to": "b"}, {"from": "a", "to": "c"}]}"#,
        );
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let (children, _) = expand(&d, &theme, &fonts);
        let pinned = children
            .iter()
            .find_map(|o| match o {
                Object::Shape(s) if s.id == "d/b" => o.frame(),
                _ => None,
            })
            .unwrap();
        assert_eq!((pinned.x, pinned.y), (340.0, 24.0), "the pin wins verbatim");
        // Unpinned nodes still get the computed layout, and the expansion
        // stays deterministic.
        let unpinned = children
            .iter()
            .find_map(|o| match o {
                Object::Shape(s) if s.id == "d/c" => o.frame(),
                _ => None,
            })
            .unwrap();
        assert_ne!((unpinned.x, unpinned.y), (340.0, 24.0));
        let (again, _) = expand(&d, &theme, &fonts);
        assert_eq!(
            serde_json::to_string(&children).unwrap(),
            serde_json::to_string(&again).unwrap()
        );
    }

    #[test]
    fn a_straight_chain_routes_straight() {
        let mut d = from_mermaid("flowchart TD\nA --> B --> C\n").unwrap();
        d.at = Some([0.0, 0.0]);
        d.size = Some([400.0, 400.0]);
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let (children, _) = expand(&d, &theme, &fonts);
        for o in &children {
            if let Object::Shape(s) = o {
                if s.id.contains("/edge[") && s.id.ends_with(']') {
                    let path = s.d.as_deref().unwrap();
                    assert!(
                        !path.contains('Q'),
                        "an aligned chain needs no bends: {path}"
                    );
                }
            }
        }
    }

    #[test]
    fn natural_size_covers_the_layout_and_its_loops() {
        let d = from_mermaid(COFFEE).unwrap();
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let nat = natural_size(&d, &theme, &fonts);
        assert!(nat[0] > MIN_NODE_W, "{nat:?}");
        // Six layers of nodes plus channels.
        assert!(nat[1] > 6.0 * MIN_NODE_H, "{nat:?}");
        // Two loop-backs reserve side-lane room beyond the widest layer.
        let plain = from_mermaid("flowchart TD\nA --> B\n").unwrap();
        let np = natural_size(&plain, &theme, &fonts);
        assert!(
            nat[0] > np[0],
            "loops widen the natural size: {nat:?} vs {np:?}"
        );
        // A `right` diagram runs its main axis horizontally: a six-layer
        // chain drawn rightward is wider than tall. (Not an exact transpose
        // — per-layer max/sum swap roles with the boxes.)
        let mut r = from_mermaid(COFFEE).unwrap();
        r.direction = Some(DiagramDirection::Right);
        let nr = natural_size(&r, &theme, &fonts);
        assert!(nr[0] > nr[1], "{nr:?}");
    }

    #[test]
    fn expansion_reads_correctly_in_every_bundled_theme() {
        // The diamond fill derives from @surface/@accent1 and must differ
        // from the process-box tone on every ground; edges stroke @axis.
        for id in crate::theme::BUNDLED_IDS {
            let theme = crate::theme::bundled(id).unwrap();
            let mut d = from_mermaid(COFFEE).unwrap();
            d.id = "flow".to_string();
            d.at = Some([40.0, 40.0]);
            d.size = Some([640.0, 640.0]);
            let fonts = FontStack::new(&[]);
            let (children, problems) = expand(&d, &theme, &fonts);
            assert!(problems.is_empty(), "{id}: {problems:?}");
            let fill_of = |nid: &str| {
                children
                    .iter()
                    .find_map(|o| match o {
                        Object::Shape(s) if s.id == nid => s.fill.clone(),
                        _ => None,
                    })
                    .unwrap()
            };
            let diamond = fill_of("flow/C");
            let process = fill_of("flow/A");
            assert_ne!(diamond, process, "{id}: diamonds must read distinct");
            let bg = theme.bg();
            let dc = theme.color(&diamond).unwrap();
            assert!(
                dc.contrast(&bg) > 1.04,
                "{id}: diamond fill {diamond} vanishes against the ground"
            );
        }
    }

    #[test]
    fn mermaid_parses_a_realistic_flowchart() {
        let src = r#"flowchart LR
  %% the ingestion path
  A[API server] --> B(Queue)
  B -.->|drain| C((Worker))
  C --> D{OK?}
  D -->|yes| E
  subgraph backend[Backend]
    B
    C
  end
  classDef hot fill:#f96
"#;
        let (d, notes) = from_mermaid_with_notes(src).unwrap();
        assert_eq!(d.nodes.len(), 5);
        assert_eq!(d.edges.len(), 4);
        assert_eq!(d.direction, Some(DiagramDirection::Right));

        let node = |id: &str| d.nodes.iter().find(|n| n.id == id).unwrap();
        assert_eq!(node("A").label, "API server");
        assert_eq!(node("A").shape, Some(NodeShape::Rect));
        assert_eq!(node("B").shape, Some(NodeShape::RoundRect));
        assert_eq!(node("C").shape, Some(NodeShape::Ellipse));
        assert_eq!(node("D").shape, Some(NodeShape::Diamond));
        assert_eq!(node("E").label, "E", "a bare id labels itself");

        let e = &d.edges[1];
        assert_eq!((e.from.as_str(), e.to.as_str()), ("B", "C"));
        assert_eq!(e.style, Some(EdgeStyle::Dashed));
        assert_eq!(e.label.as_deref(), Some("drain"));
        assert_eq!(d.edges[3].label.as_deref(), Some("yes"));

        assert_eq!(d.lanes.len(), 1);
        assert_eq!(d.lanes[0].label.as_deref(), Some("Backend"));
        assert_eq!(node("B").lane.as_deref(), Some("backend"));
        assert_eq!(node("A").lane, None);

        assert!(notes.iter().any(|n| n.contains("classDef")), "{notes:?}");
        // The source rides along as provenance, converted once.
        assert_eq!(d.extra["provenance"]["mermaid"].as_str().unwrap(), src);
    }

    #[test]
    fn mermaid_chains_and_plain_lines() {
        let (d, _) = from_mermaid_with_notes("graph TD\nA --> B --> C\nB --- D\n").unwrap();
        assert_eq!(d.nodes.len(), 4);
        assert_eq!(d.edges.len(), 3);
        assert_eq!(
            (d.edges[0].from.as_str(), d.edges[0].to.as_str()),
            ("A", "B")
        );
        assert_eq!(
            (d.edges[1].from.as_str(), d.edges[1].to.as_str()),
            ("B", "C")
        );
        // `---` carries no arrowhead, stored explicitly because absent = true.
        assert_eq!(d.edges[2].arrow, Some(false));
        assert_eq!(d.direction, None, "TD is the default and stays implicit");
    }

    #[test]
    fn mermaid_inline_labels_and_hyphenated_ids() {
        let (d, _) =
            from_mermaid_with_notes("flowchart TD\napi-server -- calls --> auth-svc\n").unwrap();
        assert_eq!(d.nodes[0].id, "api-server");
        assert_eq!(d.edges[0].label.as_deref(), Some("calls"));
    }

    #[test]
    fn mermaid_without_nodes_fails_with_the_notes() {
        let err = from_mermaid("flowchart TD\n%% nothing here\nstyle A fill:#fff\n").unwrap_err();
        assert!(err.to_string().contains("no nodes"), "{err}");
    }

    #[test]
    fn an_unparsed_line_is_a_note_not_a_failure() {
        let (d, notes) =
            from_mermaid_with_notes("flowchart TD\nA --> B\n!! not mermaid at all\n").unwrap();
        assert_eq!(d.nodes.len(), 2);
        assert!(notes.iter().any(|n| n.contains("unparsed")), "{notes:?}");
    }

    #[test]
    fn a_mermaid_diagram_round_trips_through_the_schema() {
        let (d, _) =
            from_mermaid_with_notes("flowchart TD\nA[Start] --> B{Check}\nB -->|ok| C((Done))\n")
                .unwrap();
        let obj = Object::Diagram(d);
        let json = serde_json::to_string(&obj).unwrap();
        let back: Object = serde_json::from_str(&json).unwrap();
        let Object::Diagram(rd) = back else {
            panic!("expected diagram, got {back:?}")
        };
        assert_eq!(rd.nodes.len(), 3);
        assert_eq!(rd.edges.len(), 2);
    }

    #[cfg(feature = "icons")]
    #[test]
    fn a_node_icon_leads_the_label_and_widens_the_box() {
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let with = diagram(
            r#"{"id":"d","type":"diagram","at":[0,0],"size":[400,200],
                "nodes":[{"id":"a","label":"Train","icon":"flask"}]}"#,
        );
        let plain = diagram(
            r#"{"id":"d","type":"diagram","at":[0,0],"size":[400,200],
                "nodes":[{"id":"a","label":"Train"}]}"#,
        );
        let (cw, _) = expand(&with, &theme, &fonts);
        let (co, _) = expand(&plain, &theme, &fonts);

        let node_w = |children: &[Object]| {
            children
                .iter()
                .find_map(|o| match o {
                    Object::Shape(s) if s.id == "d/a" => o.frame().map(|f| f.w),
                    _ => None,
                })
                .unwrap()
        };
        assert!(
            node_w(&cw) > node_w(&co),
            "the leading icon widens the node box"
        );

        // The iconed node emits a separate icon child and label, icon leading.
        let icon = cw
            .iter()
            .find_map(|o| match o {
                Object::Icon(ic) if ic.id == "d/a.icon" => o.frame().map(|f| (ic.name.clone(), f)),
                _ => None,
            })
            .expect("a leading icon child");
        assert_eq!(icon.0, "flask");
        let label = cw
            .iter()
            .find_map(|o| match o {
                Object::Text(t) if t.id == "d/a.label" => o.frame(),
                _ => None,
            })
            .expect("a separate label object");
        assert!(icon.1.right() <= label.x + 0.5, "the icon leads the label");
        // A plain node keeps bound text on its shape — no separate label.
        assert!(!co.iter().any(|o| o.id() == "d/a.label"));
    }
}
