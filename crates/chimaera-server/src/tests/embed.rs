//! Embed plumbing: `POST /api/v1/fs/resolve_targets`, `/raw` caching (ETag,
//! 304, stable per-version tickets) and the directory-scoped
//! `/raw/{ticket}/{*rest}` an HTML report's relative assets load through.

use super::support::*;
use crate::AppState;

fn canon(path: &std::path::Path) -> String {
    std::fs::canonicalize(path)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    b.extend_from_slice(&w.to_be_bytes());
    b.extend_from_slice(&h.to_be_bytes());
    b.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    b
}

async fn resolve(state: &Arc<AppState>, body: serde_json::Value) -> serde_json::Value {
    let (status, answer) = request(
        state,
        Method::POST,
        "/api/v1/fs/resolve_targets",
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    answer["results"].clone()
}

async fn ticket_for(state: &Arc<AppState>, path: &std::path::Path) -> String {
    let (status, json) = request(
        state,
        Method::POST,
        "/api/v1/fs/ticket",
        Some(serde_json::json!({"path": path.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    json["ticket"].as_str().unwrap().to_string()
}

/// A GET with extra request headers and no bearer token (as an `<img>`).
async fn get_with(
    state: &Arc<AppState>,
    uri: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, axum::http::HeaderMap, bytes::Bytes) {
    let mut builder = Request::builder().uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let res = crate::app(state.clone())
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, headers, bytes)
}

#[tokio::test]
async fn resolve_targets_answers_every_target_in_one_request() {
    let state = test_state();
    let root = test_dir("embed-resolve");
    let docs = root.join("docs");
    std::fs::create_dir_all(docs.join("figs")).unwrap();
    std::fs::create_dir_all(root.join("assets")).unwrap();
    std::fs::write(docs.join("figs/plot.png"), png(640, 360)).unwrap();
    std::fs::write(docs.join("my plot.png"), png(10, 20)).unwrap();
    std::fs::write(docs.join("report.html"), "<h1>r</h1>").unwrap();
    std::fs::write(docs.join("notes.md"), "# notes").unwrap();
    std::fs::write(
        docs.join("chart.svg"),
        r#"<svg viewBox="0 0 400 300"></svg>"#,
    )
    .unwrap();
    std::fs::write(root.join("assets/logo.png"), png(32, 32)).unwrap();
    let (status, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ws}");
    let workspace_id = ws["id"].as_str().unwrap();

    let results = resolve(
        &state,
        serde_json::json!({
            "base": docs.to_string_lossy(),
            "workspace_id": workspace_id,
            "targets": [
                "figs/plot.png#xywh=0,0,10,10",
                "my%20plot.png",
                "report.html",
                "notes.md#Results",
                "chart.svg",
                "figs",
                "/assets/logo.png",
                "missing.png",
                "https://example.com/x.png",
                "figs/plot.png#xywh=0,0,10,10",
            ],
        }),
    )
    .await;
    let results = results.as_object().unwrap();
    assert_eq!(results.len(), 9, "duplicates answer once: {results:?}");

    let plot = &results["figs/plot.png#xywh=0,0,10,10"];
    assert_eq!(plot["path"], canon(&docs.join("figs/plot.png")));
    assert_eq!(plot["kind"], "file");
    assert_eq!(plot["size"], 33);
    assert_eq!(plot["mime"], "image/png");
    assert_eq!(
        (plot["width"].as_u64(), plot["height"].as_u64()),
        (Some(640), Some(360))
    );
    assert!(plot["mtime_ms"].as_u64().unwrap() > 0);
    let ticket = plot["ticket"].as_str().unwrap();
    assert!(ticket.starts_with("t-"));
    // The version is the fs/file X-Mtime token, and the ticket is the very
    // one POST /fs/ticket answers for the same file version.
    let (_, headers, _) = request_bytes(
        &state,
        Method::GET,
        &format!(
            "/api/v1/fs/file?path={}&limit=1",
            urlencode(&docs.join("figs/plot.png").to_string_lossy())
        ),
        Some("test-token"),
    )
    .await;
    assert_eq!(plot["version"], header_str(&headers, "x-mtime"));
    assert_eq!(
        ticket_for(&state, &docs.join("figs/plot.png")).await,
        ticket
    );
    let (status, _, body) =
        request_bytes(&state, Method::GET, &format!("/raw/{ticket}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], &png(640, 360)[..]);

    assert_eq!(
        results["my%20plot.png"]["path"],
        canon(&docs.join("my plot.png"))
    );
    assert_eq!(results["my%20plot.png"]["height"], 20);
    assert!(results["report.html"]["ticket"].is_string());
    assert!(results["report.html"].get("width").is_none());
    let notes = &results["notes.md#Results"];
    assert_eq!(notes["kind"], "file");
    assert!(
        notes.get("ticket").is_none(),
        "markdown loads through the API"
    );
    assert_eq!(
        (
            results["chart.svg"]["width"].as_u64(),
            results["chart.svg"]["height"].as_u64()
        ),
        (Some(400), Some(300))
    );
    assert_eq!(results["figs"]["kind"], "dir");
    assert!(results["figs"].get("ticket").is_none());
    // Root-relative: GitHub's reading, against the workspace root.
    assert_eq!(
        results["/assets/logo.png"]["path"],
        canon(&root.join("assets/logo.png"))
    );
    assert_eq!(results["missing.png"], serde_json::json!({"missing": true}));
    assert_eq!(
        results["https://example.com/x.png"],
        serde_json::json!({"missing": true})
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn resolve_targets_is_strict_and_tries_bases_in_order() {
    let state = test_state();
    let cwd = test_dir("embed-cwd");
    let spawn = test_dir("embed-spawn");
    std::fs::create_dir_all(cwd.join("sub")).unwrap();
    std::fs::write(spawn.join("only-spawn.csv"), "a,b\n1,2\n").unwrap();
    std::fs::write(cwd.join("sub/x.csv"), "a\n").unwrap();

    let results = resolve(
        &state,
        serde_json::json!({
            "base": cwd.to_string_lossy(),
            "bases": ["relative/ignored", spawn.to_string_lossy()],
            "targets": ["only-spawn.csv", "x.csv", "b/sub/x.csv", "sub/x.csv"],
        }),
    )
    .await;
    assert_eq!(
        results["only-spawn.csv"]["path"],
        canon(&spawn.join("only-spawn.csv"))
    );
    // No basename guess, no diff-prefix strip: an embed names one file.
    assert_eq!(results["x.csv"], serde_json::json!({"missing": true}));
    assert_eq!(results["b/sub/x.csv"], serde_json::json!({"missing": true}));
    assert_eq!(results["sub/x.csv"]["path"], canon(&cwd.join("sub/x.csv")));

    // A relative base is a 400.
    let (status, _) = request(
        &state,
        Method::POST,
        "/api/v1/fs/resolve_targets",
        Some(serde_json::json!({"base": "rel", "targets": []})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Bearer-authed like every other API route.
    let (status, _, _) =
        request_bytes(&state, Method::POST, "/api/v1/fs/resolve_targets", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    std::fs::remove_dir_all(&cwd).ok();
    std::fs::remove_dir_all(&spawn).ok();
}

#[tokio::test]
async fn raw_is_cacheable_and_revalidates_to_304() {
    let state = test_state();
    let root = test_dir("embed-cache");
    let path = root.join("plot.png");
    std::fs::write(&path, png(8, 8)).unwrap();
    let ticket = ticket_for(&state, &path).await;
    let uri = format!("/raw/{ticket}");

    let (status, headers, body) = get_with(&state, &uri, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.len(), 33);
    let etag = header_str(&headers, "etag").to_string();
    assert!(etag.starts_with('"') && etag.ends_with("-33\""), "{etag}");
    let cache = header_str(&headers, "cache-control");
    let max_age: u64 = cache
        .strip_prefix("private, max-age=")
        .unwrap_or_else(|| panic!("{cache}"))
        .parse()
        .unwrap();
    assert!((590..=600).contains(&max_age), "{cache}");
    assert!(header_str(&headers, "last-modified").ends_with(" GMT"));

    // The browser revalidates with the tag it holds: 304, no body, same tag.
    for sent in [
        etag.clone(),
        format!("W/{etag}"),
        format!("\"x\", {etag}"),
        "*".into(),
    ] {
        let (status, headers, body) = get_with(&state, &uri, &[("if-none-match", &sent)]).await;
        assert_eq!(status, StatusCode::NOT_MODIFIED, "{sent}");
        assert!(body.is_empty());
        assert_eq!(header_str(&headers, "etag"), etag);
        assert!(header_str(&headers, "cache-control").starts_with("private, max-age="));
    }
    let (status, _, _) = get_with(&state, &uri, &[("if-none-match", "\"stale-1\"")]).await;
    assert_eq!(status, StatusCode::OK);

    // Re-minting for the unchanged file answers the same URL…
    assert_eq!(ticket_for(&state, &path).await, ticket);
    // …a changed file answers a new one, and the old URL's tag moves on, so
    // a held copy can never stand for the new bytes.
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&path, png(16, 16)).unwrap();
    let fresh = ticket_for(&state, &path).await;
    assert_ne!(fresh, ticket);
    let (status, headers, _) = get_with(&state, &uri, &[("if-none-match", &etag)]).await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(header_str(&headers, "etag"), etag);
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn raw_asset_serves_a_reports_folder_and_nothing_outside_it() {
    use std::os::unix::fs::symlink;
    let state = test_state();
    let root = test_dir("embed-assets");
    let report = root.join("report");
    std::fs::create_dir_all(report.join("figs")).unwrap();
    std::fs::create_dir_all(report.join("pages")).unwrap();
    std::fs::create_dir_all(report.join(".git")).unwrap();
    std::fs::create_dir_all(root.join("outside")).unwrap();
    std::fs::write(
        report.join("index.html"),
        r#"<script src="app.js"></script><img src="figs/a.png">"#,
    )
    .unwrap();
    std::fs::write(report.join("app.js"), "console.log(1)").unwrap();
    std::fs::write(report.join("figs/a.png"), png(4, 4)).unwrap();
    std::fs::write(report.join("pages/other.html"), "<p>two</p>").unwrap();
    std::fs::write(report.join(".env"), "SECRET=1").unwrap();
    std::fs::write(report.join(".git/config"), "[core]").unwrap();
    std::fs::write(root.join("secret.txt"), "top secret").unwrap();
    std::fs::write(root.join("outside/x.txt"), "outside").unwrap();
    symlink(root.join("secret.txt"), report.join("link.js")).unwrap();
    symlink(root.join("outside"), report.join("sub")).unwrap();

    let ticket = ticket_for(&state, &report.join("index.html")).await;
    let get = |rest: &str| {
        let state = state.clone();
        let uri = format!("/raw/{ticket}/{rest}");
        async move { get_with(&state, &uri, &[]).await }
    };

    // The page itself, by name: the frame's relative URLs land beside it.
    let (status, headers, _) = get("index.html").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&headers, "content-security-policy"),
        "sandbox allow-scripts"
    );
    let (status, headers, body) = get("app.js").await;
    assert_eq!(status, StatusCode::OK);
    assert!(header_str(&headers, "content-type").contains("javascript"));
    assert_eq!(&body[..], b"console.log(1)");
    // No CORS: the origin-less frame may load, never read, its neighbors.
    assert!(headers.get("access-control-allow-origin").is_none());
    let (status, headers, _) = get("figs/a.png").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header_str(&headers, "content-type"), "image/png");
    // Same cache rules as /raw.
    let etag = header_str(&headers, "etag").to_string();
    let uri = format!("/raw/{ticket}/figs/a.png");
    let (status, _, _) = get_with(&state, &uri, &[("if-none-match", &etag)]).await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    // A nested page is sandboxed too.
    let (status, headers, _) = get("pages/other.html").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&headers, "content-security-policy"),
        "sandbox allow-scripts"
    );
    // Ranges work (a video beside the report).
    let (status, _, body) = get_with(&state, &uri, &[("range", "bytes=0-3")]).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(&body[..], b"\x89PNG");

    // Confinement: up, hidden, symlinked (file or directory), missing.
    for escape in [
        "../secret.txt",
        "%2e%2e/secret.txt",
        "figs/..%2F..%2Fsecret.txt",
        "figs/../../secret.txt",
        ".env",
        ".git/config",
        "figs/./a.png",
        "figs//a.png",
        "link.js",
        "sub/x.txt",
        "missing.js",
        "figs",
    ] {
        let (status, _, body) = get(escape).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{escape} leaked {body:?}");
    }

    // Only an HTML document's ticket opens its folder.
    let image_ticket = ticket_for(&state, &report.join("figs/a.png")).await;
    let (status, _, _) = get_with(&state, &format!("/raw/{image_ticket}/a.png"), &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // An unknown ticket is a 404, not a fallthrough to the app shell.
    let (status, _, _) = get_with(
        &state,
        "/raw/t-00000000000000000000000000000000/app.js",
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    std::fs::remove_dir_all(&root).ok();
}

/// Every `/raw` response says `nosniff`; a report's folder is sandboxed
/// whatever the type (an `evil.xml` holding an XHTML `<script>` would
/// otherwise run in the daemon's origin), and markup types are sandboxed on
/// the plain ticket route too — on 304 and 416 as well as 200.
#[tokio::test]
async fn raw_sandboxes_markup_and_every_report_asset() {
    let state = test_state();
    let root = test_dir("embed-sandbox");
    let report = root.join("report");
    std::fs::create_dir_all(report.join("pages")).unwrap();
    std::fs::write(report.join("index.html"), "<p>report</p>").unwrap();
    let evil = r#"<html xmlns="http://www.w3.org/1999/xhtml"><script>alert(document.domain)</script></html>"#;
    for name in [
        "evil.xml",
        "evil.xsl",
        "evil.svg",
        "evil.mml",
        "evil.xhtml",
        "evil.rss",
    ] {
        std::fs::write(report.join(name), evil).unwrap();
    }
    std::fs::write(report.join("app.js"), "console.log(1)").unwrap();
    std::fs::write(report.join("style.css"), "p{}").unwrap();
    std::fs::write(report.join("a.png"), png(2, 2)).unwrap();
    std::fs::write(report.join("blob.bin"), "<script>x()</script>").unwrap();
    std::fs::write(report.join("pages/other.html"), "<p>two</p>").unwrap();

    let ticket = ticket_for(&state, &report.join("index.html")).await;
    // Under the report's folder: HTML keeps its scripts (in the sandbox's
    // opaque origin, like the report); everything else is script-less.
    for (rest, csp) in [
        ("evil.xml", "sandbox"),
        ("evil.xsl", "sandbox"),
        ("evil.svg", "sandbox"),
        ("evil.mml", "sandbox"),
        ("evil.rss", "sandbox"),
        ("app.js", "sandbox"),
        ("style.css", "sandbox"),
        ("a.png", "sandbox"),
        ("blob.bin", "sandbox"),
        ("evil.xhtml", "sandbox allow-scripts"),
        ("pages/other.html", "sandbox allow-scripts"),
        ("index.html", "sandbox allow-scripts"),
    ] {
        let uri = format!("/raw/{ticket}/{rest}");
        let (status, headers, _) = get_with(&state, &uri, &[]).await;
        assert_eq!(status, StatusCode::OK, "{rest}");
        assert_eq!(header_str(&headers, "x-content-type-options"), "nosniff");
        assert_eq!(
            header_str(&headers, "content-security-policy"),
            csp,
            "{rest}"
        );
        assert_eq!(header_str(&headers, "referrer-policy"), "no-referrer");
        // A revalidation and an unsatisfiable range carry the same guard.
        let etag = header_str(&headers, "etag").to_string();
        let (status, headers, _) = get_with(&state, &uri, &[("if-none-match", &etag)]).await;
        assert_eq!(status, StatusCode::NOT_MODIFIED, "{rest}");
        assert_eq!(header_str(&headers, "x-content-type-options"), "nosniff");
        assert_eq!(header_str(&headers, "content-security-policy"), csp);
        let (status, headers, _) = get_with(&state, &uri, &[("range", "bytes=999999-")]).await;
        assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE, "{rest}");
        assert_eq!(header_str(&headers, "x-content-type-options"), "nosniff");
        assert_eq!(header_str(&headers, "content-security-policy"), csp);
    }
    let (status, headers, _) = get_with(&state, &format!("/raw/{ticket}/nope.js"), &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(header_str(&headers, "x-content-type-options"), "nosniff");

    // The plain ticket route: markup is sandboxed, script-less unless it is
    // HTML; types that never render as a document only get `nosniff`.
    for (name, csp) in [
        ("evil.xml", Some("sandbox")),
        ("evil.xsl", Some("sandbox")),
        ("evil.svg", Some("sandbox")),
        ("evil.mml", Some("sandbox")),
        ("evil.rss", Some("sandbox")),
        ("evil.xhtml", Some("sandbox allow-scripts")),
        ("index.html", Some("sandbox allow-scripts")),
        ("a.png", None),
        ("app.js", None),
        ("blob.bin", None),
    ] {
        let ticket = ticket_for(&state, &report.join(name)).await;
        let (status, headers, _) = get_with(&state, &format!("/raw/{ticket}"), &[]).await;
        assert_eq!(status, StatusCode::OK, "{name}");
        assert_eq!(header_str(&headers, "x-content-type-options"), "nosniff");
        assert_eq!(
            headers
                .get("content-security-policy")
                .map(|v| v.to_str().unwrap()),
            csp,
            "{name}"
        );
    }
    let (status, headers, _) =
        get_with(&state, "/raw/t-00000000000000000000000000000000", &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(header_str(&headers, "x-content-type-options"), "nosniff");
    std::fs::remove_dir_all(&root).ok();
}

/// A frame addresses its page by the canonical name the ticket mint answers:
/// an HTML report opened through a symlink, or under a hidden name, loads
/// itself and its assets; hidden names stay refused for every OTHER file.
#[tokio::test]
async fn raw_asset_serves_the_tickets_own_file_by_its_canonical_name() {
    use std::os::unix::fs::symlink;
    let state = test_state();
    let root = test_dir("embed-own-name");
    let run = root.join("runs/42");
    std::fs::create_dir_all(run.join("figs")).unwrap();
    std::fs::write(run.join("report.html"), "<img src=figs/a.png>").unwrap();
    std::fs::write(run.join("figs/a.png"), png(3, 3)).unwrap();
    std::fs::write(run.join(".summary.html"), "<p>summary</p>").unwrap();
    std::fs::write(run.join(".env"), "SECRET=1").unwrap();
    symlink(run.join("report.html"), root.join("latest.html")).unwrap();

    let mint = |path: std::path::PathBuf| {
        let state = state.clone();
        async move {
            let (status, json) = request(
                &state,
                Method::POST,
                "/api/v1/fs/ticket",
                Some(serde_json::json!({"path": path.to_string_lossy()})),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{json}");
            (
                json["ticket"].as_str().unwrap().to_string(),
                json["name"].as_str().unwrap().to_string(),
            )
        }
    };

    // Through a symlink: the ticket (and its name) is the target's.
    let (ticket, name) = mint(root.join("latest.html")).await;
    assert_eq!(name, "report.html");
    let (status, _, body) = get_with(&state, &format!("/raw/{ticket}/{name}"), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"<img src=figs/a.png>");
    let (status, _, _) = get_with(&state, &format!("/raw/{ticket}/figs/a.png"), &[]).await;
    assert_eq!(status, StatusCode::OK);
    // The link's own name is not in the target's folder.
    let (status, _, _) = get_with(&state, &format!("/raw/{ticket}/latest.html"), &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A hidden page is served by its own name, sandboxed like any report…
    let (ticket, name) = mint(run.join(".summary.html")).await;
    assert_eq!(name, ".summary.html");
    let (status, headers, body) =
        get_with(&state, &format!("/raw/{ticket}/.summary.html"), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"<p>summary</p>");
    assert_eq!(
        header_str(&headers, "content-security-policy"),
        "sandbox allow-scripts"
    );
    let (status, _, _) = get_with(&state, &format!("/raw/{ticket}/figs/a.png"), &[]).await;
    assert_eq!(status, StatusCode::OK);
    // …but its neighbors' hidden names stay refused,
    let (status, _, _) = get_with(&state, &format!("/raw/{ticket}/.env"), &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // and another page's ticket never opens it by name.
    let (report_ticket, _) = mint(run.join("report.html")).await;
    for rest in [".summary.html", "%2esummary.html", "figs/../.summary.html"] {
        let (status, _, _) = get_with(&state, &format!("/raw/{report_ticket}/{rest}"), &[]).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{rest}");
    }
    std::fs::remove_dir_all(&root).ok();
}
