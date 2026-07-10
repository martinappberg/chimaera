//! GET /download/{ticket} — browser-native downloads. An anchor navigation
//! cannot send an Authorization header, so downloads reuse the /raw story: a
//! short-lived single-path ticket, minted via the bearer-authed POST
//! /api/v1/fs/ticket, authorizes each fetch (the route is mounted outside the
//! auth layer). Files stream as-is with an attachment disposition;
//! directories stream as a zip built on the fly — memory is bounded by the
//! duplex pipe plus the deflate window, and nothing is spooled to disk.

use std::path::PathBuf;
use std::sync::Arc;

use async_zip::{Compression, ZipEntryBuilder};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use tokio::io::DuplexStream;
use tokio_util::io::ReaderStream;

use crate::AppState;

/// Hard ceiling on entries (files + directories) in one streamed zip. A
/// scratch dir on a login node can hold millions of files; past this the
/// stream is aborted (an honest failed download), never silently truncated
/// into an archive that looks complete.
const MAX_ZIP_ENTRIES: usize = 250_000;

/// Capacity of the duplex pipe between the zip task and the response body.
/// This (plus the deflate window) is the per-download RSS cost; backpressure
/// from a slow client suspends the walk rather than buffering.
const PIPE_CAPACITY: usize = 64 * 1024;

/// GET /download/{ticket} — the ticketed path as a browser download. A file
/// streams verbatim (Content-Length included, so the browser shows progress);
/// a directory streams as `<name>.zip`. Renaming or deleting the path after
/// minting simply 404s (the ticket is a per-path snapshot); a walk already in
/// flight finishes on whatever it can still read. Entry mtimes are not
/// carried into the zip (the writer's default DOS date) — a deliberate
/// omission, not worth a date dependency.
pub(crate) async fn download(
    State(state): State<Arc<AppState>>,
    Path(ticket): Path<String>,
) -> Response {
    let not_found = || (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response();
    let Some(path) = crate::lock(&state.tickets).lookup(&ticket) else {
        return not_found();
    };
    let Ok(meta) = tokio::fs::metadata(&path).await else {
        tracing::warn!(path = %path.display(), "ticketed download path unstattable");
        return not_found();
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_string());

    if meta.is_dir() {
        let (writer, reader) = tokio::io::duplex(PIPE_CAPACITY);
        let root = path.clone();
        tokio::spawn(async move {
            // A failure mid-walk drops the writer: the client sees a
            // truncated/failed download, never an incomplete archive
            // presented as success. A client disconnect closes the read
            // half, the next write errors, and the task exits.
            if let Err(err) = zip_dir(root.clone(), writer).await {
                tracing::warn!(path = %root.display(), %err, "folder download aborted");
            }
        });
        return (
            StatusCode::OK,
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/zip"),
                ),
                (
                    header::CONTENT_DISPOSITION,
                    content_disposition(&format!("{name}.zip")),
                ),
            ],
            Body::from_stream(ReaderStream::new(reader)),
        )
            .into_response();
    }

    let file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "ticketed download unreadable");
            return not_found();
        }
    };
    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_str(mime.essence_str())
                    .unwrap_or(HeaderValue::from_static("application/octet-stream")),
            ),
            (header::CONTENT_LENGTH, HeaderValue::from(meta.len())),
            (header::CONTENT_DISPOSITION, content_disposition(&name)),
        ],
        Body::from_stream(ReaderStream::new(file)),
    )
        .into_response()
}

/// Walk `root` and stream it as a zip into `pipe`. Iterative (no recursion),
/// symlinks are never followed (loop/escape safety — the quickopen walker's
/// discipline), unreadable entries are skipped like `fs/list` skips them.
/// Every directory gets an explicit `name/` entry so empty ones survive
/// extraction; entry names are rooted at the folder's own name, so unzipping
/// yields a single top-level folder.
async fn zip_dir(root: PathBuf, pipe: DuplexStream) -> anyhow::Result<()> {
    use tokio_util::compat::TokioAsyncReadCompatExt;

    let root_name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "folder".to_string());
    let mut zip = async_zip::base::write::ZipFileWriter::with_tokio(pipe);
    let mut stack = vec![root.clone()];
    let mut entries = 0usize;

    while let Some(dir) = stack.pop() {
        let rel = dir
            .strip_prefix(&root)
            .map(|r| r.to_string_lossy().into_owned())
            .unwrap_or_default();
        let dir_entry = if rel.is_empty() {
            format!("{root_name}/")
        } else {
            format!("{root_name}/{rel}/")
        };
        zip.write_entry_whole(
            ZipEntryBuilder::new(dir_entry.into(), Compression::Stored),
            &[],
        )
        .await?;
        entries += 1;

        let Ok(mut listing) = tokio::fs::read_dir(&dir).await else {
            continue; // unreadable dir: skipped, matching fs/list
        };
        while let Ok(Some(entry)) = listing.next_entry().await {
            let Ok(file_type) = entry.file_type().await else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if entries >= MAX_ZIP_ENTRIES {
                anyhow::bail!("more than {MAX_ZIP_ENTRIES} entries — refusing to zip");
            }
            if file_type.is_dir() {
                stack.push(entry.path());
                continue;
            }
            let Ok(file) = tokio::fs::File::open(entry.path()).await else {
                continue; // vanished or unreadable: skipped
            };
            let name = format!(
                "{root_name}/{}",
                entry
                    .path()
                    .strip_prefix(&root)
                    .unwrap_or(&entry.path())
                    .to_string_lossy()
            );
            let builder = ZipEntryBuilder::new(name.into(), Compression::Deflate);
            let mut entry_writer = zip.write_entry_stream(builder).await?;
            futures::io::copy(&mut file.compat(), &mut entry_writer).await?;
            entry_writer.close().await?;
            entries += 1;
        }
    }
    zip.close().await?;
    Ok(())
}

/// RFC 6266 attachment disposition: a sanitized ASCII `filename` fallback
/// plus, when the name isn't pure ASCII, an RFC 5987 `filename*` carrying the
/// exact UTF-8 name percent-encoded.
fn content_disposition(name: &str) -> HeaderValue {
    let ascii: String = name
        .chars()
        .map(|c| match c {
            '"' | '\\' => '_',
            c if c.is_ascii_graphic() || c == ' ' => c,
            _ => '_',
        })
        .collect();
    let value = if name.is_ascii() {
        format!("attachment; filename=\"{ascii}\"")
    } else {
        format!(
            "attachment; filename=\"{ascii}\"; filename*=UTF-8''{}",
            pct_encode(name)
        )
    };
    // The fallback is ASCII-sanitized above, so this cannot fail; the
    // static value is a belt-and-braces default.
    HeaderValue::from_str(&value).unwrap_or(HeaderValue::from_static("attachment"))
}

/// Percent-encode everything outside RFC 5987 `attr-char` as UTF-8 bytes.
fn pct_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'!'
            | b'#'
            | b'$'
            | b'&'
            | b'+'
            | b'-'
            | b'.'
            | b'^'
            | b'_'
            | b'`'
            | b'|'
            | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_disposition_ascii_passthrough_and_escaping() {
        assert_eq!(
            content_disposition("plot.png").to_str().unwrap(),
            "attachment; filename=\"plot.png\""
        );
        // Quotes and backslashes can't appear in a quoted-string fallback.
        assert_eq!(
            content_disposition("a\"b\\c.txt").to_str().unwrap(),
            "attachment; filename=\"a_b_c.txt\""
        );
    }

    #[test]
    fn content_disposition_non_ascii_gets_rfc5987_name() {
        let value = content_disposition("å plot.png");
        let value = value.to_str().unwrap();
        assert!(value.contains("filename=\"_ plot.png\""), "{value}");
        assert!(
            value.contains("filename*=UTF-8''%C3%A5%20plot.png"),
            "{value}"
        );
    }
}
