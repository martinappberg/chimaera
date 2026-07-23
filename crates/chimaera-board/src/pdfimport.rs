//! PDF-panel import: rasterize one page of a PDF to a PNG asset.
//!
//! Gated behind the `pdf-import` cargo feature (off by default) because
//! hayro's interpreter stack is real weight on the static musl binary the
//! daemon ships as, and the mainline figure flow exports SVG/PNG from the
//! plotting code instead (docs/board-plan.md §10). The sniff, the ceilings
//! and the refusal message compile unconditionally so the CLI dispatch and
//! its error text cannot drift between builds.

/// A PDF file starts with the `%PDF-` header (ISO 32000 §7.5.2).
pub fn sniff_pdf(bytes: &[u8]) -> bool {
    bytes.starts_with(b"%PDF-")
}

/// Documents past this page count are refused outright: this import places
/// single figure panels, not documents.
pub const MAX_PDF_PAGES: usize = 200;

/// The rasterization density ceiling; requests above it are capped so a
/// `--dpi` typo cannot ask for a gigapixel surface.
pub const MAX_DPI: f64 = 600.0;

/// What a build without the feature says. The CLI surfaces this verbatim.
pub const MISSING_FEATURE: &str = "this build lacks pdf-import (build with --features pdf-import)";

/// One rasterized PDF page, plus what the CLI needs to place and report it.
#[derive(Debug)]
pub struct PdfPageRaster {
    /// The encoded PNG (white page background, like the PDF viewer shows).
    pub png: Vec<u8>,
    /// Rendered pixel size.
    pub pixel_size: [u32; 2],
    /// The page's own size in PDF points — the natural placement size.
    pub point_size: [f64; 2],
    /// Total pages in the document, for the import's report line.
    pub page_count: usize,
}

/// Rasterize one page (1-based) of a PDF at `dpi` (capped at [`MAX_DPI`]).
///
/// Refuses: unparseable/encrypted input, >[`MAX_PDF_PAGES`]-page documents,
/// out-of-range pages, and renders over the [`crate::render::MAX_PIXELS`]
/// raster ceiling — the same refuse-rather-than-allocate stance as `render`.
#[cfg(feature = "pdf-import")]
pub fn rasterize_pdf_page(bytes: Vec<u8>, page: usize, dpi: f64) -> Result<PdfPageRaster, String> {
    use hayro::hayro_interpret::InterpreterSettings;
    use hayro::hayro_syntax::{LoadPdfError, Pdf};
    use hayro::vello_cpu::color::palette::css::WHITE;

    if page == 0 {
        return Err("pages are 1-based; the first page is --pdf-page 1".to_string());
    }
    let dpi = dpi.clamp(1.0, MAX_DPI);

    let pdf = Pdf::new(bytes).map_err(|e| match e {
        LoadPdfError::Decryption(_) => "the PDF is encrypted; decrypt it first".to_string(),
        LoadPdfError::Invalid => "not a readable PDF".to_string(),
    })?;
    let pages = pdf.pages();
    let n = pages.len();
    if n == 0 {
        return Err("the PDF has no pages".to_string());
    }
    if n > MAX_PDF_PAGES {
        return Err(format!(
            "{n} pages is over the {MAX_PDF_PAGES}-page ceiling — this import places \
             single figure panels, not documents"
        ));
    }
    if page > n {
        return Err(format!("no page {page}: the PDF has {n} page(s)"));
    }
    let p = &pages[page - 1];

    // Refuse before allocating: the ceiling logic mirrors `render`, plus
    // hayro's u16 pixmap coordinates as a hard per-side bound.
    let (w_pt, h_pt) = p.render_dimensions();
    let scale = dpi / 72.0;
    let px_w = (w_pt as f64 * scale).floor().max(1.0) as u64;
    let px_h = (h_pt as f64 * scale).floor().max(1.0) as u64;
    if px_w * px_h > crate::render::MAX_PIXELS || px_w > u16::MAX as u64 || px_h > u16::MAX as u64 {
        return Err(format!(
            "page {page} would render {px_w}×{px_h} px at {dpi:.0} dpi ({} Mpx), over the \
             {} Mpx ceiling — lower --dpi",
            px_w * px_h / 1_000_000,
            crate::render::MAX_PIXELS / 1_000_000,
        ));
    }

    let settings = hayro::RenderSettings {
        x_scale: scale as f32,
        y_scale: scale as f32,
        // A PDF page is paper: white ground, exactly what a viewer shows.
        bg_color: WHITE,
        ..Default::default()
    };
    let pixmap = hayro::render(
        p,
        &hayro::RenderCache::new(),
        &InterpreterSettings::default(),
        &settings,
    );
    let (out_w, out_h) = (pixmap.width() as u32, pixmap.height() as u32);
    let png = pixmap
        .into_png()
        .map_err(|e| format!("encoding the rendered page as PNG: {e}"))?;
    Ok(PdfPageRaster {
        png,
        pixel_size: [out_w, out_h],
        point_size: [w_pt as f64, h_pt as f64],
        page_count: n,
    })
}

/// The no-feature stub: same signature, the one clear refusal.
#[cfg(not(feature = "pdf-import"))]
pub fn rasterize_pdf_page(
    _bytes: Vec<u8>,
    _page: usize,
    _dpi: f64,
) -> Result<PdfPageRaster, String> {
    Err(MISSING_FEATURE.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_matches_the_header_only() {
        assert!(sniff_pdf(b"%PDF-1.7\n..."));
        assert!(!sniff_pdf(b"<svg xmlns='x'/>"));
        assert!(!sniff_pdf(b""));
    }

    #[cfg(not(feature = "pdf-import"))]
    #[test]
    fn without_the_feature_the_refusal_names_the_flag() {
        let err = rasterize_pdf_page(b"%PDF-1.7".to_vec(), 1, 300.0).unwrap_err();
        assert_eq!(
            err,
            "this build lacks pdf-import (build with --features pdf-import)"
        );
    }

    /// A minimal n-page PDF built with the crate's own pdf-writer: each page
    /// is `w`×`h` points with a full-bleed red rectangle.
    #[cfg(feature = "pdf-import")]
    pub(crate) fn tiny_pdf(pages: usize, w: f32, h: f32) -> Vec<u8> {
        use pdf_writer::{Content, Finish, Pdf, Rect, Ref};

        let catalog_id = Ref::new(1);
        let page_tree_id = Ref::new(2);
        let mut next = 3;
        let mut ids = Vec::new();
        let mut pdf = Pdf::new();
        pdf.catalog(catalog_id).pages(page_tree_id);
        for _ in 0..pages {
            let (page_id, content_id) = (Ref::new(next), Ref::new(next + 1));
            next += 2;
            ids.push((page_id, content_id));
        }
        pdf.pages(page_tree_id)
            .kids(ids.iter().map(|(p, _)| *p))
            .count(pages as i32);
        for (page_id, content_id) in &ids {
            let mut page = pdf.page(*page_id);
            page.media_box(Rect::new(0.0, 0.0, w, h));
            page.parent(page_tree_id);
            page.contents(*content_id);
            page.finish();
            let mut content = Content::new();
            content.set_fill_rgb(1.0, 0.0, 0.0);
            content.rect(0.0, 0.0, w, h);
            content.fill_nonzero();
            pdf.stream(*content_id, &content.finish());
        }
        pdf.finish()
    }

    #[cfg(feature = "pdf-import")]
    #[test]
    fn rasterizes_a_page_at_the_requested_density() {
        let raster = rasterize_pdf_page(tiny_pdf(1, 200.0, 100.0), 1, 300.0).unwrap();
        // 200×100 pt at 300 dpi = ×(300/72).
        assert_eq!(raster.pixel_size, [833, 416]);
        assert_eq!(raster.point_size, [200.0, 100.0]);
        assert_eq!(raster.page_count, 1);
        assert_eq!(
            crate::imginfo::png_dimensions(&raster.png),
            Some((833, 416))
        );
        // The page actually drew: the center pixel is the red rectangle.
        let pm = tiny_skia::Pixmap::decode_png(&raster.png).unwrap();
        let px = pm.pixel(416, 208).unwrap().demultiply();
        assert_eq!((px.red(), px.green(), px.blue()), (255, 0, 0));
    }

    #[cfg(feature = "pdf-import")]
    #[test]
    fn page_bounds_refuse_loudly() {
        let pdf = tiny_pdf(2, 100.0, 100.0);
        let err = rasterize_pdf_page(pdf.clone(), 3, 300.0).unwrap_err();
        assert!(
            err.contains("no page 3") && err.contains("2 page(s)"),
            "{err}"
        );
        let err = rasterize_pdf_page(pdf, 0, 300.0).unwrap_err();
        assert!(err.contains("1-based"), "{err}");
        let err = rasterize_pdf_page(tiny_pdf(MAX_PDF_PAGES + 1, 10.0, 10.0), 1, 72.0).unwrap_err();
        assert!(err.contains("200-page ceiling"), "{err}");
    }

    #[cfg(feature = "pdf-import")]
    #[test]
    fn dpi_caps_and_the_pixel_ceiling_refuses() {
        // 10_000 dpi caps to 600: 100 pt × (600/72) = 833 px.
        let raster = rasterize_pdf_page(tiny_pdf(1, 100.0, 100.0), 1, 10_000.0).unwrap();
        assert_eq!(raster.pixel_size, [833, 833]);
        // A poster-sized page at full density is over 12 Mpx: refuse, no alloc.
        let err = rasterize_pdf_page(tiny_pdf(1, 10_000.0, 10_000.0), 1, 300.0).unwrap_err();
        assert!(err.contains("Mpx ceiling"), "{err}");
    }
}
