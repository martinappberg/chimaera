//! `describe` — what the agent reads back.
//!
//! The bidirectional loop's read half: a compact, deterministic, plain-text
//! account of where everything is, in the same point coordinates the agent
//! writes. It is designed to be *cheaper to read than the JSON* — an agent
//! that has to re-read the whole board to learn one position will stop
//! looking, and the loop dies there.
//!
//! Vocabulary note: an `image` with provenance prints as `plot` — the human
//! word survives even though the schema branch does not.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::schema::{Board, Frame, Object};

/// Render the whole board as the agent-facing description.
pub fn describe(board: &Board) -> String {
    describe_with_journal(board, None)
}

/// [`describe`], plus one line pointing at the semantic edit journal when the
/// board has one: `journal: N events · latest seq M` — `journal` is
/// `(event count, latest seq)`, i.e. [`crate::journal::summary`]. The line
/// sits right under the board summary so an agent reading positions also
/// learns there is change history worth `--since`-ing.
pub fn describe_with_journal(board: &Board, journal: Option<(u64, u64)>) -> String {
    let mut s = String::new();
    let title = board.title.as_deref().unwrap_or("(untitled)");
    let _ = writeln!(
        s,
        "board {:?} · {}×{} pt · {} page{}",
        title,
        board.canvas.width(),
        board.canvas.height(),
        board.pages.len(),
        if board.pages.len() == 1 { "" } else { "s" }
    );
    if let Some((events, latest)) = journal {
        let _ = writeln!(
            s,
            "journal: {events} event{} · latest seq {latest}",
            if events == 1 { "" } else { "s" }
        );
    }
    if let Some(theme) = &board.theme {
        let _ = writeln!(s, "theme {theme}");
    }
    if let Some(brief) = &board.brief {
        if let Some(t) = &brief.thesis {
            let _ = writeln!(s, "thesis: {t}");
        }
        if let Some(a) = &brief.audience {
            let _ = writeln!(s, "audience: {a}");
        }
        if let Some(m) = brief.minutes {
            let _ = writeln!(s, "minutes: {m}");
        }
    }

    // Slot/anchor geometry is derived, never stored, so describe resolves it
    // the same way render does. The bundled default supplies spacing when no
    // theme is in hand — every bundled talk theme shares one spacing block —
    // and `None` for fonts skips the system font scan resolution never needs.
    let theme = crate::theme::default_for(false);

    for (i, page) in board.pages.iter().enumerate() {
        let _ = writeln!(s);
        let _ = write!(s, "page {} ({})", i + 1, page.id);
        if let Some(intent) = &page.intent {
            let _ = write!(s, " · {}", intent.kind);
            if let Some(why) = &intent.why {
                let _ = write!(s, " — {why}");
            }
        }
        let _ = writeln!(s);
        let resolved = crate::slots::resolve_page_frames(board, page, &theme, None);
        for obj in &page.objects {
            describe_object(&mut s, obj, 1, &resolved);
        }
        if let Some(notes) = &page.notes {
            let _ = writeln!(s, "  notes: {notes}");
        }
    }
    s
}

fn describe_object(s: &mut String, obj: &Object, depth: usize, resolved: &BTreeMap<String, Frame>) {
    let indent = "  ".repeat(depth);
    // Slot- and anchor-placed objects print BOTH the source and the
    // resolution — `slot=title → at [72, 64] size [816, 64]` — so the agent
    // sees what it wrote and where that landed without doing the arithmetic.
    let frame = resolved.get(obj.id()).copied().or_else(|| obj.frame());
    let source = placement_source(obj);
    let geo = match (&source, frame) {
        (Some(src), Some(f)) => format!(" {src} → at [{}, {}] size [{}, {}]", f.x, f.y, f.w, f.h),
        (Some(src), None) => format!(" {src} (unresolved)"),
        (None, Some(f)) => format!(" at [{}, {}] size [{}, {}]", f.x, f.y, f.w, f.h),
        (None, None) => String::new(),
    };

    match obj {
        Object::Text(t) => {
            let text = t
                .text
                .iter()
                .map(|p| p.plain_text())
                .collect::<Vec<_>>()
                .join(" / ");
            let role = t.role.as_deref().unwrap_or("body");
            let _ = writeln!(
                s,
                "{indent}{} text/{role}{geo}: {}",
                t.id,
                truncate(&text, 80)
            );
        }
        Object::Shape(sh) => {
            let text = sh
                .text
                .iter()
                .map(|p| p.plain_text())
                .collect::<Vec<_>>()
                .join(" / ");
            let _ = write!(s, "{indent}{} shape/{}{geo}", sh.id, sh.geo);
            if !text.is_empty() {
                let _ = write!(s, ": {}", truncate(&text, 60));
            }
            let _ = writeln!(s);
        }
        Object::Connector(c) => {
            let ep = |e: &crate::schema::EndPoint| -> String {
                match (&e.object, e.at) {
                    (Some(o), _) => match e.side {
                        Some(side) => format!("{o}.{side:?}").to_lowercase(),
                        None => o.clone(),
                    },
                    (None, Some(at)) => format!("[{}, {}]", at[0], at[1]),
                    _ => "?".to_string(),
                }
            };
            let label = c
                .text
                .iter()
                .map(|p| p.plain_text())
                .collect::<Vec<_>>()
                .join(" ");
            let _ = write!(
                s,
                "{indent}{} connector {} → {}",
                c.id,
                ep(&c.from),
                ep(&c.to)
            );
            if !label.is_empty() {
                let _ = write!(s, " label {:?}", label);
            }
            let _ = writeln!(s);
        }
        Object::Image(img) => {
            // The human word for an image with provenance is "plot".
            let kind = if img.provenance.is_some() {
                "plot"
            } else {
                "image"
            };
            let _ = writeln!(s, "{indent}{} {kind}{geo}: {}", img.id, img.src);
        }
        Object::Group(g) => {
            let _ = writeln!(
                s,
                "{indent}{} group{geo} ({} children)",
                g.id,
                g.objects.len()
            );
            for child in &g.objects {
                describe_object(s, child, depth + 1, resolved);
            }
        }
        Object::Chart(c) => {
            let marks = c
                .marks
                .iter()
                .map(|m| format!("{:?}", m.mark).to_lowercase())
                .collect::<Vec<_>>()
                .join("+");
            let _ = write!(
                s,
                "{indent}{} chart/{marks}{geo} · {} rows · {}",
                c.id,
                c.data.values.len(),
                c.data.origin.label()
            );
            if let (Some(x), Some(y)) = (&c.x, &c.y) {
                let _ = write!(s, " · {} × {}", x.field, y.field);
            }
            let _ = writeln!(s);
        }
        Object::Diagram(d) => {
            let _ = writeln!(
                s,
                "{indent}{} diagram{geo} · {} nodes · {} edges",
                d.id,
                d.nodes.len(),
                d.edges.len()
            );
        }
        Object::PanelLabel(pl) => {
            let _ = writeln!(s, "{indent}{} panelLabel{geo}: {:?}", pl.id, pl.label);
        }
        Object::Scalebar(sb) => {
            let _ = write!(s, "{indent}{} scalebar{geo} · {} pt", sb.id, sb.length_pt);
            if let Some(label) = &sb.label {
                let _ = write!(s, " {label:?}");
            }
            let _ = writeln!(s);
        }
        Object::SigBracket(sig) => {
            let ep = |e: &crate::schema::EndPoint| -> String {
                e.object.clone().unwrap_or_else(|| "?".to_string())
            };
            let _ = write!(
                s,
                "{indent}{} sigBracket {}↔{}",
                sig.id,
                ep(&sig.from),
                ep(&sig.to)
            );
            if let Some(label) = &sig.label {
                let _ = write!(s, " {label:?}");
            }
            let _ = writeln!(s);
        }
        Object::Legend(lg) => {
            let _ = write!(
                s,
                "{indent}{} legend{geo} · {} entr{}",
                lg.id,
                lg.entries.len(),
                if lg.entries.len() == 1 { "y" } else { "ies" }
            );
            if lg.entries.len() <= 3 {
                let _ = write!(s, " · prefer direct labels at ≤3 series");
            }
            let _ = writeln!(s);
        }
        Object::Colorbar(cb) => {
            let _ = writeln!(
                s,
                "{indent}{} colorbar{geo} · {} · [{}, {}]",
                cb.id, cb.colormap, cb.domain[0], cb.domain[1]
            );
        }
        Object::Callout(co) => {
            let _ = write!(s, "{indent}{} callout{geo}", co.id);
            if let Some(target) = co.tail.as_ref().and_then(|t| t.object.as_deref()) {
                let _ = write!(s, " → {target}");
            }
            let text = co
                .text
                .iter()
                .map(|p| p.plain_text())
                .collect::<Vec<_>>()
                .join(" / ");
            if !text.is_empty() {
                let _ = write!(s, ": {}", truncate(&text, 60));
            }
            let _ = writeln!(s);
        }
        Object::Inset(inset) => {
            let [x, y, w, h] = inset.of.px;
            let _ = writeln!(
                s,
                "{indent}{} inset{geo} of {} px [{x}, {y}, {w}, {h}]",
                inset.id, inset.of.object
            );
        }
        Object::Unknown(u) => {
            let why = match &u.error {
                Some(e) => format!("failed to parse: {}", truncate(e, 60)),
                None => "unknown to this build".to_string(),
            };
            let _ = writeln!(s, "{indent}{} {}? ({why})", u.id, u.kind);
        }
    }
}

/// How this object is placed, when not by explicit geometry alone:
/// `slot=title`, `anchor=chart.below`, `anchor=micro-1.px`. `None` for a
/// plainly-placed object, whose line stays exactly as it always was.
fn placement_source(obj: &Object) -> Option<String> {
    if let Some(slot) = obj.slot() {
        return Some(format!("slot={slot}"));
    }
    let a = crate::slots::anchor_of(obj)?;
    let target = a.object.as_deref()?;
    if a.px.is_some() {
        return Some(format!("anchor={target}.px"));
    }
    if a.data.is_some() {
        return Some(format!("anchor={target}.data"));
    }
    Some(format!(
        "anchor={target}.{}",
        a.rel.as_deref().unwrap_or("center-of")
    ))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max - 1).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_is_compact_and_names_positions() {
        let mut b = crate::parse(
            r#"{"format":"chimaera.board","formatVersion":1,"title":"Review",
                "canvas":{"size":[960,540]},
                "pages":[{"id":"bench",
                  "intent":{"kind":"claim-evidence","why":"the speed claim"},
                  "objects":[
                    {"id":"heading","type":"text","role":"heading","at":[72,64],"size":[816,56],
                     "text":["Parse time drops on every fixture"]},
                    {"id":"bench-chart","type":"chart","at":[72,152],"size":[480,320],
                     "data":{"origin":"command","values":[{"f":"a","ms":1}]},
                     "x":{"field":"f"},"y":{"field":"ms"}},
                    {"id":"fig","type":"image","src":"assets/fig.svg",
                     "at":[600,152],"size":[288,320],
                     "provenance":{"script":"scripts/fig.py"}}]}]}"#,
        )
        .unwrap();
        crate::normalize(&mut b);
        let out = describe(&b);
        assert!(
            out.contains("board \"Review\" · 960×540 pt · 1 page"),
            "{out}"
        );
        assert!(out.contains("claim-evidence — the speed claim"), "{out}");
        assert!(out.contains("heading text/heading at [72, 64]"), "{out}");
        assert!(out.contains("from command"), "{out}");
        // An image with provenance prints as plot.
        assert!(out.contains("fig plot"), "{out}");
    }

    #[test]
    fn describe_with_journal_prints_one_summary_line() {
        let b = crate::parse(
            r#"{"format":"chimaera.board","formatVersion":1,"title":"Review",
                "canvas":{"size":[100,100]},"pages":[{"id":"p","objects":[]}]}"#,
        )
        .unwrap();
        let out = describe_with_journal(&b, Some((3, 41)));
        assert_eq!(
            out.lines().nth(1).unwrap(),
            "journal: 3 events · latest seq 41",
            "right under the board summary: {out}"
        );
        let one = describe_with_journal(&b, Some((1, 1)));
        assert!(one.contains("journal: 1 event · latest seq 1"), "{one}");
        // No journal, no line — the wrapper stays byte-identical.
        assert!(!describe(&b).contains("journal:"));
    }

    #[test]
    fn describe_prints_slot_source_and_resolution() {
        let mut b = crate::parse(
            r#"{"format":"chimaera.board","formatVersion":1,"title":"Slots",
                "canvas":{"size":[960,540]},
                "pages":[{"id":"p","layout":"title-body","objects":[
                  {"id":"heading","type":"text","role":"heading","slot":"title",
                   "text":["Slot-placed heading"]},
                  {"id":"note","type":"shape","geo":"rect","size":[48,32],
                   "anchor":{"object":"heading","rel":"below"}}]}]}"#,
        )
        .unwrap();
        crate::normalize(&mut b);
        let out = describe(&b);
        // Both the source and the resolution, on one line.
        assert!(
            out.contains("heading text/heading slot=title → at [72, 64] size [816, 64]"),
            "{out}"
        );
        assert!(
            out.contains("note shape/rect anchor=heading.below → at [456, 128] size [48, 32]"),
            "{out}"
        );
    }

    #[test]
    fn describe_prints_the_brief_up_top() {
        let b = crate::parse(
            r#"{"format":"chimaera.board","formatVersion":1,"title":"Review",
                "canvas":{"size":[960,540]},
                "brief":{"thesis":"ship it","audience":"the team","minutes":10},
                "pages":[{"id":"p","objects":[]}]}"#,
        )
        .unwrap();
        let out = describe(&b);
        let head: Vec<&str> = out.lines().take(4).collect();
        assert_eq!(head[1], "thesis: ship it", "{out}");
        assert_eq!(head[2], "audience: the team", "{out}");
        assert_eq!(head[3], "minutes: 10", "{out}");
    }

    #[test]
    fn describe_is_deterministic() {
        let b = crate::parse(
            r#"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[100,100]},"pages":[{"id":"p","objects":[]}]}"#,
        )
        .unwrap();
        assert_eq!(describe(&b), describe(&b));
    }
}
