//! `diagram` — nodes + edges + lanes under a deterministic layered layout.
//!
//! The first composite after `chart`: the file stores ~30 lines of intent and
//! [`expand`] computes the five primitives at render time — shapes for nodes,
//! connectors with bound labels for edges, container rects for lanes. The
//! expansion is never stored, so retheme and resize are free and `git diff`
//! reads the intent.
//!
//! Layout is a minimal layered pass (Sugiyama-lite): longest-path layering
//! after deterministic cycle-breaking, two barycenter ordering sweeps, even
//! spacing scaled to fit the diagram's box. The plan names a vendored
//! `dagre-rs` here, but no maintained crate exists — these ~200 lines are the
//! deliberate deviation, and they are enough for the architecture diagrams
//! agents actually draw.
//!
//! [`from_mermaid`] converts the flowchart subset agents already emit
//! unprompted. Converted once, at import — the mermaid source is kept in the
//! object's `extra.provenance`, never re-parsed at render.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Result};

use crate::layout::FontStack;
use crate::schema::{
    ConnectorObject, DiagramDirection, DiagramEdge, DiagramLane, DiagramNode, DiagramObject,
    EdgeStyle, EndPoint, Extra, Frame, NodeShape, Object, Paragraph, ShapeObject, Side, Stroke,
    TextObject,
};
use crate::theme::Theme;

/// The smallest node box, in points.
const MIN_NODE_W: f64 = 96.0;
const MIN_NODE_H: f64 = 40.0;
/// Horizontal padding around a node label inside its box.
const NODE_PAD_X: f64 = 14.0;
/// Gap between nodes within a layer.
const NODE_GAP: f64 = 24.0;
/// Gap between layers.
const LAYER_GAP: f64 = 48.0;
/// Padding a lane container adds around its members' bounding box.
const LANE_PAD: f64 = 12.0;
/// The uniform-scale floor. Below it the diagram overflows its box and the
/// expansion says so instead of shrinking nodes into illegibility.
const MIN_SCALE: f64 = 0.6;

/// Expand a diagram into primitives, page-absolute inside its `at`/`size` box.
///
/// Pure and deterministic: same diagram, theme and fonts → byte-identical
/// children. Problems (unknown edge targets, an overflowing layout, lane
/// hulls that overlap) come back as strings the renderer turns into
/// warnings — a diagram with a bad edge still draws everything else.
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

    // Nodes by id, first declaration winning on a duplicate.
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

    // Edges resolved to node indices; an edge naming an unknown node is
    // reported and skipped rather than emitting a dangling connector.
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

    let layer = layer_nodes(d.nodes.len(), &edges);
    let layers = order_layers(&layer, d.nodes.len(), &edges);
    let frames = place_nodes(d, &layers, at, size, theme, fonts, &mut problems);

    let mut children = Vec::new();
    emit_lanes(d, &frames, theme, &mut children, &mut problems);
    emit_nodes(d, &by_id, &frames, &mut children);
    emit_edges(d, &edges, &layer, &mut children);
    (children, problems)
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

/// Measure every node, space layers evenly along the direction, and scale the
/// whole arrangement uniformly to fit the box — never below [`MIN_SCALE`],
/// past which the layout overflows and says so. Gaps scale with the nodes, so
/// disjointness survives any scale.
fn place_nodes(
    d: &DiagramObject,
    layers: &[Vec<usize>],
    at: [f64; 2],
    size: [f64; 2],
    theme: &Theme,
    fonts: &FontStack,
    problems: &mut Vec<String>,
) -> Vec<Frame> {
    let role = theme.role("label").unwrap_or_else(|| theme.body());
    let boxes: Vec<(f64, f64)> = d
        .nodes
        .iter()
        .map(|n| {
            let label_w = fonts.measure(&n.label, &role.family, role.size, role.weight);
            // Ellipses and diamonds inscribe less of their box, so the label
            // needs more room to stay inside the geometry.
            let (factor, min_h) = match n.shape.unwrap_or(NodeShape::RoundRect) {
                NodeShape::Diamond => (1.6, 56.0),
                NodeShape::Ellipse => (1.3, 48.0),
                NodeShape::Rect | NodeShape::RoundRect => (1.0, MIN_NODE_H),
            };
            let w = ((label_w + NODE_PAD_X * 2.0) * factor).max(MIN_NODE_W);
            (w, min_h)
        })
        .collect();

    let down = d.direction() == DiagramDirection::Down;
    // (main, cross) is (y, x) for `down`, (x, y) for `right`.
    let main_of = |b: &(f64, f64)| if down { b.1 } else { b.0 };
    let cross_of = |b: &(f64, f64)| if down { b.0 } else { b.1 };
    let (box_main, box_cross) = if down {
        (size[1], size[0])
    } else {
        (size[0], size[1])
    };

    // Lane containers add label headroom above their members, and that
    // headroom is type — it does not scale with geometry. The gap between
    // layers must clear it even at the scale floor, or a shrunk diagram puts
    // the lane label inside the layer above.
    let has_lanes = !d.lanes.is_empty() || d.nodes.iter().any(|n| n.lane.is_some());
    let layer_gap = if has_lanes {
        let headroom = LANE_PAD + role.size * role.line_height + 8.0;
        LAYER_GAP.max(headroom / MIN_SCALE)
    } else {
        LAYER_GAP
    };

    let layer_main: Vec<f64> = layers
        .iter()
        .map(|lay| lay.iter().map(|&v| main_of(&boxes[v])).fold(0.0, f64::max))
        .collect();
    let layer_cross: Vec<f64> = layers
        .iter()
        .map(|lay| {
            let sum: f64 = lay.iter().map(|&v| cross_of(&boxes[v])).sum();
            sum + NODE_GAP * lay.len().saturating_sub(1) as f64
        })
        .collect();
    let natural_main =
        layer_main.iter().sum::<f64>() + layer_gap * layers.len().saturating_sub(1) as f64;
    let natural_cross = layer_cross.iter().copied().fold(0.0, f64::max);

    let mut scale: f64 = 1.0;
    if natural_main > 0.0 && natural_cross > 0.0 {
        scale = (box_main / natural_main)
            .min(box_cross / natural_cross)
            .min(1.0);
    }
    if scale < MIN_SCALE {
        let (nw, nh) = if down {
            (natural_cross, natural_main)
        } else {
            (natural_main, natural_cross)
        };
        problems.push(format!(
            "layout wants {nw:.0}×{nh:.0} pt but the box is {}×{} pt; scaled to the 60% floor \
             and overflowing — enlarge the diagram or split it",
            size[0], size[1]
        ));
        scale = MIN_SCALE;
    }

    let (main0, cross0) = if down { (at[1], at[0]) } else { (at[0], at[1]) };
    let mut frames = vec![
        Frame {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0
        };
        d.nodes.len()
    ];
    let mut m = main0 + (box_main - natural_main * scale) / 2.0;
    for (li, lay) in layers.iter().enumerate() {
        let mut c = cross0 + (box_cross - layer_cross[li] * scale) / 2.0;
        for &v in lay {
            let (bw, bh) = (boxes[v].0 * scale, boxes[v].1 * scale);
            let node_main = main_of(&boxes[v]) * scale;
            let node_cross = cross_of(&boxes[v]) * scale;
            let nm = m + (layer_main[li] * scale - node_main) / 2.0;
            frames[v] = if down {
                Frame {
                    x: c,
                    y: nm,
                    w: bw,
                    h: bh,
                }
            } else {
                Frame {
                    x: nm,
                    y: c,
                    w: bw,
                    h: bh,
                }
            };
            c += node_cross + NODE_GAP * scale;
        }
        m += layer_main[li] * scale + layer_gap * scale;
    }
    frames
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
    let label_h = role.size * role.line_height;
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
            x: hull.x - LANE_PAD,
            y: hull.y - LANE_PAD - label_h - 6.0,
            w: hull.w + LANE_PAD * 2.0,
            h: hull.h + LANE_PAD * 2.0 + label_h + 6.0,
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

        out.push(Object::Shape(ShapeObject {
            id: format!("{}/lane.{lane_id}", d.id),
            kind: Default::default(),
            geo: "rect".to_string(),
            d: None,
            slot: None,
            at: Some([rect.x, rect.y]),
            size: Some([rect.w, rect.h]),
            fill: Some("@surface".to_string()),
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
        }));
        out.push(Object::Text(TextObject {
            id: format!("{}/lane.{lane_id}.label", d.id),
            kind: Default::default(),
            role: Some("label".to_string()),
            slot: None,
            at: Some([rect.x + LANE_PAD, rect.y + 4.0]),
            size: Some([(rect.w - LANE_PAD * 2.0).max(1.0), label_h]),
            text: vec![Paragraph::Plain(label.unwrap_or(lane_id).to_string())],
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
    out: &mut Vec<Object>,
) {
    for (i, n) in d.nodes.iter().enumerate() {
        // A duplicate declaration was already reported; emitting it too would
        // draw two shapes under one derived id.
        if by_id.get(n.id.as_str()) != Some(&i) {
            continue;
        }
        let f = frames[i];
        out.push(Object::Shape(ShapeObject {
            id: format!("{}/{}", d.id, n.id),
            kind: Default::default(),
            geo: n.shape.unwrap_or(NodeShape::RoundRect).geo().to_string(),
            d: None,
            slot: None,
            at: Some([f.x, f.y]),
            size: Some([f.w, f.h]),
            fill: Some(n.fill.clone().unwrap_or_else(|| "@surface".to_string())),
            fill_opacity: None,
            stroke: Some(Stroke {
                color: Some("@edge".to_string()),
                width: Some(1.0),
                dash: None,
                opacity: None,
                cap: None,
                join: None,
                extra: Extra::new(),
            }),
            radius: None,
            text: vec![Paragraph::Plain(n.label.clone())],
            role: Some("label".to_string()),
            align: None,
            anchor: None,
            alt: None,
            link: None,
            rotation: None,
            flip_h: false,
            flip_v: false,
            extra: Extra::new(),
        }));
    }
}

/// Connectors bound to the generated node shapes by id, labels bound at the
/// midpoint — an edge label that survives any node move by construction.
fn emit_edges(
    d: &DiagramObject,
    edges: &[(usize, usize, usize)],
    layer: &[usize],
    out: &mut Vec<Object>,
) {
    let down = d.direction() == DiagramDirection::Down;
    for &(f, t, ei) in edges {
        let e = &d.edges[ei];
        // Cross-layer edges leave and enter on the layer-facing sides; a
        // same-layer edge lets the renderer face the endpoints at each other.
        let (from_side, to_side) = match layer[f].cmp(&layer[t]) {
            std::cmp::Ordering::Less if down => (Some(Side::Bottom), Some(Side::Top)),
            std::cmp::Ordering::Less => (Some(Side::Right), Some(Side::Left)),
            std::cmp::Ordering::Greater if down => (Some(Side::Top), Some(Side::Bottom)),
            std::cmp::Ordering::Greater => (Some(Side::Left), Some(Side::Right)),
            std::cmp::Ordering::Equal => (None, None),
        };
        let dashed = e.style == Some(EdgeStyle::Dashed);
        out.push(Object::Connector(ConnectorObject {
            id: format!("{}/edge[{ei}]", d.id),
            kind: Default::default(),
            geo: Some("straight".to_string()),
            from: EndPoint {
                object: Some(format!("{}/{}", d.id, e.from)),
                side: from_side,
                at: None,
                extra: Extra::new(),
            },
            to: EndPoint {
                object: Some(format!("{}/{}", d.id, e.to)),
                side: to_side,
                at: None,
                extra: Extra::new(),
            },
            stroke: dashed.then(|| Stroke {
                color: None,
                width: None,
                dash: Some(vec![4.0, 3.0]),
                opacity: None,
                cap: None,
                join: None,
                extra: Extra::new(),
            }),
            head_end: None,
            tail_end: e.arrow.unwrap_or(true).then(|| "arrow".to_string()),
            text: e
                .label
                .as_ref()
                .map(|l| vec![Paragraph::Plain(l.clone())])
                .unwrap_or_default(),
            label_at: None,
            role: None,
            alt: None,
            extra: Extra::new(),
        }));
    }
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

    fn expand_five() -> (Vec<Object>, Vec<String>) {
        let d = diagram(FIVE_NODE_TWO_LANE);
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        expand(&d, &theme, &fonts)
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
        let node_frames: Vec<(String, Frame)> = children
            .iter()
            .filter_map(|o| match o {
                Object::Shape(s) if !s.id.contains("/lane.") => {
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
        // Connectors bind to derived node ids.
        assert!(children.iter().any(|o| matches!(o, Object::Connector(c)
            if c.from.object.as_deref() == Some("arch/ui")
            && c.to.object.as_deref() == Some("arch/api"))));
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
        assert!(!children.iter().any(|o| matches!(o, Object::Connector(_))));
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
        let shapes = children
            .iter()
            .filter(|o| matches!(o, Object::Shape(_)))
            .count();
        let connectors = children
            .iter()
            .filter(|o| matches!(o, Object::Connector(_)))
            .count();
        assert_eq!(shapes, 3, "every node placed despite the cycle");
        assert_eq!(connectors, 3, "the back-edge still draws");
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
}
