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

use std::collections::BTreeSet;

use crate::layout::FontStack;
use crate::normalize::{Diagnostic, Severity};
use crate::presets::{tier_of, tier_rank, Preset};
use crate::schema::{Board, ChartObject, Object, Paragraph, Stroke};
use crate::theme::Theme;

/// Run the legality lint over a normalized board.
pub fn lint(board: &Board, theme: &Theme) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let canvas = &board.canvas;

    for page in &board.pages {
        let index = crate::normalize::index_page(page);
        // Every id on the page, framed or not — composite targets may name
        // slot- or anchor-placed objects that `index` (explicit geometry
        // only) cannot see.
        let by_id: std::collections::BTreeMap<&str, &Object> =
            page.walk().map(|o| (o.id(), o)).collect();
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
                Object::Diagram(d) => {
                    for node in &d.nodes {
                        if let Some(fill) = node.fill.as_deref() {
                            let field = format!("nodes[{:?}].fill", node.id);
                            check_color(fill, theme, &page.id, &d.id, &field, &mut diags);
                        }
                    }
                }
                Object::PanelLabel(pl) => {
                    if let Some(target) = pl.anchor.as_ref().and_then(|a| a.object.as_deref()) {
                        if !by_id.contains_key(target) {
                            diags.push(
                                Diagnostic::new(
                                    Severity::Error,
                                    format!(
                                        "panelLabel anchors to {target:?}, which is not on this \
                                         page"
                                    ),
                                )
                                .at(&page.id, &pl.id)
                                .field("anchor.object"),
                            );
                        }
                    }
                }
                Object::Scalebar(sb) => {
                    if !(sb.length_pt.is_finite() && sb.length_pt > 0.0) {
                        diags.push(
                            Diagnostic::new(
                                Severity::Error,
                                format!(
                                    "lengthPt {} is not a positive length in points",
                                    sb.length_pt
                                ),
                            )
                            .at(&page.id, &sb.id)
                            .field("lengthPt"),
                        );
                    }
                    if let Some(color) = sb.stroke.as_ref().and_then(|s| s.color.as_deref()) {
                        check_color(color, theme, &page.id, &sb.id, "stroke.color", &mut diags);
                    }
                }
                Object::SigBracket(sig) => {
                    for (name, ep) in [("from", &sig.from), ("to", &sig.to)] {
                        match ep.object.as_deref() {
                            Some(target) if !by_id.contains_key(target) => diags.push(
                                Diagnostic::new(
                                    Severity::Error,
                                    format!(
                                        "sigBracket {name} binds to {target:?}, which is not on \
                                         this page"
                                    ),
                                )
                                .at(&page.id, &sig.id)
                                .field(name),
                            ),
                            Some(_) => {}
                            None => diags.push(
                                Diagnostic::new(
                                    Severity::Error,
                                    format!("sigBracket {name} has no `object`"),
                                )
                                .at(&page.id, &sig.id)
                                .field(name),
                            ),
                        }
                    }
                }
                Object::Legend(lg) => {
                    // The plan prefers direct labels; a legend this small is
                    // chart chrome its marks could carry themselves.
                    if lg.entries.len() <= 3 {
                        diags.push(
                            Diagnostic::new(
                                Severity::Warning,
                                format!(
                                    "legend has {} entr{}; ≤3 series read better as direct \
                                     labels on the marks",
                                    lg.entries.len(),
                                    if lg.entries.len() == 1 { "y" } else { "ies" }
                                ),
                            )
                            .at(&page.id, &lg.id)
                            .field("entries"),
                        );
                    }
                    for (i, e) in lg.entries.iter().enumerate() {
                        if let Some(color) = e.color.as_deref() {
                            let field = format!("entries[{i}].color");
                            check_color(color, theme, &page.id, &lg.id, &field, &mut diags);
                        }
                    }
                }
                Object::Colorbar(cb) => {
                    if crate::colormap::sample(&cb.colormap, 0.0).is_none() {
                        diags.push(
                            Diagnostic::new(
                                Severity::Error,
                                format!(
                                    "colormap {:?} is not bundled; bundled maps are {}",
                                    cb.colormap,
                                    crate::colormap::NAMES.join(", ")
                                ),
                            )
                            .at(&page.id, &cb.id)
                            .field("colormap"),
                        );
                    }
                    let [lo, hi] = cb.domain;
                    if !(lo.is_finite() && hi.is_finite()) || lo == hi {
                        diags.push(
                            Diagnostic::new(
                                Severity::Error,
                                format!("domain [{lo}, {hi}] is degenerate"),
                            )
                            .at(&page.id, &cb.id)
                            .field("domain"),
                        );
                    }
                }
                Object::Callout(co) => {
                    check_colors_in_paragraphs(&co.text, theme, &page.id, &co.id, &mut diags);
                    if let Some(tail) = &co.tail {
                        match tail.object.as_deref() {
                            Some(target) if !by_id.contains_key(target) => diags.push(
                                Diagnostic::new(
                                    Severity::Error,
                                    format!(
                                        "callout tail points at {target:?}, which is not on \
                                         this page"
                                    ),
                                )
                                .at(&page.id, &co.id)
                                .field("tail.object"),
                            ),
                            Some(_) => {}
                            None => diags.push(
                                Diagnostic::new(
                                    Severity::Error,
                                    "callout tail has no `object`".to_string(),
                                )
                                .at(&page.id, &co.id)
                                .field("tail.object"),
                            ),
                        }
                    }
                }
                Object::Inset(inset) => match by_id.get(inset.of.object.as_str()) {
                    None => diags.push(
                        Diagnostic::new(
                            Severity::Error,
                            format!("inset of.object {:?} is not on this page", inset.of.object),
                        )
                        .at(&page.id, &inset.id)
                        .field("of.object"),
                    ),
                    Some(Object::Image(img)) => {
                        if img.pixel_size.is_none() {
                            diags.push(
                                Diagnostic::new(
                                    Severity::Warning,
                                    format!(
                                        "inset needs the target's pixelSize; {:?} has none, so \
                                         the crop and the source-region mark cannot be computed",
                                        inset.of.object
                                    ),
                                )
                                .at(&page.id, &inset.id)
                                .field("of.px"),
                            );
                        }
                    }
                    Some(other) => diags.push(
                        Diagnostic::new(
                            Severity::Error,
                            format!(
                                "inset of.object {:?} is a {}, not an image",
                                inset.of.object,
                                other.kind()
                            ),
                        )
                        .at(&page.id, &inset.id)
                        .field("of.object"),
                    ),
                },
                Object::Image(_) | Object::Group(_) => {}
            }
        }
    }

    diags
}

/// Run the target lint: the legality profile plus the preset's floors and
/// rules. `lint()` stays the legality baseline; this appends what the *venue*
/// requires — sub-floor type and strokes, under-DPI rasters, the export-floor
/// census, and the preset's content rules. Floors are Errors (§3.5: below the
/// target's minimum refuses, it does not advise); heuristics stay Warnings.
pub fn lint_target(
    board: &Board,
    theme: &Theme,
    preset: &Preset,
    fonts: &FontStack,
) -> Vec<Diagnostic> {
    let mut diags = lint(board, theme);
    let floors = &preset.floors;

    // Role names actually drawn on this board, for the font-resolution check.
    let mut used_roles: BTreeSet<&str> = BTreeSet::new();

    for page in &board.pages {
        // Export-floor census over top-level objects: a group is one fate
        // (its lowest child's), so children are not double-counted.
        for obj in &page.objects {
            let (tier, reason) = tier_of(obj);
            if tier_rank(tier) < tier_rank(floors.export_floor) {
                diags.push(
                    Diagnostic::new(
                        Severity::Error,
                        format!(
                            "exports at {tier:?} ({reason}); {} floors exports at {:?}",
                            preset.id, floors.export_floor
                        ),
                    )
                    .at(&page.id, obj.id()),
                );
            }
        }

        for obj in page.walk() {
            match obj {
                Object::Text(t) => {
                    let role = t.role.as_deref().unwrap_or("body");
                    used_roles.insert(role);
                    check_role_floor(role, theme, preset, &page.id, &t.id, &mut diags);
                }
                Object::Shape(sh) => {
                    if !sh.text.is_empty() {
                        let role = sh.role.as_deref().unwrap_or("body");
                        used_roles.insert(role);
                        check_role_floor(role, theme, preset, &page.id, &sh.id, &mut diags);
                    }
                    check_stroke_floor(
                        sh.stroke.as_ref(),
                        preset,
                        &page.id,
                        &sh.id,
                        "stroke.width",
                        &mut diags,
                    );
                }
                Object::Connector(c) => {
                    if !c.text.is_empty() {
                        // The renderer labels connectors in `label` when no
                        // role is declared.
                        let role = c.role.as_deref().unwrap_or("label");
                        used_roles.insert(role);
                        check_role_floor(role, theme, preset, &page.id, &c.id, &mut diags);
                    }
                    check_stroke_floor(
                        c.stroke.as_ref(),
                        preset,
                        &page.id,
                        &c.id,
                        "stroke.width",
                        &mut diags,
                    );
                }
                Object::Image(img) => {
                    if img.src.to_ascii_lowercase().ends_with(".svg") {
                        continue; // vector: no DPI to check
                    }
                    match (img.pixel_size, img.size) {
                        (Some(px), Some(sz)) if sz[0] > 0.0 && sz[1] > 0.0 => {
                            // Effective DPI at placed size; the worse axis is
                            // the one a reviewer sees.
                            let dpi = (px[0] / (sz[0] / 72.0)).min(px[1] / (sz[1] / 72.0));
                            if dpi + 1e-9 < floors.min_effective_dpi {
                                diags.push(
                                    Diagnostic::new(
                                        Severity::Error,
                                        format!(
                                            "{:.0} effective dpi ({}×{} px placed at {}×{} pt); \
                                             {} floors rasters at {:.0} dpi",
                                            dpi,
                                            px[0],
                                            px[1],
                                            sz[0],
                                            sz[1],
                                            preset.id,
                                            floors.min_effective_dpi
                                        ),
                                    )
                                    .at(&page.id, &img.id)
                                    .field("size"),
                                );
                            }
                        }
                        (None, _) => {
                            diags.push(
                                Diagnostic::new(
                                    Severity::Warning,
                                    format!(
                                        "pixelSize unknown; cannot verify effective dpi against \
                                         the {} dpi floor of {}",
                                        floors.min_effective_dpi, preset.id
                                    ),
                                )
                                .at(&page.id, &img.id)
                                .field("pixelSize"),
                            );
                        }
                        // A raster with known pixels but no placed size is not
                        // yet at any effective DPI.
                        (Some(_), _) => {}
                    }
                }
                Object::Chart(c) => check_chart_rules(c, preset, &page.id, &mut diags),
                // A scalebar's stroke is stored; its bar must clear the
                // venue's line-weight floor like any drawn stroke.
                Object::Scalebar(sb) => check_stroke_floor(
                    sb.stroke.as_ref(),
                    preset,
                    &page.id,
                    &sb.id,
                    "stroke.width",
                    &mut diags,
                ),
                Object::Group(_)
                | Object::Diagram(_)
                | Object::PanelLabel(_)
                | Object::SigBracket(_)
                | Object::Legend(_)
                | Object::Colorbar(_)
                | Object::Callout(_)
                | Object::Inset(_)
                | Object::Unknown(_) => {}
            }
        }
    }

    // Every used role's family stack must resolve to a real face — an export
    // that would silently substitute is refused, not shipped (§8).
    for role_name in used_roles {
        let Some(role) = theme.role(role_name) else {
            continue; // lint() already warned about the unknown role
        };
        if fonts
            .resolve(&role.family, role.weight, role.italic.unwrap_or(false))
            .is_none()
        {
            diags.push(
                Diagnostic::new(
                    Severity::Error,
                    format!(
                        "no face resolves for role {role_name:?} (stack {}); exports would \
                         substitute silently",
                        role.family.join(", ")
                    ),
                )
                .field("type"),
            );
        }
    }

    diags
}

/// The per-role floor at this target: `role.minPt × min_pt_scale`. A role
/// *sized* below its scaled floor is an Error on every object that uses it.
fn check_role_floor(
    role_name: &str,
    theme: &Theme,
    preset: &Preset,
    page: &str,
    id: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let Some(role) = theme.role(role_name) else {
        return; // lint() already warned about the unknown role
    };
    let floor = role.min_pt * preset.floors.min_pt_scale;
    if role.size + 1e-9 < floor {
        diags.push(
            Diagnostic::new(
                Severity::Error,
                format!(
                    "role {role_name:?} is {} pt; {} floors it at {} pt \
                     (minPt {} × scale {})",
                    role.size, preset.id, floor, role.min_pt, preset.floors.min_pt_scale
                ),
            )
            .at(page, id)
            .field("role"),
        );
    }
}

fn check_stroke_floor(
    stroke: Option<&Stroke>,
    preset: &Preset,
    page: &str,
    id: &str,
    field: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let Some(width) = stroke.and_then(|s| s.width) else {
        return; // an unset width takes the theme default, which is on-floor
    };
    let floor = preset.floors.min_line_width_pt;
    if width + 1e-9 < floor {
        diags.push(
            Diagnostic::new(
                Severity::Error,
                format!(
                    "{width} pt stroke; {} floors line weight at {floor} pt",
                    preset.id
                ),
            )
            .at(page, id)
            .field(field),
        );
    }
}

/// The preset's chart rules: axis units, and the refused-feature check.
fn check_chart_rules(c: &ChartObject, preset: &Preset, page: &str, diags: &mut Vec<Diagnostic>) {
    if preset.rules.require_axis_units {
        for (axis, channel) in [("x", c.x.as_ref()), ("y", c.y.as_ref())] {
            let Some(title) = channel.and_then(|ch| ch.title.as_deref()) else {
                continue; // a titleless axis is a different (style) concern
            };
            if !has_unit_parenthetical(title) {
                // Deliberately a Warning, never an Error: "(fold change)" vs
                // "(a.u.)" is judgement a paren-matcher cannot hold.
                diags.push(
                    Diagnostic::new(
                        Severity::Warning,
                        format!(
                            "axis title {title:?} carries no unit parenthetical; {} wants a \
                             unit — \"{title} (s)\" or \"{title} (a.u.)\"",
                            preset.id
                        ),
                    )
                    .at(page, &c.id)
                    .field(&format!("{axis}.title")),
                );
            }
        }
    }

    // Refusals are advisory strings the skill reads before authoring. None of
    // the bundled refusals ("pie", "second-y", "histogram") is expressible in
    // the mark vocabulary — there is no pie mark, no second y channel, and no
    // binning transform — so structurally a compliant board cannot violate
    // them. The one hole is `extra`: a future or foreign writer smuggling a
    // refused feature as an unknown key is caught here; anything else emits
    // nothing, by design.
    for refused in &preset.rules.refuses {
        let smuggled = c.extra.contains_key(refused.as_str())
            || c.marks
                .iter()
                .any(|m| m.extra.contains_key(refused.as_str()));
        if smuggled {
            diags.push(
                Diagnostic::new(
                    Severity::Error,
                    format!("carries {refused:?}, which {} refuses", preset.id),
                )
                .at(page, &c.id),
            );
        }
    }
}

/// Unit-ish: a `(` with a matching `)` after it, non-empty between.
fn has_unit_parenthetical(title: &str) -> bool {
    match (title.find('('), title.rfind(')')) {
        (Some(open), Some(close)) => close > open + 1,
        _ => false,
    }
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

    // --- lint --target -----------------------------------------------------

    fn target_linted(objects: &str, theme_id: &str, target: &str) -> Vec<Diagnostic> {
        let mut b = crate::parse(&format!(
            r#"{{"format":"chimaera.board","formatVersion":1,
                "canvas":{{"size":[960,540]}},
                "pages":[{{"id":"p1","objects":[{objects}]}}]}}"#
        ))
        .unwrap();
        crate::normalize(&mut b);
        let theme = crate::theme::bundled(theme_id).unwrap();
        let preset = crate::presets::get(target).unwrap();
        lint_target(&b, &theme, preset, &FontStack::new(&[]))
    }

    #[test]
    fn a_sub_floor_stroke_errors_with_the_numbers() {
        // 0.2 pt under pub-nature-single's 0.5 pt line-weight floor.
        let diags = target_linted(
            r#"{"id":"s","type":"shape","geo":"rect","at":[0,0],"size":[80,80],
                "stroke":{"color":"@edge","width":0.2}}"#,
            "figure-light",
            "pub-nature-single",
        );
        let e = diags
            .iter()
            .find(|d| d.severity == Severity::Error && d.field.as_deref() == Some("stroke.width"))
            .expect("a stroke-floor error");
        assert!(e.message.contains("0.2"), "{}", e.message);
        assert!(e.message.contains("0.5"), "{}", e.message);
        assert_eq!(e.object.as_deref(), Some("s"));
    }

    #[test]
    fn an_on_floor_stroke_is_clean_under_a_permissive_target() {
        let diags = target_linted(
            r#"{"id":"s","type":"shape","geo":"rect","at":[0,0],"size":[80,80],
                "stroke":{"color":"@edge","width":2}}"#,
            "talk-light",
            "talk-16x9",
        );
        assert!(
            diags.iter().all(|d| d.severity != Severity::Error),
            "{diags:?}"
        );
    }

    #[test]
    fn an_under_dpi_raster_errors_with_the_numbers() {
        // 100 px placed 400 pt wide is 18 dpi against a 300 dpi floor.
        let diags = target_linted(
            r#"{"id":"i","type":"image","src":"assets/blot.png","at":[0,0],
                "size":[400,300],"pixelSize":[100,75]}"#,
            "figure-light",
            "pub-nature-single",
        );
        let e = diags
            .iter()
            .find(|d| d.severity == Severity::Error && d.message.contains("dpi"))
            .expect("a dpi error");
        assert!(e.message.contains("18"), "{}", e.message);
        assert!(e.message.contains("300"), "{}", e.message);
        assert_eq!(e.object.as_deref(), Some("i"));
    }

    #[test]
    fn an_unknown_pixel_size_warns_cannot_verify() {
        let diags = target_linted(
            r#"{"id":"i","type":"image","src":"assets/blot.png","at":[0,0],"size":[80,80]}"#,
            "figure-light",
            "pub-nature-single",
        );
        let w = diags
            .iter()
            .find(|d| d.severity == Severity::Warning && d.message.contains("cannot verify"))
            .expect("a cannot-verify warning");
        assert_eq!(w.field.as_deref(), Some("pixelSize"));
    }

    #[test]
    fn a_raster_under_a_vector_export_floor_is_a_census_error() {
        let diags = target_linted(
            r#"{"id":"i","type":"image","src":"assets/shot.png","at":[0,0],
                "size":[80,80],"pixelSize":[1000,1000]}"#,
            "figure-light",
            "pub-nature-single",
        );
        let e = diags
            .iter()
            .find(|d| d.severity == Severity::Error && d.message.contains("floors exports"))
            .expect("an export-floor error");
        assert!(e.message.contains("Raster"), "{}", e.message);
        assert!(
            e.message.contains("raster pixels at placed size"),
            "names the reason: {}",
            e.message
        );
        assert!(e.message.contains("Vector"), "{}", e.message);
    }

    #[test]
    fn a_sub_floor_role_errors_under_a_scaled_target() {
        // figure-light's label is 7 pt with minPt 5; pub-plos scales floors
        // by 1.6, so the floor is 8 pt and 7 pt type refuses.
        let diags = target_linted(
            r#"{"id":"t","type":"text","role":"label","at":[0,0],"size":[80,16],
                "text":["n = 12"]}"#,
            "figure-light",
            "pub-plos",
        );
        let e = diags
            .iter()
            .find(|d| d.severity == Severity::Error && d.field.as_deref() == Some("role"))
            .expect("a role-floor error");
        assert!(e.message.contains('7'), "{}", e.message);
        assert!(e.message.contains('8'), "{}", e.message);
        // The same role clears pub-nature-single's unscaled floors.
        let diags = target_linted(
            r#"{"id":"t","type":"text","role":"label","at":[0,0],"size":[80,16],
                "text":["n = 12"]}"#,
            "figure-light",
            "pub-nature-single",
        );
        assert!(
            diags
                .iter()
                .all(|d| d.field.as_deref() != Some("role") || d.severity != Severity::Error),
            "{diags:?}"
        );
    }

    #[test]
    fn a_unitless_axis_title_warns_only_on_publication_targets() {
        let chart = r#"{"id":"c","type":"chart","at":[0,0],"size":[200,160],
            "data":{"origin":"stated-by-user","values":[{"t":1,"v":2}]},
            "x":{"field":"t","type":"quantitative","title":"Time"},
            "y":{"field":"v","type":"quantitative","title":"Signal (a.u.)"}}"#;
        let diags = target_linted(chart, "figure-light", "pub-nature-single");
        let w = diags
            .iter()
            .find(|d| d.message.contains("unit parenthetical"))
            .expect("a unit warning for the x axis");
        assert_eq!(w.severity, Severity::Warning, "heuristic, never an Error");
        assert!(w.message.contains("Time"), "{}", w.message);
        assert_eq!(w.field.as_deref(), Some("x.title"));
        assert!(
            !diags.iter().any(|d| d.field.as_deref() == Some("y.title")),
            "\"(a.u.)\" satisfies the rule"
        );
        // The same chart is silent on a talk target.
        let diags = target_linted(chart, "talk-light", "talk-16x9");
        assert!(
            diags
                .iter()
                .all(|d| !d.message.contains("unit parenthetical")),
            "{diags:?}"
        );
    }

    #[test]
    fn a_smuggled_refused_feature_is_an_error() {
        // No refused feature is expressible in the mark vocabulary; the one
        // hole is `extra`, and the venue refusal catches it there.
        let diags = target_linted(
            r#"{"id":"c","type":"chart","at":[0,0],"size":[200,160],
                "data":{"origin":"stated-by-user","values":[{"t":1,"v":2}]},
                "pie":true}"#,
            "figure-light",
            "pub-nature-single",
        );
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Error && d.message.contains("\"pie\"")),
            "{diags:?}"
        );
    }
}
