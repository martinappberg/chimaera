//! `POST /api/v1/fs/resolve_targets` — what a document (or a chat message)
//! needs to know about every file its links and embeds name, in ONE round
//! trip: the resolved path, kind, size, version, mime, image dimensions read
//! from the header bytes, and a `/raw` ticket for the kinds that load through
//! one. Over an ssh tunnel each cold request costs about two round trips
//! (docs/perf-remote-plan.md, finding F2), so a document with thirty embeds
//! must not make thirty; knowing the dimensions up front is what lets the
//! placeholders reserve their final size, so nothing jumps as bytes arrive.
//!
//! The daemon only stats and reads headers here. Nothing is decoded: image
//! sizes come from at most [`HEADER_READ_CAP`] bytes of the file's header
//! (PNG, JPEG incl. the EXIF rotation, GIF, WebP, BMP; SVG `width`/`height`/
//! `viewBox`), and everything else is drawn by the browser.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use axum::extract::State;
use axum::response::Response;
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::AppState;

/// Most targets one request answers (a long document batches client-side).
const MAX_TARGETS: usize = 200;
/// Longest target considered, in bytes: longer strings are not paths.
const MAX_TARGET_BYTES: usize = 2048;
/// Most extra `bases` one request may add after `base`.
const MAX_BASES: usize = 8;
/// Wall-clock budget for one request: 200 targets over a cold NFS mount is
/// up to 200 canonicalize + stat + header reads. Targets not reached in time
/// are left out of the answer (unknown, not missing) and the client asks
/// again when it needs them.
const RESOLVE_BUDGET: Duration = Duration::from_secs(5);
/// Most bytes read from one image to learn its size. Skipped JPEG segments
/// are seeked over, not read, so a large EXIF block does not count.
const HEADER_READ_CAP: u64 = 64 * 1024;
/// Most JPEG segments walked before the frame header must have appeared.
const MAX_JPEG_SEGMENTS: usize = 64;
/// Bytes of an EXIF segment inspected for the orientation tag (IFD0 sits at
/// its start; the thumbnail that makes EXIF large comes later).
const EXIF_PROBE_BYTES: u64 = 1024;
/// Largest believable image side; a header claiming more is ignored.
const MAX_SIDE: u32 = 100_000;

/// Files a browser loads through a `/raw` ticket (`<img>`, pdf.js, a
/// sandboxed frame, `<video>`/`<audio>`): only these get one. The rest are
/// read through the bearer-authed API (`fs/file`, `fs/table`, …).
const TICKETED: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "ico", "avif", "pdf", "html", "htm",
    "xhtml", "mp4", "webm", "m4v", "ogv", "mov", "mp3", "wav", "m4a", "flac", "ogg", "oga", "opus",
    "aac",
];
/// Images whose header this module reads for a size.
const SIZED: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp"];

#[derive(Deserialize)]
pub(crate) struct ResolveTargetsRequest {
    /// The directory relative targets resolve against (a document's folder,
    /// a session's working directory). Absolute.
    base: String,
    /// Further directories tried after `base`, in order (chat: the spawn
    /// directory, the workspace root). Absolute entries only; additive.
    #[serde(default)]
    bases: Option<Vec<String>>,
    /// The workspace a root-relative `/x` target also resolves against.
    #[serde(default)]
    workspace_id: Option<String>,
    targets: Vec<String>,
}

/// POST /api/v1/fs/resolve_targets {base, bases?, workspace_id?, targets} →
/// `{results: {[target]: {path, kind, size, version, mtime_ms, mime, width?,
/// height?, ticket?} | {missing: true}}}`.
///
/// A target is written as a document writes it: `figs/plot.png`,
/// `../data.csv#row=1-20`, `my%20plot.png`, `/docs/x.md`, `file:///abs`.
/// The fragment and any query never take part in resolution, and escapes are
/// decoded — so a caller holding a real filesystem path (`/scratch/run#2/…`,
/// `/data/50%/…`) percent-escapes `%`, `#`, `?` and whitespace first (the
/// UI's `pathTarget`). Resolution is strict, exactly like document links
/// (`fs/validate` with `strict`): the exact join onto `base` then each of
/// `bases`; an absolute (or `~`) target as-is, and a root-relative `/x` that
/// misses also onto the workspace root (GitHub's reading). No diff-prefix
/// strip, no index guess — an embed names one file. Web URLs, and anything
/// that is neither a regular file nor a directory, are `missing`.
///
/// Per hit: `path` canonical; `kind` `file`|`dir`; `size` bytes; `version`
/// the `X-Mtime` token (so a client can tell a changed file from its cached
/// copy); `mtime_ms`; `mime` guessed from the extension (advisory);
/// `width`/`height` for PNG/JPEG/GIF/WebP/BMP/SVG when the header says; and
/// `ticket` — a `/raw` ticket, shared with `POST /fs/ticket` for the same
/// file version — only for kinds loaded through one ([`TICKETED`]).
///
/// Bounds: ≤ [`MAX_TARGETS`] targets (each ≤ [`MAX_TARGET_BYTES`]),
/// ≤ [`MAX_BASES`] extra bases, under the shared filesystem limiter with a
/// [`RESOLVE_BUDGET`]; a target not reached in time is absent from
/// `results`. Image headers: ≤ [`HEADER_READ_CAP`] bytes, opened
/// `O_NONBLOCK` (a FIFO named `plot.png` never parks the worker), never
/// decoded.
pub(crate) async fn resolve_targets(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ResolveTargetsRequest>,
) -> Response {
    if !Path::new(&body.base).is_absolute() {
        return crate::fs::bad_request(&anyhow::anyhow!(
            "base {:?} is not an absolute path",
            body.base
        ));
    }
    let root = body
        .workspace_id
        .as_deref()
        .and_then(|id| crate::lock(&state.workspaces).get(id))
        .map(|workspace| workspace.root);
    crate::fs::blocking_json(move || {
        let deadline = Instant::now() + RESOLVE_BUDGET;
        let mut bases = vec![PathBuf::from(&body.base)];
        for extra in body.bases.iter().flatten() {
            if bases.len() > MAX_BASES {
                break;
            }
            let extra = Path::new(extra);
            if extra.is_absolute() && !bases.iter().any(|b| b == extra) {
                bases.push(extra.to_path_buf());
            }
        }
        let mut results = serde_json::Map::new();
        for target in body.targets.iter().take(MAX_TARGETS) {
            if results.contains_key(target) {
                continue;
            }
            if Instant::now() >= deadline {
                break;
            }
            let answer = target_path(target)
                .and_then(|path| resolve(&path, &bases, root.as_deref()))
                .and_then(|path| describe(&state, &path))
                .unwrap_or_else(|| json!({"missing": true}));
            results.insert(target.clone(), answer);
        }
        Ok(json!({ "results": results }))
    })
    .await
}

/// The file part of a link or embed target, decoded; `None` when it names
/// nothing on this host (a web URL, an empty or in-page `#anchor`).
fn target_path(raw: &str) -> Option<String> {
    let t = raw.trim();
    if t.is_empty() || t.len() > MAX_TARGET_BYTES {
        return None;
    }
    let t = t
        .strip_prefix('<')
        .and_then(|inner| inner.strip_suffix('>'))
        .unwrap_or(t);
    let t = t.split_once('#').map_or(t, |(path, _)| path);
    let t = t.split_once('?').map_or(t, |(path, _)| path);
    let t = if let Some(rest) = strip_file_scheme(t) {
        rest
    } else if has_scheme(t) || t.starts_with("//") {
        return None;
    } else {
        t
    };
    let decoded = percent_decode(t);
    (!decoded.is_empty()).then_some(decoded)
}

/// `file:///abs` and `file://localhost/abs` as `/abs`; any other host is
/// not this machine.
fn strip_file_scheme(t: &str) -> Option<&str> {
    let head = t.get(..5)?;
    if !head.eq_ignore_ascii_case("file:") {
        return None;
    }
    let rest = &t[5..];
    match rest.strip_prefix("//") {
        None => Some(rest),
        Some(authority) if authority.starts_with('/') => Some(authority),
        Some(authority) => authority
            .strip_prefix("localhost")
            .filter(|p| p.starts_with('/')),
    }
}

/// A URL scheme (`https:`, `data:`, `mailto:`) leads the target.
fn has_scheme(t: &str) -> bool {
    let Some((scheme, _)) = t.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
}

/// `%XX` escapes decoded; the input as-is when the result is not UTF-8.
fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// Canonicalize the first place `target` names: as-is when absolute (or
/// `~`), else joined onto each base in order; a root-relative `/x` that
/// misses is tried under the workspace root.
fn resolve(target: &str, bases: &[PathBuf], root: Option<&Path>) -> Option<PathBuf> {
    let expanded = crate::fs::expand_tilde(target).ok()?;
    if expanded.is_absolute() {
        if let Ok(hit) = std::fs::canonicalize(&expanded) {
            return Some(hit);
        }
        let under_root = target.strip_prefix('/')?.trim_start_matches('/');
        if under_root.is_empty() {
            return None;
        }
        return std::fs::canonicalize(root?.join(under_root)).ok();
    }
    bases
        .iter()
        .find_map(|base| std::fs::canonicalize(base.join(&expanded)).ok())
}

/// The answer for one resolved path; `None` for anything that is neither a
/// regular file nor a directory (a socket, a device, a vanished file).
fn describe(state: &AppState, path: &Path) -> Option<serde_json::Value> {
    let meta = std::fs::metadata(path).ok()?;
    let version = crate::fs::mtime_token(&meta);
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    if meta.is_dir() {
        return Some(json!({
            "path": path.to_string_lossy(),
            "kind": "dir",
            "size": 0,
            "version": version,
            "mtime_ms": mtime_ms,
            "mime": "inode/directory",
        }));
    }
    if !meta.is_file() {
        return None;
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let mut info = json!({
        "path": path.to_string_lossy(),
        "kind": "file",
        "size": meta.len(),
        "version": version,
        "mtime_ms": mtime_ms,
        "mime": mime.essence_str(),
    });
    if SIZED.contains(&ext.as_str()) {
        if let Some((width, height)) = image_dimensions(path, &ext) {
            info["width"] = json!(width);
            info["height"] = json!(height);
        }
    }
    if TICKETED.contains(&ext.as_str()) {
        let ticket = crate::lock(&state.tickets).mint(path.to_path_buf(), Some(version));
        info["ticket"] = json!(ticket);
    }
    Some(info)
}

/// Open a regular file for header reads: `O_NONBLOCK`, and re-checked on the
/// descriptor, so a FIFO or device swapped in after the stat cannot block.
fn open_regular(path: &Path) -> Option<std::fs::File> {
    let fd = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )
    .ok()?;
    let file = std::fs::File::from(fd);
    file.metadata().ok()?.is_file().then_some(file)
}

/// An image's pixel size from its header, or `None` when the header does not
/// say (or is not what the extension claims). Raster formats are sniffed by
/// their magic bytes, so a JPEG saved as `.png` still answers.
pub(crate) fn image_dimensions(path: &Path, ext: &str) -> Option<(u32, u32)> {
    let mut file = open_regular(path)?;
    let dims = if ext == "svg" {
        let mut text = Vec::new();
        (&mut file)
            .take(HEADER_READ_CAP)
            .read_to_end(&mut text)
            .ok()?;
        svg_dimensions(&String::from_utf8_lossy(&text))?
    } else {
        let mut head = Vec::with_capacity(32);
        (&mut file).take(32).read_to_end(&mut head).ok()?;
        if head.starts_with(&[0xFF, 0xD8]) {
            jpeg_dimensions(&mut file)?
        } else {
            raster_dimensions(&head)?
        }
    };
    let plausible = |side: u32| (1..=MAX_SIDE).contains(&side);
    (plausible(dims.0) && plausible(dims.1)).then_some(dims)
}

fn be16(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 2)
        .map(|s| u32::from(u16::from_be_bytes([s[0], s[1]])))
}

fn le16(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 2)
        .map(|s| u32::from(u16::from_le_bytes([s[0], s[1]])))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn le24(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 3)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], 0]))
}

/// PNG, GIF, WebP (lossy, lossless, extended) and BMP from the first 32
/// bytes.
fn raster_dimensions(head: &[u8]) -> Option<(u32, u32)> {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    if head.starts_with(PNG) && head.get(12..16) == Some(b"IHDR".as_slice()) {
        return Some((be32(head, 16)?, be32(head, 20)?));
    }
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return Some((le16(head, 6)?, le16(head, 8)?));
    }
    if head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP".as_slice()) {
        return match head.get(12..16)? {
            b"VP8 " => {
                if head.get(23..26)? != [0x9D, 0x01, 0x2A] {
                    return None;
                }
                Some((le16(head, 26)? & 0x3FFF, le16(head, 28)? & 0x3FFF))
            }
            b"VP8L" => {
                if *head.get(20)? != 0x2F {
                    return None;
                }
                let bits = le32(head, 21)?;
                Some(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1))
            }
            b"VP8X" => Some((le24(head, 24)? + 1, le24(head, 27)? + 1)),
            _ => None,
        };
    }
    if head.starts_with(b"BM") {
        let dib = le32(head, 14)?;
        if dib == 12 {
            return Some((le16(head, 18)?, le16(head, 20)?));
        }
        if dib >= 40 {
            // Signed: a negative height is a top-down bitmap.
            let w = i32::from_le_bytes(head.get(18..22)?.try_into().ok()?);
            let h = i32::from_le_bytes(head.get(22..26)?.try_into().ok()?);
            return Some((w.unsigned_abs(), h.unsigned_abs()));
        }
    }
    None
}

/// Reads from the start of a JPEG within [`HEADER_READ_CAP`]; seeks are
/// free (they skip segment bodies without reading them).
struct Capped<'a, R> {
    inner: &'a mut R,
    left: u64,
}

impl<R: Read + Seek> Capped<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> Option<()> {
        let n = buf.len() as u64;
        if n > self.left {
            return None;
        }
        self.left -= n;
        self.inner.read_exact(buf).ok()
    }

    fn skip(&mut self, n: u64) -> Option<()> {
        let n = i64::try_from(n).ok()?;
        self.inner.seek(SeekFrom::Current(n)).ok().map(|_| ())
    }
}

/// A JPEG's size from its first frame header (SOFn), walking the segments
/// before it. EXIF orientations 5–8 (a phone photo taken upright) are drawn
/// rotated by the browser, so their sides are swapped here.
fn jpeg_dimensions<R: Read + Seek>(inner: &mut R) -> Option<(u32, u32)> {
    inner.seek(SeekFrom::Start(0)).ok()?;
    let mut r = Capped {
        inner,
        left: HEADER_READ_CAP,
    };
    let mut soi = [0u8; 2];
    r.read(&mut soi)?;
    if soi != [0xFF, 0xD8] {
        return None;
    }
    let mut rotated = false;
    let mut byte = [0u8; 1];
    for _ in 0..MAX_JPEG_SEGMENTS {
        r.read(&mut byte)?;
        if byte[0] != 0xFF {
            return None;
        }
        // Any number of 0xFF fill bytes may precede the marker code.
        let mut code = 0xFF;
        for _ in 0..16 {
            r.read(&mut byte)?;
            code = byte[0];
            if code != 0xFF {
                break;
            }
        }
        match code {
            0xFF => return None,
            // Standalone markers carry no length.
            0x01 | 0xD0..=0xD8 => continue,
            // End of image, or the scan began before any frame header.
            0xD9 | 0xDA => return None,
            _ => {}
        }
        let mut len = [0u8; 2];
        r.read(&mut len)?;
        let body = u64::from(u16::from_be_bytes(len)).checked_sub(2)?;
        let frame = matches!(code, 0xC0..=0xCF) && !matches!(code, 0xC4 | 0xC8 | 0xCC);
        if frame {
            let mut sof = [0u8; 5];
            if body < 5 {
                return None;
            }
            r.read(&mut sof)?;
            let h = u32::from(u16::from_be_bytes([sof[1], sof[2]]));
            let w = u32::from(u16::from_be_bytes([sof[3], sof[4]]));
            return Some(if rotated { (h, w) } else { (w, h) });
        }
        if code == 0xE1 {
            let probe = body.min(EXIF_PROBE_BYTES);
            let mut app = vec![0u8; usize::try_from(probe).ok()?];
            r.read(&mut app)?;
            rotated |= exif_rotates(&app);
            r.skip(body - probe)?;
            continue;
        }
        r.skip(body)?;
    }
    None
}

/// Whether an APP1 segment's EXIF orientation (tag 0x0112 in IFD0) turns
/// the image a quarter (values 5–8).
fn exif_rotates(app: &[u8]) -> bool {
    let Some(tiff) = app.strip_prefix(b"Exif\0\0") else {
        return false;
    };
    let little = match tiff.get(0..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => return false,
    };
    let u16_at = |at: usize| {
        if little {
            le16(tiff, at)
        } else {
            be16(tiff, at)
        }
    };
    let u32_at = |at: usize| {
        if little {
            le32(tiff, at)
        } else {
            be32(tiff, at)
        }
    };
    let Some(ifd) = u32_at(4).and_then(|at| usize::try_from(at).ok()) else {
        return false;
    };
    let Some(count) = u16_at(ifd) else {
        return false;
    };
    for i in 0..(count as usize).min(64) {
        let entry = ifd + 2 + i * 12;
        if u16_at(entry) == Some(0x0112) {
            return matches!(u16_at(entry + 8), Some(5..=8));
        }
    }
    false
}

/// An SVG's intrinsic size from its root element: `width`/`height` in
/// px (or unitless, or pt), else the `viewBox`, filling in a missing side
/// from the viewBox's ratio. Percentages and font-relative units say
/// nothing about the drawing's own size and are ignored.
fn svg_dimensions(text: &str) -> Option<(u32, u32)> {
    let mut from = 0;
    let open = loop {
        let at = from + text[from..].find("<svg")?;
        let next = text[at + 4..].chars().next()?;
        if next.is_whitespace() || next == '>' || next == '/' {
            break at + 4;
        }
        from = at + 4;
    };
    let tag = &text[open..];
    let mut quote: Option<char> = None;
    let mut end = None;
    for (i, c) in tag.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '>' => {
                end = Some(i);
                break;
            }
            None => {}
        }
    }
    let attrs = &tag[..end?];
    let width = svg_attr(attrs, "width").and_then(svg_length);
    let height = svg_attr(attrs, "height").and_then(svg_length);
    let view = svg_attr(attrs, "viewBox").and_then(view_box);
    let (w, h) = match (width, height, view) {
        (Some(w), Some(h), _) => (w, h),
        (Some(w), None, Some((vw, vh))) => (w, w * vh / vw),
        (None, Some(h), Some((vw, vh))) => (h * vw / vh, h),
        (None, None, Some(view)) => view,
        _ => return None,
    };
    let side = |v: f64| {
        let v = v.round();
        (v >= 1.0 && v <= f64::from(MAX_SIDE)).then_some(v as u32)
    };
    Some((side(w)?, side(h)?))
}

/// One attribute's value in a start tag's attribute text (`name="v"` or
/// `name='v'`), matched as a whole name (`stroke-width` is not `width`).
fn svg_attr<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let mut offset = 0;
    while let Some(i) = attrs[offset..].find(name) {
        let at = offset + i;
        offset = at + name.len();
        let whole = attrs[..at]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace);
        if !whole {
            continue;
        }
        let Some(value) = attrs[offset..].trim_start().strip_prefix('=') else {
            continue;
        };
        let value = value.trim_start();
        let quote = value.chars().next()?;
        if quote != '"' && quote != '\'' {
            continue;
        }
        let body = &value[1..];
        return body.find(quote).map(|close| &body[..close]);
    }
    None
}

/// A length in CSS px: unitless or `px`, `pt` converted (4/3 px each).
fn svg_length(v: &str) -> Option<f64> {
    let v = v.trim();
    let (number, scale) = if let Some(n) = v.strip_suffix("px") {
        (n, 1.0)
    } else if let Some(n) = v.strip_suffix("pt") {
        (n, 4.0 / 3.0)
    } else {
        (v, 1.0)
    };
    let n: f64 = number.trim().parse().ok()?;
    (n.is_finite() && n > 0.0).then_some(n * scale)
}

/// `viewBox="min-x min-y width height"` → (width, height).
fn view_box(v: &str) -> Option<(f64, f64)> {
    let parts: Vec<f64> = v
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|p| !p.is_empty())
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    match parts.as_slice() {
        [_, _, w, h] if w.is_finite() && h.is_finite() && *w > 0.0 && *h > 0.0 => Some((*w, *h)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        b
    }

    #[test]
    fn raster_headers_answer_their_size() {
        assert_eq!(raster_dimensions(&png(640, 480)), Some((640, 480)));
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&[0x20, 0x03, 0x58, 0x02]);
        gif.resize(32, 0);
        assert_eq!(raster_dimensions(&gif), Some((800, 600)));

        let mut vp8x = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0".to_vec();
        vp8x.extend_from_slice(&[0, 0, 0, 0]);
        vp8x.extend_from_slice(&[0xFF, 0x03, 0x00]); // 1024 - 1
        vp8x.extend_from_slice(&[0xFF, 0x02, 0x00]); // 768 - 1
        vp8x.resize(32, 0);
        assert_eq!(raster_dimensions(&vp8x), Some((1024, 768)));

        let mut vp8l = b"RIFF\0\0\0\0WEBPVP8L\0\0\0\0\x2f".to_vec();
        let bits: u32 = (300 - 1) | ((200 - 1) << 14);
        vp8l.extend_from_slice(&bits.to_le_bytes());
        vp8l.resize(32, 0);
        assert_eq!(raster_dimensions(&vp8l), Some((300, 200)));

        let mut vp8 = b"RIFF\0\0\0\0WEBPVP8 \0\0\0\0\0\0\0\x9d\x01\x2a".to_vec();
        vp8.extend_from_slice(&320u16.to_le_bytes());
        vp8.extend_from_slice(&240u16.to_le_bytes());
        vp8.resize(32, 0);
        assert_eq!(raster_dimensions(&vp8), Some((320, 240)));

        let mut bmp = b"BM".to_vec();
        bmp.resize(14, 0);
        bmp.extend_from_slice(&40u32.to_le_bytes());
        bmp.extend_from_slice(&50i32.to_le_bytes());
        bmp.extend_from_slice(&(-70i32).to_le_bytes()); // top-down
        bmp.resize(32, 0);
        assert_eq!(raster_dimensions(&bmp), Some((50, 70)));

        assert_eq!(raster_dimensions(b"not an image at all, just text.."), None);
        assert_eq!(raster_dimensions(&png(640, 480)[..20]), None, "short");
    }

    fn jpeg(segments: &[(u8, Vec<u8>)], w: u16, h: u16) -> Vec<u8> {
        let mut b = vec![0xFF, 0xD8];
        for (code, body) in segments {
            b.extend_from_slice(&[0xFF, *code]);
            b.extend_from_slice(&u16::try_from(body.len() + 2).unwrap().to_be_bytes());
            b.extend_from_slice(body);
        }
        b.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        b.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02]);
        b
    }

    fn exif(orientation: u16) -> Vec<u8> {
        let mut b = b"Exif\0\0II\x2a\0\x08\0\0\0".to_vec();
        b.extend_from_slice(&1u16.to_le_bytes()); // one entry
        b.extend_from_slice(&0x0112u16.to_le_bytes());
        b.extend_from_slice(&3u16.to_le_bytes()); // SHORT
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&orientation.to_le_bytes());
        b.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        b
    }

    #[test]
    fn jpeg_walks_to_the_frame_header_and_honors_rotation() {
        let jfif = (0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0".to_vec());
        let plain = jpeg(std::slice::from_ref(&jfif), 400, 300);
        assert_eq!(
            jpeg_dimensions(&mut std::io::Cursor::new(plain)),
            Some((400, 300))
        );
        let upright = jpeg(&[jfif.clone(), (0xE1, exif(6))], 4032, 3024);
        assert_eq!(
            jpeg_dimensions(&mut std::io::Cursor::new(upright)),
            Some((3024, 4032)),
            "orientation 6 swaps the sides"
        );
        let unrotated = jpeg(&[(0xE1, exif(1))], 4032, 3024);
        assert_eq!(
            jpeg_dimensions(&mut std::io::Cursor::new(unrotated)),
            Some((4032, 3024))
        );
        // Big segments before the frame header are seeked over, not read:
        // 3 × 60 KB of comments stay inside the 64 KB read budget.
        let big = vec![(0xFE, vec![b'x'; 60_000]); 3];
        let padded = jpeg(&big, 64, 48);
        assert!(padded.len() > 180_000);
        assert_eq!(
            jpeg_dimensions(&mut std::io::Cursor::new(padded)),
            Some((64, 48))
        );
        // A scan before any frame header, or truncation, answers nothing.
        let mut truncated = jpeg(&[], 10, 10);
        truncated.truncate(8);
        assert_eq!(jpeg_dimensions(&mut std::io::Cursor::new(truncated)), None);
    }

    #[test]
    fn svg_size_from_attributes_or_view_box() {
        let size = |s: &str| svg_dimensions(s);
        assert_eq!(size(r#"<svg width="300" height="150">"#), Some((300, 150)));
        assert_eq!(
            size(
                r#"<?xml version="1.0"?><!-- <svgx> --><svg xmlns="http://www.w3.org/2000/svg" width="10px" height='20px' stroke-width="9">"#
            ),
            Some((10, 20))
        );
        assert_eq!(size(r#"<svg viewBox="0 0 640 480">"#), Some((640, 480)));
        assert_eq!(
            size(r#"<svg width="320" viewBox="0,0,640,480">"#),
            Some((320, 240))
        );
        assert_eq!(
            size(r#"<svg width="100%" height="100%" viewBox="0 0 800 600">"#),
            Some((800, 600)),
            "percentages defer to the viewBox"
        );
        assert_eq!(size(r#"<svg width="72pt" height="36pt">"#), Some((96, 48)));
        assert_eq!(size(r#"<svg width="5em">"#), None);
        assert_eq!(size("<svgfoo width=\"1\" height=\"1\">"), None);
        assert_eq!(
            size(r#"<svg data-x="a>b" width="4" height="2">"#),
            Some((4, 2))
        );
        assert_eq!(size("plain text"), None);
    }

    #[test]
    fn targets_drop_fragments_decode_escapes_and_refuse_urls() {
        assert_eq!(
            target_path("figs/plot.png#xywh=0,0,5,5").as_deref(),
            Some("figs/plot.png")
        );
        assert_eq!(target_path("a.csv?raw=1#row=2").as_deref(), Some("a.csv"));
        assert_eq!(
            target_path("my%20plot%E2%9C%93.png").as_deref(),
            Some("my plot✓.png")
        );
        assert_eq!(target_path("<my plot.png>").as_deref(), Some("my plot.png"));
        assert_eq!(target_path("bad%zz.png").as_deref(), Some("bad%zz.png"));
        assert_eq!(target_path("tail%2").as_deref(), Some("tail%2"));
        assert_eq!(
            target_path("file:///tmp/x.pdf").as_deref(),
            Some("/tmp/x.pdf")
        );
        assert_eq!(
            target_path("file://localhost/tmp/x.pdf").as_deref(),
            Some("/tmp/x.pdf")
        );
        assert_eq!(target_path("file://elsewhere/tmp/x.pdf"), None);
        assert_eq!(target_path("https://example.com/a.png"), None);
        assert_eq!(target_path("data:image/png;base64,AAAA"), None);
        assert_eq!(target_path("//cdn.example.com/a.png"), None);
        assert_eq!(target_path("#heading"), None);
        assert_eq!(target_path("   "), None);
    }
}
