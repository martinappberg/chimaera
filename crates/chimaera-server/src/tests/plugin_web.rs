//! Web views (docs/design/rich-views-plan.md, Part 2), proven with their
//! fixture (`plugins/test-web`, built into `plugins/dist-test` by
//! `scripts/build-plugins.sh`): the manifest's `[web]` and the card's Can
//! list, the bundle unpacked from `web-bundle()`, a page under its grant
//! (its CSP, CORS, the page tail, `~url`, `~save`), data by Range, and an
//! agent's page under the one fixed rule (its CSP, the `chimaera:lib`
//! rewrite, its data key, the files beside it) — each refused outside its
//! access and where its plugin is off.

use serde_json::{json, Value};

use super::support::*;
use crate::{lock, AppState};

const PID: &str = "test-web";

async fn switch(state: &Arc<AppState>, ws: &str, pid: &str, on: bool) {
    let (status, body) = request(
        state,
        Method::PUT,
        &format!("/api/v1/workspaces/{ws}/plugins/{pid}"),
        Some(json!({"on": on})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// A workspace with the web fixture switched on and a `sample.csv`.
async fn web_workspace(label: &str) -> (Arc<AppState>, String, PathBuf) {
    crate::plugins::test_catalog::web();
    let state = test_state();
    let ws = make_workspace(&state, label).await;
    switch(&state, &ws, PID, true).await;
    let root = lock(&state.workspaces).get(&ws).unwrap().root;
    std::fs::write(root.join("sample.csv"), "chrom,pos\nchr1,100\nchr2,200\n").unwrap();
    (state, ws, root)
}

async fn mint(state: &Arc<AppState>, ws: &str, pid: &str, entry: &str) -> (StatusCode, Value) {
    request(
        state,
        Method::POST,
        &format!("/api/v1/workspaces/{ws}/plugins/{pid}/web"),
        Some(json!({"entry": entry})),
    )
    .await
}

/// A request the way a page with no origin makes it: no bearer token.
async fn page_get(
    state: &Arc<AppState>,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Vec<u8>>,
) -> (StatusCode, axum::http::HeaderMap, bytes::Bytes) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let body = body.map(Body::from).unwrap_or_else(Body::empty);
    let res = crate::app(state.clone())
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, headers, bytes)
}

async fn get(
    state: &Arc<AppState>,
    uri: &str,
) -> (StatusCode, axum::http::HeaderMap, bytes::Bytes) {
    page_get(state, Method::GET, uri, &[], None).await
}

fn json_of(bytes: &bytes::Bytes) -> Value {
    serde_json::from_slice(bytes).unwrap_or(Value::Null)
}

#[test]
fn the_manifest_declares_pages_libraries_and_hosts_and_the_card_says_so() {
    let m = crate::plugins::test_catalog::web();
    let web = m.web.as_ref().expect("[web]");
    assert_eq!(web.pages, vec!["view.html"]);
    assert_eq!(web.libraries, vec!["testlib"]);
    assert_eq!(web.network, vec!["data.example.org"]);
    let lines: Vec<String> = m.caps.lines().into_iter().map(|l| l.text).collect();
    for want in [
        "Draws its own views (web pages in a sandbox)",
        "Lends its testlib library to agents' pages where it is switched on",
        "Its pages reach data.example.org",
    ] {
        assert!(lines.iter().any(|l| l == want), "{want} in {lines:?}");
    }
    // A sandbox is a sandbox: web views alone never make it privileged.
    assert_eq!(m.caps.tier(), crate::plugins::capabilities::Tier::Sandboxed);

    // `[web]` needs a 0.3 component; a bad host is refused by name.
    let text = crate::plugins::test_catalog::dist_test_text("test-web/plugin.toml");
    let old = text.replace("api = \"0.3\"", "api = \"0.2\"");
    let err = crate::plugins::parse_manifest(&old).unwrap_err();
    assert!(err.contains("[web] needs api = \"0.3\""), "{err}");
    let bad = text.replace("\"data.example.org\"", "\"https://data.example.org\"");
    let err = crate::plugins::parse_manifest(&bad).unwrap_err();
    assert!(err.contains("bare public hostname"), "{err}");

    // Its board asks for a sidebar row, which the wire carries; only a tab
    // view may, and only from 0.3.
    assert!(m.views.iter().any(|v| v.id == "board" && v.rail));
    let wire = crate::plugins::platform::wire(&m);
    assert_eq!(wire["views"][0]["rail"], json!(true));
    let panel = text.replace("slot = \"tab\"", "slot = \"panel\"");
    let err = crate::plugins::parse_manifest(&panel).unwrap_err();
    assert!(err.contains("only a tab view"), "{err}");
    let old = text
        .replace("api = \"0.3\"", "api = \"0.2\"")
        .replace("[web]\npages = [\"view.html\"]\nlibraries = [\"testlib\"]\nnetwork = [\"data.example.org\"]\n", "");
    let err = crate::plugins::parse_manifest(&old).unwrap_err();
    assert!(err.contains("rail needs api = \"0.3\""), "{err}");
}

#[tokio::test]
async fn a_plugin_page_is_served_under_its_grant() {
    let (state, ws, _) = web_workspace("web-grant").await;

    // Only a page the manifest names.
    let (status, _) = mint(&state, &ws, PID, "testlib/lib.js").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = mint(&state, &ws, PID, "view.html").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let url = body["url"].as_str().unwrap().to_string();
    assert!(
        url.starts_with("/web/w-") && url.ends_with("/view.html"),
        "{url}"
    );
    // Asked again: the same URL (the grant renewed, not replaced).
    let (_, again) = mint(&state, &ws, PID, "view.html").await;
    assert_eq!(again["url"], body["url"]);
    let grant = url
        .trim_start_matches("/web/")
        .split('/')
        .next()
        .unwrap()
        .to_string();

    // The page: its plugin's CSP (its declared host, the evals, no CDN),
    // readable by a page with no origin, with the page tail when asked, and
    // its `chimaera:web.css` pointing at this daemon.
    let (status, headers, bytes) = get(&state, &format!("{url}?bridge=1")).await;
    assert_eq!(status, StatusCode::OK);
    let csp = header_str(&headers, "content-security-policy");
    assert!(csp.starts_with("sandbox allow-scripts;"), "{csp}");
    assert!(
        csp.contains("connect-src 'self' https://data.example.org;"),
        "{csp}"
    );
    assert!(csp.contains("'unsafe-eval' 'wasm-unsafe-eval'"), "{csp}");
    assert!(!csp.contains("jsdelivr"), "{csp}");
    assert_eq!(header_str(&headers, "access-control-allow-origin"), "*");
    let page = std::str::from_utf8(&bytes).unwrap();
    assert!(
        page.contains(&format!("href=\"/lib/{grant}/~css\"")),
        "rewritten"
    );
    assert!(!page.contains("chimaera:web.css"));
    assert!(page.contains(chimaera_core::PAGE_API_JS));
    assert!(page.contains("__chimaeraPaneView"));

    // Its own files: CORS-readable (a module script from a page with no
    // origin is a CORS fetch), and never a page with scripts of its own.
    let (status, headers, bytes) = get(&state, &format!("/web/{grant}/testlib/lib.js")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(header_str(&headers, "content-type").contains("javascript"));
    assert_eq!(header_str(&headers, "access-control-allow-origin"), "*");
    assert_eq!(header_str(&headers, "content-security-policy"), "sandbox");
    assert!(std::str::from_utf8(&bytes)
        .unwrap()
        .contains("hello from testlib"));
    // Nothing outside the bundle, nothing hidden in it.
    for rest in ["../plugin.toml", ".complete", "nope.js"] {
        let (status, _, _) = get(&state, &format!("/web/{grant}/{rest}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{rest}");
    }
    // Preflights are answered.
    let (status, headers, _) = page_get(
        &state,
        Method::OPTIONS,
        &format!("/web/{grant}/~save?name=x.png"),
        &[
            ("origin", "null"),
            ("access-control-request-method", "POST"),
        ],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(header_str(&headers, "access-control-allow-methods").contains("POST"));

    // The CSS a page links.
    let (status, headers, bytes) = get(&state, &format!("/lib/{grant}/~css")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(header_str(&headers, "content-type").starts_with("text/css"));
    assert_eq!(&bytes[..], chimaera_core::WEB_CSS.as_bytes());

    // Switched off: the grant answers nothing, and no new one is made.
    switch(&state, &ws, PID, false).await;
    let (status, _, _) = get(&state, &url).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = mint(&state, &ws, PID, "view.html").await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_plugin_page_reads_workspace_files_by_range_within_its_access() {
    // `root` is for the Unix-only link check below.
    #[cfg_attr(not(unix), allow(unused_variables))]
    let (state, ws, root) = web_workspace("web-data").await;
    let (_, body) = mint(&state, &ws, PID, "view.html").await;
    let url = body["url"].as_str().unwrap();
    let grant = url
        .trim_start_matches("/web/")
        .split('/')
        .next()
        .unwrap()
        .to_string();
    let ask = |path: &str| format!("/web/{grant}/~url?path={path}");

    let (status, headers, bytes) = get(&state, &ask("sample.csv")).await;
    assert_eq!(status, StatusCode::OK, "{bytes:?}");
    assert_eq!(header_str(&headers, "access-control-allow-origin"), "*");
    let data = json_of(&bytes)["url"].as_str().unwrap().to_string();
    assert!(data.starts_with("/webdata/d-"), "{data}");
    // The same file, unchanged: the same URL.
    let (_, _, again) = get(&state, &ask("sample.csv")).await;
    assert_eq!(json_of(&again)["url"], json!(data));

    let (status, headers, bytes) =
        page_get(&state, Method::GET, &data, &[("range", "bytes=0-8")], None).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(&bytes[..], b"chrom,pos");
    assert_eq!(header_str(&headers, "content-range"), "bytes 0-8/28");
    assert_eq!(header_str(&headers, "access-control-allow-origin"), "*");
    assert!(header_str(&headers, "access-control-expose-headers").contains("Content-Range"));

    // Outside the workspace, or through a link: refused.
    let (status, _, _) = get(&state, &ask("../etc/passwd")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // A symlink needs privileges on Windows.
    #[cfg(unix)]
    {
        let outside = test_dir("web-data-outside");
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("away")).unwrap();
        let (status, _, bytes) = get(&state, &ask("away/secret.txt")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            std::str::from_utf8(&bytes).unwrap().contains("link"),
            "{bytes:?}"
        );
    }

    // `save` lands in its output folder, which `url` then reads.
    let png = b"\x89PNG\r\n\x1a\nfixture".to_vec();
    let (status, headers, bytes) = page_get(
        &state,
        Method::POST,
        &format!("/web/{grant}/~save?name=pictures/one.png"),
        &[("content-type", "image/png")],
        Some(png.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{bytes:?}");
    assert_eq!(header_str(&headers, "access-control-allow-origin"), "*");
    assert_eq!(json_of(&bytes)["path"], json!("output:pictures/one.png"));
    let (_, _, bytes) = get(&state, &ask("output:pictures/one.png")).await;
    let saved = json_of(&bytes)["url"].as_str().unwrap().to_string();
    let (status, _, bytes) = get(&state, &saved).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&bytes[..], &png[..]);
    let (status, _, _) = get(&state, &format!("/web/{grant}/~save?name=x.png")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a save is a POST");

    // Off: the data URL goes with it.
    switch(&state, &ws, PID, false).await;
    let (status, _, _) = get(&state, &data).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_plugin_without_file_access_reads_only_its_own_output() {
    crate::plugins::test_catalog::web();
    let text = crate::plugins::test_catalog::dist_test_text("test-web/plugin.toml")
        .replace("id = \"test-web\"", "id = \"test-web-blind\"")
        .replace("[access]\nfiles = \"read\"\n", "");
    crate::plugins::test_catalog::add(
        &text,
        crate::plugins::test_catalog::dist_test_bytes("test-web/plugin.wasm"),
    );
    let state = test_state();
    let ws = make_workspace(&state, "web-blind").await;
    switch(&state, &ws, "test-web-blind", true).await;
    let root = lock(&state.workspaces).get(&ws).unwrap().root;
    std::fs::write(root.join("sample.csv"), "a,b\n").unwrap();
    let (status, body) = mint(&state, &ws, "test-web-blind", "view.html").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let grant = body["url"]
        .as_str()
        .unwrap()
        .trim_start_matches("/web/")
        .split('/')
        .next()
        .unwrap()
        .to_string();
    let (status, _, bytes) = get(&state, &format!("/web/{grant}/~url?path=sample.csv")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(std::str::from_utf8(&bytes)
        .unwrap()
        .contains("[access] files"));
}

async fn page_ticket(state: &Arc<AppState>, path: &std::path::Path) -> String {
    let (status, json) = request(
        state,
        Method::POST,
        "/api/v1/fs/ticket",
        Some(json!({"path": path.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    json["ticket"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn an_agent_page_imports_a_plugin_library_and_reads_the_files_beside_it() {
    let (state, ws, root) = web_workspace("web-agent").await;
    let figs = root.join("figs");
    std::fs::create_dir_all(figs.join(".hidden")).unwrap();
    std::fs::write(figs.join("data.csv"), "x,y\n1,2\n").unwrap();
    std::fs::write(figs.join(".hidden/secret.csv"), "no").unwrap();
    std::fs::write(root.join("above.csv"), "no").unwrap();
    let page = "<!doctype html><link rel=stylesheet href=\"chimaera:web.css\">\
                <script type=module>import {hello} from \"chimaera:lib/testlib/lib.mjs\";</script>";
    std::fs::write(figs.join("demo.html"), page).unwrap();
    let t = page_ticket(&state, &figs.join("demo.html")).await;

    // Served: the fixed rule plus the library's host, and its addresses
    // rewritten under a library ticket of their own. Never a key, however
    // the page asks for itself.
    let (status, headers, bytes) = get(&state, &format!("/raw/{t}/demo.html?bridge=1")).await;
    assert_eq!(status, StatusCode::OK);
    let csp = header_str(&headers, "content-security-policy");
    assert_eq!(
        csp,
        crate::plugins::web::agent_page_csp(&["data.example.org".into()])
    );
    assert!(csp.contains("https://cdn.jsdelivr.net") && csp.contains("https://cdn.plot.ly"));
    let body = std::str::from_utf8(&bytes).unwrap();
    let lib = body
        .split("from \"/lib/")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .expect("a rewritten import")
        .to_string();
    assert!(lib.starts_with("l-"), "{body}");
    assert!(
        body.contains(&format!("from \"/lib/{lib}/testlib/lib.mjs\"")),
        "{body}"
    );
    assert!(body.contains(&format!("href=\"/lib/{lib}/~css\"")));

    // The key, only for a host's bearer ask; its plugin is on, so nothing
    // to offer.
    let ask = |ticket: String| {
        let state = state.clone();
        async move {
            request(
                &state,
                Method::POST,
                "/api/v1/fs/page-key",
                Some(json!({"ticket": ticket})),
            )
            .await
        }
    };
    let (status, access) = ask(t.clone()).await;
    assert_eq!(status, StatusCode::OK, "{access}");
    let key = access["key"].as_str().unwrap().to_string();
    assert!(key.starts_with("k-"), "{key}");
    assert_eq!(access["needs"], json!([]));
    // Asked for by the page itself, it still carries no key.
    for asked in ["?bridge=1", "?api=1", ""] {
        let (_, _, bytes) = get(&state, &format!("/raw/{t}/demo.html{asked}")).await;
        assert!(
            !std::str::from_utf8(&bytes).unwrap().contains(&key),
            "{asked}"
        );
    }
    let (status, _) = ask("t-nope".into()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The library, readable by the page; the plugin's, where it is on.
    let (status, headers, bytes) = get(&state, &format!("/lib/{lib}/testlib/lib.mjs")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header_str(&headers, "access-control-allow-origin"), "*");
    assert!(std::str::from_utf8(&bytes)
        .unwrap()
        .contains("export const hello"));

    // Its folder by Range, through the key — never above it, never hidden.
    let (status, headers, bytes) = page_get(
        &state,
        Method::GET,
        &format!("/webdata/{key}/data.csv"),
        &[("range", "bytes=0-2")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(&bytes[..], b"x,y");
    assert_eq!(header_str(&headers, "access-control-allow-origin"), "*");
    for rest in ["../above.csv", ".hidden/secret.csv", "nope.csv"] {
        let (status, _, _) = get(&state, &format!("/webdata/{key}/{rest}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{rest}");
    }
    // Neither the page ticket nor the library ticket is a key, and the
    // folder route stays unreadable: a page loaded any other way reads
    // nothing.
    for not_a_key in [&t, &lib] {
        let (status, _, _) = get(&state, &format!("/webdata/{not_a_key}/data.csv")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    let (status, headers, _) = get(&state, &format!("/raw/{t}/data.csv")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get("access-control-allow-origin").is_none());

    // Where the plugin is off: no library, no host, and the host's ask
    // names the plugin to offer.
    switch(&state, &ws, PID, false).await;
    let (status, _, _) = get(&state, &format!("/lib/{lib}/testlib/lib.mjs")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, headers, _) = get(&state, &format!("/raw/{t}/demo.html?api=1")).await;
    assert_eq!(
        header_str(&headers, "content-security-policy"),
        crate::plugins::web::agent_page_csp(&[])
    );
    let (_, access) = ask(t.clone()).await;
    assert_eq!(access["needs"][0]["plugin"], json!("test-web"), "{access}");
}

#[tokio::test]
async fn a_plain_report_keeps_its_bytes_under_the_fixed_rule() {
    let state = test_state();
    let dir = test_dir("web-plain");
    let page = "<!doctype html><h1>report</h1><img src=\"https://example.org/logo.png\">";
    std::fs::write(dir.join("report.html"), page).unwrap();
    let t = page_ticket(&state, &dir.join("report.html")).await;
    let (status, headers, bytes) = get(&state, &format!("/raw/{t}/report.html")).await;
    assert_eq!(status, StatusCode::OK);
    // Untouched (nothing to rewrite), ranges still offered, and confined.
    assert_eq!(&bytes[..], page.as_bytes());
    assert_eq!(header_str(&headers, "accept-ranges"), "bytes");
    let csp = header_str(&headers, "content-security-policy");
    assert!(csp.contains("img-src 'self' data: blob:;"), "{csp}");
    assert!(csp.contains("connect-src 'self';"), "{csp}");
}

#[tokio::test]
async fn a_0_3_plugin_reads_a_workspace_file_from_an_offset() {
    // `root` is for the Unix-only link check below.
    #[cfg_attr(not(unix), allow(unused_variables))]
    let (state, ws, root) = web_workspace("web-read-at").await;
    let read_at = |args: Value| {
        let state = state.clone();
        let ws = ws.clone();
        async move {
            let args = urlencoding_lite(&args.to_string());
            request(
                &state,
                Method::GET,
                &format!("/api/v1/workspaces/{ws}/plugins/{PID}/query/read-at?args={args}"),
                None,
            )
            .await
        }
    };
    // `sample.csv` is "chrom,pos\nchr1,100\nchr2,200\n": `chr1,` starts at 10.
    let (status, body) = read_at(json!({"path": "sample.csv", "offset": 10, "cap": 5})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["text"], json!("chr1,"));
    // At or past the end: nothing, not an error.
    let (status, body) = read_at(json!({"path": "sample.csv", "offset": 1000, "cap": 5})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["text"], json!(""));
    // The same confinement as `read`: no `..`, no link.
    // A symlink needs privileges on Windows; there `away` is simply missing.
    #[cfg(unix)]
    {
        let outside = test_dir("web-read-at-outside");
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("away")).unwrap();
    }
    for path in ["../x", "away/secret.txt"] {
        let (status, body) = read_at(json!({"path": path, "offset": 0, "cap": 5})).await;
        assert_ne!(status, StatusCode::OK, "{path}: {body}");
    }
}

/// Percent-encode a query value (the JSON `args`).
fn urlencoding_lite(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}
