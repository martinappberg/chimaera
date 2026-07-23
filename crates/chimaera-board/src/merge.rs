//! The per-object three-way merge: `git merge` for `.board.json`, keyed on
//! slug ids instead of lines — the format's locked promise (§3.4/§6.6 of the
//! plan). Because ids are stable and the format has zero churn fields, two
//! divergent histories of the same board merge structurally, not textually.
//!
//! Semantics, most specific rule first:
//!
//! - **Objects are keyed by id globally** (across pages), so page
//!   restructuring never orphans an object. The merge unit is a page's
//!   top-level object; a `group` merges as one unit, its children riding in
//!   its `objects` field.
//! - Unchanged everywhere → base's version. Changed on one side only → that
//!   side. Deleted on one side + untouched on the other → deleted. Deleted
//!   vs modified → the modified version survives, with a [`Conflict`] note.
//! - Changed on both sides → a field-level three-way over the object's
//!   canonical JSON top-level fields: one-side change wins silently,
//!   both-same wins, both-different keeps OURS and records a [`Conflict`].
//!   [`Object::Unknown`] merges as an opaque value — this build cannot see
//!   its fields, so guessing a field merge would corrupt it.
//! - **Page membership follows the mover.** Pages added on either side are
//!   kept; a page deleted on either side is deleted, and its *surviving*
//!   objects land on the nearest surviving predecessor page in the survivor
//!   side's order (the structural rule: an object survives where it
//!   survives). Page order is ours', with theirs-only pages inserted after
//!   their nearest surviving predecessor — deterministic, never appended to
//!   a grab-bag tail.
//!
//! The output re-serializes through the canonical writer, so a merge is
//! byte-stable and `merge(x, x, x)` is the identity on canonical input.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};

use crate::schema::{Board, Object};

/// One collision the merge had to resolve by policy rather than by a clean
/// three-way rule. `page` is where the object landed in the *merged* output;
/// board-level conflicts carry an empty page. Sentinel fields: `(page)` for
/// divergent moves, `(object)` for opaque both-changed unknowns,
/// `(delete vs modify)` for a deletion racing an edit.
#[derive(Debug, Clone)]
pub struct Conflict {
    pub page: String,
    pub object: String,
    pub field: String,
    pub ours: String,
    pub theirs: String,
}

impl Conflict {
    /// One human-readable line for the merge driver's stderr report.
    pub fn render(&self) -> String {
        let scope = if self.page.is_empty() {
            "board"
        } else {
            &self.page
        };
        // Delete-vs-modify keeps the *modified* side, which the value strings
        // already say; every other conflict resolves ours-wins.
        let kept = if self.field == "(delete vs modify)" {
            ""
        } else {
            " — ours kept"
        };
        format!(
            "conflict: {scope} · {} · {}: ours {} vs theirs {}{kept}",
            self.object, self.field, self.ours, self.theirs
        )
    }
}

/// A merged board plus everything it could not merge cleanly. The board is
/// always complete and writable — conflicts are notes about ours-wins
/// resolutions, never markers inside the file.
#[derive(Debug)]
pub struct MergeOutcome {
    pub board: Board,
    pub conflicts: Vec<Conflict>,
}

/// Three-way merge `ours` and `theirs` against their common ancestor `base`.
/// All three parse leniently and normalize first, so sugar spelling
/// differences (bare-string paragraphs, unsnapped geometry) never read as
/// edits.
pub fn merge(base: &str, ours: &str, theirs: &str) -> Result<MergeOutcome> {
    let base = Side::build(base, "base")?;
    let ours = Side::build(ours, "ours")?;
    let theirs = Side::build(theirs, "theirs")?;
    let mut conflicts = Vec::new();

    // Board-level fields (title, theme, canvas, brief, extras) merge exactly
    // like an object's fields.
    let mut root = merge_fields(
        Some(&base.board_meta),
        &ours.board_meta,
        &theirs.board_meta,
        "",
        "(board)",
        &mut conflicts,
    );

    // Which pages survive: a base page must survive on BOTH sides (a deletion
    // on either side wins over mere presence — its objects are judged
    // individually below); a page new on either side is kept.
    let survives = |pid: &String| {
        if base.page_meta.contains_key(pid) {
            ours.page_meta.contains_key(pid) && theirs.page_meta.contains_key(pid)
        } else {
            ours.page_meta.contains_key(pid) || theirs.page_meta.contains_key(pid)
        }
    };
    let mut merged_order: Vec<String> = ours
        .page_order
        .iter()
        .filter(|p| survives(p))
        .cloned()
        .collect();
    let ours_pages: BTreeSet<&String> = ours.page_order.iter().collect();
    for (i, pid) in theirs.page_order.iter().enumerate() {
        if !survives(pid) || ours_pages.contains(pid) {
            continue;
        }
        // Nearest surviving predecessor in theirs' order; earlier theirs-only
        // pages are already inserted, so a run of them keeps its own order.
        let at = theirs.page_order[..i]
            .iter()
            .rev()
            .find_map(|p| merged_order.iter().position(|m| m == p));
        match at {
            Some(p) => merged_order.insert(p + 1, pid.clone()),
            None => merged_order.insert(0, pid.clone()),
        }
    }
    let mut merged_pages: BTreeSet<String> = merged_order.iter().cloned().collect();

    // Per-object decisions, keyed by id across the whole board.
    let ids: BTreeSet<&String> = base
        .objects
        .keys()
        .chain(ours.objects.keys())
        .chain(theirs.objects.keys())
        .collect();
    let mut kept: BTreeMap<String, Kept> = BTreeMap::new();
    for id in ids {
        let b = base.objects.get(id);
        let o = ours.objects.get(id);
        let t = theirs.objects.get(id);
        match (b, o, t) {
            // Deleted on both sides, or never existed.
            (_, None, None) => {}
            // Added on one side only.
            (None, Some(oe), None) => {
                let page = land(&oe.page, &ours.page_order, &merged_pages, &merged_order);
                kept.insert(id.clone(), Kept::new(oe.value.clone(), page));
            }
            (None, None, Some(te)) => {
                let page = land(&te.page, &theirs.page_order, &merged_pages, &merged_order);
                kept.insert(id.clone(), Kept::new(te.value.clone(), page));
            }
            // Deleted in theirs. Untouched in ours → the deletion wins;
            // modified (value OR page) in ours → the edit wins, noted.
            (Some(be), Some(oe), None) => {
                if oe.value != be.value || oe.page != be.page {
                    let page = land(&oe.page, &ours.page_order, &merged_pages, &merged_order);
                    conflicts.push(Conflict {
                        page: page.clone(),
                        object: id.clone(),
                        field: "(delete vs modify)".to_string(),
                        ours: "modified (kept)".to_string(),
                        theirs: "deleted".to_string(),
                    });
                    kept.insert(id.clone(), Kept::new(oe.value.clone(), page));
                }
            }
            // Deleted in ours, mirror of the above.
            (Some(be), None, Some(te)) => {
                if te.value != be.value || te.page != be.page {
                    let page = land(&te.page, &theirs.page_order, &merged_pages, &merged_order);
                    conflicts.push(Conflict {
                        page: page.clone(),
                        object: id.clone(),
                        field: "(delete vs modify)".to_string(),
                        ours: "deleted".to_string(),
                        theirs: "modified (kept)".to_string(),
                    });
                    kept.insert(id.clone(), Kept::new(te.value.clone(), page));
                }
            }
            // Present on both sides (base optional: add/add when absent).
            (be, Some(oe), Some(te)) => {
                let b_page = be.map(|e| e.page.as_str());
                // Page membership: follow the mover; both moved differently
                // (or add/add onto different pages) → ours, noted.
                let (target, side_order, diverged) = if oe.page == te.page {
                    (&oe.page, &ours.page_order, None)
                } else if Some(oe.page.as_str()) == b_page {
                    (&te.page, &theirs.page_order, None)
                } else if Some(te.page.as_str()) == b_page {
                    (&oe.page, &ours.page_order, None)
                } else {
                    (&oe.page, &ours.page_order, Some(te.page.clone()))
                };
                let page = land(target, side_order, &merged_pages, &merged_order);
                if let Some(theirs_page) = diverged {
                    conflicts.push(Conflict {
                        page: page.clone(),
                        object: id.clone(),
                        field: "(page)".to_string(),
                        ours: oe.page.clone(),
                        theirs: theirs_page,
                    });
                }

                let b_val = be.map(|e| &e.value);
                let value = if oe.value == te.value {
                    oe.value.clone()
                } else if b_val == Some(&oe.value) {
                    te.value.clone()
                } else if b_val == Some(&te.value) {
                    oe.value.clone()
                } else if oe.unknown || te.unknown || be.is_some_and(|e| e.unknown) {
                    // An unknown object is opaque bytes to this build; a field
                    // merge over JSON we don't understand could combine halves
                    // of two valid states into an invalid one.
                    conflicts.push(Conflict {
                        page: page.clone(),
                        object: id.clone(),
                        field: "(object)".to_string(),
                        ours: brief(Some(&oe.value)),
                        theirs: brief(Some(&te.value)),
                    });
                    oe.value.clone()
                } else {
                    // Every non-Unknown variant serializes as a JSON object.
                    let om = oe.value.as_object().expect("objects serialize as maps");
                    let tm = te.value.as_object().expect("objects serialize as maps");
                    Value::Object(merge_fields(
                        b_val.and_then(Value::as_object),
                        om,
                        tm,
                        &page,
                        id,
                        &mut conflicts,
                    ))
                };
                kept.insert(id.clone(), Kept::new(value, page));
            }
        }
    }

    // Both sides deleted every page but an edit survived a delete-vs-modify:
    // resurrect the survivors' pages so nothing silently vanishes.
    if merged_order.is_empty() && !kept.is_empty() {
        let needed: BTreeSet<&String> = kept.values().map(|k| &k.page).collect();
        for side in [&ours, &theirs, &base] {
            for pid in &side.page_order {
                if needed.contains(pid) && !merged_pages.contains(pid) {
                    merged_pages.insert(pid.clone());
                    merged_order.push(pid.clone());
                }
            }
        }
    }

    // Assemble pages: metadata three-way + z-order (ours' order, then theirs'
    // additions in their order, then landed strays in id order).
    let mut placed: BTreeSet<&String> = BTreeSet::new();
    let mut pages_json = Vec::with_capacity(merged_order.len());
    for pid in &merged_order {
        let bm = base.page_meta.get(pid);
        let mut meta = match (ours.page_meta.get(pid), theirs.page_meta.get(pid)) {
            (Some(om), Some(tm)) => merge_fields(bm, om, tm, pid, "(page)", &mut conflicts),
            (Some(om), None) => om.clone(),
            (None, Some(tm)) => tm.clone(),
            // Only reachable for a resurrected base page.
            (None, None) => bm.cloned().unwrap_or_default(),
        };
        let mut order: Vec<&String> = Vec::new();
        for listing in [ours.page_objects.get(pid), theirs.page_objects.get(pid)] {
            for id in listing.into_iter().flatten() {
                if !placed.contains(id) && kept.get(id).is_some_and(|k| &k.page == pid) {
                    order.push(id);
                    placed.insert(id);
                }
            }
        }
        for (id, k) in &kept {
            if &k.page == pid && !placed.contains(id) {
                order.push(id);
                placed.insert(id);
            }
        }
        let objects: Vec<Value> = order.iter().map(|id| kept[*id].value.clone()).collect();
        meta.insert("objects".to_string(), Value::Array(objects));
        pages_json.push(Value::Object(meta));
    }
    root.insert("pages".to_string(), Value::Array(pages_json));

    // Through the lenient parser: a field-merged combination that no longer
    // parses as its type degrades to a preserved Unknown, never a brick.
    let mut board: Board =
        serde_json::from_value(Value::Object(root)).context("assembling the merged board")?;
    crate::normalize(&mut board);
    Ok(MergeOutcome { board, conflicts })
}

/// One surviving object: its merged value and the merged page it lands on.
struct Kept {
    value: Value,
    page: String,
}

impl Kept {
    fn new(value: Value, page: String) -> Self {
        Kept { value, page }
    }
}

/// One parsed input, indexed for the merge: canonical JSON per object (keyed
/// by id), page metadata minus `objects`, and both orders.
struct Side {
    board_meta: Map<String, Value>,
    page_meta: BTreeMap<String, Map<String, Value>>,
    page_order: Vec<String>,
    page_objects: BTreeMap<String, Vec<String>>,
    objects: BTreeMap<String, ObjEntry>,
}

struct ObjEntry {
    page: String,
    value: Value,
    unknown: bool,
}

impl Side {
    fn build(src: &str, label: &str) -> Result<Side> {
        let mut board = crate::parse(src).with_context(|| format!("parsing the {label} board"))?;
        // Normalizing first means sugar differences never read as edits; it
        // also guarantees every object carries an id (generated if blank).
        crate::normalize(&mut board);

        let mut root = serde_json::to_value(&board).context("serializing for merge")?;
        let root = root.as_object_mut().expect("a board serializes as a map");
        root.remove("pages");
        let mut side = Side {
            board_meta: std::mem::take(root),
            page_meta: BTreeMap::new(),
            page_order: Vec::new(),
            page_objects: BTreeMap::new(),
            objects: BTreeMap::new(),
        };
        for page in &board.pages {
            if side.page_meta.contains_key(&page.id) {
                bail!(
                    "duplicate page id {:?} in {label}: ids are the merge key",
                    page.id
                );
            }
            let mut pv = serde_json::to_value(page).context("serializing for merge")?;
            let pv = pv.as_object_mut().expect("a page serializes as a map");
            pv.remove("objects");
            side.page_meta.insert(page.id.clone(), std::mem::take(pv));
            side.page_order.push(page.id.clone());
            let mut order = Vec::with_capacity(page.objects.len());
            for obj in &page.objects {
                let id = obj.id().to_string();
                if side.objects.contains_key(&id) {
                    bail!(
                        "duplicate object id {id:?} in {label}: ids are the merge key, \
                         never auto-renamed"
                    );
                }
                side.objects.insert(
                    id.clone(),
                    ObjEntry {
                        page: page.id.clone(),
                        value: serde_json::to_value(obj).context("serializing for merge")?,
                        unknown: matches!(obj, Object::Unknown(_)),
                    },
                );
                order.push(id);
            }
            side.page_objects.insert(page.id.clone(), order);
        }
        Ok(side)
    }
}

/// The field-level three-way over one JSON map: one-side change wins,
/// both-same wins, both-different keeps ours and records a conflict. A `None`
/// on any leg is an absent field, so additions and removals fall out of the
/// same four rules.
fn merge_fields(
    base: Option<&Map<String, Value>>,
    ours: &Map<String, Value>,
    theirs: &Map<String, Value>,
    page: &str,
    object: &str,
    conflicts: &mut Vec<Conflict>,
) -> Map<String, Value> {
    let empty = Map::new();
    let base = base.unwrap_or(&empty);
    let keys: BTreeSet<&String> = ours
        .keys()
        .chain(theirs.keys())
        .chain(base.keys())
        .collect();
    let mut out = Map::new();
    for k in keys {
        let (b, o, t) = (base.get(k), ours.get(k), theirs.get(k));
        let winner = if o == t {
            o
        } else if o == b {
            t
        } else if t == b {
            o
        } else {
            conflicts.push(Conflict {
                page: page.to_string(),
                object: object.to_string(),
                field: k.clone(),
                ours: brief(o),
                theirs: brief(t),
            });
            o
        };
        if let Some(v) = winner {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

/// Where an object targeting `page` actually lands: the page itself when it
/// survives, else the nearest surviving predecessor in the placing side's
/// page order, else the first merged page. Returns the target verbatim only
/// when no page survives at all — the resurrection pass then re-creates it.
fn land(
    page: &str,
    side_order: &[String],
    merged_pages: &BTreeSet<String>,
    merged_order: &[String],
) -> String {
    if merged_pages.contains(page) {
        return page.to_string();
    }
    if let Some(pos) = side_order.iter().position(|p| p == page) {
        for prior in side_order[..pos].iter().rev() {
            if merged_pages.contains(prior) {
                return prior.clone();
            }
        }
    }
    merged_order
        .first()
        .cloned()
        .unwrap_or_else(|| page.to_string())
}

/// A one-line rendering of a conflicting value for the report. Truncated —
/// the report names the field; the file itself carries the resolution.
fn brief(v: Option<&Value>) -> String {
    match v {
        None => "removed".to_string(),
        Some(v) => {
            let s = v.to_string();
            if s.chars().count() > 80 {
                let cut: String = s.chars().take(79).collect();
                format!("{cut}…")
            } else {
                s
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The plan's canonical scene: a chart and a callout on `results`, a note
    // on `methods`. Geometry sits on the 8 pt grid so normalize is a no-op.
    const BASE: &str = r#"{
      "format": "chimaera.board",
      "formatVersion": 1,
      "title": "Bench results",
      "canvas": { "size": [960, 540] },
      "pages": [
        { "id": "results", "objects": [
          { "id": "bench-chart", "type": "shape", "geo": "rect",
            "at": [80, 120], "size": [400, 256] },
          { "id": "callout", "type": "shape", "geo": "roundRect",
            "at": [520, 152], "size": [200, 88], "fill": "@accent1" }
        ] },
        { "id": "methods", "objects": [
          { "id": "note", "type": "text", "at": [80, 80], "size": [400, 80],
            "text": ["methods"] }
        ] }
      ]
    }"#;

    fn edit(src: &str, f: impl FnOnce(&mut Value)) -> String {
        let mut v: Value = serde_json::from_str(src).unwrap();
        f(&mut v);
        serde_json::to_string(&v).unwrap()
    }

    fn shape<'a>(board: &'a Board, id: &str) -> (&'a str, &'a crate::schema::ShapeObject) {
        match board.objects().find(|(_, o)| o.id() == id) {
            Some((page, Object::Shape(s))) => (page, s),
            other => panic!("expected shape {id}, found {other:?}"),
        }
    }

    fn ids_on<'a>(board: &'a Board, page: &str) -> Vec<&'a str> {
        board
            .pages
            .iter()
            .find(|p| p.id == page)
            .unwrap()
            .objects
            .iter()
            .map(|o| o.id())
            .collect()
    }

    #[test]
    fn disjoint_edits_merge_cleanly_with_zero_loss() {
        // The plan's canonical case: the agent restyles `callout` while the
        // human moves `bench-chart`.
        let ours = edit(BASE, |v| {
            v["pages"][0]["objects"][1]["fill"] = "@accent2".into();
        });
        let theirs = edit(BASE, |v| {
            v["pages"][0]["objects"][0]["at"] = serde_json::json!([96, 128]);
        });
        let out = merge(BASE, &ours, &theirs).unwrap();
        assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
        let (_, callout) = shape(&out.board, "callout");
        assert_eq!(callout.fill.as_deref(), Some("@accent2"));
        let (_, chart) = shape(&out.board, "bench-chart");
        assert_eq!(chart.at, Some([96.0, 128.0]));
    }

    #[test]
    fn both_sides_different_fields_of_one_object_merge_without_conflict() {
        let ours = edit(BASE, |v| {
            v["pages"][0]["objects"][1]["fill"] = "@accent2".into();
        });
        let theirs = edit(BASE, |v| {
            v["pages"][0]["objects"][1]["at"] = serde_json::json!([528, 160]);
        });
        let out = merge(BASE, &ours, &theirs).unwrap();
        assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
        let (_, callout) = shape(&out.board, "callout");
        assert_eq!(callout.fill.as_deref(), Some("@accent2"));
        assert_eq!(callout.at, Some([528.0, 160.0]));
    }

    #[test]
    fn same_field_conflict_keeps_ours_and_reports() {
        let ours = edit(BASE, |v| {
            v["pages"][0]["objects"][1]["fill"] = "@accent2".into();
        });
        let theirs = edit(BASE, |v| {
            v["pages"][0]["objects"][1]["fill"] = "@accent3".into();
        });
        let out = merge(BASE, &ours, &theirs).unwrap();
        let (_, callout) = shape(&out.board, "callout");
        assert_eq!(callout.fill.as_deref(), Some("@accent2"), "ours wins");
        assert_eq!(out.conflicts.len(), 1, "{:?}", out.conflicts);
        let c = &out.conflicts[0];
        assert_eq!((c.page.as_str(), c.object.as_str()), ("results", "callout"));
        assert_eq!(c.field, "fill");
        assert!(c.ours.contains("@accent2") && c.theirs.contains("@accent3"));
        assert!(c.render().contains("ours kept"), "{}", c.render());
    }

    #[test]
    fn add_add_of_different_objects_keeps_both_in_ours_then_theirs_order() {
        let ours = edit(BASE, |v| {
            v["pages"][0]["objects"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!(
                    { "id": "ours-note", "type": "text", "at": [80, 400],
                      "size": [200, 80], "text": ["ours"] }
                ));
        });
        let theirs = edit(BASE, |v| {
            v["pages"][0]["objects"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!(
                    { "id": "theirs-note", "type": "text", "at": [320, 400],
                      "size": [200, 80], "text": ["theirs"] }
                ));
        });
        let out = merge(BASE, &ours, &theirs).unwrap();
        assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
        assert_eq!(
            ids_on(&out.board, "results"),
            ["bench-chart", "callout", "ours-note", "theirs-note"]
        );
    }

    #[test]
    fn delete_vs_untouched_deletes() {
        let ours = edit(BASE, |v| {
            v["pages"][0]["objects"].as_array_mut().unwrap().remove(1);
        });
        let out = merge(BASE, &ours, BASE).unwrap();
        assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
        assert_eq!(ids_on(&out.board, "results"), ["bench-chart"]);
    }

    #[test]
    fn delete_vs_modify_keeps_the_modified_side_both_ways() {
        let deleted = edit(BASE, |v| {
            v["pages"][0]["objects"].as_array_mut().unwrap().remove(1);
        });
        let restyled = edit(BASE, |v| {
            v["pages"][0]["objects"][1]["fill"] = "@accent3".into();
        });

        // Ours deleted, theirs modified → theirs survives.
        let out = merge(BASE, &deleted, &restyled).unwrap();
        let (_, callout) = shape(&out.board, "callout");
        assert_eq!(callout.fill.as_deref(), Some("@accent3"));
        assert_eq!(out.conflicts.len(), 1);
        let c = &out.conflicts[0];
        assert_eq!(c.field, "(delete vs modify)");
        assert!(c.theirs.contains("kept"), "{}", c.render());

        // Ours modified, theirs deleted → ours survives.
        let out = merge(BASE, &restyled, &deleted).unwrap();
        let (_, callout) = shape(&out.board, "callout");
        assert_eq!(callout.fill.as_deref(), Some("@accent3"));
        assert_eq!(out.conflicts.len(), 1);
        assert!(out.conflicts[0].ours.contains("kept"));
    }

    #[test]
    fn a_move_between_pages_follows_the_mover_even_against_a_restyle() {
        let ours = edit(BASE, |v| {
            v["pages"][0]["objects"][0]["fill"] = "@accent4".into();
        });
        let theirs = edit(BASE, |v| {
            let obj = v["pages"][0]["objects"].as_array_mut().unwrap().remove(0);
            v["pages"][1]["objects"].as_array_mut().unwrap().push(obj);
        });
        let out = merge(BASE, &ours, &theirs).unwrap();
        assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
        let (page, chart) = shape(&out.board, "bench-chart");
        assert_eq!(page, "methods", "the mover is followed");
        assert_eq!(
            chart.fill.as_deref(),
            Some("@accent4"),
            "the restyle rides along"
        );
        assert_eq!(ids_on(&out.board, "methods"), ["note", "bench-chart"]);
        assert_eq!(ids_on(&out.board, "results"), ["callout"]);
    }

    #[test]
    fn theirs_only_pages_insert_after_their_nearest_surviving_predecessor() {
        let ours = edit(BASE, |v| {
            v["pages"].as_array_mut().unwrap().push(serde_json::json!(
                { "id": "ours-end", "objects": [] }
            ));
        });
        let theirs = edit(BASE, |v| {
            v["pages"]
                .as_array_mut()
                .unwrap()
                .insert(1, serde_json::json!({ "id": "extra", "objects": [] }));
        });
        let out = merge(BASE, &ours, &theirs).unwrap();
        assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
        let order: Vec<&str> = out.board.pages.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(order, ["results", "extra", "methods", "ours-end"]);
    }

    #[test]
    fn survivors_of_a_deleted_page_land_on_the_nearest_surviving_predecessor() {
        // Ours deletes the whole `methods` page; theirs had moved `note` on
        // it. The page dies, the modified object survives — on `results`.
        let ours = edit(BASE, |v| {
            v["pages"].as_array_mut().unwrap().remove(1);
        });
        let theirs = edit(BASE, |v| {
            v["pages"][1]["objects"][0]["at"] = serde_json::json!([96, 80]);
        });
        let out = merge(BASE, &ours, &theirs).unwrap();
        assert_eq!(out.board.pages.len(), 1);
        assert_eq!(
            ids_on(&out.board, "results"),
            ["bench-chart", "callout", "note"]
        );
        assert_eq!(out.conflicts.len(), 1, "{:?}", out.conflicts);
        let c = &out.conflicts[0];
        assert_eq!(c.field, "(delete vs modify)");
        assert_eq!(c.page, "results", "the note names the landing page");
    }

    #[test]
    fn unknown_objects_merge_as_opaque_values() {
        let with_unknown = edit(BASE, |v| {
            v["pages"][0]["objects"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!(
                    { "id": "mystery", "type": "wibble", "payload": 1 }
                ));
        });
        let ours = edit(&with_unknown, |v| {
            v["pages"][0]["objects"][2]["payload"] = 2.into();
        });
        let theirs = edit(&with_unknown, |v| {
            v["pages"][0]["objects"][2]["payload"] = 3.into();
        });
        let out = merge(&with_unknown, &ours, &theirs).unwrap();
        assert_eq!(out.conflicts.len(), 1, "{:?}", out.conflicts);
        assert_eq!(out.conflicts[0].field, "(object)");
        let raw = serde_json::to_value(
            out.board
                .objects()
                .find(|(_, o)| o.id() == "mystery")
                .unwrap()
                .1,
        )
        .unwrap();
        assert_eq!(raw["payload"], 2, "ours wins wholesale");

        // One-side change to an unknown is clean.
        let out = merge(&with_unknown, &ours, &with_unknown).unwrap();
        assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
    }

    #[test]
    fn board_level_fields_merge_like_object_fields() {
        let ours = edit(BASE, |v| {
            v["title"] = "Bench results v2".into();
        });
        let theirs = edit(BASE, |v| {
            v["theme"] = "talk-light".into();
        });
        let out = merge(BASE, &ours, &theirs).unwrap();
        assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
        assert_eq!(out.board.title.as_deref(), Some("Bench results v2"));
        assert_eq!(out.board.theme.as_deref(), Some("talk-light"));

        let theirs = edit(BASE, |v| {
            v["title"] = "Bench results v3".into();
        });
        let out = merge(BASE, &ours, &theirs).unwrap();
        assert_eq!(out.board.title.as_deref(), Some("Bench results v2"));
        assert_eq!(out.conflicts.len(), 1);
        assert_eq!(out.conflicts[0].object, "(board)");
        assert_eq!(out.conflicts[0].field, "title");
        assert!(out.conflicts[0].page.is_empty());
    }

    #[test]
    fn merge_output_is_byte_stable_and_identity_on_identical_inputs() {
        // merge(x, x, x) is exactly x's normalized canonical form.
        let mut board = crate::parse(BASE).unwrap();
        crate::normalize(&mut board);
        let canonical = crate::to_string(&board).unwrap();
        let out = merge(BASE, BASE, BASE).unwrap();
        assert!(out.conflicts.is_empty());
        assert_eq!(crate::to_string(&out.board).unwrap(), canonical);

        // And a real merge's output is a fixed point of the whole pipeline.
        let ours = edit(BASE, |v| {
            v["pages"][0]["objects"][1]["fill"] = "@accent2".into();
        });
        let theirs = edit(BASE, |v| {
            v["pages"][0]["objects"][0]["at"] = serde_json::json!([96, 128]);
        });
        let merged = crate::to_string(&merge(BASE, &ours, &theirs).unwrap().board).unwrap();
        let again = crate::to_string(&merge(&merged, &merged, &merged).unwrap().board).unwrap();
        assert_eq!(merged, again, "canonical output must be a fixed point");
    }

    #[test]
    fn duplicate_ids_refuse_to_merge() {
        let dup = edit(BASE, |v| {
            v["pages"][1]["objects"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!(
                    { "id": "callout", "type": "shape", "geo": "rect",
                      "at": [80, 200], "size": [80, 80] }
                ));
        });
        let err = merge(BASE, &dup, BASE).unwrap_err();
        assert!(err.to_string().contains("duplicate object id"), "{err}");
        assert!(err.to_string().contains("ours"), "names the side: {err}");
    }
}
