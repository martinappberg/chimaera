//! `lint` — the checks that refuse, and the ones that only report.
//!
//! The set is deliberately narrow: false positives are a real cost, and the
//! plan refuses whole categories (general overlap, data-ink ratio, whitespace
//! balance, "wrong hierarchy") because they are judgement, not measurement.
//! Every finding names object, field, measured value and expected value.
//!
//! Three profiles live here: the legality lint ([`lint`]: duplicate ids,
//! inline data caps, off-canvas, unresolved tokens/endpoints, unknown
//! objects), the target lint ([`lint_target`]: the venue's floors, tiers and
//! rules), and the style lint ([`lint_style`]: the measured near-miss set).
//! [`lint_fix`] repairs the mechanically-unambiguous classes in place.

use std::collections::{BTreeMap, BTreeSet};

use crate::layout::FontStack;
use crate::normalize::{Diagnostic, Severity, GRID_PT, MIN_EXTENT_PT};
use crate::presets::{tier_of, tier_rank, Preset};
use crate::schema::{Board, ChartObject, Frame, Object, Page, Paragraph, Stroke};
use crate::theme::{Theme, TypeRole};

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
                Object::Table(t) => {
                    for row in &t.rows {
                        check_colors_in_paragraphs(row, theme, &page.id, &t.id, &mut diags);
                    }
                    if let Some(cols) = &t.columns {
                        if !t.rows.is_empty() && cols.len() != t.column_count() {
                            diags.push(
                                Diagnostic::new(
                                    Severity::Warning,
                                    format!(
                                        "columns states {} widths for a {}-column grid; the \
                                         equal split is used",
                                        cols.len(),
                                        t.column_count()
                                    ),
                                )
                                .at(&page.id, &t.id)
                                .field("columns"),
                            );
                        }
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
                        // A node icon must name a real glyph — the leading icon
                        // renders a placeholder otherwise, exactly like a bare
                        // `icon` object, so lint names it as an Error too.
                        if let Some(name) = node.icon.as_deref() {
                            if crate::icons::enabled() && crate::icons::lookup(name).is_none() {
                                diags.push(
                                    Diagnostic::new(
                                        Severity::Error,
                                        format!(
                                            "node {:?} names unknown icon {name:?}; run \
                                             `chimaera board icons {name}` to find one",
                                            node.id
                                        ),
                                    )
                                    .at(&page.id, &d.id)
                                    .field(&format!("nodes[{:?}].icon", node.id)),
                                );
                            }
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
                // The one named C6 exception: an equation is notation, not
                // prose, so it never counts as verified text — but the
                // carve-out requires the LaTeX to travel as alt, and the TeX
                // itself must compile or the render is a placeholder.
                Object::Equation(eq) => {
                    if eq.alt.trim().is_empty() {
                        diags.push(
                            Diagnostic::new(
                                Severity::Error,
                                "equation alt is empty; the C6 picture exception requires alt \
                                 carrying the LaTeX source",
                            )
                            .at(&page.id, &eq.id)
                            .field("alt"),
                        );
                    }
                    if eq.tex.trim().is_empty() {
                        diags.push(
                            Diagnostic::new(Severity::Error, "equation tex is empty")
                                .at(&page.id, &eq.id)
                                .field("tex"),
                        );
                    } else if let Err(e) = crate::equation::render_tex_svg(
                        &eq.tex,
                        eq.em_size.unwrap_or_else(|| theme.body().size),
                    ) {
                        // A build without the math feature cannot verify the
                        // TeX; that is a warning, not a claim the TeX is bad.
                        let (sev, msg) = if e == crate::equation::MISSING_FEATURE {
                            (Severity::Warning, format!("cannot verify tex: {e}"))
                        } else {
                            (
                                Severity::Error,
                                format!("tex does not compile ({e}); it renders as a placeholder"),
                            )
                        };
                        diags.push(Diagnostic::new(sev, msg).at(&page.id, &eq.id).field("tex"));
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
                // A bundled icon must name a real glyph, or it renders a
                // placeholder — an Error so an export is never quietly broken.
                // A build without the feature cannot check the name; the
                // renderer's own placeholder-with-reason covers that case.
                Object::Icon(ic) => {
                    if ic.name.trim().is_empty() {
                        diags.push(
                            Diagnostic::new(Severity::Error, "icon name is empty")
                                .at(&page.id, &ic.id)
                                .field("name"),
                        );
                    } else if crate::icons::enabled() && crate::icons::lookup(&ic.name).is_none() {
                        diags.push(
                            Diagnostic::new(
                                Severity::Error,
                                format!(
                                    "unknown icon {:?}; run `chimaera board icons {}` to find a \
                                     name (it renders as a placeholder)",
                                    ic.name, ic.name
                                ),
                            )
                            .at(&page.id, &ic.id)
                            .field("name"),
                        );
                    }
                }
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
    // The theme ramp's CVD-safe series cap, computed lazily: the Machado
    // simulation runs only when a chart actually encodes multiple series.
    let mut series_cap: Option<usize> = None;

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
                Object::Chart(c) => {
                    check_chart_rules(c, preset, &page.id, &mut diags);
                    check_series_cap(c, theme, &page.id, &mut series_cap, &mut diags);
                }
                // Table cells are text at the cell role's size, so they count
                // toward the venue's per-role floor exactly as bound shape
                // text does.
                Object::Table(t) => {
                    if t.rows.iter().any(|r| !r.is_empty()) {
                        let role = t.role.as_deref().unwrap_or("body");
                        used_roles.insert(role);
                        check_role_floor(role, theme, preset, &page.id, &t.id, &mut diags);
                    }
                }
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
                // An equation is the C6 exception: notation lands as a
                // picture, so it contributes no verified text and no role
                // floor — its TeX is checked by the legality lint instead.
                Object::Group(_)
                | Object::Diagram(_)
                | Object::Equation(_)
                | Object::Icon(_)
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

/// The plan's *computed series cap* (§9): a chart may not encode more series
/// than the theme ramp can keep apart under CVD simulation. The cap is the
/// largest ramp prefix with no pair under ΔE 8 (Machado 2009, all three
/// dichromacies) — [`crate::cvd::safe_series_cap`] — computed at most once
/// per lint run and only when some chart actually encodes ≥2 series.
fn check_series_cap(
    c: &ChartObject,
    theme: &Theme,
    page: &str,
    cap: &mut Option<usize>,
    diags: &mut Vec<Diagnostic>,
) {
    let series = distinct_series(c);
    if series < 2 {
        return;
    }
    let cap = *cap.get_or_insert_with(|| {
        let ramp: Vec<crate::theme::Rgb> = theme
            .chart
            .categorical
            .iter()
            .filter_map(|r| theme.color(r))
            .collect();
        crate::cvd::safe_series_cap(&ramp)
    });
    if series > cap {
        diags.push(
            Diagnostic::new(
                Severity::Error,
                format!(
                    "{series} series exceed the theme ramp's CVD-safe cap of {cap} \
                     (all-pairs ΔE ≥ 8 under Machado 2009); split the chart or drop series"
                ),
            )
            .at(page, &c.id)
            .field("color"),
        );
    }
}

/// Distinct values of the color channel's field over the inline rows — how
/// many ramp colors the chart will actually consume. No color channel (or no
/// inline rows to read) counts as one series.
fn distinct_series(c: &ChartObject) -> usize {
    let Some(color) = c.color.as_ref() else {
        return 1;
    };
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for row in &c.data.values {
        if let Some(v) = row.get(color.field.as_str()) {
            seen.insert(match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            });
        }
    }
    seen.len().max(1)
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

// ---------------------------------------------------------------------------
// --style: the measured near-miss profile
// ---------------------------------------------------------------------------

/// The style lint: measured geometry findings over **resolved** frames
/// ([`crate::slots::resolve_page_frames`] — the same geometry truth render
/// and describe use, so a slot-placed object is judged where it actually
/// lands).
///
/// Severities follow §3.5. Errors under `strict`, warnings by default:
/// near-miss alignment (matching edges 0 < Δ < 3 pt — the highest-value
/// check; 1.5 pt off is always a mistake, never a choice) · near-miss
/// spacing (3+ objects in a row/column with gaps differing by 0 < Δ < 3) ·
/// off-grid geometry (explicit `at`/`size` not on the 8 pt grid; slot
/// geometry is on-grid by construction and never checked) · overfull box ·
/// margin violation. Always warnings: underfull box (<40% of frame height
/// used) · distinct-value counts per page (>2 resolved families, >1
/// non-neutral accent among literal colors) · override budget (>4 run-level
/// size/family/color overrides per page; objects with role `"code"` exempt) ·
/// title widow · a free `at` where the page's layout still has unclaimed
/// slots.
///
/// **Refused at any severity, deliberately unimplemented** (§3.5): general
/// object overlap (callouts over panels are the entire point of the
/// annotation layer) · panel-extent consistency (a wide time series beside a
/// square heatmap is correct; plot-area *edges* matter, panel *extents*
/// don't) · data-ink ratio (empirically contested — Tufte's direction lives
/// in theme defaults, never in a rule) · whitespace balance · "wrong
/// hierarchy" (the last two are judgement, and judgement is the loop's 5%).
pub fn lint_style(
    board: &Board,
    theme: &Theme,
    fonts: &FontStack,
    strict: bool,
) -> Vec<Diagnostic> {
    let base = if strict {
        Severity::Error
    } else {
        Severity::Warning
    };
    let mut diags = Vec::new();
    // The layout grid's 8 pt-quantized snap targets, computed once — advisory
    // geometry that only bites when the author declared a `canvas.grid`.
    let grid_lines = crate::schema::grid_lines(&board.canvas);

    for page in &board.pages {
        let resolved = crate::slots::resolve_page_frames(board, page, theme, Some(fonts));
        // Z-order, so pair findings attach to the later object and name the
        // earlier one — the same "second snaps to first" contract lint_fix
        // repairs by.
        let framed: Vec<(&str, Frame)> = page
            .walk()
            .filter_map(|o| resolved.get(o.id()).map(|f| (o.id(), *f)))
            .collect();

        near_miss_alignment(&framed, &page.id, base, &mut diags);
        near_miss_spacing(&framed, &page.id, base, &mut diags);
        off_grid(page, base, &mut diags);
        if let Some((xs, ys)) = &grid_lines {
            near_miss_grid(page, xs, ys, base, &mut diags);
        }
        text_boxes(page, &resolved, theme, fonts, base, &mut diags);
        margin_violations(board, page, &resolved, theme, base, &mut diags);
        page_budgets(page, theme, fonts, &mut diags);
        title_widows(page, &resolved, theme, fonts, &mut diags);
        free_at(board, page, theme, &mut diags);
        untraceable_data(page, &mut diags);
    }

    diags
}

/// A chart whose inline values were produced by the agent (`command` /
/// `derived-by-agent`) with nothing that says HOW — no `source` binding and
/// no `trace`. A gentle nudge, never an error (even under --strict): the
/// chart is legal, but a later session cannot answer "how was this
/// calculated" from the file alone.
fn untraceable_data(page: &Page, diags: &mut Vec<Diagnostic>) {
    use crate::schema::DataOrigin;
    for obj in page.walk() {
        let Object::Chart(c) = obj else { continue };
        let produced = matches!(
            c.data.origin,
            DataOrigin::Command | DataOrigin::DerivedByAgent
        );
        if produced
            && !c.data.values.is_empty()
            && c.data.source.is_none()
            && c.data.trace.is_none()
        {
            diags.push(
                Diagnostic::new(
                    Severity::Warning,
                    "untraceable data: state how these values were produced (`trace`) or bind \
                     the source file (`source`)",
                )
                .at(&page.id, obj.id())
                .field("data"),
            );
        }
    }
}

const ALIGN_TOLERANCE_PT: f64 = 3.0;
const EPS: f64 = 1e-6;

/// A frame measurement (an edge coordinate), named so the edge tables stay
/// readable.
type EdgeOf = fn(&Frame) -> f64;

/// Render a point value without float noise: `81.5`, not `81.500000000001`.
fn pt(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn near_miss_alignment(
    framed: &[(&str, Frame)],
    page: &str,
    base: Severity,
    diags: &mut Vec<Diagnostic>,
) {
    let edges: [(&str, EdgeOf); 4] = [
        ("left", |f| f.x),
        ("right", Frame::right),
        ("top", |f| f.y),
        ("bottom", Frame::bottom),
    ];
    for i in 0..framed.len() {
        for j in (i + 1)..framed.len() {
            let ((a, fa), (b, fb)) = (framed[i], framed[j]);
            for (edge, of) in edges {
                let (va, vb) = (of(&fa), of(&fb));
                let d = (va - vb).abs();
                if d > EPS && d < ALIGN_TOLERANCE_PT {
                    diags.push(
                        Diagnostic::new(
                            base,
                            format!(
                                "near-miss alignment: {edge} edge at {} vs {a:?} at {} \
                                 (Δ {} pt; aligned is 0 or ≥ {ALIGN_TOLERANCE_PT})",
                                pt(vb),
                                pt(va),
                                pt(d)
                            ),
                        )
                        .at(page, b)
                        .field("at"),
                    );
                }
            }
        }
    }
}

/// Rows are objects whose top edges match within 0.5 pt, columns objects
/// whose left edges do — the bands a reader's eye actually groups. Within a
/// band of 3+, consecutive gaps that differ by 0 < Δ < 3 are the
/// machine-placement tell (20/22/20).
fn near_miss_spacing(
    framed: &[(&str, Frame)],
    page: &str,
    base: Severity,
    diags: &mut Vec<Diagnostic>,
) {
    let bands = |key: fn(&Frame) -> f64, order: fn(&Frame) -> f64| -> Vec<Vec<(&str, Frame)>> {
        let mut sorted: Vec<(&str, Frame)> = framed.to_vec();
        sorted.sort_by(|a, b| {
            (key(&a.1), order(&a.1))
                .partial_cmp(&(key(&b.1), order(&b.1)))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut out: Vec<Vec<(&str, Frame)>> = Vec::new();
        for item in sorted {
            match out.last_mut() {
                Some(band) if (key(&band.last().unwrap().1) - key(&item.1)).abs() <= 0.5 => {
                    band.push(item)
                }
                _ => out.push(vec![item]),
            }
        }
        out
    };

    let mut check = |axis: &str,
                     bands: Vec<Vec<(&str, Frame)>>,
                     start: fn(&Frame) -> f64,
                     end: fn(&Frame) -> f64| {
        for band in bands.iter().filter(|b| b.len() >= 3) {
            for w in band.windows(3) {
                let [(a, fa), (b, fb), (c, fc)] = [w[0], w[1], w[2]];
                let (g1, g2) = (start(&fb) - end(&fa), start(&fc) - end(&fb));
                if g1 <= 0.0 || g2 <= 0.0 {
                    continue; // overlapping or touching — not a spacing run
                }
                let d = (g1 - g2).abs();
                if d > EPS && d < ALIGN_TOLERANCE_PT {
                    diags.push(
                        Diagnostic::new(
                            base,
                            format!(
                                "near-miss spacing: gaps {} and {} pt across {a:?} · {b:?} · \
                                 {c:?} in a {axis} (Δ {} pt)",
                                pt(g1),
                                pt(g2),
                                pt(d)
                            ),
                        )
                        .at(page, b)
                        .field("at"),
                    );
                }
            }
        }
    };

    check("row", bands(|f| f.y, |f| f.x), |f| f.x, Frame::right);
    check("column", bands(|f| f.x, |f| f.y), |f| f.y, Frame::bottom);
}

/// Only explicitly-placed geometry: a slot frame is on-grid by construction,
/// and an anchored offset is a binding, not a stated coordinate.
fn off_grid(page: &Page, base: Severity, diags: &mut Vec<Diagnostic>) {
    let on = |v: f64| ((v / GRID_PT).round() * GRID_PT - v).abs() <= EPS;
    for obj in page.walk() {
        let Some(f) = obj.frame() else { continue };
        if !(on(f.x) && on(f.y)) {
            diags.push(
                Diagnostic::new(
                    base,
                    format!(
                        "off-grid: at [{}, {}] is not on the {GRID_PT} pt grid (nearest [{}, {}])",
                        pt(f.x),
                        pt(f.y),
                        snap8(f.x),
                        snap8(f.y)
                    ),
                )
                .at(&page.id, obj.id())
                .field("at"),
            );
        }
        if !(on(f.w) && on(f.h)) {
            diags.push(
                Diagnostic::new(
                    base,
                    format!(
                        "off-grid: size [{}, {}] is not on the {GRID_PT} pt grid \
                         (nearest [{}, {}])",
                        pt(f.w),
                        pt(f.h),
                        snap8(f.w).max(MIN_EXTENT_PT),
                        snap8(f.h).max(MIN_EXTENT_PT)
                    ),
                )
                .at(&page.id, obj.id())
                .field("size"),
            );
        }
    }
}

/// The layout-grid profile (only when the board declares a `canvas.grid`): an
/// object's top-left `at` almost on a column/row line — the same 1.5-pt-is-a-
/// mistake logic as peer near-miss — is a base-severity finding `lint_fix`
/// snaps; an object entirely off the grid gets a gentle **Info** nudge, never
/// a warning, because floating an accent off the grid can be a deliberate
/// choice. Group envelopes are skipped: their geometry is their children's
/// union, so the actionable subject is a child, not the derived box.
fn near_miss_grid(
    page: &Page,
    xs: &[f64],
    ys: &[f64],
    base: Severity,
    diags: &mut Vec<Diagnostic>,
) {
    let nearest = |lines: &[f64], v: f64| -> Option<f64> {
        lines.iter().copied().min_by(|a, b| {
            (a - v)
                .abs()
                .partial_cmp(&(b - v).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    };
    for obj in page.walk() {
        if matches!(obj, Object::Group(_)) {
            continue;
        }
        let Some(f) = obj.frame() else { continue };
        let mut present = 0u8;
        let mut aligned_or_near = false;
        for (unit, edge, v, lines) in [("column", "left", f.x, xs), ("row", "top", f.y, ys)] {
            if lines.is_empty() {
                continue;
            }
            present += 1;
            let Some(line) = nearest(lines, v) else {
                continue;
            };
            let d = (v - line).abs();
            if d <= EPS {
                aligned_or_near = true;
            } else if d < ALIGN_TOLERANCE_PT {
                aligned_or_near = true;
                diags.push(
                    Diagnostic::new(
                        base,
                        format!(
                            "near-miss grid alignment: {edge} edge at {} is {} pt off the layout \
                             grid {unit} at {} (aligned is 0 or ≥ {ALIGN_TOLERANCE_PT})",
                            pt(v),
                            pt(d),
                            pt(line)
                        ),
                    )
                    .at(&page.id, obj.id())
                    .field("at"),
                );
            }
        }
        if present > 0 && !aligned_or_near {
            let nx = nearest(xs, f.x).unwrap_or(f.x);
            let ny = if ys.is_empty() {
                f.y
            } else {
                nearest(ys, f.y).unwrap_or(f.y)
            };
            diags.push(
                Diagnostic::new(
                    Severity::Info,
                    format!(
                        "off the layout grid: at [{}, {}] sits on no cell of the {}-column grid \
                         (nearest corner [{}, {}])",
                        pt(f.x),
                        pt(f.y),
                        xs.len(),
                        pt(nx),
                        pt(ny)
                    ),
                )
                .at(&page.id, obj.id())
                .field("at"),
            );
        }
    }
}

/// Overfull (base severity) and underfull (always a warning) text boxes,
/// measured against the resolved frame.
fn text_boxes(
    page: &Page,
    resolved: &BTreeMap<String, Frame>,
    theme: &Theme,
    fonts: &FontStack,
    base: Severity,
    diags: &mut Vec<Diagnostic>,
) {
    for obj in page.walk() {
        let (id, role_name, paras, underfull_applies) = match obj {
            Object::Text(t) => (
                t.id.as_str(),
                t.role.as_deref().unwrap_or("body"),
                &t.text,
                true,
            ),
            // A shape's text is usually a short label inside a panel; a
            // mostly-empty panel is normal, so only overfull applies.
            Object::Shape(sh) if !sh.text.is_empty() => (
                sh.id.as_str(),
                sh.role.as_deref().unwrap_or("body"),
                &sh.text,
                false,
            ),
            // A table's budget is per cell, not per box; underfull cells are
            // normal (a short number in a wide column), so only overfull
            // applies.
            Object::Table(t) => {
                table_cell_overfull(t, page, resolved, theme, fonts, base, diags);
                continue;
            }
            _ => continue,
        };
        let Some(frame) = resolved.get(id) else {
            continue;
        };
        if paras.is_empty() {
            continue;
        }
        let role = theme.role(role_name).unwrap_or_else(|| theme.body());
        let block = text_block_height(paras, role, fonts, frame.w);
        if block > frame.h + 0.5 {
            diags.push(
                Diagnostic::new(
                    base,
                    format!(
                        "overfull box: text measures {:.0} pt against a {:.0} pt frame",
                        block, frame.h
                    ),
                )
                .at(&page.id, id)
                .field("size"),
            );
        } else if underfull_applies && block > 0.0 && block < 0.4 * frame.h {
            diags.push(
                Diagnostic::new(
                    Severity::Warning,
                    format!(
                        "underfull box: text fills {:.0} pt of a {:.0} pt frame (under 40%)",
                        block, frame.h
                    ),
                )
                .at(&page.id, id)
                .field("size"),
            );
        }
    }
}

/// Per-cell overfull, under the renderer's own geometry rule: columns take
/// [`crate::schema::TableObject::column_widths`], rows split the frame height
/// equally, and every cell loses the fixed padding on each side. A cell whose
/// measured text exceeds its budget is reported with the cell named — the
/// grid never resizes to fit.
fn table_cell_overfull(
    t: &crate::schema::TableObject,
    page: &Page,
    resolved: &BTreeMap<String, Frame>,
    theme: &Theme,
    fonts: &FontStack,
    base: Severity,
    diags: &mut Vec<Diagnostic>,
) {
    let Some(frame) = resolved.get(t.id.as_str()) else {
        return;
    };
    let cols = t.column_count();
    if t.rows.is_empty() || cols == 0 {
        return;
    }
    let widths = t.column_widths(frame.w);
    let row_h = frame.h / t.rows.len() as f64;
    let role = theme
        .role(t.role.as_deref().unwrap_or("body"))
        .unwrap_or_else(|| theme.body());
    let pad = crate::render::TABLE_CELL_PAD_PT;
    for (ri, row) in t.rows.iter().enumerate() {
        for (ci, cell) in row.iter().enumerate() {
            let avail_w = (widths[ci] - pad * 2.0).max(1.0);
            let avail_h = row_h - pad * 2.0;
            let block = text_block_height(std::slice::from_ref(cell), role, fonts, avail_w);
            if block > avail_h + 0.5 {
                diags.push(
                    Diagnostic::new(
                        base,
                        format!(
                            "overfull cell [{ri}][{ci}]: text measures {block:.0} pt against \
                             a {avail_h:.0} pt cell",
                        ),
                    )
                    .at(&page.id, &t.id)
                    .field("rows"),
                );
            }
        }
    }
}

/// The conservative height estimate `--style` shares with the renderer:
/// plain paragraphs greedy-wrap through the same [`FontStack`] the renderer
/// shapes with, each line at `size × lineHeight`; a rich paragraph is one
/// line at its largest run size — exactly `emit_text_block`'s layout today,
/// with paragraph spacing ignored. Measured height therefore equals what the
/// renderer draws, and errs low if rich-run wrapping lands later — an
/// overfull finding is never a false positive.
fn text_block_height(paras: &[Paragraph], role: &TypeRole, fonts: &FontStack, width: f64) -> f64 {
    let size = role.size.max(role.min_pt);
    let mut h = 0.0;
    for p in paras {
        match p {
            Paragraph::Plain(text) => {
                let lines = fonts.wrap(text, &role.family, size, role.weight, width);
                h += lines.len() as f64 * size * role.line_height;
            }
            Paragraph::Rich(rich) => {
                let max = rich.runs.iter().filter_map(|r| r.size).fold(size, f64::max);
                h += max * role.line_height;
            }
        }
    }
    h
}

fn margin_violations(
    board: &Board,
    page: &Page,
    resolved: &BTreeMap<String, Frame>,
    theme: &Theme,
    base: Severity,
    diags: &mut Vec<Diagnostic>,
) {
    let [mt, mr, mb, ml] = theme.spacing.margin;
    let (cw, ch) = (board.canvas.width(), board.canvas.height());
    for obj in page.walk() {
        // Slot geometry respects margins by construction (full-bleed ignores
        // them on purpose), and a fully off-canvas object is parked — the
        // legality lint already reports it.
        if obj.slot().is_some() && obj.frame().is_none() {
            continue;
        }
        let Some(f) = resolved.get(obj.id()) else {
            continue;
        };
        if f.right() < 0.0 || f.bottom() < 0.0 || f.x > cw || f.y > ch {
            continue;
        }
        let mut crossed = Vec::new();
        if f.x < ml - EPS {
            crossed.push(format!("left edge {} vs margin {}", pt(f.x), pt(ml)));
        }
        if f.y < mt - EPS {
            crossed.push(format!("top edge {} vs margin {}", pt(f.y), pt(mt)));
        }
        if f.right() > cw - mr + EPS {
            crossed.push(format!(
                "right edge {} vs margin at {}",
                pt(f.right()),
                pt(cw - mr)
            ));
        }
        if f.bottom() > ch - mb + EPS {
            crossed.push(format!(
                "bottom edge {} vs margin at {}",
                pt(f.bottom()),
                pt(ch - mb)
            ));
        }
        if !crossed.is_empty() {
            diags.push(
                Diagnostic::new(base, format!("margin violation: {}", crossed.join("; ")))
                    .at(&page.id, obj.id())
                    .field("at"),
            );
        }
    }
}

/// The per-page distinct-value censuses and the override budget — always
/// warnings: these are budgets, not measurements of a single mistake.
fn page_budgets(page: &Page, theme: &Theme, fonts: &FontStack, diags: &mut Vec<Diagnostic>) {
    let resolve_family = |families: &[String], weight: u16| -> String {
        fonts
            .resolve(families, weight, false)
            .map(|r| r.family)
            .or_else(|| families.first().cloned())
            .unwrap_or_else(|| "sans-serif".to_string())
    };

    let mut families: BTreeSet<String> = BTreeSet::new();
    let mut accents: BTreeSet<String> = BTreeSet::new();
    let mut overrides = 0usize;

    let mut literal = |color: Option<&str>| {
        let Some(c) = color else { return };
        let Some(rgb) = crate::theme::parse_hex(c) else {
            return; // @tokens are the theme's business, not an accent smell
        };
        let (max, min) = (rgb.r.max(rgb.g).max(rgb.b), rgb.r.min(rgb.g).min(rgb.b));
        if max - min > 16 {
            accents.insert(rgb.hex());
        }
    };

    for obj in page.walk() {
        let (paras, role_name): (&[Paragraph], &str) = match obj {
            Object::Text(t) => (&t.text, t.role.as_deref().unwrap_or("body")),
            Object::Shape(sh) => {
                literal(sh.fill.as_deref());
                literal(sh.stroke.as_ref().and_then(|s| s.color.as_deref()));
                if sh.text.is_empty() {
                    continue;
                }
                (&sh.text, sh.role.as_deref().unwrap_or("body"))
            }
            Object::Connector(c) => {
                literal(c.stroke.as_ref().and_then(|s| s.color.as_deref()));
                if c.text.is_empty() {
                    continue;
                }
                (&c.text, c.role.as_deref().unwrap_or("label"))
            }
            _ => continue,
        };
        let role = theme.role(role_name).unwrap_or_else(|| theme.body());
        families.insert(resolve_family(&role.family, role.weight));
        let exempt = role_name == "code";
        for p in paras {
            let Paragraph::Rich(rich) = p else { continue };
            for r in &rich.runs {
                literal(r.color.as_deref());
                if let Some(f) = &r.family {
                    families.insert(resolve_family(std::slice::from_ref(f), role.weight));
                }
                if !exempt {
                    overrides += usize::from(r.size.is_some())
                        + usize::from(r.family.is_some())
                        + usize::from(r.color.is_some());
                }
            }
        }
    }

    let mut page_diag = |message: String, field: &str| {
        let mut d = Diagnostic::new(Severity::Warning, message).field(field);
        d.page = Some(page.id.clone());
        diags.push(d);
    };

    if families.len() > 2 {
        page_diag(
            format!(
                "page resolves {} font families ({}); the budget is 2",
                families.len(),
                families.iter().cloned().collect::<Vec<_>>().join(", ")
            ),
            "text",
        );
    }
    if accents.len() > 1 {
        page_diag(
            format!(
                "page carries {} non-neutral literal accents ({}); one accent is the budget — \
                 route the rest through @tokens",
                accents.len(),
                accents.iter().cloned().collect::<Vec<_>>().join(", ")
            ),
            "fill",
        );
    }
    if overrides > 4 {
        page_diag(
            format!(
                "page carries {overrides} run-level size/family/color overrides; the budget is 4 \
                 (role \"code\" exempt)"
            ),
            "text",
        );
    }
}

/// A title or heading whose last wrapped line is a single word: the widow the
/// eye reads as a mistake. Uses the same greedy wrap as the renderer.
fn title_widows(
    page: &Page,
    resolved: &BTreeMap<String, Frame>,
    theme: &Theme,
    fonts: &FontStack,
    diags: &mut Vec<Diagnostic>,
) {
    for obj in page.walk() {
        let Object::Text(t) = obj else { continue };
        if !matches!(t.role.as_deref(), Some("title") | Some("heading")) {
            continue;
        }
        let Some(frame) = resolved.get(&t.id) else {
            continue;
        };
        let Some(last_para) = t.text.last() else {
            continue;
        };
        let role = theme
            .role(t.role.as_deref().unwrap_or("title"))
            .unwrap_or_else(|| theme.body());
        let lines = fonts.wrap(
            &last_para.plain_text(),
            &role.family,
            role.size.max(role.min_pt),
            role.weight,
            frame.w,
        );
        if lines.len() >= 2 {
            let last = lines.last().map(String::as_str).unwrap_or("");
            if !last.is_empty() && !last.contains(' ') {
                diags.push(
                    Diagnostic::new(
                        Severity::Warning,
                        format!(
                            "title widow: the last wrapped line is the one-word {last:?} over \
                             {} lines; rewrap or rephrase",
                            lines.len()
                        ),
                    )
                    .at(&page.id, &t.id)
                    .field("text"),
                );
            }
        }
    }
}

/// A hand-placed object on a page whose layout still has unclaimed slots:
/// the escape hatch is being used where the primary path was available.
fn free_at(board: &Board, page: &Page, theme: &Theme, diags: &mut Vec<Diagnostic>) {
    let Some(layout_name) = page.layout.as_deref() else {
        return;
    };
    let Some(slots) = crate::slots::layout(layout_name, board.canvas.size, &theme.spacing) else {
        return; // the resolver already warns about an unknown layout
    };
    let claimed: BTreeSet<&str> = page.walk().filter_map(|o| o.slot()).collect();
    let unclaimed: Vec<&str> = slots
        .keys()
        .map(String::as_str)
        .filter(|s| !claimed.contains(s))
        .collect();
    if unclaimed.is_empty() {
        return;
    }
    for obj in page.walk() {
        let placeable = matches!(
            obj,
            Object::Text(_)
                | Object::Shape(_)
                | Object::Image(_)
                | Object::Chart(_)
                | Object::Diagram(_)
                | Object::Icon(_)
        );
        if placeable && obj.slot().is_none() && obj.frame().is_some() {
            diags.push(
                Diagnostic::new(
                    Severity::Warning,
                    format!(
                        "free `at` on a page with layout {layout_name:?}; unclaimed slots: {}",
                        unclaimed.join(", ")
                    ),
                )
                .at(&page.id, obj.id())
                .field("at"),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// --fix: the mechanically-repairable classes
// ---------------------------------------------------------------------------

/// Repair the classes whose fix is unambiguous, in place, returning one line
/// per applied fix. In order: off-canvas objects clamp back into the canvas
/// (the clamped coordinate floors to the grid so the later snap cannot push
/// it back out) · sub-floor run-size overrides raise to their role's floor
/// (the renderer clamps them anyway; the file should say what draws) ·
/// near-miss-aligned edges snap the SECOND object to the first (Δ < 3 pt
/// only, one snap per object per axis) · off-grid `at`/`size` snap to the
/// 8 pt grid.
///
/// Geometry fixes touch only top-level objects with explicit `at`/`size` —
/// never slot-placed objects, whose geometry is derived at read time, and
/// never group children, whose envelope `normalize()` re-unions. The run
/// raise is content, not geometry, so it applies everywhere.
pub fn lint_fix(board: &mut Board, theme: &Theme) -> Vec<String> {
    let mut fixes = Vec::new();
    let (cw, ch) = (board.canvas.width(), board.canvas.height());
    // The 8 pt-quantized layout-grid snap targets, resolved before the pages
    // are borrowed mutably (and `None` unless a `canvas.grid` is declared).
    let grid_lines = crate::schema::grid_lines(&board.canvas);

    for page in &mut board.pages {
        // 1 — clamp off-canvas objects back in.
        for obj in &mut page.objects {
            if obj.slot().is_some() {
                continue;
            }
            let Some(f) = obj.frame() else { continue };
            let parked = f.right() < 0.0 || f.bottom() < 0.0 || f.x > cw || f.y > ch;
            if !parked {
                continue;
            }
            let clamp = |v: f64, max: f64| {
                let c = v.clamp(0.0, max.max(0.0));
                if c != v {
                    // Floor to the grid so pass 4's rounding cannot push the
                    // object back over the canvas edge.
                    ((c / GRID_PT).floor() * GRID_PT).max(0.0)
                } else {
                    c
                }
            };
            let (nx, ny) = (clamp(f.x, cw - f.w), clamp(f.y, ch - f.h));
            obj.set_at([nx, ny]);
            fixes.push(format!(
                "clamped {} into the {cw}×{ch} canvas: at [{}, {}] → [{}, {}]",
                obj.id(),
                pt(f.x),
                pt(f.y),
                pt(nx),
                pt(ny)
            ));
        }

        // 2 — raise sub-floor run overrides to the role floor.
        raise_run_floors(&mut page.objects, theme, &mut fixes);

        // 3 — snap near-miss-aligned edges (second object to the first).
        let mut frames: Vec<(usize, Frame)> = page
            .objects
            .iter()
            .enumerate()
            .filter(|(_, o)| o.slot().is_none())
            .filter_map(|(i, o)| o.frame().map(|f| (i, f)))
            .collect();
        // One snap per object per axis: a left-edge snap must not be undone
        // by a subsequent right-edge near-miss on the same pair.
        let mut snapped: BTreeSet<(usize, char)> = BTreeSet::new();
        for i in 0..frames.len() {
            for j in (i + 1)..frames.len() {
                let (fa, (jj, fb)) = (frames[i].1, frames[j]);
                let edges: [(&str, char, f64, f64); 4] = [
                    ("left", 'x', fa.x, fb.x),
                    ("right", 'x', fa.right(), fb.right()),
                    ("top", 'y', fa.y, fb.y),
                    ("bottom", 'y', fa.bottom(), fb.bottom()),
                ];
                for (edge, axis, va, vb) in edges {
                    let d = (va - vb).abs();
                    if d <= EPS || d >= ALIGN_TOLERANCE_PT || snapped.contains(&(jj, axis)) {
                        continue;
                    }
                    let mut nf = frames[j].1;
                    match edge {
                        "left" => nf.x = fa.x,
                        "right" => nf.x = fa.right() - nf.w,
                        "top" => nf.y = fa.y,
                        _ => nf.y = fa.bottom() - nf.h,
                    }
                    let a_id = page.objects[frames[i].0].id().to_string();
                    page.objects[jj].set_at([nf.x, nf.y]);
                    let b_id = page.objects[jj].id().to_string();
                    fixes.push(format!(
                        "snapped {b_id} {edge} edge {} → {} (aligns with {a_id})",
                        pt(vb),
                        pt(va)
                    ));
                    frames[j].1 = nf;
                    snapped.insert((jj, axis));
                }
            }
        }

        // 3b — snap near-grid `at` edges to the layout grid, when one is
        // declared. Targets are 8 pt-quantized, so pass 4 preserves them.
        // Groups are skipped (normalize re-unions their envelope, so a snap
        // here would be undone); only near-misses move, never a far object.
        if let Some((xs, ys)) = &grid_lines {
            let snap_axis = |lines: &[f64], v: f64| -> Option<f64> {
                let line = lines.iter().copied().min_by(|a, b| {
                    (a - v)
                        .abs()
                        .partial_cmp(&(b - v).abs())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })?;
                let d = (v - line).abs();
                (d > EPS && d < ALIGN_TOLERANCE_PT).then_some(line)
            };
            for obj in &mut page.objects {
                if obj.slot().is_some() || matches!(obj, Object::Group(_)) {
                    continue;
                }
                let Some(f) = obj.frame() else { continue };
                let nx = snap_axis(xs, f.x);
                let ny = if ys.is_empty() {
                    None
                } else {
                    snap_axis(ys, f.y)
                };
                if nx.is_none() && ny.is_none() {
                    continue;
                }
                let (tx, ty) = (nx.unwrap_or(f.x), ny.unwrap_or(f.y));
                obj.set_at([tx, ty]);
                fixes.push(format!(
                    "snapped {} to the layout grid: at [{}, {}] → [{}, {}]",
                    obj.id(),
                    pt(f.x),
                    pt(f.y),
                    pt(tx),
                    pt(ty)
                ));
            }
        }

        // 4 — snap explicit geometry to the grid.
        for obj in &mut page.objects {
            if obj.slot().is_some() {
                continue;
            }
            let Some(f) = obj.frame() else { continue };
            let (nx, ny) = (snap8(f.x), snap8(f.y));
            let (nw, nh) = (snap8(f.w).max(MIN_EXTENT_PT), snap8(f.h).max(MIN_EXTENT_PT));
            if (nx, ny, nw, nh) == (f.x, f.y, f.w, f.h) {
                continue;
            }
            obj.set_at([nx, ny]);
            obj.set_size([nw, nh]);
            let mut parts = Vec::new();
            if (nx, ny) != (f.x, f.y) {
                parts.push(format!(
                    "at [{}, {}] → [{}, {}]",
                    pt(f.x),
                    pt(f.y),
                    pt(nx),
                    pt(ny)
                ));
            }
            if (nw, nh) != (f.w, f.h) {
                parts.push(format!(
                    "size [{}, {}] → [{}, {}]",
                    pt(f.w),
                    pt(f.h),
                    pt(nw),
                    pt(nh)
                ));
            }
            fixes.push(format!(
                "snapped {} to the {GRID_PT} pt grid: {}",
                obj.id(),
                parts.join(", ")
            ));
        }
    }

    fixes
}

fn snap8(v: f64) -> f64 {
    if !v.is_finite() {
        return 0.0;
    }
    (v / GRID_PT).round() * GRID_PT
}

/// Raise every rich-run `size` override below its role's floor, recursing
/// into groups (a run override is content, not geometry, so group children
/// participate).
fn raise_run_floors(objects: &mut [Object], theme: &Theme, fixes: &mut Vec<String>) {
    for obj in objects {
        let (id, role_name, paras) = match obj {
            Object::Text(t) => (
                t.id.clone(),
                t.role.clone().unwrap_or_else(|| "body".into()),
                &mut t.text,
            ),
            Object::Shape(sh) => (
                sh.id.clone(),
                sh.role.clone().unwrap_or_else(|| "body".into()),
                &mut sh.text,
            ),
            Object::Connector(c) => (
                c.id.clone(),
                c.role.clone().unwrap_or_else(|| "label".into()),
                &mut c.text,
            ),
            Object::Callout(co) => (co.id.clone(), "caption".to_string(), &mut co.text),
            Object::Group(g) => {
                raise_run_floors(&mut g.objects, theme, fixes);
                continue;
            }
            _ => continue,
        };
        let floor = theme
            .role(&role_name)
            .unwrap_or_else(|| theme.body())
            .min_pt;
        for p in paras {
            let Paragraph::Rich(rich) = p else { continue };
            for r in &mut rich.runs {
                if let Some(size) = r.size {
                    if size + 1e-9 < floor {
                        r.size = Some(floor);
                        fixes.push(format!(
                            "raised {id} run override {} pt → {} pt (role {role_name:?} floor)",
                            pt(size),
                            pt(floor)
                        ));
                    }
                }
            }
        }
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

    #[cfg(feature = "icons")]
    #[test]
    fn an_unknown_icon_name_is_an_error_and_a_known_one_is_clean() {
        let diags =
            linted(r#"{"id":"ic","type":"icon","at":[0,0],"size":[48,48],"name":"no-such-xyz"}"#);
        let e = diags
            .iter()
            .find(|d| d.severity == Severity::Error && d.field.as_deref() == Some("name"))
            .expect("an unknown-icon error");
        assert!(e.message.contains("unknown icon"), "{}", e.message);
        assert_eq!(e.object.as_deref(), Some("ic"));
        // A real icon lints clean.
        let ok = linted(r#"{"id":"ic","type":"icon","at":[0,0],"size":[48,48],"name":"flask"}"#);
        assert!(ok.iter().all(|d| d.severity != Severity::Error), "{ok:?}");
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

    // --- equations (the C6 exception) --------------------------------------

    #[test]
    fn an_empty_equation_alt_is_an_error_naming_the_c6_condition() {
        let diags = linted(
            r#"{"id":"eq","type":"equation","at":[0,0],"size":[100,50],
                "tex":"x^2","alt":"  "}"#,
        );
        let e = diags
            .iter()
            .find(|d| d.severity == Severity::Error && d.field.as_deref() == Some("alt"))
            .expect("an empty-alt error");
        assert!(e.message.contains("LaTeX source"), "{}", e.message);
        assert_eq!(e.object.as_deref(), Some("eq"));
    }

    #[test]
    fn an_empty_equation_tex_is_an_error() {
        let diags = linted(
            r#"{"id":"eq","type":"equation","at":[0,0],"size":[100,50],
                "tex":"","alt":"nothing"}"#,
        );
        assert!(
            diags.iter().any(|d| d.severity == Severity::Error
                && d.field.as_deref() == Some("tex")
                && d.message.contains("empty")),
            "{diags:?}"
        );
    }

    #[cfg(feature = "math")]
    #[test]
    fn tex_that_does_not_compile_is_a_lint_error_with_the_reason() {
        let diags = linted(
            r#"{"id":"eq","type":"equation","at":[0,0],"size":[100,50],
                "tex":"x \\right)","alt":"broken"}"#,
        );
        let e = diags
            .iter()
            .find(|d| d.severity == Severity::Error && d.field.as_deref() == Some("tex"))
            .expect("a compile error");
        assert!(e.message.contains("TeX error"), "{}", e.message);
    }

    #[cfg(feature = "math")]
    #[test]
    fn a_well_formed_equation_lints_clean() {
        let diags = linted(
            r#"{"id":"eq","type":"equation","at":[0,0],"size":[100,50],
                "tex":"\\sum_{i=1}^{n} x_i","alt":"sum of x_i"}"#,
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[cfg(not(feature = "math"))]
    #[test]
    fn without_the_math_feature_tex_verification_degrades_to_a_warning() {
        let diags = linted(
            r#"{"id":"eq","type":"equation","at":[0,0],"size":[100,50],
                "tex":"x^2","alt":"x squared"}"#,
        );
        assert!(
            diags.iter().any(|d| d.severity == Severity::Warning
                && d.message.contains("cannot verify tex")),
            "{diags:?}"
        );
        assert!(
            diags.iter().all(|d| d.severity != Severity::Error),
            "{diags:?}"
        );
    }

    #[cfg(feature = "math")]
    #[test]
    fn an_equation_is_not_counted_as_verified_text_by_the_target_lint() {
        // The C6 exception: notation exports as a picture, so the target
        // lint applies no role floor and no font-resolution requirement to
        // it — a target-linted board holding only an equation stays clean,
        // where a text object would put its role on the census.
        let diags = target_linted(
            r#"{"id":"eq","type":"equation","at":[0,0],"size":[200,100],
                "tex":"\\hat{\\beta}","alt":"beta hat"}"#,
            "talk-light",
            "talk-16x9",
        );
        assert!(diags.is_empty(), "{diags:?}");
        // At a venue whose export floor is Vector, the census flags the
        // picture fate exactly as it does any raster — the C6 carve-out is
        // about text accounting, never a fidelity exemption.
        let diags = target_linted(
            r#"{"id":"eq","type":"equation","at":[0,0],"size":[200,100],
                "tex":"\\hat{\\beta}","alt":"beta hat"}"#,
            "figure-light",
            "pub-nature-single",
        );
        assert!(
            diags.iter().any(|d| d.severity == Severity::Error
                && d.message.contains("floors exports at")
                && d.message.contains("OMML")),
            "{diags:?}"
        );
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

    #[test]
    fn too_many_series_for_the_ramp_errors_naming_the_cap() {
        // The talk ramp is the full 7-color Okabe–Ito set, which passes
        // all-pairs CVD — so the computed cap is 7, and 8 series refuse.
        let rows: String = (0..8)
            .map(|i| format!(r#"{{"t":{i},"v":1,"s":"s{i}"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        let chart = format!(
            r#"{{"id":"c","type":"chart","at":[0,0],"size":[400,300],
                "data":{{"origin":"command","values":[{rows}]}},
                "x":{{"field":"t","type":"quantitative"}},
                "y":{{"field":"v","type":"quantitative"}},
                "color":{{"field":"s","type":"nominal"}}}}"#
        );
        let diags = target_linted(&chart, "talk-light", "talk-16x9");
        let e = diags
            .iter()
            .find(|d| d.message.contains("CVD-safe cap"))
            .expect("a series-cap error");
        assert_eq!(e.severity, Severity::Error);
        assert!(e.message.contains('8'), "{}", e.message);
        assert!(e.message.contains('7'), "names the cap: {}", e.message);
        assert_eq!(e.object.as_deref(), Some("c"));
        assert_eq!(e.field.as_deref(), Some("color"));

        // Three series sit well under the cap: no CVD finding.
        let rows: String = (0..3)
            .map(|i| format!(r#"{{"t":{i},"v":1,"s":"s{i}"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        let chart = format!(
            r#"{{"id":"c","type":"chart","at":[0,0],"size":[400,300],
                "data":{{"origin":"command","values":[{rows}]}},
                "x":{{"field":"t","type":"quantitative"}},
                "y":{{"field":"v","type":"quantitative"}},
                "color":{{"field":"s","type":"nominal"}}}}"#
        );
        let diags = target_linted(&chart, "talk-light", "talk-16x9");
        assert!(
            diags.iter().all(|d| !d.message.contains("CVD-safe cap")),
            "{diags:?}"
        );
    }

    // --- lint --style --------------------------------------------------------

    /// Parsed but NOT normalized: normalize snaps to the grid, and the style
    /// lint exists precisely to measure the file as it stands.
    fn style_board(objects: &str) -> crate::Board {
        crate::parse(&format!(
            r#"{{"format":"chimaera.board","formatVersion":1,
                "canvas":{{"size":[960,540]}},
                "pages":[{{"id":"p1","objects":[{objects}]}}]}}"#
        ))
        .unwrap()
    }

    fn style_linted(objects: &str, strict: bool) -> Vec<Diagnostic> {
        lint_style(
            &style_board(objects),
            &crate::theme::default_for(true),
            &FontStack::new(&[]),
            strict,
        )
    }

    fn rect(id: &str, at: [f64; 2], size: [f64; 2]) -> String {
        format!(
            r#"{{"id":"{id}","type":"shape","geo":"rect","at":[{},{}],"size":[{},{}]}}"#,
            at[0], at[1], size[0], size[1]
        )
    }

    #[test]
    fn an_overfull_table_cell_is_reported_with_the_cell_named() {
        let diags = style_linted(
            r#"{"id":"tb","type":"table","at":[80,80],"size":[240,48],
                "rows":[["a cell holding far more prose than a 24 pt row can seat","b"],
                        ["c","d"]]}"#,
            false,
        );
        let f = diags
            .iter()
            .find(|d| d.message.contains("overfull cell"))
            .expect("an overfull-cell finding");
        assert_eq!(f.object.as_deref(), Some("tb"));
        assert!(f.message.contains("[0][0]"), "{}", f.message);
        // Short cells never trip an underfull finding.
        assert!(
            !diags.iter().any(|d| d.message.contains("underfull")),
            "{diags:?}"
        );
    }

    #[test]
    fn a_table_with_mismatched_columns_warns_and_counts_role_floors() {
        let diags = linted(
            r#"{"id":"tb","type":"table","at":[80,80],"size":[320,160],
                "columns":[2,1],
                "rows":[["a","b","c"]]}"#,
        );
        let f = diags
            .iter()
            .find(|d| d.message.contains("3-column grid"))
            .expect("a columns-mismatch finding");
        assert_eq!(f.object.as_deref(), Some("tb"));
        assert!(f.message.contains("2 widths"), "{}", f.message);

        // Cells count toward the venue's per-role floor: talk-dark body
        // (20 pt, minPt 18) under pub-plos's 1.6× scale errors exactly as
        // bound shape text would.
        let target = target_linted(
            r#"{"id":"tb","type":"table","at":[80,80],"size":[320,160],
                "rows":[["a","b"]]}"#,
            "talk-dark",
            "pub-plos",
        );
        assert!(
            target
                .iter()
                .any(|d| d.object.as_deref() == Some("tb") && d.message.contains("floors")),
            "{target:?}"
        );
    }

    #[test]
    fn near_miss_alignment_fires_at_1_5_pt_naming_both_objects() {
        let diags = style_linted(
            &[
                rect("a", [80.0, 64.0], [160.0, 80.0]),
                rect("b", [81.5, 200.0], [160.0, 80.0]),
            ]
            .join(","),
            false,
        );
        let f = diags
            .iter()
            .find(|d| d.message.contains("near-miss alignment"))
            .expect("an alignment finding");
        assert_eq!(f.severity, Severity::Warning, "warning by default");
        assert_eq!(f.object.as_deref(), Some("b"), "attached to the second");
        assert!(
            f.message.contains("\"a\""),
            "names the first: {}",
            f.message
        );
        assert!(f.message.contains("81.5"), "{}", f.message);
        assert!(f.message.contains("80"), "{}", f.message);
        assert!(f.message.contains("left"), "names the edge: {}", f.message);
    }

    #[test]
    fn near_miss_alignment_is_silent_at_exactly_0_and_at_8() {
        for delta in [0.0, 8.0] {
            let diags = style_linted(
                &[
                    rect("a", [80.0, 64.0], [160.0, 80.0]),
                    rect("b", [80.0 + delta, 200.0], [160.0, 80.0]),
                ]
                .join(","),
                false,
            );
            assert!(
                diags
                    .iter()
                    .all(|d| !d.message.contains("near-miss alignment")),
                "Δ={delta}: {diags:?}"
            );
        }
    }

    #[test]
    fn strict_flips_the_measured_severities() {
        let objects = [
            rect("a", [80.0, 64.0], [160.0, 80.0]),
            rect("b", [81.5, 200.0], [160.0, 80.0]),
        ]
        .join(",");
        let relaxed = style_linted(&objects, false);
        let strict = style_linted(&objects, true);
        let sev = |diags: &[Diagnostic]| {
            diags
                .iter()
                .find(|d| d.message.contains("near-miss alignment"))
                .map(|d| d.severity)
        };
        assert_eq!(sev(&relaxed), Some(Severity::Warning));
        assert_eq!(sev(&strict), Some(Severity::Error));
    }

    #[test]
    fn near_miss_spacing_flags_uneven_gaps_in_a_row() {
        // Gaps of 24 and 26 across an aligned row of three.
        let diags = style_linted(
            &[
                rect("a", [80.0, 64.0], [160.0, 80.0]),  // right 240
                rect("b", [264.0, 64.0], [160.0, 80.0]), // gap 24, right 424
                rect("c", [450.0, 64.0], [160.0, 80.0]), // gap 26
            ]
            .join(","),
            false,
        );
        let f = diags
            .iter()
            .find(|d| d.message.contains("near-miss spacing"))
            .expect("a spacing finding");
        assert!(f.message.contains("24"), "{}", f.message);
        assert!(f.message.contains("26"), "{}", f.message);
        for id in ["\"a\"", "\"b\"", "\"c\""] {
            assert!(f.message.contains(id), "{}", f.message);
        }
    }

    #[test]
    fn off_grid_geometry_names_the_nearest_snap() {
        let diags = style_linted(&rect("a", [81.0, 131.0], [301.0, 49.0]), false);
        let at = diags
            .iter()
            .find(|d| d.message.contains("off-grid") && d.field.as_deref() == Some("at"))
            .expect("an off-grid at finding");
        assert!(at.message.contains("81"), "{}", at.message);
        assert!(at.message.contains("[80, 128]"), "{}", at.message);
        let size = diags
            .iter()
            .find(|d| d.message.contains("off-grid") && d.field.as_deref() == Some("size"))
            .expect("an off-grid size finding");
        assert!(size.message.contains("[304, 48]"), "{}", size.message);
    }

    /// A styled board carrying a 12-column layout grid (80 pt columns).
    fn grid_style_linted(objects: &str, strict: bool) -> Vec<Diagnostic> {
        let b = crate::parse(&format!(
            r#"{{"format":"chimaera.board","formatVersion":1,
                "canvas":{{"size":[960,540],"grid":{{"cols":12}}}},
                "pages":[{{"id":"p1","objects":[{objects}]}}]}}"#
        ))
        .unwrap();
        lint_style(
            &b,
            &crate::theme::default_for(true),
            &FontStack::new(&[]),
            strict,
        )
    }

    #[test]
    fn a_near_grid_edge_is_flagged_but_an_exact_one_is_not() {
        // Left edge at 83 is 3 pt off the 80 pt column line — a warning that
        // names the line; the same box at 80 is silent.
        let near = grid_style_linted(&rect("box", [82.0, 88.0], [160.0, 80.0]), false);
        let f = near
            .iter()
            .find(|d| d.message.contains("near-miss grid alignment"))
            .expect("a grid near-miss");
        assert_eq!(f.severity, Severity::Warning);
        assert_eq!(f.object.as_deref(), Some("box"));
        assert!(f.message.contains("column at 80"), "{}", f.message);

        let exact = grid_style_linted(&rect("box", [80.0, 88.0], [160.0, 80.0]), false);
        assert!(
            exact
                .iter()
                .all(|d| !d.message.contains("near-miss grid alignment")),
            "on the line is silent: {exact:?}"
        );
    }

    #[test]
    fn a_fully_off_grid_object_gets_a_gentle_info_nudge() {
        // 200 is far from every 80 pt column line → an Info nudge, never a
        // warning (floating an accent off the grid can be a choice).
        let diags = grid_style_linted(&rect("accent", [200.0, 88.0], [120.0, 40.0]), false);
        let f = diags
            .iter()
            .find(|d| d.message.contains("off the layout grid"))
            .expect("a gentle nudge");
        assert_eq!(f.severity, Severity::Info, "info, not a warning");
        assert!(f.message.contains("12-column"), "{}", f.message);
        // And a board with NO grid never speaks about the layout grid.
        let none = style_linted(&rect("accent", [200.0, 88.0], [120.0, 40.0]), false);
        assert!(
            none.iter().all(
                |d| !d.message.contains("layout grid") && !d.message.contains("grid alignment")
            ),
            "no grid, no grid findings: {none:?}"
        );
    }

    #[test]
    fn lint_fix_snaps_a_near_grid_edge_to_the_layout_grid() {
        let mut b = crate::parse(
            r#"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[960,540],"grid":{"cols":12}},
                "pages":[{"id":"p1","objects":[
                  {"id":"box","type":"shape","geo":"rect","at":[82,88],"size":[160,80]}]}]}"#,
        )
        .unwrap();
        let fixes = lint_fix(&mut b, &crate::theme::default_for(true));
        assert!(
            fixes
                .iter()
                .any(|f| f.contains("snapped box to the layout grid")),
            "{fixes:?}"
        );
        assert_eq!(b.pages[0].objects[0].frame().unwrap().x, 80.0);
        // The repaired board re-normalizes and re-lints clean of grid findings.
        crate::normalize(&mut b);
        let after = lint_style(
            &b,
            &crate::theme::default_for(true),
            &FontStack::new(&[]),
            false,
        );
        assert!(
            after.iter().all(|d| !d.message.contains("grid alignment")),
            "{after:?}"
        );
    }

    #[test]
    fn slot_placed_pages_produce_no_measured_findings() {
        // Slots are on-grid, aligned and inside the margins by construction.
        let b = crate::parse(
            r#"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[960,540]},
                "pages":[{"id":"p1","layout":"two-up","objects":[
                  {"id":"t","type":"text","role":"title","slot":"title","text":["Two columns"]},
                  {"id":"l","type":"shape","geo":"rect","slot":"body-left"},
                  {"id":"r","type":"shape","geo":"rect","slot":"body-right"}]}]}"#,
        )
        .unwrap();
        let diags = lint_style(
            &b,
            &crate::theme::default_for(true),
            &FontStack::new(&[]),
            false,
        );
        for needle in ["near-miss", "off-grid", "margin violation", "free `at`"] {
            assert!(
                diags.iter().all(|d| !d.message.contains(needle)),
                "{needle}: {diags:?}"
            );
        }
    }

    #[test]
    fn a_margin_crossing_names_the_edge_and_the_margin() {
        // talk margins are [64, 72, 64, 72]: x 16 crosses the 72 pt left.
        let diags = style_linted(&rect("a", [16.0, 200.0], [160.0, 80.0]), false);
        let f = diags
            .iter()
            .find(|d| d.message.contains("margin violation"))
            .expect("a margin finding");
        assert!(f.message.contains("left edge 16"), "{}", f.message);
        assert!(f.message.contains("72"), "{}", f.message);
        assert_eq!(f.object.as_deref(), Some("a"));
    }

    #[test]
    fn the_override_budget_exempts_code() {
        let runs = |role: &str| {
            format!(
                r#"{{"id":"t","type":"text","role":"{role}","at":[80,64],"size":[320,160],
                    "text":[{{"runs":[{{"t":"a","size":19}},{{"t":"b","size":19}},
                                      {{"t":"c","size":19}},{{"t":"d","size":19}},
                                      {{"t":"e","size":19}}]}}]}}"#
            )
        };
        // Five run overrides on a body text: over the budget of 4.
        let diags = style_linted(&runs("body"), false);
        let f = diags
            .iter()
            .find(|d| d.message.contains("override"))
            .expect("a budget finding");
        assert_eq!(f.severity, Severity::Warning);
        assert!(f.message.contains('5'), "{}", f.message);
        assert!(f.message.contains('4'), "{}", f.message);
        // Budgets stay warnings even under --strict.
        let strict = style_linted(&runs("body"), true);
        let f = strict
            .iter()
            .find(|d| d.message.contains("override"))
            .expect("a budget finding under strict");
        assert_eq!(f.severity, Severity::Warning, "budgets never escalate");
        // The same five overrides on role "code" are exempt.
        let diags = style_linted(&runs("code"), false);
        assert!(
            diags.iter().all(|d| !d.message.contains("override")),
            "{diags:?}"
        );
    }

    #[test]
    fn a_title_widow_is_detected_and_a_single_line_is_not() {
        let widow = r#"{"id":"h","type":"text","role":"title","at":[80,64],"size":[240,240],
            "text":["Results overview Antidisestablishmentarianism"]}"#;
        let diags = style_linted(widow, false);
        let f = diags
            .iter()
            .find(|d| d.message.contains("title widow"))
            .expect("a widow finding");
        assert_eq!(f.object.as_deref(), Some("h"));
        assert!(
            f.message.contains("Antidisestablishmentarianism"),
            "{}",
            f.message
        );
        // One short line has no last-line widow to speak of.
        let single = r#"{"id":"h","type":"text","role":"title","at":[80,64],"size":[640,80],
            "text":["Results"]}"#;
        let diags = style_linted(single, false);
        assert!(
            diags.iter().all(|d| !d.message.contains("title widow")),
            "{diags:?}"
        );
    }

    #[test]
    fn a_free_at_warns_when_the_layout_has_unclaimed_slots() {
        let b = crate::parse(
            r#"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[960,540]},
                "pages":[{"id":"p1","layout":"title-body","objects":[
                  {"id":"t","type":"text","role":"title","slot":"title","text":["Hi"]},
                  {"id":"hand","type":"shape","geo":"rect","at":[80,200],"size":[160,80]}]}]}"#,
        )
        .unwrap();
        let diags = lint_style(
            &b,
            &crate::theme::default_for(true),
            &FontStack::new(&[]),
            false,
        );
        let f = diags
            .iter()
            .find(|d| d.message.contains("free `at`"))
            .expect("a free-at finding");
        assert_eq!(f.object.as_deref(), Some("hand"));
        assert!(f.message.contains("body"), "names the slot: {}", f.message);
    }

    #[test]
    fn untraceable_produced_values_get_a_gentle_nudge() {
        let chart = |data: &str| {
            crate::parse(&format!(
                r#"{{"format":"chimaera.board","formatVersion":1,
                    "canvas":{{"size":[960,540]}},
                    "pages":[{{"id":"p1","objects":[
                      {{"id":"c","type":"chart","at":[80,80],"size":[480,320],
                       "data":{data},
                       "x":{{"field":"f","type":"nominal"}},
                       "y":{{"field":"v","type":"quantitative"}}}}]}}]}}"#
            ))
            .unwrap()
        };
        let style = |b: &Board, strict: bool| {
            lint_style(
                b,
                &crate::theme::default_for(true),
                &FontStack::new(&[]),
                strict,
            )
        };
        let nudged = |diags: &[Diagnostic]| {
            diags
                .iter()
                .find(|d| d.message.contains("untraceable data"))
                .cloned()
        };

        // command + inline values + neither source nor trace → the nudge…
        let bare = chart(r#"{"origin":"command","values":[{"f":"a","v":1}]}"#);
        let d = nudged(&style(&bare, false)).expect("a nudge");
        assert_eq!(d.object.as_deref(), Some("c"));
        assert_eq!(d.field.as_deref(), Some("data"));
        // …which stays a warning even under --strict: a nudge, never a block.
        assert_eq!(
            nudged(&style(&bare, true)).unwrap().severity,
            Severity::Warning
        );

        // A trace, a source binding, or a human/file origin all satisfy it.
        for ok in [
            chart(r#"{"origin":"command","values":[{"f":"a","v":1}],"trace":"wc -l per file"}"#),
            chart(r#"{"origin":"file","source":"bench.csv","sha256":"00"}"#),
            chart(r#"{"origin":"stated-by-user","values":[{"f":"a","v":1}]}"#),
        ] {
            assert!(
                nudged(&style(&ok, false)).is_none(),
                "{:?}",
                style(&ok, false)
            );
        }
    }

    // --- lint --fix ----------------------------------------------------------

    #[test]
    fn lint_fix_clamps_off_canvas_raises_sub_floor_and_reports() {
        let mut b = style_board(
            &[
                // Fully off the 960×540 canvas: x 2000.
                r#"{"id":"lost","type":"text","at":[2000,96],"size":[104,48],"text":["parked"]}"#
                    .to_string(),
                // A 2 pt run override under body's 18 pt floor. (x 88, so
                // the only left-edge near-miss on this page is a vs e.)
                r#"{"id":"tiny","type":"text","at":[88,64],"size":[320,96],
                    "text":[{"runs":[{"t":"small","size":2}]}]}"#
                    .to_string(),
                rect("a", [80.0, 200.0], [160.0, 80.0]),
                // 1 pt left near-miss vs a, and off-grid y.
                rect("e", [81.0, 331.0], [160.0, 80.0]),
            ]
            .join(","),
        );
        let fixes = lint_fix(&mut b, &crate::theme::default_for(true));

        let frame = |id: &str| {
            b.pages[0]
                .objects
                .iter()
                .find(|o| o.id() == id)
                .unwrap()
                .frame()
                .unwrap()
        };
        // Clamped inside, on the grid: x = 960 - 104 = 856.
        assert_eq!((frame("lost").x, frame("lost").y), (856.0, 96.0));
        assert!(
            fixes
                .iter()
                .any(|f| f.contains("clamped lost") && f.contains("2000") && f.contains("856")),
            "{fixes:?}"
        );
        // The run override rose to the role floor and said so.
        let Object::Text(t) = &b.pages[0].objects[1] else {
            panic!()
        };
        let Paragraph::Rich(rich) = &t.text[0] else {
            panic!()
        };
        assert_eq!(rich.runs[0].size, Some(18.0));
        assert!(
            fixes
                .iter()
                .any(|f| f.contains("raised tiny") && f.contains("2") && f.contains("18")),
            "{fixes:?}"
        );
        // e snapped to a's left edge, then its y snapped to the grid.
        assert_eq!((frame("e").x, frame("e").y), (80.0, 328.0));
        assert!(
            fixes
                .iter()
                .any(|f| f.contains("snapped e left edge") && f.contains("aligns with a")),
            "{fixes:?}"
        );
        assert!(
            fixes
                .iter()
                .any(|f| f.contains("snapped e to the 8 pt grid")),
            "{fixes:?}"
        );
        // Aligned-and-on-grid objects are untouched.
        assert_eq!((frame("a").x, frame("a").y), (80.0, 200.0));
    }

    #[test]
    fn lint_fix_never_touches_slot_placed_objects() {
        let mut b = crate::parse(
            r#"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[960,540]},
                "pages":[{"id":"p1","layout":"title-body","objects":[
                  {"id":"t","type":"text","role":"title","slot":"title","text":["Hi"]}]}]}"#,
        )
        .unwrap();
        let before = crate::to_string(&b).unwrap();
        let fixes = lint_fix(&mut b, &crate::theme::default_for(true));
        assert!(fixes.is_empty(), "{fixes:?}");
        assert_eq!(crate::to_string(&b).unwrap(), before, "no bytes moved");
    }
}
