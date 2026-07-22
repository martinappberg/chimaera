//! `lint` — the checks that refuse, and the ones that only report.
//!
//! The set is deliberately narrow: false positives are a real cost, and the
//! plan refuses whole categories (general overlap, data-ink ratio, whitespace
//! balance, "wrong hierarchy") because they are judgement, not measurement.
//! Every finding names object, field, measured value and expected value.
//!
//! Slice 0 ships the legality profile only: duplicate ids, inline data caps,
//! sub-floor text, off-canvas, unresolved theme tokens, unresolved connector
//! endpoints, unknown objects. `--style` (near-miss alignment and friends)
//! arrives with the pane, where its findings can be clicked.

use crate::normalize::{Diagnostic, Severity};
use crate::schema::{Board, Object, Paragraph};
use crate::theme::Theme;

/// Run the legality lint over a normalized board.
pub fn lint(board: &Board, theme: &Theme) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let canvas = &board.canvas;

    for page in &board.pages {
        let index = crate::normalize::index_page(page);
        for obj in page.walk() {
            // Off-canvas: parked is legal, invisible-by-accident is not worth
            // the silence.
            if let Some(f) = obj.frame() {
                if f.right() < 0.0
                    || f.bottom() < 0.0
                    || f.x > canvas.width()
                    || f.y > canvas.height()
                {
                    diags.push(
                        Diagnostic::new(
                            Severity::Warning,
                            format!(
                                "off-canvas: at [{}, {}] on a {}×{} canvas",
                                f.x,
                                f.y,
                                canvas.width(),
                                canvas.height()
                            ),
                        )
                        .at(&page.id, obj.id())
                        .field("at"),
                    );
                }
            }

            match obj {
                Object::Text(t) => {
                    check_colors_in_paragraphs(&t.text, theme, &page.id, &t.id, &mut diags);
                    if let Some(role) = t.role.as_deref() {
                        if theme.role(role).is_none() {
                            diags.push(
                                Diagnostic::new(
                                    Severity::Warning,
                                    format!(
                                        "role {role:?} is not in theme {:?}; body is used",
                                        theme.id
                                    ),
                                )
                                .at(&page.id, &t.id)
                                .field("role"),
                            );
                        }
                    }
                }
                Object::Shape(sh) => {
                    if let Some(fill) = sh.fill.as_deref() {
                        check_color(fill, theme, &page.id, &sh.id, "fill", &mut diags);
                    }
                    if let Some(stroke) = sh.stroke.as_ref().and_then(|s| s.color.as_deref()) {
                        check_color(stroke, theme, &page.id, &sh.id, "stroke.color", &mut diags);
                    }
                    if sh.geo == "path" && sh.d.is_none() {
                        diags.push(
                            Diagnostic::new(Severity::Error, "geo \"path\" requires `d`")
                                .at(&page.id, &sh.id)
                                .field("d"),
                        );
                    }
                }
                Object::Connector(c) => {
                    for (name, ep) in [("from", &c.from), ("to", &c.to)] {
                        if let Some(target) = ep.object.as_deref() {
                            if !index.contains_key(target) {
                                diags.push(
                                    Diagnostic::new(
                                        Severity::Error,
                                        format!(
                                            "connector {name} binds to {target:?}, which is not \
                                             on this page"
                                        ),
                                    )
                                    .at(&page.id, &c.id)
                                    .field(name),
                                );
                            }
                        } else if ep.at.is_none() {
                            diags.push(
                                Diagnostic::new(
                                    Severity::Error,
                                    format!("connector {name} has neither `object` nor `at`"),
                                )
                                .at(&page.id, &c.id)
                                .field(name),
                            );
                        }
                    }
                }
                Object::Chart(c) => {
                    if c.data.values.len() > crate::normalize::MAX_INLINE_ROWS {
                        diags.push(
                            Diagnostic::new(
                                Severity::Error,
                                format!(
                                    "{} inline rows exceeds the {}-row cap",
                                    c.data.values.len(),
                                    crate::normalize::MAX_INLINE_ROWS
                                ),
                            )
                            .at(&page.id, &c.id)
                            .field("data.values"),
                        );
                    }
                }
                Object::Unknown(u) => {
                    diags.push(
                        Diagnostic::new(
                            Severity::Warning,
                            match &u.error {
                                Some(e) => format!("type {:?} failed to parse: {e}", u.kind),
                                None => {
                                    format!("type {:?} is unknown to this build", u.kind)
                                }
                            },
                        )
                        .at(&page.id, &u.id),
                    );
                }
                Object::Image(_) | Object::Group(_) => {}
            }
        }
    }

    diags
}

fn check_colors_in_paragraphs(
    paras: &[Paragraph],
    theme: &Theme,
    page: &str,
    id: &str,
    diags: &mut Vec<Diagnostic>,
) {
    for p in paras {
        if let Paragraph::Rich(rich) = p {
            for r in &rich.runs {
                if let Some(c) = r.color.as_deref() {
                    check_color(c, theme, page, id, "color", diags);
                }
            }
        }
    }
}

fn check_color(
    reference: &str,
    theme: &Theme,
    page: &str,
    id: &str,
    field: &str,
    diags: &mut Vec<Diagnostic>,
) {
    if theme.color(reference).is_none() {
        diags.push(
            Diagnostic::new(
                Severity::Error,
                format!(
                    "{reference:?} does not resolve in theme {:?}; tokens are {}",
                    theme.id,
                    theme
                        .palette
                        .keys()
                        .map(|k| format!("@{k}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
            )
            .at(page, id)
            .field(field),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linted(objects: &str) -> Vec<Diagnostic> {
        let mut b = crate::parse(&format!(
            r#"{{"format":"chimaera.board","formatVersion":1,
                "canvas":{{"size":[960,540]}},
                "pages":[{{"id":"p1","objects":[{objects}]}}]}}"#
        ))
        .unwrap();
        crate::normalize(&mut b);
        lint(&b, &crate::theme::default_for(true))
    }

    #[test]
    fn an_unknown_token_is_an_error_that_lists_the_palette() {
        let diags = linted(
            r#"{"id":"s","type":"shape","geo":"rect","at":[0,0],"size":[80,80],"fill":"@nope"}"#,
        );
        let e = diags
            .iter()
            .find(|d| d.severity == Severity::Error)
            .expect("an error");
        assert!(e.message.contains("@nope"));
        assert!(e.message.contains("@accent1"), "must name the real tokens");
    }

    #[test]
    fn a_literal_color_is_legal() {
        let diags = linted(
            r##"{"id":"s","type":"shape","geo":"rect","at":[0,0],"size":[80,80],"fill":"#ff0000"}"##,
        );
        assert!(
            diags.iter().all(|d| d.severity != Severity::Error),
            "{diags:?}"
        );
    }

    #[test]
    fn a_dangling_connector_is_an_error_naming_the_target() {
        let diags = linted(
            r#"{"id":"c","type":"connector","from":{"object":"ghost","side":"left"},
                "to":{"at":[10,10]}}"#,
        );
        assert!(diags
            .iter()
            .any(|d| d.severity == Severity::Error && d.message.contains("ghost")));
    }

    #[test]
    fn off_canvas_warns_with_the_numbers() {
        let diags =
            linted(r#"{"id":"t","type":"text","at":[2000,64],"size":[100,40],"text":["lost"]}"#);
        let w = diags
            .iter()
            .find(|d| d.message.contains("off-canvas"))
            .unwrap();
        assert!(w.message.contains("2000"), "{}", w.message);
        assert!(w.message.contains("960×540"), "{}", w.message);
    }
}
