//! Chunked, resumable uploads — the transport the web UI uses for every
//! OS-desktop drop and paste.
//!
//! Why not one request per file: a remote window reaches this daemon through
//! ONE ssh connection that also carries its terminals, chat sockets and the
//! app's tunnel health check. A single unpaced request body fills that
//! connection's send queue (4 MB on macOS) for the whole transfer, and
//! everything else waits behind it — measured at ~4 s per round trip on a
//! 1 MB/s uplink and ~16 s at 250 KB/s, past the health check's 2 s deadline
//! and the reconnect's 15 s "is this master wedged" bound, so the app tore
//! down the tunnel the upload was riding. And a drop at byte N cost all N.
//!
//! So the client sends the file in pieces and waits for each answer before
//! sending the next: no more than one piece is ever queued on the link (it
//! sizes them to about half a second), every answer is a byte count this
//! daemon holds, and after a drop it asks where the daemon stands
//! and goes on from there.
//!
//! ```text
//! POST   /uploads                  {session | dir, name, size} → {id, offset, chunk_max}
//! PUT    /uploads/{id}?offset=N    body = the next bytes        → {offset}
//! GET    /uploads/{id}                                          → {offset, size, done}
//! POST   /uploads/{id}/finish      {chain?}                     → {path, name, size}
//! DELETE /uploads/{id}
//! ```
//!
//! What keeps the landed file equal to the sent one:
//! - a piece is committed only at the offset the daemon is at (409 +
//!   `offset` otherwise), so a retried or replayed piece is never appended
//!   twice;
//! - a piece that stops arriving, breaks off or fails its `x-chunk-sha256`
//!   is cut back off the temp file before anything else is written;
//! - `finish` requires every declared byte, compares the client's digest of
//!   the pieces it sent (`chain`: each piece's SHA-256 folded into a running one, in
//!   order) with the daemon's own, and syncs the file before the rename, so
//!   a full disk or an exceeded quota is an error and never a short file.
//!
//! Memory is one network read per upload whatever the file size. The bytes
//! stream to the same hidden temp sibling as the one-request routes and are
//! published by the same rename.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::time::Instant;

use super::{
    dir_usage, human_bytes, mention_safe_name, publish_in_dir, publish_in_session,
    resolve_upload_dir, sanitize_name, session_known, session_refusal, settle, too_large,
    write_refusal, MAX_SESSION_UPLOAD_BYTES, MAX_SESSION_UPLOAD_FILES,
};
use crate::AppState;

/// The largest file a folder upload takes. It lands on the user's own
/// filesystem in bounded memory, piece by piece, so the ceiling is only a
/// guard against an accident; the free-space check at `begin` is the limit
/// that matters.
pub(crate) const MAX_UPLOAD_BYTES: u64 = 64 * 1024 * 1024 * 1024;
/// The largest single piece. The client sizes pieces to the link (about half
/// a second each) and never asks for more than half of this.
pub(crate) const MAX_CHUNK_BYTES: u64 = 16 * 1024 * 1024;
/// Uploads that may be open at once (the UI sends a drop's files one at a
/// time, so this is several windows' worth).
const MAX_OPEN_UPLOADS: usize = 32;
/// An upload nobody has touched for this long is abandoned: its temp file
/// goes. Long enough for a laptop to sleep through a meeting and resume.
const IDLE_EXPIRY: Duration = Duration::from_secs(30 * 60);
/// How long a finished upload's answer is kept, so a `finish` whose answer
/// was lost to a dropped link gets the same answer when it is asked again.
const FINISHED_KEEP: Duration = Duration::from_secs(10 * 60);
/// A piece whose bytes stop arriving for this long is given up (its partial
/// tail cut off), so a half-dead connection cannot hold the upload.
const CHUNK_IDLE: Duration = Duration::from_secs(60);
/// Sync the temp file at least this often, so a full disk or an exceeded
/// quota on a network filesystem surfaces while the upload runs and the
/// final sync is short.
const SYNC_EVERY_BYTES: u64 = 64 * 1024 * 1024;
/// Finished uploads' answers kept at once; past it they are all dropped (the
/// client forgets one itself as soon as it has the answer).
const MAX_FINISHED_KEPT: usize = 64;
const REAP_TICK: Duration = Duration::from_secs(60);

/// Every open upload, by id. Per process: a restarted daemon has none, and
/// its client starts the file again.
#[derive(Default)]
pub(crate) struct Uploads {
    registry: Mutex<Registry>,
    /// Test override for [`CHUNK_IDLE`], in milliseconds (0 = the default).
    chunk_idle_ms: AtomicU64,
}

#[derive(Default)]
struct Registry {
    entries: HashMap<String, Arc<Upload>>,
    /// Whether the reaper task is running; it stops when nothing is left.
    reaping: bool,
}

enum Dest {
    /// A session's landing pad (`uploads_root/<id>/`).
    Session(String),
    /// A folder the user chose, as the request named it (the git watcher is
    /// nudged with that spelling, like the other fs mutations).
    Dir(String),
}

struct Upload {
    dest: Dest,
    /// The canonical destination directory.
    dir: PathBuf,
    /// The sanitized (for a session: mention-safe) name to publish under.
    name: String,
    /// The file's whole length, declared at `begin`.
    size: u64,
    tmp: PathBuf,
    /// The record that lets the next daemon remove `tmp` if this one dies.
    marker: PathBuf,
    /// Bytes committed: what `GET` answers without waiting for the writer.
    committed: AtomicU64,
    /// Published: only its answer is kept, for a `finish` asked again.
    done: std::sync::atomic::AtomicBool,
    /// Bumped by every request that wants the writer's seat. The latest one
    /// wins: a piece still being read for a connection its client gave up on
    /// yields as soon as it sees a newer turn.
    turn: tokio::sync::watch::Sender<u64>,
    seat: tokio::sync::Mutex<Seat>,
    touched: Mutex<Instant>,
}

struct Seat {
    /// `None` once the upload finished or was abandoned.
    file: Option<tokio::fs::File>,
    offset: u64,
    /// A piece was being written when its request went away (its future can
    /// be dropped at any await), so the file may hold bytes past `offset`.
    dirty: bool,
    unsynced: u64,
    /// Every committed piece's SHA-256 folded in, in order (`fold_chain`).
    chain: [u8; 32],
    /// The answer `finish` gave, kept for a retry.
    outcome: Option<serde_json::Value>,
}

impl Upload {
    fn touch(&self) {
        *crate::lock(&self.touched) = Instant::now();
    }

    /// Claim the writer's seat: any piece in flight sees the new turn and
    /// yields, then this request gets the lock.
    async fn take_seat(&self) -> (u64, tokio::sync::MutexGuard<'_, Seat>) {
        let mut mine = 0;
        self.turn.send_modify(|turn| {
            *turn += 1;
            mine = *turn;
        });
        (mine, self.seat.lock().await)
    }
}

impl Uploads {
    fn get(&self, id: &str) -> Option<Arc<Upload>> {
        crate::lock(&self.registry).entries.get(id).cloned()
    }

    fn remove(&self, id: &str) {
        crate::lock(&self.registry).entries.remove(id);
    }

    fn chunk_idle(&self) -> Duration {
        match self.chunk_idle_ms.load(Ordering::Relaxed) {
            0 => CHUNK_IDLE,
            ms => Duration::from_millis(ms),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_chunk_idle(&self, idle: Duration) {
        self.chunk_idle_ms
            .store(idle.as_millis() as u64, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn open_count(&self) -> usize {
        crate::lock(&self.registry).entries.len()
    }

    /// Drop a session's open uploads: its landing pad is being pruned, temp
    /// files included, so only the records are left to forget.
    pub(crate) fn forget_session(&self, session_id: &str) {
        self.forget(|dest| matches!(dest, Dest::Session(id) if id == session_id));
    }

    /// Drop every session's open uploads (close-all / shutdown prune the
    /// whole landing pad). Folder uploads are the user's own and stay.
    pub(crate) fn forget_sessions(&self) {
        self.forget(|dest| matches!(dest, Dest::Session(_)));
    }

    fn forget(&self, gone: impl Fn(&Dest) -> bool) {
        let dropped: Vec<Arc<Upload>> = {
            let mut registry = crate::lock(&self.registry);
            let ids: Vec<String> = registry
                .entries
                .iter()
                .filter(|(_, upload)| gone(&upload.dest))
                .map(|(id, _)| id.clone())
                .collect();
            ids.iter()
                .filter_map(|id| registry.entries.remove(id))
                .collect()
        };
        if dropped.is_empty() {
            return;
        }
        tokio::task::spawn_blocking(move || {
            for upload in dropped {
                let _ = std::fs::remove_file(&upload.marker);
            }
        });
    }
}

fn refuse(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({"error": message.into()}))).into_response()
}

fn unknown_upload() -> Response {
    refuse(
        StatusCode::NOT_FOUND,
        "the host no longer has this upload (it was cancelled, it expired, or the daemon restarted)",
    )
}

/// Where the records of open uploads live (`<data>/uploads-open/<token>.json`):
/// beside, not inside, the landing pad the boot prune walks.
fn markers_dir(state: &AppState) -> PathBuf {
    state.uploads_root.with_file_name("uploads-open")
}

/// Bytes free to this user on the filesystem holding `dir` (blocking).
fn free_bytes(dir: &Path) -> Option<u64> {
    let st = rustix::fs::statvfs(dir).ok()?;
    Some(st.f_bavail.saturating_mul(st.f_frsize))
}

#[derive(Deserialize)]
pub(crate) struct Begin {
    /// The session whose landing pad takes the file …
    #[serde(default)]
    session: Option<String>,
    /// … or the absolute folder that does. Exactly one of the two.
    #[serde(default)]
    dir: Option<String>,
    /// Suggested filename; sanitized to a strict basename server-side.
    #[serde(default)]
    name: Option<String>,
    /// The file's whole length in bytes.
    size: u64,
}

/// POST /api/v1/uploads — open an upload and answer `{id, offset, chunk_max}`.
/// Everything that can refuse the file is decided here, before a byte of it
/// crosses the link: the name, the destination, the caps (413), the room
/// left on the destination filesystem (507), the number of open uploads
/// (429).
pub(crate) async fn begin(State(state): State<Arc<AppState>>, Json(req): Json<Begin>) -> Response {
    to_completion(begin_upload(state, req)).await
}

/// Run a handler's work as its own task, so a request that goes away
/// mid-way (the link dropped, the client timed out) cannot stop it between
/// two steps that belong together: registering an upload and opening its
/// file, renaming a file into place and recording that it was published.
/// A piece's own handler is not run this way: it must stop with its request.
async fn to_completion(
    work: impl std::future::Future<Output = Response> + Send + 'static,
) -> Response {
    tokio::spawn(work).await.unwrap_or_else(|err| {
        tracing::error!(%err, "an upload task failed");
        refuse(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the upload failed on the host",
        )
    })
}

async fn begin_upload(state: Arc<AppState>, req: Begin) -> Response {
    let Some(name) = sanitize_name(req.name.as_deref().unwrap_or("upload.bin")) else {
        return refuse(StatusCode::BAD_REQUEST, "invalid file name");
    };
    let (dest, dir, name) = match (req.session, req.dir) {
        (Some(id), None) => {
            if !session_known(&state, &id) {
                return refuse(StatusCode::NOT_FOUND, format!("unknown session {id}"));
            }
            let dir = state.uploads_root.join(&id);
            if let Err(err) = tokio::fs::create_dir_all(&dir).await {
                return super::internal(&dir, "failed to create uploads dir", &err.into());
            }
            let (existing, count) = dir_usage(&dir).await;
            // A refusal names the file as the user knows it, not as it lands.
            if let Some(refusal) = session_refusal(&name, Some(req.size), existing, count) {
                return refusal;
            }
            (Dest::Session(id), dir, mention_safe_name(&name))
        }
        (None, Some(raw)) => {
            if req.size > MAX_UPLOAD_BYTES {
                return too_large(format!(
                    "{name} is {}; an upload can be at most {}",
                    human_bytes(req.size),
                    human_bytes(MAX_UPLOAD_BYTES)
                ));
            }
            let dir = match resolve_upload_dir(&raw).await {
                Ok(dir) => dir,
                Err(refusal) => return refusal,
            };
            (Dest::Dir(raw), dir, name)
        }
        _ => {
            return refuse(
                StatusCode::BAD_REQUEST,
                "name exactly one destination: session or dir",
            )
        }
    };

    // A filesystem that cannot hold the file says so now. Best effort: a
    // quota is not visible here and surfaces at the next sync instead.
    let probe = dir.clone();
    let free = tokio::task::spawn_blocking(move || free_bytes(&probe))
        .await
        .ok()
        .flatten();
    if let Some(free) = free.filter(|free| *free < req.size) {
        return refuse(
            StatusCode::INSUFFICIENT_STORAGE,
            format!(
                "not enough space on the host: {name} is {} and {} has {} free",
                human_bytes(req.size),
                dir.display(),
                human_bytes(free)
            ),
        );
    }

    let id = chimaera_core::generate_token();
    let token = &id[..8];
    let tmp = dir.join(format!(".{name}.{token}.tmp"));
    let marker = markers_dir(&state).join(format!("{token}.json"));
    let upload = Arc::new(Upload {
        dest,
        dir,
        name,
        size: req.size,
        tmp: tmp.clone(),
        marker: marker.clone(),
        committed: AtomicU64::new(0),
        done: std::sync::atomic::AtomicBool::new(false),
        turn: tokio::sync::watch::Sender::new(0),
        seat: tokio::sync::Mutex::new(Seat {
            file: None,
            offset: 0,
            dirty: false,
            unsynced: 0,
            chain: [0; 32],
            outcome: None,
        }),
        touched: Mutex::new(Instant::now()),
    });
    // Hold the seat from before the upload is findable until its file is
    // open, so no request can see it without one.
    let mut seat = upload.seat.lock().await;
    let spawn_reaper = {
        let mut registry = crate::lock(&state.chunked_uploads.registry);
        // Finished uploads are only answers kept for a retry: they take no
        // slot, and go first when there are many of them.
        let finished = registry
            .entries
            .values()
            .filter(|upload| upload.done.load(Ordering::SeqCst))
            .count();
        if finished >= MAX_FINISHED_KEPT {
            registry
                .entries
                .retain(|_, upload| !upload.done.load(Ordering::SeqCst));
        }
        let open = registry
            .entries
            .values()
            .filter(|upload| !upload.done.load(Ordering::SeqCst))
            .count();
        if open >= MAX_OPEN_UPLOADS {
            return refuse(
                StatusCode::TOO_MANY_REQUESTS,
                "too many uploads are in progress on this host; try again when one finishes",
            );
        }
        registry.entries.insert(id.clone(), upload.clone());
        !std::mem::replace(&mut registry.reaping, true)
    };
    if spawn_reaper {
        tokio::spawn(reaper(state.clone()));
    }

    // The record first: a daemon killed between the two leaves a record of a
    // file that does not exist, never a file without a record.
    let record = json!({"tmp": tmp.to_string_lossy()}).to_string();
    let marker_path = marker.clone();
    let recorded = tokio::task::spawn_blocking(move || {
        crate::persist::atomic_write_json(&marker_path, record)
    })
    .await;
    if !matches!(recorded, Ok(Ok(()))) {
        tracing::warn!(marker = %marker.display(), "could not record an open upload; a daemon crash would leave its temp file");
    }
    match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .await
    {
        Ok(file) => seat.file = Some(file),
        Err(err) => {
            drop(seat);
            state.chunked_uploads.remove(&id);
            let _ = tokio::fs::remove_file(&marker).await;
            return super::internal(&tmp, "failed to open upload file", &err.into());
        }
    }
    Json(json!({"id": id, "offset": 0, "chunk_max": MAX_CHUNK_BYTES})).into_response()
}

/// GET /api/v1/uploads/{id} — where the daemon stands: `{offset, size, done}`.
/// Never waits for a piece in flight; `offset` counts committed bytes only.
pub(crate) async fn status(
    State(state): State<Arc<AppState>>,
    UrlPath(id): UrlPath<String>,
) -> Response {
    let Some(upload) = state.chunked_uploads.get(&id) else {
        return unknown_upload();
    };
    upload.touch();
    let offset = upload.committed.load(Ordering::SeqCst);
    Json(json!({
        "offset": offset,
        "size": upload.size,
        "done": upload.done.load(Ordering::SeqCst),
    }))
    .into_response()
}

#[derive(Deserialize)]
pub(crate) struct ChunkQuery {
    /// Where in the file this piece starts; it must be where the daemon is.
    offset: u64,
}

/// Why a piece was not committed. Every case leaves the temp file exactly
/// `offset` bytes long.
enum ChunkStop {
    /// The piece itself is at fault; the upload goes on.
    Refused(Response),
    /// The filesystem failed; the upload is over.
    Fatal(Response),
}

/// PUT /api/v1/uploads/{id}?offset=N — append the next piece and answer the
/// new `{offset}`. 409 with the daemon's `offset` when N is not where it
/// stands (a replayed piece, or a client that lost an answer) or when a
/// newer request took over; 422 when the piece does not match its
/// `x-chunk-sha256`; 408 when its bytes stop arriving; 413 past the piece or
/// file size. Each of those leaves the upload open at its old offset. A
/// write or sync that fails ends the upload (507 for a full disk or quota).
pub(crate) async fn chunk(
    State(state): State<Arc<AppState>>,
    UrlPath(id): UrlPath<String>,
    Query(query): Query<ChunkQuery>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let Some(upload) = state.chunked_uploads.get(&id) else {
        return unknown_upload();
    };
    upload.touch();
    let declared = headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|len| len > MAX_CHUNK_BYTES) {
        return too_large(format!(
            "an upload piece can be at most {}",
            human_bytes(MAX_CHUNK_BYTES)
        ));
    }
    let expected = match headers.get("x-chunk-sha256") {
        None => None,
        Some(value) => match value.to_str().ok().and_then(parse_sha256) {
            Some(digest) => Some(digest),
            None => return refuse(StatusCode::BAD_REQUEST, "x-chunk-sha256 is not a SHA-256"),
        },
    };

    let (mine, mut seat) = upload.take_seat().await;
    let outcome = write_chunk(
        &state,
        &upload,
        &mut seat,
        mine,
        query.offset,
        expected,
        body,
    )
    .await;
    upload.touch();
    match outcome {
        Ok(offset) => Json(json!({"offset": offset})).into_response(),
        Err(ChunkStop::Refused(response)) => response,
        Err(ChunkStop::Fatal(response)) => {
            abandon(&state, &id, &upload, &mut seat).await;
            response
        }
    }
}

#[allow(
    clippy::result_large_err,
    reason = "the Err is the HTTP response the handler returns as it is"
)]
async fn write_chunk(
    state: &AppState,
    upload: &Upload,
    seat: &mut Seat,
    mine: u64,
    offset: u64,
    expected: Option<[u8; 32]>,
    body: Body,
) -> Result<u64, ChunkStop> {
    let fatal =
        |upload: &Upload, err: &std::io::Error| ChunkStop::Fatal(write_refusal(&upload.tmp, err));
    if seat.outcome.is_some() {
        return Err(ChunkStop::Refused(refuse(
            StatusCode::CONFLICT,
            "this upload has already finished",
        )));
    }
    let start = seat.offset;
    let Some(file) = seat.file.as_mut() else {
        return Err(ChunkStop::Refused(unknown_upload()));
    };
    // An earlier piece's request may have vanished mid-write; whatever it
    // left past the committed offset goes before anything is added.
    if seat.dirty {
        rewind(file, start)
            .await
            .map_err(|err| fatal(upload, &err))?;
        seat.dirty = false;
    }
    if offset != start {
        return Err(ChunkStop::Refused(offset_mismatch(start)));
    }
    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(|err| fatal(upload, &err))?;

    let idle = state.chunked_uploads.chunk_idle();
    let mut turn = upload.turn.subscribe();
    let mut hasher = Sha256::new();
    let mut received: u64 = 0;
    let mut stream = body.into_data_stream();
    seat.dirty = true;
    // `Some(refusal)` once the piece has to be cut back off.
    let stopped: Option<Response> = loop {
        let next = tokio::select! {
            biased;
            () = async {
                // The borrow `wait_for` answers with is dropped here, inside
                // the branch, so it never lives across an await.
                let _ = turn.wait_for(|turn| *turn != mine).await;
            } => {
                break Some(refuse(
                    StatusCode::CONFLICT,
                    "a newer request took over this upload",
                ));
            }
            next = tokio::time::timeout(idle, stream.next()) => next,
        };
        let bytes = match next {
            Err(_) => {
                break Some(refuse(
                    StatusCode::REQUEST_TIMEOUT,
                    "the upload stopped arriving",
                ))
            }
            Ok(None) => break None,
            Ok(Some(Err(err))) => {
                break Some(refuse(
                    StatusCode::BAD_REQUEST,
                    format!("upload stream failed: {err}"),
                ))
            }
            Ok(Some(Ok(bytes))) => bytes,
        };
        received += bytes.len() as u64;
        if received > MAX_CHUNK_BYTES {
            break Some(too_large(format!(
                "an upload piece can be at most {}",
                human_bytes(MAX_CHUNK_BYTES)
            )));
        }
        if start + received > upload.size {
            break Some(too_large(format!(
                "more bytes arrived than the {} this upload declared",
                human_bytes(upload.size)
            )));
        }
        hasher.update(&bytes);
        if let Err(err) = file.write_all(&bytes).await {
            return Err(fatal(upload, &err));
        }
    };
    let digest: [u8; 32] = hasher.finalize().into();
    let stopped = stopped.or_else(|| {
        (expected.is_some_and(|expected| expected != digest)).then(|| {
            refuse(
                StatusCode::UNPROCESSABLE_ENTITY,
                "a piece of the upload arrived changed (its checksum does not match)",
            )
        })
    });
    if let Some(refusal) = stopped {
        rewind(file, start)
            .await
            .map_err(|err| fatal(upload, &err))?;
        seat.dirty = false;
        return Err(ChunkStop::Refused(refusal));
    }

    // Commit. No await below yields the seat to another request, and a
    // request dropped here leaves `dirty` set, so its bytes are cut back.
    file.flush().await.map_err(|err| fatal(upload, &err))?;
    seat.unsynced += received;
    if seat.unsynced >= SYNC_EVERY_BYTES {
        file.sync_data().await.map_err(|err| fatal(upload, &err))?;
        seat.unsynced = 0;
    }
    seat.offset = start + received;
    seat.chain = fold_chain(&seat.chain, &digest);
    seat.dirty = false;
    upload.committed.store(seat.offset, Ordering::SeqCst);
    Ok(seat.offset)
}

/// Cut the temp file back to its committed length.
async fn rewind(file: &mut tokio::fs::File, offset: u64) -> std::io::Result<()> {
    file.flush().await?;
    file.set_len(offset).await?;
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    Ok(())
}

fn offset_mismatch(offset: u64) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({
            "error": "the upload is not at that offset",
            "offset": offset,
        })),
    )
        .into_response()
}

/// One step of the running digest both ends keep: `SHA-256(chain ‖ piece)`,
/// from 32 zero bytes. A chain, not one hash over the file, because a
/// browser's WebCrypto can only hash a whole buffer at once; this way each
/// end hashes one piece and 64 bytes at a time, and the two results agree
/// only if the same pieces were committed in the same order.
fn fold_chain(chain: &[u8; 32], piece: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(chain);
    hasher.update(piece);
    hasher.finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn parse_sha256(hex: &str) -> Option<[u8; 32]> {
    let (pairs, rest) = hex.trim().as_bytes().as_chunks::<2>();
    if pairs.len() != 32 || !rest.is_empty() {
        return None;
    }
    let mut out = [0u8; 32];
    for (byte, pair) in out.iter_mut().zip(pairs) {
        let pair = std::str::from_utf8(pair).ok()?;
        *byte = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(out)
}

#[derive(Deserialize, Default)]
pub(crate) struct Finish {
    /// The client's running digest (hex) of every piece it sent, in order
    /// (`fold_chain`). Absent from a client that cannot hash (no WebCrypto).
    #[serde(default)]
    chain: Option<String>,
}

/// POST /api/v1/uploads/{id}/finish — verify and publish; answers the same
/// `{path, name, size}` as the one-request routes. 409 with `offset` while
/// bytes are missing (the upload stays open). A `chain` that differs from
/// the daemon's own (422), a failed sync (507/500) or an overrun session
/// quota (413) ends the upload and removes its temp file. Asked again after
/// it succeeded, it gives the same answer.
pub(crate) async fn finish(
    State(state): State<Arc<AppState>>,
    UrlPath(id): UrlPath<String>,
    body: Option<Json<Finish>>,
) -> Response {
    let chain = body.and_then(|Json(finish)| finish.chain);
    to_completion(finish_upload(state, id, chain)).await
}

async fn finish_upload(state: Arc<AppState>, id: String, chain: Option<String>) -> Response {
    let Some(upload) = state.chunked_uploads.get(&id) else {
        return unknown_upload();
    };
    upload.touch();
    let (_, mut guard) = upload.take_seat().await;
    let seat = &mut *guard;
    if let Some(outcome) = &seat.outcome {
        return Json(outcome.clone()).into_response();
    }
    let start = seat.offset;
    let Some(file) = seat.file.as_mut() else {
        return unknown_upload();
    };
    if seat.dirty {
        if let Err(err) = rewind(file, start).await {
            abandon(&state, &id, &upload, seat).await;
            return write_refusal(&upload.tmp, &err);
        }
        seat.dirty = false;
    }
    if start != upload.size {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": format!(
                    "the upload is incomplete: the host has {} of {}",
                    human_bytes(start),
                    human_bytes(upload.size)
                ),
                "offset": start,
            })),
        )
            .into_response();
    }
    let ours = hex(&seat.chain);
    if chain.is_some_and(|theirs| !theirs.trim().eq_ignore_ascii_case(&ours)) {
        abandon(&state, &id, &upload, seat).await;
        return refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            "the upload did not verify: what the host received differs from what was sent",
        );
    }
    // The sync is where a network filesystem reports a full disk or an
    // exceeded quota for bytes it had only cached; the length check is what
    // makes "published" mean "all of it". The handle stays in the seat until
    // both pass, so a `finish` whose request went away can be asked again.
    let settled = match settle(file).await {
        Ok(()) => file.metadata().await.map(|meta| meta.len()),
        Err(err) => Err(err),
    };
    match settled {
        Ok(len) if len == upload.size => {}
        Ok(len) => {
            abandon(&state, &id, &upload, seat).await;
            return refuse(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "the host kept {} of {}; the upload was discarded",
                    human_bytes(len),
                    human_bytes(upload.size)
                ),
            );
        }
        Err(err) => {
            abandon(&state, &id, &upload, seat).await;
            return write_refusal(&upload.tmp, &err);
        }
    }

    if let Dest::Session(session) = &upload.dest {
        // Re-check with every in-flight temp file included: two uploads can
        // both pass `begin`, and the later finish sees their sum.
        let (bytes, files) = dir_usage(&upload.dir).await;
        if !session_known(&state, session) {
            abandon(&state, &id, &upload, seat).await;
            return refuse(StatusCode::NOT_FOUND, format!("unknown session {session}"));
        }
        if bytes > MAX_SESSION_UPLOAD_BYTES || files > MAX_SESSION_UPLOAD_FILES {
            abandon(&state, &id, &upload, seat).await;
            return too_large("session upload quota reached during concurrent uploads".to_string());
        }
    }
    seat.file = None;
    let published = match &upload.dest {
        Dest::Session(_) => publish_in_session(&upload.dir, &upload.tmp, &upload.name).await,
        Dest::Dir(_) => publish_in_dir(&upload.dir, &upload.tmp, &upload.name).await,
    };
    let (target, final_name) = match published {
        Ok(published) => published,
        Err(refusal) => {
            abandon(&state, &id, &upload, seat).await;
            return refusal;
        }
    };
    // Recorded before anything else awaits: the file is published, and a
    // retried `finish` must get this answer rather than a failed rename.
    let outcome = json!({
        "path": target.to_string_lossy(),
        "name": final_name,
        "size": upload.size,
    });
    seat.outcome = Some(outcome.clone());
    upload.done.store(true, Ordering::SeqCst);
    upload.touch();
    let _ = tokio::fs::remove_file(&upload.marker).await;
    if let Dest::Dir(raw) = &upload.dest {
        // Nudge the git watcher so the tree/panel refetch without polling.
        crate::git::mark_path_dirty(&state, raw).await;
    }
    Json(outcome).into_response()
}

/// DELETE /api/v1/uploads/{id} — cancel: the temp file goes. A finished
/// upload is only forgotten (its file is the user's now); an unknown id is
/// already gone. Always 204.
pub(crate) async fn cancel(
    State(state): State<Arc<AppState>>,
    UrlPath(id): UrlPath<String>,
) -> Response {
    to_completion(async move {
        if let Some(upload) = state.chunked_uploads.get(&id) {
            let (_, mut seat) = upload.take_seat().await;
            if seat.outcome.is_some() {
                state.chunked_uploads.remove(&id);
            } else {
                abandon(&state, &id, &upload, &mut seat).await;
            }
        }
        StatusCode::NO_CONTENT.into_response()
    })
    .await
}

/// End an upload that will not be published: close and remove its temp
/// file, its record, and its registry entry. Caller holds the seat.
async fn abandon(state: &AppState, id: &str, upload: &Upload, seat: &mut Seat) {
    // The entry goes last: if this is cut short, the upload is still listed
    // (with no file, so every request gets 404) and the reaper finishes the
    // job, instead of a temp file nothing points at.
    seat.file = None;
    let _ = tokio::fs::remove_file(&upload.tmp).await;
    let _ = tokio::fs::remove_file(&upload.marker).await;
    state.chunked_uploads.remove(id);
}

/// Abandon uploads nobody touched for [`IDLE_EXPIRY`] and forget finished
/// ones after [`FINISHED_KEEP`]. Runs only while uploads are open.
async fn reaper(state: Arc<AppState>) {
    loop {
        tokio::time::sleep(REAP_TICK).await;
        let entries: Vec<(String, Arc<Upload>)> = {
            let mut registry = crate::lock(&state.chunked_uploads.registry);
            if registry.entries.is_empty() {
                registry.reaping = false;
                return;
            }
            registry
                .entries
                .iter()
                .map(|(id, upload)| (id.clone(), upload.clone()))
                .collect()
        };
        for (id, upload) in entries {
            let idle = crate::lock(&upload.touched).elapsed();
            // A seat that is taken is an upload at work.
            let Ok(mut seat) = upload.seat.try_lock() else {
                continue;
            };
            if seat.outcome.is_some() {
                if idle >= FINISHED_KEEP {
                    state.chunked_uploads.remove(&id);
                }
            } else if idle >= IDLE_EXPIRY {
                tracing::info!(tmp = %upload.tmp.display(), "abandoning an idle upload");
                abandon(&state, &id, &upload, &mut seat).await;
            }
        }
    }
}

/// Boot-time sweep: remove the temp files of uploads that were open when the
/// previous daemon stopped — a restart, a crash, a job that hit its time
/// limit. Their records name them; nothing is found by guessing at names in
/// the user's folders. Detached so it never delays boot.
pub(crate) fn spawn_boot_sweep(state: Arc<AppState>) {
    let dir = markers_dir(&state);
    tokio::task::spawn_blocking(move || {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        for entry in entries.flatten() {
            let marker = entry.path();
            let token = marker
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".json"));
            let Some(token) = token else {
                // A record write the last daemon never finished.
                let _ = std::fs::remove_file(&marker);
                continue;
            };
            // An upload this daemon opened since boot is not a leftover.
            let live = crate::lock(&state.chunked_uploads.registry)
                .entries
                .keys()
                .any(|id| id.starts_with(token));
            if live {
                continue;
            }
            if let Some(tmp) = recorded_tmp(&marker, token) {
                match std::fs::remove_file(&tmp) {
                    Ok(()) => {
                        tracing::info!(tmp = %tmp.display(), "removed an interrupted upload's temp file")
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => {
                        // Keep the record: a filesystem that is away now
                        // (an unmounted scratch dir) gets another try.
                        tracing::warn!(tmp = %tmp.display(), %err, "could not remove an interrupted upload's temp file");
                        continue;
                    }
                }
            }
            let _ = std::fs::remove_file(&marker);
        }
    });
}

/// The temp file a record names — only when it has the shape `begin` gives
/// one (`.<name>.<token>.tmp`, absolute), so a damaged record can never
/// point the sweep at anything else.
fn recorded_tmp(marker: &Path, token: &str) -> Option<PathBuf> {
    let crate::persist::Bounded::Ok(bytes) = crate::persist::read_bounded(marker, 16 * 1024) else {
        return None;
    };
    let record: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let tmp = PathBuf::from(record.get("tmp")?.as_str()?);
    let name = tmp.file_name()?.to_str()?;
    (tmp.is_absolute() && name.starts_with('.') && name.ends_with(&format!(".{token}.tmp")))
        .then_some(tmp)
}
