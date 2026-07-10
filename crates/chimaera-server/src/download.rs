//! GET /download/{ticket} — browser-native downloads. An anchor navigation
//! cannot send an Authorization header, so downloads reuse the /raw story: a
//! short-lived single-path ticket, minted via the bearer-authed POST
//! /api/v1/fs/ticket, authorizes each fetch (the route is mounted outside the
//! auth layer). Files stream as-is with an attachment disposition;
//! directories stream as a zip built on the fly — memory is bounded by the
//! duplex pipe plus the deflate window, and nothing is spooled to disk.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_zip::{
    AttributeCompatibility, Compression, ZipDateTime, ZipDateTimeBuilder, ZipEntryBuilder,
};
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
/// flight finishes on whatever it can still read. Entry mtimes ride into the
/// zip as DOS datetimes in UTC (zip carries no timezone; without them every
/// extracted file would read as 1980, the DOS epoch).
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
        let utc_offset = local_utc_offset().await;
        tokio::spawn(async move {
            // A failure mid-walk drops the writer: the client sees a
            // truncated/failed download, never an incomplete archive
            // presented as success. A client disconnect closes the read
            // half, the next write errors, and the task exits.
            if let Err(err) = zip_dir(root.clone(), writer, utc_offset).await {
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
async fn zip_dir(root: PathBuf, pipe: DuplexStream, utc_offset: i64) -> anyhow::Result<()> {
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
        let dir_meta = tokio::fs::metadata(&dir).await.ok();
        zip.write_entry_whole(
            stamp(
                ZipEntryBuilder::new(dir_entry.into(), Compression::Stored),
                dir_meta.as_ref(),
                utc_offset,
                0o040_755, // drwxr-xr-x
            ),
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
            let meta = file.metadata().await.ok();
            let builder = stamp(
                ZipEntryBuilder::new(name.into(), Compression::Deflate),
                meta.as_ref(),
                utc_offset,
                0o100_644, // -rw-r--r--
            );
            let mut entry_writer = zip.write_entry_stream(builder).await?;
            futures::io::copy(&mut file.compat(), &mut entry_writer).await?;
            entry_writer.close().await?;
            entries += 1;
        }
    }
    zip.close().await?;
    Ok(())
}

/// The daemon host's current UTC offset in seconds, via `date +%z` (POSIX;
/// prints e.g. `-0700`). std exposes no timezone and the workspace stays
/// unsafe-free, so a subprocess it is — one per folder download, noise next
/// to streaming the archive. Falls back to UTC (0) if `date` misbehaves.
async fn local_utc_offset() -> i64 {
    match tokio::process::Command::new("date")
        .arg("+%z")
        .output()
        .await
    {
        Ok(out) if out.status.success() => {
            parse_utc_offset(String::from_utf8_lossy(&out.stdout).trim())
        }
        _ => 0,
    }
}

/// Parse a `[+-]HHMM` (or `[+-]HH:MM`) UTC offset into seconds; 0 on
/// anything unexpected.
fn parse_utc_offset(s: &str) -> i64 {
    let (sign, rest) = match s.as_bytes().first() {
        Some(b'+') => (1, &s[1..]),
        Some(b'-') => (-1, &s[1..]),
        _ => return 0,
    };
    let digits = rest.replace(':', "");
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return 0;
    }
    let hours: i64 = digits[..2].parse().unwrap_or(0);
    let minutes: i64 = digits[2..].parse().unwrap_or(0);
    sign * (hours * 3600 + minutes * 60)
}

/// Stamp a zip entry with its file's mtime and unix mode. Left unstamped,
/// the writer's defaults extract as 1980-dated, mode-000 files (Info-ZIP
/// applies the zeroed attribute bits verbatim). The full st_mode goes in —
/// type bits like Info-ZIP writes them, so the executable bit survives on
/// scripts; `fallback` covers an unreadable metadata.
fn stamp(
    builder: ZipEntryBuilder,
    meta: Option<&std::fs::Metadata>,
    utc_offset: i64,
    fallback: u32,
) -> ZipEntryBuilder {
    use std::os::unix::fs::PermissionsExt;
    let mode = meta.map(|m| m.permissions().mode()).unwrap_or(fallback);
    let builder = builder
        .attribute_compatibility(AttributeCompatibility::Unix)
        .unix_permissions((mode & 0xFFFF) as u16);
    match meta.and_then(|m| m.modified().ok()) {
        Some(mtime) => builder.last_modification_date(zip_datetime(mtime, utc_offset)),
        None => builder,
    }
}

/// A file mtime as a zip DOS datetime in the daemon host's local time
/// (`utc_offset` seconds east of UTC): zip carries no timezone, and local
/// wall-clock is the format's convention, so extraction shows the time the
/// user saw on the file. One offset per archive, captured at download time —
/// entries last touched in a different DST phase skew by the DST hour (the
/// standard zip caveat), and a remote daemon stamps ITS host's local time.
/// DOS dates span 1980–2107 at 2-second granularity — out-of-range mtimes
/// clamp, and the builder itself masks the seconds' low bit.
fn zip_datetime(mtime: SystemTime, utc_offset: i64) -> ZipDateTime {
    let secs = mtime
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
        + utc_offset;
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    let (hour, minute, second) = (
        (rem / 3600) as u32,
        (rem % 3600 / 60) as u32,
        (rem % 60) as u32,
    );
    let clamped = if year < 1980 {
        (1980, 1, 1, 0, 0, 0)
    } else if year > 2107 {
        (2107, 12, 31, 23, 59, 58)
    } else {
        (year, month, day, hour, minute, second)
    };
    ZipDateTimeBuilder::new()
        .year(clamped.0)
        .month(clamped.1)
        .day(clamped.2)
        .hour(clamped.3)
        .minute(clamped.4)
        .second(clamped.5)
        .build()
}

/// Proleptic-Gregorian civil date from days since 1970-01-01 — Howard
/// Hinnant's `civil_from_days`, the standard allocation-free algorithm
/// (keeps a date dependency out of the daemon).
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (
        (if month <= 2 { year + 1 } else { year }) as i32,
        month,
        day,
    )
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

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_783), (2024, 3, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29)); // leap day
    }

    #[test]
    fn zip_datetime_carries_mtime_and_clamps_the_dos_floor() {
        // 2024-03-01T04:05:06Z.
        let mtime =
            UNIX_EPOCH + std::time::Duration::from_secs(19_783 * 86_400 + 4 * 3600 + 5 * 60 + 6);
        let dt = zip_datetime(mtime, 0);
        assert_eq!(
            (
                dt.year(),
                dt.month(),
                dt.day(),
                dt.hour(),
                dt.minute(),
                dt.second()
            ),
            (2024, 3, 1, 4, 5, 6), // even second survives DOS 2s granularity
        );
        // A negative UTC offset shifts across midnight into the previous
        // local day (04:05:06Z at UTC-7 is 21:05:06 the day before).
        let dt = zip_datetime(mtime, -7 * 3600);
        assert_eq!(
            (dt.year(), dt.month(), dt.day(), dt.hour()),
            (2024, 2, 29, 21),
        );
        // Pre-1980 mtimes clamp to the DOS floor instead of panicking the
        // builder (its year field is `year - 1980` in a u16).
        let dt = zip_datetime(UNIX_EPOCH, 0);
        assert_eq!((dt.year(), dt.month(), dt.day()), (1980, 1, 1));
        // ... even when the offset would push the wall clock below 1970.
        let dt = zip_datetime(UNIX_EPOCH, -7 * 3600);
        assert_eq!((dt.year(), dt.month(), dt.day()), (1980, 1, 1));
    }

    #[test]
    fn parse_utc_offset_handles_common_forms() {
        assert_eq!(parse_utc_offset("-0700"), -7 * 3600);
        assert_eq!(parse_utc_offset("+0200"), 2 * 3600);
        assert_eq!(parse_utc_offset("+05:30"), 5 * 3600 + 30 * 60);
        assert_eq!(parse_utc_offset("+0000"), 0);
        // Garbage (an empty or word answer) falls back to UTC.
        assert_eq!(parse_utc_offset(""), 0);
        assert_eq!(parse_utc_offset("UTC"), 0);
        assert_eq!(parse_utc_offset("+07"), 0);
    }
}
