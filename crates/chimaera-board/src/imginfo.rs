//! Shared image plumbing: byte sniffing, intrinsic sizes, base64, and the
//! sanitize/rasterize helpers for imported SVG figures.
//!
//! One copy on purpose. The renderer ([`crate::render`]), the PPTX writer
//! ([`crate::export::pptx`]) and the CLI's figure import all need to answer
//! "what is this file and how big is it natively" — two sniffers would drift
//! and disagree about the same bytes.

use std::sync::Arc;

use usvg::fontdb;

/// What a byte buffer claims to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImgKind {
    Png,
    Jpeg,
    Svg,
    Unknown,
}

/// Sniff an image's kind from its magic bytes, falling back to the `.svg`
/// extension / a `<svg` prefix scan for text.
pub fn sniff_image(bytes: &[u8], src: &str) -> ImgKind {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return ImgKind::Png;
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        return ImgKind::Jpeg;
    }
    let head = &bytes[..bytes.len().min(1024)];
    if src.to_ascii_lowercase().ends_with(".svg")
        || std::str::from_utf8(head)
            .map(|s| s.contains("<svg"))
            .unwrap_or(false)
    {
        return ImgKind::Svg;
    }
    ImgKind::Unknown
}

/// PNG intrinsic size from the IHDR chunk.
pub fn png_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 24 || &b[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes([b[16], b[17], b[18], b[19]]);
    let h = u32::from_be_bytes([b[20], b[21], b[22], b[23]]);
    (w > 0 && h > 0).then_some((w, h))
}

/// JPEG intrinsic size from the first SOF segment (SOF0–SOF2 and friends).
pub fn jpeg_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 4 || b[0] != 0xFF || b[1] != 0xD8 {
        return None;
    }
    let mut i = 2usize;
    while i + 3 < b.len() {
        if b[i] != 0xFF {
            return None; // lost sync — refuse rather than misread
        }
        let marker = b[i + 1];
        match marker {
            0xFF => {
                i += 1;
                continue;
            }
            0x01 | 0xD0..=0xD8 => {
                i += 2;
                continue;
            }
            0xD9 | 0xDA => return None, // end / entropy data before any SOF
            _ => {}
        }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        let is_sof = matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_sof {
            if i + 9 <= b.len() {
                let h = u16::from_be_bytes([b[i + 5], b[i + 6]]) as u32;
                let w = u16::from_be_bytes([b[i + 7], b[i + 8]]) as u32;
                return (w > 0 && h > 0).then_some((w, h));
            }
            return None;
        }
        i += 2 + len;
    }
    None
}

/// The natural pixel size for a sniffed raster kind; `None` for vector or
/// undecodable input.
pub fn raster_dimensions(kind: ImgKind, bytes: &[u8]) -> Option<(u32, u32)> {
    match kind {
        ImgKind::Png => png_dimensions(bytes),
        ImgKind::Jpeg => jpeg_dimensions(bytes),
        ImgKind::Svg | ImgKind::Unknown => None,
    }
}

/// Standard base64 with padding — the ~20 lines that keep a data URI from
/// costing a dependency.
pub fn base64_encode(bytes: &[u8]) -> String {
    const TBL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TBL[(n >> 18) as usize & 63] as char);
        out.push(TBL[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TBL[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TBL[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// An imported SVG after the usvg round-trip: normalized markup plus the
/// document's own size in user units.
pub struct SanitizedSvg {
    pub xml: String,
    pub width: f64,
    pub height: f64,
}

/// Parse an *untrusted* SVG through usvg and re-serialize it.
///
/// The round-trip is the sanitizer: usvg's tree model has no scripts, no
/// event handlers and no foreignObject, so none of them can survive into the
/// output — which matters because `page_svg` is also exported as text SVG,
/// not only rasterized. File-path `href`s are refused outright (untrusted
/// markup gets no disk reads); data URIs still resolve and re-embed.
/// `id_prefix` namespaces the document's internal ids so two inlined figures
/// cannot capture each other's defs.
pub fn sanitize_svg(
    src: &str,
    fontdb: Arc<fontdb::Database>,
    id_prefix: &str,
) -> Result<SanitizedSvg, String> {
    let opt = usvg::Options {
        fontdb,
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: usvg::ImageHrefResolver::default_data_resolver(),
            resolve_string: Box::new(|_, _| None),
        },
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(src, &opt).map_err(|e| e.to_string())?;
    let size = tree.size();
    let write = usvg::WriteOptions {
        id_prefix: Some(id_prefix.to_string()),
        preserve_text: false,
        indent: usvg::Indent::None,
        ..Default::default()
    };
    Ok(SanitizedSvg {
        xml: tree.to_string(&write),
        width: size.width() as f64,
        height: size.height() as f64,
    })
}

/// Rasterize sanitized SVG markup to a PNG of exactly `px_w`×`px_h`,
/// stretching non-uniformly — the placed box is the truth, matching the
/// renderer's `preserveAspectRatio="none"` placement.
pub fn rasterize_svg(
    xml: &str,
    fontdb: Arc<fontdb::Database>,
    px_w: u32,
    px_h: u32,
) -> Result<Vec<u8>, String> {
    let opt = usvg::Options {
        fontdb,
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(xml, &opt).map_err(|e| e.to_string())?;
    let size = tree.size();
    let mut pixmap = tiny_skia::Pixmap::new(px_w.max(1), px_h.max(1))
        .ok_or_else(|| format!("cannot allocate a {px_w}×{px_h} surface"))?;
    let transform = tiny_skia::Transform::from_scale(
        px_w.max(1) as f32 / size.width().max(1.0),
        px_h.max(1) as f32 / size.height().max(1.0),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    pixmap.encode_png().map_err(|e| e.to_string())
}

/// Distinct `#rrggbb` paints in the `fill`/`stroke`/`stop-color` attributes of
/// a *sanitized* SVG, in first-seen order. usvg's writer emits every color as
/// lowercase 6-digit hex, so hex scanning is exhaustive after the round-trip.
pub fn paint_colors(svg: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for attr in ["fill=\"#", "stroke=\"#", "stop-color=\"#"] {
        let mut rest = svg;
        while let Some(p) = rest.find(attr) {
            rest = &rest[p + attr.len()..];
            let end = rest.find('"').unwrap_or(rest.len());
            let hex = &rest[..end];
            if matches!(hex.len(), 3 | 6) && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                let canon = if hex.len() == 3 {
                    let mut s = String::with_capacity(7);
                    s.push('#');
                    for c in hex.chars() {
                        s.push(c.to_ascii_lowercase());
                        s.push(c.to_ascii_lowercase());
                    }
                    s
                } else {
                    format!("#{}", hex.to_ascii_lowercase())
                };
                if !out.contains(&canon) {
                    out.push(canon);
                }
            }
        }
    }
    out
}

/// Recolor a monochrome sanitized SVG to `tint_hex` (`#rrggbb`). More than
/// two distinct paints is refused with the count — tinting a polychrome
/// figure would erase an encoding, and the caller warns with the number.
pub fn apply_tint(svg: &str, tint_hex: &str) -> Result<String, usize> {
    let colors = paint_colors(svg);
    if colors.len() > 2 {
        return Err(colors.len());
    }
    let mut out = svg.to_string();
    for c in &colors {
        for attr in ["fill", "stroke", "stop-color"] {
            out = out.replace(
                &format!("{attr}=\"{c}\""),
                &format!("{attr}=\"{tint_hex}\""),
            );
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::FontStack;

    #[test]
    fn base64_matches_the_reference_vectors() {
        // RFC 4648 test vectors.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn sniffers_read_sizes_from_real_bytes() {
        let mut pm = tiny_skia::Pixmap::new(3, 2).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(10, 20, 30, 255));
        let png = pm.encode_png().unwrap();
        assert_eq!(sniff_image(&png, "x.png"), ImgKind::Png);
        assert_eq!(png_dimensions(&png), Some((3, 2)));
        assert_eq!(raster_dimensions(ImgKind::Png, &png), Some((3, 2)));
        assert_eq!(sniff_image(b"<svg xmlns='x'/>", "fig.svg"), ImgKind::Svg);
        assert_eq!(sniff_image(b"garbage", "x.bin"), ImgKind::Unknown);
    }

    #[test]
    fn sanitize_round_trip_strips_a_script_element() {
        let dirty = r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
            <script>alert(1)</script>
            <rect x="0" y="0" width="10" height="10" fill="#ff0000"
                  onclick="alert(2)"/>
        </svg>"##;
        let out = sanitize_svg(dirty, FontStack::new(&[]).db(), "t-").unwrap();
        assert!(!out.xml.contains("<script"), "{}", out.xml);
        assert!(!out.xml.contains("alert"), "{}", out.xml);
        assert!(!out.xml.contains("onclick"), "{}", out.xml);
        assert!(
            out.xml.contains("#ff0000"),
            "the drawing survives: {}",
            out.xml
        );
        assert_eq!((out.width, out.height), (10.0, 10.0));
    }

    #[test]
    fn sanitize_refuses_file_hrefs_but_keeps_data_uris() {
        let sneaky = r##"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4">
            <image href="/etc/hostname" width="4" height="4"/>
        </svg>"##;
        let out = sanitize_svg(sneaky, FontStack::new(&[]).db(), "t-").unwrap();
        assert!(!out.xml.contains("hostname"), "{}", out.xml);
        assert!(!out.xml.contains("<image"), "file ref dropped: {}", out.xml);
    }

    #[test]
    fn tint_recolors_monochrome_and_refuses_polychrome() {
        let mono = r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
            <rect width="10" height="10" fill="#333333"/>
            <path d="M 0 0 L 10 10" stroke="#333333" fill="none"/>
        </svg>"##;
        let san = sanitize_svg(mono, FontStack::new(&[]).db(), "t-").unwrap();
        let tinted = apply_tint(&san.xml, "#7cb8ff").unwrap();
        assert!(tinted.contains("#7cb8ff"), "{tinted}");
        assert!(!tinted.contains("#333333"), "{tinted}");

        let poly = r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
            <rect width="3" height="10" fill="#ff0000"/>
            <rect x="3" width="3" height="10" fill="#00ff00"/>
            <rect x="6" width="4" height="10" fill="#0000ff"/>
        </svg>"##;
        let san = sanitize_svg(poly, FontStack::new(&[]).db(), "t-").unwrap();
        assert_eq!(apply_tint(&san.xml, "#7cb8ff").unwrap_err(), 3);
    }

    #[test]
    fn rasterize_fills_the_requested_pixels() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
            <rect width="10" height="10" fill="#ff0000"/>
        </svg>"##;
        let png = rasterize_svg(svg, FontStack::new(&[]).db(), 20, 8).unwrap();
        assert_eq!(png_dimensions(&png), Some((20, 8)));
        let pm = tiny_skia::Pixmap::decode_png(&png).unwrap();
        let px = pm.pixel(10, 4).unwrap().demultiply();
        assert_eq!((px.red(), px.green(), px.blue()), (255, 0, 0));
    }
}
