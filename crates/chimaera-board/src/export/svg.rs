//! SVG export, in two variants (plan §11 — matplotlib's own tradeoff,
//! surfaced as a choice).
//!
//! [`SvgVariant::Text`] is byte-for-byte the SVG the renderer rasterizes:
//! real `<text>` elements, editable in Illustrator and Inkscape, correct
//! wherever the fonts are installed. [`SvgVariant::Outlined`] runs that same
//! SVG through usvg — the same parser and the same `fontdb` the renderer
//! draws with — which resolves every glyph to a `<path>`, so the export
//! renders identically on machines without the fonts.

use anyhow::{Context, Result};

use crate::layout::FontStack;
use crate::schema::Board;
use crate::theme::Theme;

/// Which of the two SVG exports to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvgVariant {
    /// Real `<text>` elements — editable, but needs the fonts at the viewer.
    Text,
    /// Text resolved to `<path>` outlines — glyph fidelity without the font.
    Outlined,
}

/// Export one page as SVG.
pub fn export_svg(
    board: &Board,
    page_index: usize,
    theme: &Theme,
    fonts: &FontStack,
    variant: SvgVariant,
) -> Result<String> {
    let page = board
        .pages
        .get(page_index)
        .with_context(|| format!("board has no page {page_index}"))?;
    // Render-path diagnostics (off-canvas, overfull) belong to `render` and
    // `lint`; the export is the same emission and does not re-report them.
    let mut diags = Vec::new();
    let svg = crate::render::page_svg(board, page, theme, fonts, None, &mut diags)?;
    match variant {
        SvgVariant::Text => Ok(svg),
        SvgVariant::Outlined => {
            let opt = usvg::Options {
                fontdb: fonts.db(),
                ..Default::default()
            };
            let tree = usvg::Tree::from_str(&svg, &opt).context("parsing the generated SVG")?;
            // usvg's writer flattens text to paths unless told to preserve
            // it; stated explicitly because this default *is* the variant.
            let write = usvg::WriteOptions {
                preserve_text: false,
                ..Default::default()
            };
            Ok(tree.to_string(&write))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::FontStack;

    const DECK: &str = r#"{
      "format": "chimaera.board", "formatVersion": 1,
      "title": "t",
      "canvas": { "size": [960, 540] },
      "pages": [{
        "id": "p1",
        "objects": [
          { "id": "title", "type": "text", "role": "title", "at": [72, 64], "size": [816, 80],
            "text": ["The parser rewrite is 3× faster"] },
          { "id": "callout", "type": "shape", "geo": "roundRect", "at": [600, 200], "size": [288, 96],
            "fill": "@surface", "stroke": {"color": "@accent1", "width": 1.5},
            "text": [{"runs": [{"t": "3.3× median", "b": true}]}] }
        ]
      }]
    }"#;

    fn board() -> Board {
        let mut b = crate::parse(DECK).unwrap();
        crate::normalize(&mut b);
        b
    }

    #[test]
    fn text_variant_keeps_text_and_parses_back() {
        let b = board();
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let svg = export_svg(&b, 0, &theme, &fonts, SvgVariant::Text).unwrap();
        assert!(svg.contains("<text"), "real <text> elements survive: {svg}");
        // The export must round-trip through the same parser that would
        // rasterize it.
        let opt = usvg::Options {
            fontdb: fonts.db(),
            ..Default::default()
        };
        usvg::Tree::from_str(&svg, &opt).expect("text-variant SVG parses");
    }

    #[test]
    fn outlined_variant_has_paths_and_no_text() {
        let b = board();
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let svg = export_svg(&b, 0, &theme, &fonts, SvgVariant::Outlined).unwrap();
        assert!(!svg.contains("<text"), "no <text> may survive: {svg}");
        assert!(svg.contains("<path"), "glyphs become paths: {svg}");
    }

    #[test]
    fn both_variants_are_deterministic() {
        let b = board();
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        for variant in [SvgVariant::Text, SvgVariant::Outlined] {
            let a = export_svg(&b, 0, &theme, &fonts, variant).unwrap();
            let c = export_svg(&b, 0, &theme, &fonts, variant).unwrap();
            assert_eq!(a, c, "same board, same bytes ({variant:?})");
        }
    }

    #[test]
    fn a_missing_page_is_an_error() {
        let b = board();
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let err = export_svg(&b, 7, &theme, &fonts, SvgVariant::Text).unwrap_err();
        assert!(err.to_string().contains("no page 7"), "{err}");
    }
}
