//! PDF export — every page of the deck, one document, real selectable text
//! with subsetted embedded fonts (plan §11).
//!
//! svg2pdf's high-level `to_pdf` writes exactly one page, so the multi-page
//! document is assembled here through its documented lower-level path:
//! each page's SVG (the same emission the renderer rasterizes) becomes a
//! Form XObject via [`svg2pdf::to_chunk`], and one `pdf-writer` document
//! places one XObject per page at the canvas size in points. This is a real
//! single document — shared catalog and page tree — not concatenated PDFs.
//!
//! Determinism: this writer adds no document-info, producer or dates, and
//! chunk renumbering follows chunk order. The one nondeterminism left is
//! inside svg2pdf 0.13 itself: `Context::write_global_objects` iterates its
//! font map — a `std::collections::HashMap` with randomized order — so when
//! a page embeds two or more fonts, the font *object order* (and therefore
//! the exact bytes) can differ between identical runs. The content is
//! identical either way. No public API pins that order, so the byte-identity
//! test below sticks to a single-font board and the general determinism
//! claim stays with SVG.

use std::collections::HashMap;

use anyhow::{bail, Context as _, Result};
use pdf_writer::{Content, Finish, Name, Pdf, Rect, Ref};

use crate::layout::FontStack;
use crate::schema::Board;
use crate::theme::Theme;

/// Export the whole board as one multi-page PDF.
pub fn export_pdf(board: &Board, theme: &Theme, fonts: &FontStack) -> Result<Vec<u8>> {
    if board.pages.is_empty() {
        bail!("board has no pages to export");
    }
    let w = board.canvas.width() as f32;
    let h = board.canvas.height() as f32;

    let mut alloc = Ref::new(1);
    let catalog_id = alloc.bump();
    let page_tree_id = alloc.bump();

    // Convert every page first so the page tree can be written complete.
    struct PdfPage {
        page_id: Ref,
        content_id: Ref,
        svg_id: Ref,
        chunk: pdf_writer::Chunk,
    }
    let mut pages = Vec::with_capacity(board.pages.len());
    for (i, page) in board.pages.iter().enumerate() {
        let mut diags = Vec::new();
        let svg = crate::render::page_svg(board, page, theme, fonts, None, &mut diags)?;
        let opt = usvg::Options {
            fontdb: fonts.db(),
            ..Default::default()
        };
        let tree = usvg::Tree::from_str(&svg, &opt)
            .with_context(|| format!("page {} ({}): parsing the generated SVG", i + 1, page.id))?;
        // embed_text keeps text selectable; svg2pdf subsets and embeds the
        // fonts itself from the tree's fontdb.
        let (chunk, svg_id) = svg2pdf::to_chunk(&tree, svg2pdf::ConversionOptions::default())
            .map_err(|e| anyhow::anyhow!("page {} ({}): converting to PDF: {e}", i + 1, page.id))?;
        // Lift the chunk's local refs into the document's ref space. The map
        // is only a lookup; new ids are handed out in chunk order, so the
        // renumbering itself is deterministic.
        let mut map = HashMap::new();
        let chunk = chunk.renumber(|old| *map.entry(old).or_insert_with(|| alloc.bump()));
        let svg_id = *map
            .get(&svg_id)
            .context("svg2pdf returned an XObject ref outside its own chunk")?;
        pages.push(PdfPage {
            page_id: alloc.bump(),
            content_id: alloc.bump(),
            svg_id,
            chunk,
        });
    }

    let mut pdf = Pdf::new();
    pdf.catalog(catalog_id).pages(page_tree_id);
    pdf.pages(page_tree_id)
        .count(pages.len() as i32)
        .kids(pages.iter().map(|p| p.page_id));

    let svg_name = Name(b"S1");
    for p in &pages {
        // The XObject is 1 pt × 1 pt by convention; scale it to the canvas.
        let mut content = Content::new();
        content
            .transform([w, 0.0, 0.0, h, 0.0, 0.0])
            .x_object(svg_name);
        pdf.stream(p.content_id, &content.finish());

        let mut page = pdf.page(p.page_id);
        page.media_box(Rect::new(0.0, 0.0, w, h));
        page.parent(page_tree_id);
        page.contents(p.content_id);
        let mut resources = page.resources();
        resources.x_objects().pair(svg_name, p.svg_id);
        resources.finish();
        page.finish();

        pdf.extend(&p.chunk);
    }

    Ok(pdf.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::FontStack;

    const TWO_PAGE_DECK: &str = r#"{
      "format": "chimaera.board", "formatVersion": 1,
      "title": "t",
      "canvas": { "size": [960, 540] },
      "pages": [
        {
          "id": "cover",
          "objects": [
            { "id": "title", "type": "text", "role": "title", "at": [72, 64], "size": [816, 80],
              "text": ["The parser rewrite is 3× faster"] },
            { "id": "box", "type": "shape", "geo": "roundRect", "at": [600, 200], "size": [288, 96],
              "fill": "@surface", "stroke": {"color": "@accent1", "width": 1.5} }
          ]
        },
        {
          "id": "results",
          "objects": [
            { "id": "h", "type": "text", "role": "heading", "at": [72, 64], "size": [816, 60],
              "text": ["Median latency, before and after"] },
            { "id": "chart", "type": "chart", "at": [72, 176], "size": [480, 288],
              "data": { "origin": "command", "values": [
                {"f": "large", "ms": 812, "build": "before"},
                {"f": "large", "ms": 244, "build": "after"}]},
              "x": {"field": "f"}, "y": {"field": "ms"}, "color": {"field": "build"},
              "marks": [{"mark": "bar", "stack": "group"}] }
          ]
        }
      ]
    }"#;

    fn board(json: &str) -> Board {
        let mut b = crate::parse(json).unwrap();
        crate::normalize(&mut b);
        b
    }

    #[test]
    fn a_two_page_deck_becomes_one_real_pdf() {
        let b = board(TWO_PAGE_DECK);
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let pdf = export_pdf(&b, &theme, &fonts).unwrap();
        assert_eq!(&pdf[..5], b"%PDF-", "PDF header");
        assert!(
            pdf.len() > 2048,
            "embedded fonts and content: {}",
            pdf.len()
        );
        // One document, two page objects in one page tree.
        let text = String::from_utf8_lossy(&pdf);
        assert_eq!(
            text.matches("/Type /Pages").count(),
            1,
            "one shared page tree"
        );
        // "/Type /Page" is a prefix of "/Type /Pages": two pages + the tree.
        assert_eq!(
            text.matches("/Type /Page").count(),
            3,
            "two /Type /Page objects plus the page tree"
        );
    }

    #[test]
    fn an_empty_board_is_refused() {
        let b = board(
            r#"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[400,200]},"pages":[]}"#,
        );
        let err =
            export_pdf(&b, &crate::theme::default_for(true), &FontStack::new(&[])).unwrap_err();
        assert!(err.to_string().contains("no pages"), "{err}");
    }

    // Byte-identity holds only while a single font family is embedded — see
    // the module comment on svg2pdf's font-map iteration order. This deck
    // styles everything with one role family, which is that case.
    #[test]
    fn export_is_deterministic_for_a_single_font_deck() {
        let b = board(
            r#"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[400,200]},
                "pages":[{"id":"p","objects":[
                  {"id":"t","type":"text","role":"body","at":[8,8],"size":[384,80],
                   "text":["one face only"]}]}]}"#,
        );
        let theme = crate::theme::default_for(true);
        let fonts = FontStack::new(&[]);
        let a = export_pdf(&b, &theme, &fonts).unwrap();
        let c = export_pdf(&b, &theme, &fonts).unwrap();
        assert_eq!(a, c, "same board, same bytes");
    }
}
