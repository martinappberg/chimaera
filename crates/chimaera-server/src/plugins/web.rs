//! Web views (docs/design/rich-views-plan.md, Part 2): a plugin's own pages,
//! the libraries it lends agents' pages, and the data either kind of page
//! reads.
//!
//! - **The bundle.** A 0.3 plugin's `web-bundle()` export hands over a tar of
//!   its pages, which the host unpacks once per build into
//!   `<output root>/.web/<id>-<sha>/` (reconstructible: it is the wasm's
//!   own bytes) with the tools' safe unpacker, files and folders only.
//! - **Grants.** `POST /workspaces/{id}/plugins/{pid}/web {entry}` (bearer)
//!   checks the plugin is on and the entry is one of its `[web] pages`, and
//!   answers `/web/{grant}/{entry}`. The grant ticket is the authority for
//!   everything under it: the bundle's files (`ACAO *`: they are the
//!   plugin's own bytes, and a module script from a page with no origin is a
//!   CORS fetch), `~url` (a data URL within the plugin's `[access]`) and
//!   `~save` (into its output folder). Every request checks again that the
//!   plugin is on there and still this build.
//! - **Data.** `/webdata/{ticket}` serves one file a grant's `~url` named;
//!   `/webdata/{page}/{*rest}` a file in an agent page's own folder or
//!   below. Both send `ACAO *`, which `/raw/{t}/{rest}` never does: the
//!   page that may read is one whose CSP lets it send nowhere else.
//! - **Libraries.** `/lib/{page}/{name}/{*file}` serves an active plugin's
//!   library to an agent's page in a workspace where that plugin is on;
//!   `/lib/{page}/~css` is `chimaera-web.css`. The daemon rewrites a page's
//!   `chimaera:lib/` and `chimaera:web.css` to these when it serves it
//!   ([`agent_page`]).
//! - **The CSPs.** An agent's page reads files beside it, loads libraries
//!   from active plugins and the well-known library CDNs, and connects only
//!   to the daemon, plus the declared hosts of a plugin library it imports.
//!   A plugin's page loads only from the daemon and connects to the daemon
//!   and its own declared hosts. Both keep `sandbox allow-scripts` (no
//!   origin), `'unsafe-eval'` and `'wasm-unsafe-eval'` (a page with no
//!   origin and no network gains nothing from eval; GenomeSpy compiles
//!   expressions and inflates in wasm).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path as UrlPath, Query, RawQuery, State};
use axum::http::header::{self, HeaderName, HeaderValue};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use super::capabilities::{Access, FilesAccess};
use super::Manifest;
use crate::fs::{Caching, PageTail, Serve};
use crate::AppState;

/// Pages, libraries and hosts one plugin may declare.
pub(crate) const PAGES_MAX: usize = 16;
pub(crate) const LIBRARIES_MAX: usize = 8;
pub(crate) const HOSTS_MAX: usize = 16;
/// An unpacked bundle: GenomeSpy's whole `dist/bundle` is ~3 MB, and the
/// component holding it is capped at 16 MiB.
const BUNDLE_MAX: u64 = 64 << 20;
const BUNDLE_ENTRIES_MAX: usize = 4096;
/// A grant or data ticket lives this long from its last mint; a page that
/// is shown asks again before then.
const TICKET_TTL: Duration = Duration::from_secs(10 * 60);
const TICKETS_MAX: usize = 4096;
/// An agent page's data key: as long as an agent's file page (a person
/// may work in a page all day); each load of the page renews it.
const KEY_TTL: Duration = Duration::from_secs(12 * 60 * 60);
/// How long "this plugin is on there" is trusted between requests: a
/// genome view's range reads come in bursts.
const ACTIVE_FOR: Duration = Duration::from_secs(2);
/// A page is read whole to rewrite its `chimaera:` addresses only up to
/// this size; a bigger one is served as it is.
pub(crate) const REWRITE_MAX: u64 = 4 << 20;
/// A `web` node's `data`, and one `~save`.
pub(crate) const DATA_MAX: usize = 64 * 1024;
pub(crate) const SAVE_MAX: usize = 8 << 20;

/// The library CDNs an agent's page may load scripts, styles and fonts
/// from: the three general ones, and Plotly's and Bokeh's own, which their
/// exports name. One fixed list: there is nothing to configure.
pub(crate) const LIBRARY_CDNS: &[&str] = &[
    "https://cdnjs.cloudflare.com",
    "https://cdn.jsdelivr.net",
    "https://unpkg.com",
    "https://cdn.plot.ly",
    "https://cdn.bokeh.org",
];

/// `[web]` (0.3).
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct WebDecl {
    /// Pages in the bundle a `web` node may show (bundle-relative `.html`).
    #[serde(default)]
    pub(crate) pages: Vec<String>,
    /// Top-level folders of the bundle lent to agents' pages, served at
    /// `chimaera:lib/<name>/…` where the plugin is on.
    #[serde(default)]
    pub(crate) libraries: Vec<String>,
    /// Hosts its pages (and agents' pages importing its libraries) may
    /// reach, over https.
    #[serde(default)]
    pub(crate) network: Vec<String>,
}

fn plain_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 200
        && p.split('/').all(|part| {
            !part.is_empty()
                && !part.starts_with('.')
                && !part.starts_with('~')
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
}

fn library_name(n: &str) -> bool {
    (1..=40).contains(&n.len())
        && n.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A bare public hostname: lowercase labels, a dot, no port, no address,
/// not localhost.
pub(crate) fn host_name(h: &str) -> bool {
    let labels: Vec<&str> = h.split('.').collect();
    h.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|l| {
            (1..=63).contains(&l.len())
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
        && !labels
            .last()
            .is_some_and(|l| l.bytes().all(|b| b.is_ascii_digit()))
        && !matches!(labels.last(), Some(&"localhost"))
}

/// `[web]`'s own consistency (`plugins::validate`).
pub(crate) fn validate(d: &WebDecl) -> Result<(), String> {
    if d.pages.len() > PAGES_MAX {
        return Err(format!("[web] pages lists at most {PAGES_MAX} pages"));
    }
    let mut seen = BTreeSet::new();
    for page in &d.pages {
        let html = page.ends_with(".html") || page.ends_with(".htm");
        if !plain_path(page) || !html {
            return Err(format!(
                "[web] page {page:?} must be a .html file in the bundle: letters, digits, \
                 `._-` and `/`, no `..`, nothing hidden"
            ));
        }
        if !seen.insert(page) {
            return Err(format!("[web] page {page:?} is listed twice"));
        }
    }
    if d.libraries.len() > LIBRARIES_MAX {
        return Err(format!(
            "[web] libraries lists at most {LIBRARIES_MAX} names"
        ));
    }
    let mut seen = BTreeSet::new();
    for name in &d.libraries {
        if !library_name(name) {
            return Err(format!(
                "[web] library {name:?} must be 1–40 lowercase letters, digits and dashes \
                 (a top-level folder of the bundle)"
            ));
        }
        if !seen.insert(name) {
            return Err(format!("[web] library {name:?} is listed twice"));
        }
    }
    if d.network.len() > HOSTS_MAX {
        return Err(format!("[web] network lists at most {HOSTS_MAX} hosts"));
    }
    let mut seen = BTreeSet::new();
    for host in &d.network {
        if !host_name(host) {
            return Err(format!(
                "[web] network host {host:?} must be a bare public hostname \
                 (no scheme, port, path, address or wildcard)"
            ));
        }
        if !seen.insert(host) {
            return Err(format!("[web] network host {host:?} is listed twice"));
        }
    }
    Ok(())
}

/// The `Content-Security-Policy` of an agent's page: one fixed rule
/// (`hosts`: the declared hosts of plugin libraries it imports).
pub(crate) fn agent_page_csp(hosts: &[String]) -> String {
    page_csp(LIBRARY_CDNS, hosts)
}

/// The `Content-Security-Policy` of a plugin's own page.
pub(crate) fn plugin_page_csp(hosts: &[String]) -> String {
    page_csp(&[], hosts)
}

fn page_csp(cdns: &[&str], hosts: &[String]) -> String {
    let cdns: String = cdns.iter().map(|c| format!(" {c}")).collect();
    let hosts: String = hosts.iter().map(|h| format!(" https://{h}")).collect();
    format!(
        "sandbox allow-scripts; default-src 'none'; \
         script-src 'self' 'unsafe-inline' 'unsafe-eval' 'wasm-unsafe-eval' blob: data:{cdns}; \
         style-src 'self' 'unsafe-inline'{cdns}; font-src 'self' data:{cdns}; \
         img-src 'self' data: blob:{hosts}; media-src 'self' data: blob:; \
         connect-src 'self'{hosts}; worker-src 'self' blob:; frame-src 'self'; \
         form-action 'none'; base-uri 'self'"
    )
}

fn header_value(s: &str) -> HeaderValue {
    HeaderValue::from_str(s).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// The headers a page served under `csp` carries on every status.
pub(crate) fn page_security(csp: &str) -> Vec<(HeaderName, HeaderValue)> {
    vec![
        (
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ),
        (header::CONTENT_SECURITY_POLICY, header_value(csp)),
        (
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ),
    ]
}

/// What every response a page with no origin may read carries.
fn cors() -> Vec<(HeaderName, HeaderValue)> {
    vec![
        (
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        ),
        (
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            HeaderValue::from_static("Content-Range, Content-Length, Accept-Ranges, ETag"),
        ),
    ]
}

fn with_cors(mut response: Response) -> Response {
    response.headers_mut().extend(cors());
    response
}

fn refused(status: StatusCode, why: &str) -> Response {
    with_cors((status, Json(json!({"error": why}))).into_response())
}

/// `OPTIONS` on the web routes: a page's range reads and its saves are
/// preflighted (a `Range` past one span, a typed body).
pub(crate) async fn preflight() -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    let h = response.headers_mut();
    h.extend(cors());
    h.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    h.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("range, content-type, if-none-match"),
    );
    h.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("600"),
    );
    response
}

/// Short-lived tickets, each standing for one `T`, re-minted to the same
/// ticket while its key is alive.
struct Tickets<T> {
    map: HashMap<String, (T, String, Instant)>,
    by_key: HashMap<String, String>,
}

impl<T> Default for Tickets<T> {
    fn default() -> Self {
        Tickets {
            map: HashMap::new(),
            by_key: HashMap::new(),
        }
    }
}

impl<T: Clone> Tickets<T> {
    fn purge(&mut self) {
        let now = Instant::now();
        let by_key = &mut self.by_key;
        self.map.retain(|_, (_, key, expires)| {
            let alive = *expires > now;
            if !alive {
                by_key.remove(key);
            }
            alive
        });
    }

    fn mint(&mut self, prefix: &str, key: String, value: T, ttl: Duration) -> String {
        self.purge();
        let expires = Instant::now() + ttl;
        if let Some(ticket) = self.by_key.get(&key) {
            if let Some(entry) = self.map.get_mut(ticket) {
                entry.0 = value;
                entry.2 = entry.2.max(expires);
                return ticket.clone();
            }
        }
        if self.map.len() >= TICKETS_MAX {
            if let Some(oldest) = self
                .map
                .iter()
                .min_by_key(|(_, (_, _, e))| *e)
                .map(|(t, _)| t.clone())
            {
                if let Some((_, key, _)) = self.map.remove(&oldest) {
                    self.by_key.remove(&key);
                }
            }
        }
        let ticket = format!("{prefix}-{}", &chimaera_core::generate_token()[..32]);
        self.by_key.insert(key.clone(), ticket.clone());
        self.map.insert(ticket.clone(), (value, key, expires));
        ticket
    }

    fn get(&mut self, ticket: &str) -> Option<T> {
        self.purge();
        self.map.get(ticket).map(|(v, _, _)| v.clone())
    }
}

/// A page grant: a plugin's page in one workspace, from one build.
#[derive(Clone)]
struct Grant {
    plugin: String,
    ws: String,
    sha: Arc<str>,
    dir: PathBuf,
}

/// A data ticket: one file, opened beneath `root` without following links.
#[derive(Clone)]
struct DataFile {
    plugin: String,
    ws: String,
    root: PathBuf,
    rel: PathBuf,
}

/// The web views' state (`Platform::web`).
#[derive(Default)]
pub(crate) struct Web {
    grants: Mutex<Tickets<Grant>>,
    data: Mutex<Tickets<DataFile>>,
    /// An agent page's data key → the page (keyed by its `/raw` ticket).
    keys: Mutex<Tickets<PathBuf>>,
    /// One unpack at a time per build.
    unpacking: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// (plugin, workspace) → whether it was on there, and when that was read.
    active: Mutex<HashMap<(String, String), (bool, Instant)>>,
}

/// Where `m`'s bundle is unpacked.
fn bundle_dir(state: &AppState, m: &Manifest) -> PathBuf {
    let sha: String = m.wasm.sha256.chars().take(16).collect();
    state
        .plugin_platform
        .output_root
        .join(".web")
        .join(format!("{}-{sha}", m.id))
}

/// `m`'s web bundle, unpacked (asked of the plugin in `ws` the first time).
pub(crate) async fn bundle(
    state: &Arc<AppState>,
    m: &Manifest,
    ws: &str,
) -> Result<PathBuf, String> {
    let dir = bundle_dir(state, m);
    let gate = crate::lock(&state.plugin_platform.web.unpacking)
        .entry(dir.to_string_lossy().into_owned())
        .or_default()
        .clone();
    let _one = gate.lock().await;
    let complete = dir.join(".complete");
    if tokio::fs::metadata(&complete).await.is_ok() {
        return Ok(dir);
    }
    let bytes = state.plugin_runtime.web_bundle(state, m, ws).await?;
    if bytes.is_empty() {
        return Err(format!("{} has no web pages", m.name));
    }
    let id = m.id.clone();
    let name = m.name.clone();
    tokio::task::spawn_blocking(move || unpack(&dir, &id, &name, &bytes))
        .await
        .map_err(|e| format!("unpacking {}'s pages failed: {e}", m.name))?
}

/// Unpack `bytes` into `dir` through a staging folder, then drop the
/// plugin's other builds' bundles (blocking).
fn unpack(dir: &Path, id: &str, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let web_root = dir.parent().unwrap_or(dir);
    std::fs::create_dir_all(web_root).map_err(|e| format!("{}: {e}", web_root.display()))?;
    let staging = web_root.join(format!(
        ".tmp-{id}-{}",
        &chimaera_core::generate_token()[..12]
    ));
    super::toolchain::unpack_tar_bytes(bytes, &staging, BUNDLE_MAX, BUNDLE_ENTRIES_MAX)
        .map_err(|e| format!("{name}'s web bundle: {e}"))?;
    let done = std::fs::write(staging.join(".complete"), b"")
        .map_err(|e| e.to_string())
        .and_then(|()| {
            let _ = std::fs::remove_dir_all(dir);
            std::fs::rename(&staging, dir).map_err(|e| e.to_string())
        });
    if let Err(err) = done {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("{name}'s web bundle: {err}"));
    }
    let own = dir.file_name().map(|n| n.to_os_string());
    if let Ok(entries) = std::fs::read_dir(web_root) {
        for entry in entries.flatten() {
            let n = entry.file_name();
            let other =
                n.to_string_lossy().starts_with(&format!("{id}-")) && Some(&n) != own.as_ref();
            if other {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    tracing::info!(plugin = %id, bytes = bytes.len(), "plugin web bundle unpacked");
    Ok(dir.to_path_buf())
}

/// `plugin` was switched on or off somewhere: what was read of it goes.
pub(crate) fn forget(state: &AppState, plugin: &str) {
    crate::lock(&state.plugin_platform.web.active).retain(|(p, _), _| p != plugin);
}

/// Whether `plugin` is on in `ws` (read at most every [`ACTIVE_FOR`]).
async fn on_in(state: &AppState, plugin: &str, ws: &str) -> bool {
    let key = (plugin.to_string(), ws.to_string());
    if let Some((on, at)) = crate::lock(&state.plugin_platform.web.active).get(&key) {
        if at.elapsed() < ACTIVE_FOR {
            return *on;
        }
    }
    let on = super::active(state, ws)
        .await
        .iter()
        .any(|m| m.id == plugin);
    let mut cache = crate::lock(&state.plugin_platform.web.active);
    if cache.len() > TICKETS_MAX {
        cache.clear();
    }
    cache.insert(key, (on, Instant::now()));
    on
}

#[derive(Deserialize)]
pub(crate) struct MintBody {
    entry: String,
}

/// POST /api/v1/workspaces/{id}/plugins/{pid}/web {entry} — a page of the
/// plugin's bundle for a `web` node: `{url}`, under a grant ticket.
pub(crate) async fn mint_route(
    State(state): State<Arc<AppState>>,
    UrlPath((ws, pid)): UrlPath<(String, String)>,
    Json(body): Json<MintBody>,
) -> Response {
    let m = match super::screens::active_plugin(&state, &ws, &pid).await {
        Ok(m) => m,
        Err(response) => return response,
    };
    let Some(decl) = m.web.as_ref().filter(|d| d.pages.contains(&body.entry)) else {
        return super::not_found(&format!("{}'s page {}", m.name, body.entry));
    };
    let _ = decl;
    let dir = match bundle(&state, &m, &ws).await {
        Ok(dir) => dir,
        Err(err) => {
            tracing::warn!(plugin = %m.id, %err, "plugin web bundle");
            return (StatusCode::BAD_GATEWAY, Json(json!({"error": err}))).into_response();
        }
    };
    let grant = Grant {
        plugin: m.id.clone(),
        ws: ws.clone(),
        sha: m.wasm.sha256.clone(),
        dir,
    };
    let key = format!("{}\n{}\n{}", m.id, ws, m.wasm.sha256);
    let ticket = crate::lock(&state.plugin_platform.web.grants).mint("w", key, grant, TICKET_TTL);
    // Still on: it was a moment ago, and a page's first loads follow.
    crate::lock(&state.plugin_platform.web.active)
        .insert((m.id.clone(), ws.clone()), (true, Instant::now()));
    Json(json!({"url": format!("/web/{ticket}/{}", body.entry)})).into_response()
}

/// A live grant whose plugin is still on in its workspace and still runs
/// the build the grant was made for.
async fn grant(state: &AppState, ticket: &str) -> Option<(Grant, Arc<Manifest>)> {
    let g = crate::lock(&state.plugin_platform.web.grants).get(ticket)?;
    let m = super::manifest(state, &g.plugin).filter(|m| m.wasm.sha256 == g.sha)?;
    on_in(state, &g.plugin, &g.ws).await.then_some((g, m))
}

fn not_found() -> Response {
    with_cors(crate::fs::raw_not_found())
}

async fn open(dir: PathBuf, rel: PathBuf) -> Option<(tokio::fs::File, std::fs::Metadata)> {
    let permit = crate::fs::FILESYSTEM_WORK.acquire().await.ok()?;
    let opened = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        crate::fs::open_asset(&dir, &rel)
    })
    .await;
    match opened {
        Ok(Ok((file, meta))) => Some((tokio::fs::File::from_std(file), meta)),
        _ => None,
    }
}

pub(crate) fn is_html(name: &Path) -> bool {
    name.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        ["html", "htm", "xhtml"]
            .iter()
            .any(|h| e.eq_ignore_ascii_case(h))
    })
}

/// A page's bytes, if it is small enough to rewrite and names a
/// `chimaera:` address.
pub(crate) async fn page_bytes(
    file: &mut tokio::fs::File,
    meta: &std::fs::Metadata,
) -> Option<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    if meta.len() > REWRITE_MAX {
        return None;
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(REWRITE_MAX).read_to_end(&mut bytes).await.ok()?;
    memchr_find(&bytes, b"chimaera:").then_some(bytes)
}

fn memchr_find(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// The library names a page imports (`chimaera:lib/<name>/…`).
pub(crate) fn library_names(page: &[u8]) -> Vec<String> {
    const LIB: &[u8] = b"chimaera:lib/";
    let mut names = BTreeSet::new();
    let mut i = 0;
    while let Some(at) = page[i..].windows(LIB.len()).position(|w| w == LIB) {
        let start = i + at + LIB.len();
        let end = page[start..]
            .iter()
            .position(|b| !(b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-'))
            .map_or(page.len(), |n| start + n);
        if let Ok(name) = std::str::from_utf8(&page[start..end]) {
            if library_name(name) && page.get(end) == Some(&b'/') {
                names.insert(name.to_string());
            }
        }
        i = end.max(start);
    }
    names.into_iter().collect()
}

/// `page` with its `chimaera:lib/` and `chimaera:web.css` addresses
/// pointing at `/lib/{ticket}/`.
pub(crate) fn rewrite(page: &[u8], ticket: &str) -> Vec<u8> {
    let lib = format!("/lib/{ticket}/");
    let css = format!("/lib/{ticket}/~css");
    let mut out = Vec::with_capacity(page.len() + 64);
    let mut i = 0;
    while i < page.len() {
        if page[i..].starts_with(b"chimaera:lib/") {
            out.extend_from_slice(lib.as_bytes());
            i += b"chimaera:lib/".len();
        } else if page[i..].starts_with(b"chimaera:web.css") {
            out.extend_from_slice(css.as_bytes());
            i += b"chimaera:web.css".len();
        } else {
            out.push(page[i]);
            i += 1;
        }
    }
    out
}

/// FNV-1a: an `ETag` mark stable across builds.
pub(crate) fn fnv(bytes: &[u8]) -> String {
    let h = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{h:016x}")
}

/// GET /web/{grant}/{*rest} — a file of the plugin's bundle (its pages get
/// the plugin CSP, and the page tail when asked), or `~url?path=`.
pub(crate) async fn serve_route(
    State(state): State<Arc<AppState>>,
    UrlPath((ticket, rest)): UrlPath<(String, String)>,
    RawQuery(query): RawQuery,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let Some((g, m)) = grant(&state, &ticket).await else {
        return not_found();
    };
    if rest == "~url" {
        let path = params.get("path").cloned().unwrap_or_default();
        return data_url(&state, &g, &m, &path).await;
    }
    let Some(rel) = crate::fs::asset_relative(&rest) else {
        return not_found();
    };
    let Some((mut file, meta)) = open(g.dir.clone(), rel.clone()).await else {
        return not_found();
    };
    let decl = m.web.clone().unwrap_or_default();
    let serve = if is_html(&rel) {
        let tail = decl
            .pages
            .contains(&rest)
            .then(|| PageTail::asked(query.as_deref()))
            .flatten();
        let body = page_bytes(&mut file, &meta).await.map(|bytes| {
            let page = rewrite(&bytes, &ticket);
            let mark = fnv(&page);
            (page, mark)
        });
        Serve {
            caching: Caching::Revalidate,
            security: page_security(&plugin_page_csp(&decl.network)),
            extra: cors(),
            tail,
            more: None,
            body,
        }
    } else {
        Serve {
            caching: Caching::Revalidate,
            security: crate::fs::folder_security(&rel),
            extra: cors(),
            tail: None,
            more: None,
            body: None,
        }
    };
    crate::fs::serve_file(file, &meta, &rel, &headers, serve).await
}

/// `~url?path=`: a data URL for a workspace file (within the plugin's
/// `[access]`) or one of its own `output:` files.
async fn data_url(state: &AppState, g: &Grant, m: &Manifest, path: &str) -> Response {
    let (root, rel) = if path.starts_with("output:") {
        let root = super::output::folder(&state.plugin_platform.output_root, &g.plugin, &g.ws);
        match super::output::relative(path) {
            Ok(rel) => (root, rel),
            Err(err) => return refused(StatusCode::BAD_REQUEST, &err),
        }
    } else {
        if Access::of(m).files != FilesAccess::Read {
            return refused(
                StatusCode::FORBIDDEN,
                &format!("{} may not read workspace files ([access] files)", m.name),
            );
        }
        let Some(root) = crate::lock(&state.workspaces)
            .get(&g.ws)
            .map(|w| w.root.clone())
        else {
            return not_found();
        };
        match super::hostfns::relative(path) {
            Ok(rel) => (root, rel),
            Err(err) => return refused(StatusCode::BAD_REQUEST, &err),
        }
    };
    let (r, l) = (root.clone(), rel.clone());
    let version = tokio::task::spawn_blocking(move || open_data(&r, &l))
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r);
    let version = match version {
        Ok((_, meta)) => crate::fs::mtime_token(&meta),
        Err(err) => return refused(StatusCode::NOT_FOUND, &format!("{path}: {err}")),
    };
    let key = format!("{}\n{}\n{}", root.display(), rel.display(), version);
    let file = DataFile {
        plugin: g.plugin.clone(),
        ws: g.ws.clone(),
        root,
        rel,
    };
    let ticket = crate::lock(&state.plugin_platform.web.data).mint("d", key, file, TICKET_TTL);
    with_cors(Json(json!({"url": format!("/webdata/{ticket}")})).into_response())
}

/// Open `rel` beneath `root`, every component without following a link,
/// as a regular file (blocking).
fn open_data(root: &Path, rel: &Path) -> Result<(std::fs::File, std::fs::Metadata), String> {
    let dir = std::fs::File::from(
        rustix::fs::open(
            root,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|e| e.to_string())?,
    );
    let file = crate::download::open_beneath(
        &dir,
        rel,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NONBLOCK,
    )
    .map_err(|e| super::hostfns::refused(root, rel, e))?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("not a file".into());
    }
    Ok((file, meta))
}


/// GET /webdata/{ticket} — the one file a grant's `~url` named.
pub(crate) async fn data_route(
    State(state): State<Arc<AppState>>,
    UrlPath(ticket): UrlPath<String>,
    headers: HeaderMap,
) -> Response {
    let Some(f) = crate::lock(&state.plugin_platform.web.data).get(&ticket) else {
        return not_found();
    };
    if !on_in(&state, &f.plugin, &f.ws).await {
        return not_found();
    }
    let (root, rel) = (f.root.clone(), f.rel.clone());
    let opened = tokio::task::spawn_blocking(move || open_data(&root, &rel)).await;
    let Ok(Ok((file, meta))) = opened else {
        return not_found();
    };
    let serve = Serve {
        caching: Caching::Revalidate,
        security: crate::fs::folder_security(&f.rel),
        extra: cors(),
        tail: None,
        more: None,
        body: None,
    };
    crate::fs::serve_file(
        tokio::fs::File::from_std(file),
        &meta,
        &f.rel,
        &headers,
        serve,
    )
    .await
}

/// GET /webdata/{key}/{*rest} — a file in an agent page's own folder or
/// below (the folder route's rules), readable by the page: `key` is the
/// one the page got from a host that shows it inside the navigation guard
/// ([`page_more`]).
pub(crate) async fn page_data_route(
    State(state): State<Arc<AppState>>,
    UrlPath((ticket, rest)): UrlPath<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let Some(page) = crate::lock(&state.plugin_platform.web.keys).get(&ticket) else {
        return not_found();
    };
    if !crate::fs::opens_its_folder(&page) {
        return not_found();
    }
    let (Some(rel), Some(dir)) = (crate::fs::asset_relative(&rest), page.parent()) else {
        return not_found();
    };
    let Some((file, meta)) = open(dir.to_path_buf(), rel.clone()).await else {
        return not_found();
    };
    let serve = Serve {
        caching: Caching::Revalidate,
        security: crate::fs::folder_security(&rel),
        extra: cors(),
        tail: None,
        more: None,
        body: None,
    };
    crate::fs::serve_file(file, &meta, &rel, &headers, serve).await
}

/// The workspaces whose root holds `path`.
async fn workspaces_holding(state: &AppState, path: &Path) -> Vec<String> {
    let roots: Vec<(String, PathBuf)> = crate::lock(&state.workspaces)
        .list()
        .into_iter()
        .map(|w| (w.id.clone(), w.root.clone()))
        .collect();
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        roots
            .into_iter()
            .filter(|(_, root)| {
                let canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
                path.starts_with(&canon) || path.starts_with(root)
            })
            .map(|(id, _)| id)
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// The workspaces a `/lib` ticket speaks for: a grant's own, or those
/// holding an agent's page.
async fn lib_workspaces(state: &AppState, ticket: &str) -> Option<Vec<String>> {
    if ticket.starts_with("w-") {
        return grant(state, ticket).await.map(|(g, _)| vec![g.ws]);
    }
    let page = crate::lock(&state.tickets).lookup(ticket)?;
    if !crate::fs::opens_its_folder(&page) {
        return None;
    }
    Some(workspaces_holding(state, &page).await)
}

/// GET /lib/{ticket}/{*rest} — `~css`, or `<name>/<file>` of a library an
/// active plugin lends in a workspace the page belongs to.
pub(crate) async fn lib_route(
    State(state): State<Arc<AppState>>,
    UrlPath((ticket, rest)): UrlPath<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let Some(workspaces) = lib_workspaces(&state, &ticket).await else {
        return not_found();
    };
    if rest == "~css" {
        let mut response = (
            [
                (header::CONTENT_TYPE, "text/css; charset=utf-8"),
                (header::CACHE_CONTROL, "private, max-age=600"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            chimaera_core::WEB_CSS,
        )
            .into_response();
        response.headers_mut().extend(cors());
        return response;
    }
    let Some((name, file)) = rest.split_once('/') else {
        return not_found();
    };
    let Some(rel) = crate::fs::asset_relative(file) else {
        return not_found();
    };
    for ws in &workspaces {
        for m in super::active(&state, ws).await {
            let lends = m
                .web
                .as_ref()
                .is_some_and(|d| d.libraries.iter().any(|l| l == name));
            if !lends {
                continue;
            }
            let Ok(dir) = bundle(&state, &m, ws).await else {
                continue;
            };
            let Some((file, meta)) = open(dir.join(name), rel.clone()).await else {
                return not_found();
            };
            let serve = Serve {
                caching: Caching::Revalidate,
                security: crate::fs::folder_security(&rel),
                extra: cors(),
                tail: None,
                more: None,
                body: None,
            };
            return crate::fs::serve_file(file, &meta, &rel, &headers, serve).await;
        }
    }
    not_found()
}

#[derive(Deserialize)]
pub(crate) struct SaveQuery {
    name: String,
}

/// POST /web/{grant}/~save?name= — a page's export into its plugin's
/// output folder (`output:<name>`), charged against its quota.
pub(crate) async fn save_route(
    State(state): State<Arc<AppState>>,
    UrlPath((ticket, rest)): UrlPath<(String, String)>,
    Query(q): Query<SaveQuery>,
    body: axum::body::Bytes,
) -> Response {
    if rest != "~save" {
        return not_found();
    }
    let Some((g, _)) = grant(&state, &ticket).await else {
        return not_found();
    };
    if body.len() > SAVE_MAX {
        return refused(
            StatusCode::PAYLOAD_TOO_LARGE,
            &format!("a save is at most {} MiB", SAVE_MAX >> 20),
        );
    }
    let rel = match super::output::relative(&q.name) {
        Ok(rel) => rel,
        Err(err) => return refused(StatusCode::BAD_REQUEST, &err),
    };
    if let Err(err) = super::output::fits(&rel, body.len()) {
        return refused(StatusCode::BAD_REQUEST, &err);
    }
    if let Err(err) = super::output::charge(&state, &g.plugin, body.len() as u64).await {
        return refused(StatusCode::INSUFFICIENT_STORAGE, &err);
    }
    let dir = super::output::folder(&state.plugin_platform.output_root, &g.plugin, &g.ws);
    let target = rel.clone();
    let written = tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        super::output::write(&dir, &target, &body)
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|r| r);
    match written {
        Ok(()) => with_cors(
            Json(json!({"path": format!("output:{}", rel.to_string_lossy())})).into_response(),
        ),
        Err(err) => refused(StatusCode::INTERNAL_SERVER_ERROR, &err),
    }
}

/// A plugin a page imports a library of, that is installed but off in the
/// page's workspace: the page's host offers to turn it on.
#[derive(serde::Serialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Need {
    pub(crate) plugin: String,
    pub(crate) name: String,
    pub(crate) library: String,
}

/// An agent's page as served: its CSP, and — when it names `chimaera:`
/// addresses — the rewritten page and its `ETag` mark.
pub(crate) struct AgentPage {
    pub(crate) csp: String,
    pub(crate) body: Option<(Vec<u8>, String)>,
    /// Plugins lending a library it imports that are off where it is.
    pub(crate) needs: Vec<Need>,
}

/// An agent's HTML page (`/raw`) under the fixed rule: `bytes` is the page
/// when it is small enough to read and names a `chimaera:` address.
pub(crate) async fn agent_page(
    state: &AppState,
    ticket: &str,
    page: &Path,
    bytes: Option<Vec<u8>>,
) -> AgentPage {
    let Some(bytes) = bytes else {
        return AgentPage {
            csp: agent_page_csp(&[]),
            body: None,
            needs: Vec::new(),
        };
    };
    let names = library_names(&bytes);
    let (hosts, needs) = if names.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        libraries_for(state, page, &names).await
    };
    let body = rewrite(&bytes, ticket);
    let csp = agent_page_csp(&hosts);
    let mark = fnv(&[body.as_slice(), csp.as_bytes()].concat());
    AgentPage {
        csp,
        body: Some((body, mark)),
        needs,
    }
}

/// What an agent's page shown by Chimaera's own hosts gets before the page
/// API (`fs::serve_page`): the key its `chimaera.url` reads its folder with,
/// and the plugins its libraries need here. A page loaded any other way (a
/// browser pane, a direct visit) has no key, so it can't read its folder.
pub(crate) fn page_more(state: &AppState, ticket: &str, page: &Path, needs: &[Need]) -> String {
    let key = crate::lock(&state.plugin_platform.web.keys).mint(
        "k",
        ticket.to_string(),
        page.to_path_buf(),
        KEY_TTL,
    );
    let mut out = format!("\n<script>globalThis.__chimaeraDataKey=\"{key}\";</script>");
    if !needs.is_empty() {
        let json = serde_json::to_string(needs)
            .unwrap_or_else(|_| "[]".into())
            .replace("</", "<\\/");
        out.push_str(&format!(
            "\n<script>globalThis.__chimaeraNeeds=Object.freeze({json});</script>"
        ));
    }
    out
}

/// The hosts the libraries `names` bring (from plugins on in a workspace
/// holding `page`), and the plugins lending one that are off there.
async fn libraries_for(
    state: &AppState,
    page: &Path,
    names: &[String],
) -> (Vec<String>, Vec<Need>) {
    let workspaces = workspaces_holding(state, page).await;
    let mut hosts = BTreeSet::new();
    let mut found = BTreeSet::new();
    for ws in &workspaces {
        for m in super::active(state, ws).await {
            let Some(d) = &m.web else { continue };
            for name in names {
                if d.libraries.contains(name) {
                    found.insert(name.clone());
                    hosts.extend(d.network.iter().cloned());
                }
            }
        }
    }
    let mut needs = Vec::new();
    if !workspaces.is_empty() {
        for name in names.iter().filter(|n| !found.contains(*n)) {
            let lender = super::catalog(state)
                .iter()
                .find(|m| m.web.as_ref().is_some_and(|d| d.libraries.contains(name)))
                .cloned();
            if let Some(m) = lender {
                needs.push(Need {
                    plugin: m.id.clone(),
                    name: m.name.clone(),
                    library: name.clone(),
                });
            }
        }
    }
    (hosts.into_iter().collect(), needs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_are_bare_public_names() {
        for good in [
            "data.genomespy.app",
            "hgdownload.soe.ucsc.edu",
            "a-b.example.org",
        ] {
            assert!(host_name(good), "{good}");
        }
        for bad in [
            "localhost",
            "example",
            "https://example.org",
            "example.org:443",
            "*.example.org",
            "127.0.0.1",
            "Example.org",
            "example.org/x",
            "-a.example.org",
            "",
        ] {
            assert!(!host_name(bad), "{bad}");
        }
    }

    #[test]
    fn a_page_names_its_libraries_and_is_rewritten() {
        let page = br#"<link href="chimaera:web.css"><script src="chimaera:lib/genome-spy/index.js"></script>
<script type="module">import x from "chimaera:lib/test-lib/lib.mjs";</script> chimaera:lib/Bad/x chimaera:lib/none"#;
        assert_eq!(library_names(page), vec!["genome-spy", "test-lib"]);
        let out = String::from_utf8(rewrite(page, "t-1")).unwrap();
        assert!(out.contains(r#"href="/lib/t-1/~css""#), "{out}");
        assert!(
            out.contains(r#"src="/lib/t-1/genome-spy/index.js""#),
            "{out}"
        );
        assert!(out.contains(r#"from "/lib/t-1/test-lib/lib.mjs""#), "{out}");
        assert!(!out.contains("chimaera:"), "{out}");
    }

    #[test]
    fn the_csps_send_nothing_anywhere_undeclared() {
        let agent = agent_page_csp(&["data.example.org".into()]);
        assert!(agent.starts_with("sandbox allow-scripts;"));
        assert!(agent.contains("default-src 'none'"));
        assert!(agent.contains("connect-src 'self' https://data.example.org;"));
        assert!(agent.contains("https://cdn.jsdelivr.net"));
        assert!(agent.contains("'unsafe-eval' 'wasm-unsafe-eval'"));
        assert!(agent.contains("form-action 'none'"));
        let plugin = plugin_page_csp(&[]);
        assert!(plugin.contains("connect-src 'self';"));
        assert!(!plugin.contains("cdn"));
    }

    #[test]
    fn web_decl_is_checked() {
        let ok = WebDecl {
            pages: vec!["view.html".into(), "pages/genome.html".into()],
            libraries: vec!["genome-spy".into()],
            network: vec!["data.genomespy.app".into()],
        };
        assert_eq!(validate(&ok), Ok(()));
        for bad in [
            WebDecl {
                pages: vec!["../x.html".into()],
                ..Default::default()
            },
            WebDecl {
                pages: vec!["x.js".into()],
                ..Default::default()
            },
            WebDecl {
                pages: vec![".hidden/x.html".into()],
                ..Default::default()
            },
            WebDecl {
                libraries: vec!["Bad".into()],
                ..Default::default()
            },
            WebDecl {
                network: vec!["https://x.org".into()],
                ..Default::default()
            },
            WebDecl {
                pages: vec!["a.html".into(), "a.html".into()],
                ..Default::default()
            },
        ] {
            assert!(validate(&bad).is_err(), "{bad:?}");
        }
    }
}
