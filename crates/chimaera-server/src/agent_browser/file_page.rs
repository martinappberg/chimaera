//! Files as agent pages: `browser_navigate` with a `path` (or a `file://`
//! URL) opens a file from the daemon's host, so an agent can check the HTML
//! report it just wrote without starting a web server.
//!
//! A file is never loaded as `file://`: that would hand the page the whole
//! filesystem. It loads the way the HTML preview loads it, under a ticket
//! (`GET /raw/{ticket}/{name}`, `fs::raw_asset`): sandboxed with an opaque
//! origin, relative assets served from its own folder downward only, no
//! CORS, so the page can't read the files around it. The agent sees the
//! page exactly as the user's preview shows it.
//!
//! The ticket is a transport detail. Every URL reported back to the agent
//! is put in its terms (`file:///dir/rest`, [`file_url`]) through a small
//! ticket → file registry kept here, and a reload mints the ticket again
//! ([`remint`]), so an expired ticket or a rewritten report never leaves the
//! agent on a dead or stale address.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use super::TargetUrl;
use crate::AppState;

/// A file page's ticket lives this long (renewed by each mint of the same
/// unchanged file): longer than a preview's, since an agent can spend a
/// while in one page; the idle reaper closes the page itself sooner.
const FILE_TICKET_TTL: Duration = Duration::from_secs(3600);
/// What opens as a page: what the feature is for (a report, a plot), not
/// any file. `browser_navigate` is pre-allowed, so a wider set would show an
/// agent files its permission mode would have asked about.
const PAGE_EXTENSIONS: &[&str] = &[
    "html", "htm", "xhtml", "svg", "png", "jpg", "jpeg", "gif", "webp", "avif",
];
/// Tickets remembered for mapping URLs back to files (oldest dropped).
const REGISTRY_MAX: usize = 256;

/// Ticket → the file it was minted for, for every file page this daemon
/// opened for an agent. Bounded; kept past the ticket's expiry so an old
/// URL still maps back to its file.
#[derive(Default)]
pub(crate) struct FileTickets {
    entries: Mutex<VecDeque<(String, PathBuf)>>,
}

impl FileTickets {
    fn remember(&self, ticket: &str, path: &Path) {
        let mut entries = crate::lock(&self.entries);
        if entries.iter().any(|(t, _)| t == ticket) {
            return;
        }
        if entries.len() >= REGISTRY_MAX {
            entries.pop_front();
        }
        entries.push_back((ticket.to_string(), path.to_path_buf()));
    }

    fn file_of(&self, ticket: &str) -> Option<PathBuf> {
        crate::lock(&self.entries)
            .iter()
            .rev()
            .find(|(t, _)| t == ticket)
            .map(|(_, p)| p.clone())
    }
}

/// What `browser_navigate` was asked to open, when it names a file: the
/// `path` argument, or a `url` that is a `file:` URL. `None` for anything
/// else (an http address, back/forward/reload).
pub(crate) fn requested(path_arg: Option<&str>, url: Option<&str>) -> Option<String> {
    if let Some(path) = path_arg {
        return Some(path.trim().to_string());
    }
    let url = url?.trim();
    let local = crate::embed::strip_file_scheme(url)?;
    // A file URL's query or fragment is not part of the file's name.
    let local = local.split(['?', '#']).next().unwrap_or(local);
    Some(crate::embed::percent_decode(local))
}

/// Open `raw` (absolute, `~/…`, or relative to the session's working
/// directory, then its workspace root) as a page target on this daemon.
pub(crate) async fn target(state: &AppState, agent: &str, raw: &str) -> Result<TargetUrl, String> {
    let cwd = state
        .sessions
        .get(agent)
        .map(|info| info.cwd)
        .or_else(|| state.chat.get(agent).map(|info| info.cwd));
    let root = {
        // Sequential locks, never nested (see `mcp::workspace_of`).
        let ws_id = crate::lock(&state.session_workspaces).get(agent).cloned();
        ws_id.and_then(|id| crate::lock(&state.workspaces).get(&id).map(|w| w.root))
    };
    let raw = raw.to_string();
    let found = crate::doc_check::run_blocking(move || {
        let path = crate::doc_check::resolve_for_agent(&raw, cwd.as_deref(), root.as_deref())?;
        let canonical =
            std::fs::canonicalize(&path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        let meta = std::fs::metadata(&canonical)
            .map_err(|e| anyhow::anyhow!("{}: {e}", canonical.display()))?;
        if !meta.is_file() {
            anyhow::bail!("{} is not a file", canonical.display());
        }
        if !is_page_file(&canonical) {
            anyhow::bail!(
                "{} is not a page: an HTML file, SVG or image opens here; read other files \
                 directly",
                canonical.display()
            );
        }
        Ok((canonical, crate::fs::mtime_token(&meta)))
    })
    .await
    .map_err(|e| format!("Not loaded: {e:#}."))?;
    let (path, version) = found;
    Ok(mint(state, &path, Some(version), ""))
}

/// Whether `path` names a page by its extension ([`PAGE_EXTENSIONS`]).
fn is_page_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| PAGE_EXTENSIONS.iter().any(|p| e.eq_ignore_ascii_case(p)))
}

/// Mint (or renew) the ticket for `path` and the target that loads it,
/// `suffix` (a relative path under its folder, plus query and fragment)
/// after the ticket, or the file's own name when empty.
fn mint(state: &AppState, path: &Path, version: Option<String>, suffix: &str) -> TargetUrl {
    let ticket = crate::lock(&state.tickets).mint_for(path.to_path_buf(), version, FILE_TICKET_TTL);
    state.browsers.files.remember(&ticket, path);
    let rest = if suffix.is_empty() {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        encode_segment(&name)
    } else {
        suffix.to_string()
    };
    TargetUrl {
        host: "127.0.0.1".to_string(),
        port: state.port,
        path: format!("/raw/{ticket}/{rest}"),
    }
}

/// A reload of a file page whose ticket changed: minted again for the
/// page's file, it is a new ticket once the old one expired or the file was
/// rewritten, and this is the address under it. `None` when `href` is not a
/// file page this daemon opened, or its ticket still stands (a plain reload
/// then revalidates everything).
pub(crate) async fn remint(state: &AppState, href: &str) -> Option<TargetUrl> {
    let (ticket, suffix) = split_raw(href)?;
    let path = state.browsers.files.file_of(ticket)?;
    let suffix = suffix.to_string();
    let stat = path.clone();
    let version = crate::doc_check::run_blocking(move || {
        Ok(std::fs::metadata(&stat)
            .ok()
            .map(|m| crate::fs::mtime_token(&m)))
    })
    .await
    .ok()
    .flatten();
    let fresh = mint(state, &path, version, &suffix);
    (!fresh.path.starts_with(&format!("/raw/{ticket}/"))).then_some(fresh)
}

/// `href` in the agent's terms when it is a file page: `file:///dir/rest`
/// (query and fragment kept). `None` for anything else.
pub(crate) fn file_url(state: &AppState, href: &str) -> Option<String> {
    let (ticket, suffix) = split_raw(href)?;
    let file = state.browsers.files.file_of(ticket)?;
    let cut = suffix.find(['?', '#']).unwrap_or(suffix.len());
    let (rest, tail) = suffix.split_at(cut);
    let rest = crate::embed::percent_decode(rest);
    let local = match (rest.as_str(), file.parent()) {
        ("", _) | (_, None) => file,
        (rest, Some(dir)) => dir.join(rest),
    };
    Some(format!("file://{}{tail}", local.display()))
}

/// Every file-page URL in free text (a console line) put in the agent's
/// terms; anything else left alone.
pub(crate) fn map_text(state: &AppState, text: &str) -> String {
    if !text.contains("/raw/t-") {
        return text.to_string();
    }
    text.split_inclusive(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '"' | '\''))
        .map(|run| {
            let end = run
                .find(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '"' | '\''))
                .unwrap_or(run.len());
            let (word, sep) = run.split_at(end);
            // A stack frame's `:line:col` stays after the mapped URL.
            let (url, pos) = split_position(word);
            match file_url(state, url) {
                Some(mapped) => format!("{mapped}{pos}{sep}"),
                None => run.to_string(),
            }
        })
        .collect()
}

/// Whether `path` (a URL's path) is a ticketed file on this daemon: the
/// one thing on the daemon's own port an agent page may load.
pub(crate) fn is_raw_path(path: &str) -> bool {
    path.starts_with("/raw/t-")
}

/// Whether `url` is the daemon's own `/favicon.ico` (the request a browser
/// makes by itself for a file page, which the page's opaque origin blocks).
pub(crate) fn is_daemon_favicon(state: &AppState, url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let Some((authority, path)) = rest.split_once('/') else {
        return false;
    };
    let Some((host, port)) = authority.rsplit_once(':') else {
        return false;
    };
    path == "favicon.ico"
        && port.parse() == Ok(state.port)
        && crate::proxy::is_loopback_host(&crate::proxy::normalize_host(host))
}

/// Whether `target` is a file page on this daemon (not an app to proxy).
pub(crate) fn is_file_target(state: &AppState, target: &TargetUrl) -> bool {
    target.port == state.port
        && crate::proxy::is_loopback_host(&crate::proxy::normalize_host(&target.host))
        && is_raw_path(&target.path)
}

/// `…/raw/{ticket}/{suffix}` of an absolute or origin-relative URL →
/// (`ticket`, `suffix`), the suffix keeping its query and fragment.
fn split_raw(href: &str) -> Option<(&str, &str)> {
    let path = match href.split_once("://") {
        Some((_, rest)) => &rest[rest.find('/')?..],
        None => href,
    };
    let after = path.strip_prefix("/raw/")?;
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let (ticket, rest) = after.split_at(end);
    if !ticket.starts_with("t-") {
        return None;
    }
    Some((ticket, rest.strip_prefix('/').unwrap_or(rest)))
}

/// A trailing `:12` or `:12:5` (a stack frame's position) split off.
fn split_position(word: &str) -> (&str, &str) {
    let mut cut = word.len();
    for _ in 0..2 {
        match word[..cut].rfind(':') {
            Some(at) if at + 1 < cut && word[at + 1..cut].bytes().all(|b| b.is_ascii_digit()) => {
                cut = at;
            }
            _ => break,
        }
    }
    word.split_at(cut)
}

/// One path segment percent-encoded (everything but RFC 3986 unreserved).
fn encode_segment(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_or_a_file_url_names_a_file() {
        assert_eq!(
            requested(Some(" out/r.html "), None).as_deref(),
            Some("out/r.html")
        );
        assert_eq!(
            requested(None, Some("file:///tmp/my%20report.html#top")).as_deref(),
            Some("/tmp/my report.html")
        );
        assert_eq!(
            requested(None, Some("file://localhost/tmp/r.html")).as_deref(),
            Some("/tmp/r.html")
        );
        assert_eq!(requested(None, Some("file://elsewhere/tmp/r.html")), None);
        assert_eq!(requested(None, Some("http://localhost:8888/")), None);
        assert_eq!(requested(None, Some("reload")), None);
        assert_eq!(requested(None, None), None);
    }

    #[test]
    fn raw_urls_split_into_ticket_and_rest() {
        assert_eq!(
            split_raw("http://127.0.0.1:9/raw/t-abc/figs/a.png?x=1#y"),
            Some(("t-abc", "figs/a.png?x=1#y"))
        );
        assert_eq!(split_raw("/raw/t-abc/r.html"), Some(("t-abc", "r.html")));
        assert_eq!(split_raw("/raw/t-abc"), Some(("t-abc", "")));
        assert_eq!(split_raw("http://127.0.0.1:9/raw/other/r.html"), None);
        assert_eq!(
            split_raw("http://127.0.0.1:9/proxy/p-1/raw/t-a/r.html"),
            None
        );
        assert!(is_raw_path("/raw/t-abc/r.html"));
        assert!(!is_raw_path("/api/v1/sessions"));
        assert!(!is_raw_path("/raw/"));
    }

    #[test]
    fn the_registry_maps_tickets_back_and_stays_bounded() {
        let files = FileTickets::default();
        files.remember("t-0", Path::new("/r/0.html"));
        assert_eq!(files.file_of("t-0"), Some(PathBuf::from("/r/0.html")));
        for i in 1..=REGISTRY_MAX {
            files.remember(&format!("t-{i}"), Path::new("/r/x.html"));
        }
        assert_eq!(files.file_of("t-0"), None);
        assert_eq!(crate::lock(&files.entries).len(), REGISTRY_MAX);
    }

    #[test]
    fn only_pages_open() {
        for ok in ["r.html", "R.HTM", "plot.svg", "fig.png", "a.webp"] {
            assert!(is_page_file(Path::new(ok)), "{ok}");
        }
        for no in [
            "id_rsa",
            ".env",
            "data.csv",
            "notes.md",
            "r.html.bak",
            "x.pdf",
        ] {
            assert!(!is_page_file(Path::new(no)), "{no}");
        }
    }

    #[test]
    fn names_encode_as_one_segment() {
        assert_eq!(encode_segment("my report#2.html"), "my%20report%232.html");
        assert_eq!(encode_segment("a/b"), "a%2Fb");
        assert_eq!(encode_segment("ok-1_2~.html"), "ok-1_2~.html");
    }

    #[test]
    fn stack_positions_split_off() {
        assert_eq!(
            split_position("http://h/raw/t-a/x.js:12:5"),
            ("http://h/raw/t-a/x.js", ":12:5")
        );
        assert_eq!(
            split_position("http://h/raw/t-a/x.js:12"),
            ("http://h/raw/t-a/x.js", ":12")
        );
        assert_eq!(
            split_position("http://h:9/raw/t-a/x.js"),
            ("http://h:9/raw/t-a/x.js", "")
        );
    }
}
