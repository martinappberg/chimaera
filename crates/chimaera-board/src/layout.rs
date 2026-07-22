//! Text measurement and font resolution.
//!
//! Layout truth is server-side. The pane never measures text in the DOM and
//! then trusts the number: a browser's `measureText` and the render engine
//! disagree about fallback, hinting and shaping, so a box sized in the DOM is
//! a box that overflows in the export. Everything that needs to know how wide
//! a string is asks this module, including the pane — via the raster it gets
//! back.
//!
//! The same `fontdb::Database` is handed to usvg at render time, and shaping
//! here uses the rustybuzz that usvg itself depends on, so a measured advance
//! and a drawn advance are the same number rather than two estimates that
//! happen to be close.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use usvg::fontdb;

/// Resolved metrics for one font at one size, in points.
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    pub ascent: f64,
    pub descent: f64,
    /// Ascent + |descent|; the em box a line occupies before line-height.
    pub height: f64,
}

/// A font database plus the family resolution Board does on top of it.
pub struct FontStack {
    db: Arc<fontdb::Database>,
    /// The families that were asked for but not found, in first-seen order.
    /// Reported by `describe` and `lint` rather than silently swallowed — a
    /// deck that renders in a different face on a login node than on a laptop
    /// is a determinism bug, and this is the only place it is detectable.
    missing: std::sync::Mutex<Vec<String>>,
}

impl FontStack {
    /// Build a stack. Workspace-vendored fonts win over system fonts, which is
    /// what makes `.chimaera/board/fonts/` meaningful: vendor a family and the
    /// render stops depending on what happens to be installed.
    pub fn new(font_dirs: &[PathBuf]) -> Self {
        let mut db = fontdb::Database::new();
        for dir in font_dirs {
            if dir.is_dir() {
                db.load_fonts_dir(dir);
            }
        }
        db.load_system_fonts();
        FontStack {
            db: Arc::new(db),
            missing: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// The stack for a workspace: its vendored fonts, then the system's.
    pub fn for_workspace(workspace: &Path) -> Self {
        FontStack::new(&[crate::board_dir(workspace).join("fonts")])
    }

    pub fn db(&self) -> Arc<fontdb::Database> {
        self.db.clone()
    }

    /// Families requested that no face could be found for.
    pub fn missing_families(&self) -> Vec<String> {
        self.missing.lock().map(|m| m.clone()).unwrap_or_default()
    }

    fn note_missing(&self, family: &str) {
        if let Ok(mut m) = self.missing.lock() {
            if !m.iter().any(|f| f == family) {
                m.push(family.to_string());
            }
        }
    }

    /// The first family in the stack that actually resolves, with its id.
    ///
    /// Generic names (`sans-serif`, `monospace`) are honoured as the last
    /// resort they are meant to be — a stack ending in `sans-serif` should
    /// render *something* rather than nothing on a bare container.
    pub fn resolve(&self, families: &[String], weight: u16, italic: bool) -> Option<ResolvedFont> {
        let style = if italic {
            fontdb::Style::Italic
        } else {
            fontdb::Style::Normal
        };
        for family in families {
            let name = family.trim();
            let db_family = match name.to_ascii_lowercase().as_str() {
                "sans-serif" => fontdb::Family::SansSerif,
                "serif" => fontdb::Family::Serif,
                "monospace" => fontdb::Family::Monospace,
                "cursive" => fontdb::Family::Cursive,
                "fantasy" => fontdb::Family::Fantasy,
                _ => fontdb::Family::Name(name),
            };
            let query = fontdb::Query {
                families: &[db_family],
                weight: fontdb::Weight(weight),
                stretch: fontdb::Stretch::Normal,
                style,
            };
            if let Some(id) = self.db.query(&query) {
                return Some(ResolvedFont {
                    id,
                    family: self
                        .db
                        .face(id)
                        .and_then(|f| f.families.first().map(|(n, _)| n.clone()))
                        .unwrap_or_else(|| name.to_string()),
                });
            }
            self.note_missing(name);
        }
        None
    }

    /// Advance width of `text` in points.
    ///
    /// Returns an estimate rather than zero when no face resolves — a chart
    /// whose gutters collapsed to nothing because a font was missing is far
    /// harder to diagnose than one whose labels are slightly misjudged, and
    /// the missing family is reported separately either way.
    pub fn measure(&self, text: &str, families: &[String], size: f64, weight: u16) -> f64 {
        if text.is_empty() {
            return 0.0;
        }
        let Some(font) = self.resolve(families, weight, false) else {
            return estimate_width(text, size);
        };
        self.db
            .with_face_data(font.id, |data, index| {
                let Some(face) = rustybuzz::Face::from_slice(data, index) else {
                    return estimate_width(text, size);
                };
                let upem = face.units_per_em() as f64;
                let mut buf = rustybuzz::UnicodeBuffer::new();
                buf.push_str(text);
                let shaped = rustybuzz::shape(&face, &[], buf);
                let units: i32 = shaped.glyph_positions().iter().map(|p| p.x_advance).sum();
                units as f64 / upem * size
            })
            .unwrap_or_else(|| estimate_width(text, size))
    }

    /// Vertical metrics at a given size.
    pub fn metrics(&self, families: &[String], size: f64, weight: u16) -> Metrics {
        let fallback = Metrics {
            ascent: size * 0.8,
            descent: size * 0.2,
            height: size,
        };
        let Some(font) = self.resolve(families, weight, false) else {
            return fallback;
        };
        self.db
            .with_face_data(font.id, |data, index| {
                let Ok(face) = ttf_parser::Face::parse(data, index) else {
                    return fallback;
                };
                let upem = face.units_per_em() as f64;
                let ascent = face.ascender() as f64 / upem * size;
                let descent = (face.descender() as f64 / upem * size).abs();
                Metrics {
                    ascent,
                    descent,
                    height: ascent + descent,
                }
            })
            .unwrap_or(fallback)
    }

    /// Greedy word wrap to `max_width` points.
    ///
    /// A single word wider than the line is emitted on its own line rather
    /// than broken mid-word: hyphenation is language-specific and getting it
    /// wrong looks worse than an overhang, which lint reports as an overfull
    /// box with the measured number.
    pub fn wrap(
        &self,
        text: &str,
        families: &[String],
        size: f64,
        weight: u16,
        max_width: f64,
    ) -> Vec<String> {
        if text.is_empty() {
            return vec![String::new()];
        }
        if max_width <= 0.0 {
            return vec![text.to_string()];
        }
        let mut lines = Vec::new();
        let mut current = String::new();
        for word in text.split_whitespace() {
            let candidate = if current.is_empty() {
                word.to_string()
            } else {
                format!("{current} {word}")
            };
            if self.measure(&candidate, families, size, weight) <= max_width || current.is_empty() {
                current = candidate;
            } else {
                lines.push(std::mem::take(&mut current));
                current = word.to_string();
            }
        }
        if !current.is_empty() || lines.is_empty() {
            lines.push(current);
        }
        lines
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedFont {
    pub id: fontdb::ID,
    /// The family that actually resolved, which is not always the one asked
    /// for.
    pub family: String,
}

/// A last-resort width estimate, used only when no face resolves at all.
/// Calibrated to a humanist sans at 0.5 em average advance.
fn estimate_width(text: &str, size: f64) -> f64 {
    text.chars().count() as f64 * size * 0.5
}

/// Render a family stack as an SVG `font-family` value, so the renderer asks
/// for exactly the stack this module measured.
pub fn css_font_family(families: &[String]) -> String {
    families
        .iter()
        .map(|f| {
            if f.contains(' ') {
                format!("'{f}'")
            } else {
                f.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack() -> FontStack {
        FontStack::new(&[])
    }

    #[test]
    fn measurement_grows_with_size_and_length() {
        let s = stack();
        let fam = vec!["sans-serif".to_string()];
        let short = s.measure("ab", &fam, 20.0, 400);
        let long = s.measure("abcdefgh", &fam, 20.0, 400);
        let big = s.measure("ab", &fam, 40.0, 400);
        assert!(long > short, "{long} !> {short}");
        assert!(big > short, "{big} !> {short}");
        // Scaling is linear in size, which is what lets lint reason about a
        // panel's placed scale without re-measuring at the new size.
        assert!((big - short * 2.0).abs() < 0.01, "{big} vs {short}");
    }

    #[test]
    fn empty_text_measures_zero() {
        assert_eq!(stack().measure("", &["sans-serif".into()], 20.0, 400), 0.0);
    }

    #[test]
    fn a_missing_family_is_recorded_not_swallowed() {
        let s = stack();
        let fam = vec!["Definitely Not A Real Font 9000".to_string()];
        let _ = s.measure("hello", &fam, 20.0, 400);
        assert!(s
            .missing_families()
            .iter()
            .any(|f| f.contains("Not A Real Font")));
    }

    #[test]
    fn wrapping_respects_the_measured_width() {
        let s = stack();
        let fam = vec!["sans-serif".to_string()];
        let text = "the quick brown fox jumps over the lazy dog";
        let width = 100.0;
        let lines = s.wrap(text, &fam, 12.0, 400, width);
        assert!(lines.len() > 1, "expected a wrap, got {lines:?}");
        for line in &lines {
            // Every line but an unbreakable single word fits.
            if line.contains(' ') {
                assert!(
                    s.measure(line, &fam, 12.0, 400) <= width + 0.01,
                    "{line:?} overflows"
                );
            }
        }
        assert_eq!(lines.join(" "), text, "wrapping must not lose words");
    }

    #[test]
    fn an_unbreakable_word_gets_its_own_line() {
        let s = stack();
        let fam = vec!["sans-serif".to_string()];
        let lines = s.wrap("a supercalifragilistic b", &fam, 12.0, 400, 20.0);
        assert!(lines.iter().any(|l| l == "supercalifragilistic"));
    }

    #[test]
    fn family_stacks_quote_only_what_needs_it() {
        let out = css_font_family(&["Helvetica Neue".into(), "Arial".into()]);
        assert_eq!(out, "'Helvetica Neue', Arial");
    }
}
