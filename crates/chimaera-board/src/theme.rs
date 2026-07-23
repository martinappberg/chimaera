//! Themes: the palette, the type scale with its per-role floors, the grid.
//!
//! Defaults are an acceptance criterion, not a nicety — the whole promise of
//! "better than PowerPoint out of the box" lives in this file. Three rules the
//! bundled themes follow, all of which are things an unconstrained generator
//! reliably gets wrong:
//!
//! - **Off-neutral grounds.** Never pure white or pure black; both make body
//!   text either glare or smear, and pure `#fff`/`#000` is the single clearest
//!   tell of a slide nobody designed.
//! - **Exactly one accent.** A palette with four "accents" has none. The
//!   categorical chart ramp is a separate thing and type may never resolve to
//!   it — a heading in a data color reads as an encoding that means something.
//! - **A modular scale, not arbitrary sizes.** Roles resolve into a ~1.25
//!   scale, so there is no `fontSize` field for an agent to reach for and no
//!   way to end up with 40 pt beside 38 pt.
//!
//! The categorical ramp is Okabe–Ito in both themes, reordered per ground for
//! contrast. It is colorblind-safe and stays distinguishable in grayscale,
//! which makes "is this palette legible" a computable property rather than a
//! taste argument.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// A resolved RGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub fn hex(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Relative luminance per WCAG 2.x, for the contrast check.
    pub fn luminance(&self) -> f64 {
        fn ch(v: u8) -> f64 {
            let s = v as f64 / 255.0;
            if s <= 0.039_28 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * ch(self.r) + 0.7152 * ch(self.g) + 0.0722 * ch(self.b)
    }

    /// WCAG contrast ratio, 1.0..=21.0.
    pub fn contrast(&self, other: &Rgb) -> f64 {
        let (a, b) = (self.luminance(), other.luminance());
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }
}

pub fn parse_hex(s: &str) -> Option<Rgb> {
    let s = s.strip_prefix('#')?;
    let v = match s.len() {
        3 => {
            let d: Vec<u8> = s
                .chars()
                .map(|c| c.to_digit(16).map(|d| d as u8))
                .collect::<Option<_>>()?;
            [d[0] * 17, d[1] * 17, d[2] * 17]
        }
        6 => {
            let n = u32::from_str_radix(s, 16).ok()?;
            [(n >> 16) as u8, (n >> 8) as u8, n as u8]
        }
        _ => return None,
    };
    Some(Rgb {
        r: v[0],
        g: v[1],
        b: v[2],
    })
}

/// One role in the type scale. Sizes come only from here — there is
/// deliberately no `fontSize` field on a text object.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeRole {
    /// A family *stack*, first available wins. Board records which one
    /// actually resolved rather than assuming, because a family that silently
    /// falls back is how a deck renders differently on a laptop and a login
    /// node.
    pub family: Vec<String>,
    pub size: f64,
    #[serde(default = "default_weight")]
    pub weight: u16,
    pub color: String,
    /// The floor for this role at this target. Per-role, not global: a chart
    /// tick label at 13 pt is correct while 13 pt body text is not, and a
    /// single global minimum cannot express that.
    pub min_pt: f64,
    #[serde(default = "default_line_height")]
    pub line_height: f64,
    #[serde(default)]
    pub tracking: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
}

fn default_weight() -> u16 {
    400
}
fn default_line_height() -> f64 {
    1.25
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spacing {
    pub grid: f64,
    /// `[top, right, bottom, left]` in points.
    pub margin: [f64; 4],
    pub gap: f64,
}

/// Chart chrome. Minimal by default: top and right spines off, thin axes,
/// direct labels over heavy legends.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartTheme {
    pub categorical: Vec<String>,
    pub axis: String,
    pub grid: String,
    #[serde(default = "default_axis_width")]
    pub axis_width: f64,
    #[serde(default = "default_series_width")]
    pub series_width: f64,
    /// Fraction of a band a bar occupies, 0..=1.
    #[serde(default = "default_bar_ratio")]
    pub bar_ratio: f64,
    /// Default continuous colormap for `rect` heatmap cells — one of the
    /// bundled names in [`crate::colormap`]. Named, never a literal ramp:
    /// perceptual uniformity is not a theme decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colormap: Option<String>,
}

fn default_axis_width() -> f64 {
    0.75
}
fn default_series_width() -> f64 {
    2.0
}
fn default_bar_ratio() -> f64 {
    0.68
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Theme {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Whether this theme sits on a dark ground. Used to pick a bundled theme
    /// that matches the app's current appearance — a white card in a dark
    /// workbench is a foreign object.
    #[serde(default)]
    pub dark: bool,
    pub palette: BTreeMap<String, String>,
    #[serde(rename = "type")]
    pub type_scale: BTreeMap<String, TypeRole>,
    pub spacing: Spacing,
    pub chart: ChartTheme,
}

impl Theme {
    /// Resolve a color reference: an `@token` through the palette, or a
    /// literal `#rrggbb`.
    ///
    /// The sigil is what makes indirection obvious to both a reader and an
    /// agent, and it maps straight onto the PPTX color model — `@`-refs export
    /// as `<a:schemeClr>` and literals as `<a:srgbClr>`, so a slide pasted into
    /// a themed deck re-themes natively.
    pub fn color(&self, reference: &str) -> Option<Rgb> {
        if let Some(token) = reference.strip_prefix('@') {
            // One level of indirection only: a token may not point at another
            // token. Chained aliases make a palette unreadable and invite
            // cycles for no expressive gain.
            let literal = self.palette.get(token)?;
            parse_hex(literal)
        } else {
            parse_hex(reference)
        }
    }

    /// Resolve a color, falling back to the foreground rather than failing —
    /// an unknown token is a lint finding, not a reason to render nothing.
    pub fn color_or_fg(&self, reference: Option<&str>) -> Rgb {
        reference
            .and_then(|r| self.color(r))
            .or_else(|| self.color("@fg"))
            .unwrap_or(Rgb { r: 0, g: 0, b: 0 })
    }

    pub fn role(&self, name: &str) -> Option<&TypeRole> {
        self.type_scale.get(name)
    }

    /// The role a text object uses when it declares none.
    pub fn body(&self) -> &TypeRole {
        self.type_scale
            .get("body")
            .expect("every theme defines a body role")
    }

    /// The n-th categorical color, wrapping. Wrapping rather than failing is
    /// deliberate — the *cap* on distinguishable series is a lint finding with
    /// a computed number, not a render-time panic.
    pub fn categorical(&self, i: usize) -> Rgb {
        let p = &self.chart.categorical;
        if p.is_empty() {
            return self.color_or_fg(None);
        }
        self.color(&p[i % p.len()])
            .unwrap_or(Rgb { r: 0, g: 0, b: 0 })
    }

    pub fn bg(&self) -> Rgb {
        self.color("@bg").unwrap_or(Rgb {
            r: 255,
            g: 255,
            b: 255,
        })
    }

    /// Check every role's resolved color against the ground it sits on.
    /// Reuses the app-theme legibility contract: text holds WCAG ≥ 4.5:1.
    pub fn contrast_findings(&self) -> Vec<String> {
        let bg = self.bg();
        let mut out = Vec::new();
        for (name, role) in &self.type_scale {
            let Some(c) = self.color(&role.color) else {
                out.push(format!(
                    "role {name:?} has unresolvable color {:?}",
                    role.color
                ));
                continue;
            };
            let ratio = c.contrast(&bg);
            if ratio < 4.5 {
                out.push(format!(
                    "role {name:?} is {ratio:.1}:1 against the background; text needs 4.5:1"
                ));
            }
        }
        out
    }

    pub fn parse(src: &str) -> Result<Theme> {
        serde_json::from_str(src).context("reading the theme")
    }

    pub fn load(path: &Path) -> Result<Theme> {
        let src =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Theme::parse(&src).with_context(|| format!("in {}", path.display()))
    }

    /// Resolve a theme by id or path: a bundled name, or a file.
    pub fn resolve(reference: &str, workspace: Option<&Path>) -> Result<Theme> {
        if let Some(t) = bundled(reference) {
            return Ok(t);
        }
        let direct = Path::new(reference);
        if direct.exists() {
            return Theme::load(direct);
        }
        if let Some(ws) = workspace {
            let candidates = [
                ws.join(reference),
                crate::board_dir(ws)
                    .join("themes")
                    .join(format!("{reference}.theme.json")),
            ];
            for c in candidates {
                if c.exists() {
                    return Theme::load(&c);
                }
            }
        }
        anyhow::bail!(
            "unknown theme {reference:?}; bundled variants are {} (or name a scheme: {})",
            BUNDLED_IDS.join(", "),
            SCHEMES.iter().map(|s| s.id).collect::<Vec<_>>().join(", ")
        )
    }
}

pub const BUNDLED_IDS: &[&str] = &["talk-dark", "talk-light", "figure-light", "figure-dark"];

/// A **scheme**: a named theme family that carries a light *and* a dark
/// variant. Naming a scheme (`talk`, `figure`) is what lets a board follow the
/// viewer's appearance — the concrete variant is chosen at render time by mode
/// — while still committing to an identity (type scale, chart chrome, font
/// families) the way a flat list of variant ids never could. Every scheme MUST
/// define both variants; a one-mode scheme is a hole a user falls through when
/// the app flips appearance.
#[derive(Debug, Clone, Copy)]
pub struct Scheme {
    pub id: &'static str,
    /// Human label for the pane's theme picker.
    pub label: &'static str,
    pub light: &'static str,
    pub dark: &'static str,
}

impl Scheme {
    /// The concrete variant id this scheme resolves to under `dark`.
    pub fn variant(&self, dark: bool) -> &'static str {
        if dark {
            self.dark
        } else {
            self.light
        }
    }
}

/// The bundled schemes — small and static; the pane's picker lists exactly
/// these. Each variant id MUST also appear in [`BUNDLED_IDS`] and [`bundled`],
/// so a scheme always resolves to a real theme in either mode (the invariant
/// the tests audit).
pub const SCHEMES: &[Scheme] = &[
    Scheme {
        id: "talk",
        label: "Talk",
        light: "talk-light",
        dark: "talk-dark",
    },
    Scheme {
        id: "figure",
        label: "Figure",
        light: "figure-light",
        dark: "figure-dark",
    },
];

/// The scheme that `auto` — and an absent theme — follows.
pub const DEFAULT_SCHEME: &str = "talk";

/// The bundled scheme with this id, if any.
pub fn scheme(id: &str) -> Option<&'static Scheme> {
    SCHEMES.iter().find(|s| s.id == id)
}

/// The picker's tag for the "pinned" case — a concrete variant or a workspace
/// theme file: a fixed ground that ignores the app's mode.
pub const PINNED_SELECTION: &str = "pinned";

/// What a board's theme reference *selects*, for the pane's OPTIONAL override
/// picker. The zero-config default is to **match the app**:
/// - `auto` — and an absent theme — returns [`AUTO_ID`] (`"auto"`): the board
///   follows the viewing app's light/dark automatically, no user action. The
///   picker shows this as "Match app (default)", selected out of the box.
/// - a scheme id (`talk`, `figure`) returns itself: an explicitly chosen
///   family that *still* follows the app's mode (an override of *which* family,
///   not of the match-the-app behavior).
/// - a pinned concrete variant, or a workspace `.theme.json`, returns
///   [`PINNED_SELECTION`]: a fixed ground the app's mode no longer moves.
///
/// The three cases are disjoint strings (no scheme is named `auto` or
/// `pinned`), so the UI can switch on this one field.
pub fn theme_selection(reference: Option<&str>) -> &'static str {
    match reference {
        None | Some(AUTO_ID) => AUTO_ID,
        Some(r) => match scheme(r) {
            Some(s) => s.id,
            None => PINNED_SELECTION,
        },
    }
}

/// The sentinel theme reference that follows the *viewer's* appearance: it is
/// resolved at render time to the default scheme's concrete variant (talk-dark
/// on a dark ground, talk-light on a light one), never stored as a theme of
/// its own. An absent `Board.theme` means the same thing — a board only ships
/// a fixed ground when it *pins* a concrete variant.
pub const AUTO_ID: &str = "auto";

/// Resolve a board's theme reference for a render. Three tiers, explicit-wins
/// throughout:
/// - `auto` — and an absent reference — follows the render's appearance via
///   the default scheme ([`DEFAULT_SCHEME`]).
/// - a **scheme** id (`talk`, `figure`) resolves to that scheme's variant for
///   the requested mode, so the card tracks the viewer's light/dark.
/// - anything else (a concrete variant id, or a workspace `.theme.json`) pins
///   regardless of mode — an explicit choice is never overridden.
///
/// Nothing here rewrites the stored reference: what the author wrote
/// (`"talk"`, `"auto"`, or a pinned `"talk-dark"`) stays in the file
/// byte-for-byte; resolution is render-time only. Every render path (daemon
/// route, CLI, export) funnels through here so a scheme id or "auto" can never
/// leak into [`Theme::resolve`] as an unknown id.
pub fn resolve_for_mode(
    reference: Option<&str>,
    dark: bool,
    workspace: Option<&Path>,
) -> Result<Theme> {
    match reference {
        None | Some(AUTO_ID) => Ok(default_for(dark)),
        Some(r) => match scheme(r) {
            Some(s) => bundled(s.variant(dark))
                .with_context(|| format!("scheme {r:?} variant failed to parse")),
            None => Theme::resolve(r, workspace),
        },
    }
}

/// The bundled themes, as the very `.theme.json` documents `board init`
/// writes out. Keeping them as source text rather than Rust literals means
/// the shipped defaults are exercised through the same parser a user's theme
/// is — a bundled theme that only works because it skipped deserialization
/// would be a lie about the format.
pub const TALK_DARK: &str = include_str!("themes/talk-dark.theme.json");
pub const TALK_LIGHT: &str = include_str!("themes/talk-light.theme.json");
/// The publication-leaning figure theme: a small type scale (9 pt body) with
/// Nature-compatible per-role floors (5 pt), the [bundled brand sans][brand]
/// leading the family stack (deterministic on a fontless render node), the
/// Okabe–Ito ramp, and thin 0.5 pt chart chrome. **Arial is retained next in
/// the stack** — a strict PLOS submission (which requires Arial, not Helvetica,
/// the trap that bounces submissions) pins Arial by editing the theme's family
/// stack (`theme-export … --format json`), since the bundled brand face now
/// resolves first everywhere.
///
/// [brand]: crate::layout::bundled::BRAND_SANS
pub const FIGURE_LIGHT: &str = include_str!("themes/figure-light.theme.json");
/// The dark counterpart to [`FIGURE_LIGHT`]: the same tight publication type
/// scale, the same brand-first family stack (Arial retained after it) and thin
/// 0.5 pt chart chrome, on a dark ground — so the `figure` scheme has a variant
/// for either appearance.
pub const FIGURE_DARK: &str = include_str!("themes/figure-dark.theme.json");

pub fn bundled(id: &str) -> Option<Theme> {
    let src = match id {
        "talk-dark" => TALK_DARK,
        "talk-light" => TALK_LIGHT,
        "figure-light" => FIGURE_LIGHT,
        "figure-dark" => FIGURE_DARK,
        _ => return None,
    };
    Theme::parse(src).ok()
}

/// The default theme for a given appearance: the default scheme's variant.
pub fn default_for(dark: bool) -> Theme {
    let s = scheme(DEFAULT_SCHEME).expect("the default scheme exists");
    bundled(s.variant(dark)).expect("bundled themes parse")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_themes_parse() {
        for id in BUNDLED_IDS {
            let t = bundled(id).unwrap_or_else(|| panic!("{id} failed to parse"));
            assert_eq!(&t.id, id);
            assert!(t.type_scale.contains_key("body"), "{id} needs a body role");
            assert!(
                t.type_scale.contains_key("title"),
                "{id} needs a title role"
            );
            assert!(!t.chart.categorical.is_empty(), "{id} needs a chart ramp");
        }
    }

    #[test]
    fn bundled_themes_meet_the_contrast_contract() {
        for id in BUNDLED_IDS {
            let t = bundled(id).unwrap();
            let findings = t.contrast_findings();
            assert!(findings.is_empty(), "{id}: {findings:?}");
        }
    }

    #[test]
    fn grounds_are_off_neutral() {
        // Pure white or pure black is the clearest tell of an undesigned deck,
        // and this is cheap to hold ourselves to.
        for id in BUNDLED_IDS {
            let bg = bundled(id).unwrap().bg();
            assert_ne!(bg.hex(), "#ffffff", "{id} background is pure white");
            assert_ne!(bg.hex(), "#000000", "{id} background is pure black");
        }
    }

    #[test]
    fn type_never_wears_a_data_color() {
        // A heading in a categorical color reads as an encoding that means
        // something. Mechanical, so it is enforced rather than advised.
        for id in BUNDLED_IDS {
            let t = bundled(id).unwrap();
            let ramp: Vec<Rgb> = t
                .chart
                .categorical
                .iter()
                .filter_map(|c| t.color(c))
                .collect();
            for (name, role) in &t.type_scale {
                let Some(c) = t.color(&role.color) else {
                    continue;
                };
                assert!(
                    !ramp.contains(&c),
                    "{id}: role {name:?} resolves to a categorical color"
                );
            }
        }
    }

    #[test]
    fn the_categorical_ramp_is_colorblind_safe_by_construction() {
        // Okabe–Ito, in either order. Asserting membership rather than
        // re-deriving CVD distance here: the palette's provenance is the
        // guarantee, and a test that recomputed it would just restate it.
        let okabe = [
            "#e69f00", "#56b4e9", "#009e73", "#f0e442", "#0072b2", "#d55e00", "#cc79a7",
        ];
        for id in BUNDLED_IDS {
            let t = bundled(id).unwrap();
            for c in &t.chart.categorical {
                let hex = t.color(c).unwrap().hex();
                assert!(
                    okabe.contains(&hex.as_str()),
                    "{id}: {hex} is not Okabe–Ito"
                );
            }
        }
    }

    #[test]
    fn every_bundled_theme_leads_text_roles_with_the_brand_sans() {
        // Brand + determinism as an acceptance criterion: every text (non-mono)
        // role leads with the bundled brand family, and that family really
        // resolves through a bare FontStack — so a fontless render node draws
        // the brand face, not a system fallback. `code` is exempt: it is the
        // bundled monospace, which also resolves.
        use crate::layout::{bundled, FontStack};
        let fonts = FontStack::new(&[]);
        for id in BUNDLED_IDS {
            let t = bundled(id).unwrap();
            for (name, role) in &t.type_scale {
                let want = if name == "code" {
                    bundled::MONO
                } else {
                    bundled::BRAND_SANS
                };
                assert_eq!(
                    role.family.first().map(String::as_str),
                    Some(want),
                    "{id}: role {name:?} must lead with {want:?}"
                );
                assert!(
                    fonts.resolve(&role.family, role.weight, false).is_some(),
                    "{id}: role {name:?} family did not resolve"
                );
            }
        }
        assert!(
            fonts.missing_families().is_empty(),
            "a bundled role family went missing: {:?}",
            fonts.missing_families()
        );
    }

    #[test]
    fn tokens_resolve_and_literals_pass_through() {
        let t = bundled("talk-dark").unwrap();
        assert!(t.color("@accent1").is_some());
        assert_eq!(t.color("#ff0000").unwrap().hex(), "#ff0000");
        assert_eq!(t.color("#f00").unwrap().hex(), "#ff0000");
        assert!(t.color("@nope").is_none());
    }

    #[test]
    fn auto_and_absent_follow_the_mode_but_a_pinned_theme_wins() {
        // "auto" is not a bundled theme — it resolves per render to the mode.
        assert!(bundled(AUTO_ID).is_none());
        assert_eq!(
            resolve_for_mode(Some("auto"), true, None).unwrap().id,
            "talk-dark"
        );
        assert_eq!(
            resolve_for_mode(Some("auto"), false, None).unwrap().id,
            "talk-light"
        );
        assert_eq!(resolve_for_mode(None, true, None).unwrap().id, "talk-dark");
        assert_eq!(
            resolve_for_mode(None, false, None).unwrap().id,
            "talk-light"
        );
        // An explicit choice is unchanged by the viewer's mode — a pinned
        // variant stays put whichever way the app is flipped.
        assert_eq!(
            resolve_for_mode(Some("talk-dark"), false, None).unwrap().id,
            "talk-dark"
        );
        assert_eq!(
            resolve_for_mode(Some("figure-light"), true, None)
                .unwrap()
                .id,
            "figure-light"
        );
        assert_eq!(
            resolve_for_mode(Some("figure-dark"), false, None)
                .unwrap()
                .id,
            "figure-dark"
        );
    }

    #[test]
    fn every_scheme_resolves_to_both_variants() {
        // The audit: a scheme is never a one-mode hole. Both variants exist,
        // parse, and carry the `dark` flag that matches the mode they serve.
        for s in SCHEMES {
            for dark in [true, false] {
                let variant = s.variant(dark);
                let t = bundled(variant)
                    .unwrap_or_else(|| panic!("scheme {} variant {variant} missing", s.id));
                assert_eq!(t.id, variant);
                assert_eq!(t.dark, dark, "{variant} sits on the mode's ground");
                // The variant is a real bundled id, listed for direct pinning.
                assert!(
                    BUNDLED_IDS.contains(&variant),
                    "{variant} not in BUNDLED_IDS"
                );
            }
        }
    }

    #[test]
    fn a_scheme_id_resolves_to_the_modes_variant() {
        // Naming the family lets the viewer's appearance pick the variant —
        // the whole point of the scheme model. The full table, both modes.
        let table = [
            ("talk", true, "talk-dark"),
            ("talk", false, "talk-light"),
            ("figure", true, "figure-dark"),
            ("figure", false, "figure-light"),
        ];
        for (id, dark, expected) in table {
            assert_eq!(
                resolve_for_mode(Some(id), dark, None).unwrap().id,
                expected,
                "scheme {id:?} in {} mode",
                if dark { "dark" } else { "light" }
            );
        }
    }

    #[test]
    fn theme_selection_tags_references_for_the_override_picker() {
        // The default is "match the app": auto/absent → "auto" (shown as
        // "Match app (default)"), NOT the default scheme's id — the picker
        // must not conflate the zero-config default with an explicit talk pick.
        assert_eq!(theme_selection(None), AUTO_ID);
        assert_eq!(theme_selection(Some(AUTO_ID)), "auto");
        // An explicitly chosen scheme is an override of which family.
        assert_eq!(theme_selection(Some("figure")), "figure");
        assert_eq!(theme_selection(Some("talk")), "talk");
        // A pinned variant or a workspace file is a fixed ground.
        assert_eq!(theme_selection(Some("talk-dark")), PINNED_SELECTION);
        assert_eq!(
            theme_selection(Some("my-workspace-theme")),
            PINNED_SELECTION
        );
    }

    #[test]
    fn a_stored_scheme_name_round_trips_unchanged() {
        // Byte stability: what the author wrote is what is stored. Resolution
        // is render-time only — "figure" never rewrites to a concrete variant.
        let mut board = crate::Board::new("t", crate::Canvas::default());
        board.theme = Some("figure".to_string());
        let bytes = crate::to_string(&board).unwrap();
        let reparsed = crate::parse(&bytes).unwrap();
        assert_eq!(reparsed.theme.as_deref(), Some("figure"));
        assert_eq!(crate::to_string(&reparsed).unwrap(), bytes);
    }

    #[test]
    fn contrast_ratio_matches_the_wcag_reference() {
        let white = parse_hex("#ffffff").unwrap();
        let black = parse_hex("#000000").unwrap();
        assert!((white.contrast(&black) - 21.0).abs() < 0.01);
        assert!((white.contrast(&white) - 1.0).abs() < 0.001);
    }
}
