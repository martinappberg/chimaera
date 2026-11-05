//! Unit tests for the Chromium engine's pure parts: discovery, key maps,
//! eval wrapping, CDP events into the rings, startup diagnostics.

use super::*;

fn shared() -> (PageShared, mpsc::Receiver<Event>) {
    let (tx, rx) = mpsc::channel(FETCH_QUEUE);
    (PageShared::new("T1".to_string(), tx), rx)
}

fn ev(method: &str, params: Value) -> Event {
    Event {
        method: method.to_string(),
        params,
        session: Some("S".to_string()),
    }
}

#[test]
fn http_targets_parse_host_and_port() {
    assert_eq!(
        http_target("http://127.0.0.1:8888/lab?x=1"),
        Some(("127.0.0.1".into(), 8888))
    );
    assert_eq!(
        http_target("https://node12/x"),
        Some(("node12".into(), 443))
    );
    assert_eq!(
        http_target("http://localhost"),
        Some(("localhost".into(), 80))
    );
    assert_eq!(
        http_target("http://user:pw@evil.example:81/"),
        Some(("evil.example".into(), 81))
    );
    assert_eq!(
        http_target("http://[::1]:3000/"),
        Some(("::1".into(), 3000))
    );
    assert_eq!(http_target("http://[::1]/"), Some(("::1".into(), 80)));
    assert_eq!(http_target("file:///etc/passwd"), None);
    assert_eq!(http_target("http://h:notaport/"), None);
    assert_eq!(http_target("http:///path"), None);
}

#[test]
fn named_keys_and_characters_map_to_cdp() {
    let enter = key_def("Enter", false).unwrap();
    assert_eq!(
        (enter.code.as_str(), enter.key_code, enter.text.as_deref()),
        ("Enter", 13, Some("\r"))
    );
    assert_eq!(key_def("ArrowLeft", false).unwrap().key_code, 37);
    assert_eq!(key_def("F5", false).unwrap().key_code, 116);
    let a = key_def("a", true).unwrap();
    assert_eq!(
        (a.key.as_str(), a.code.as_str(), a.key_code),
        ("A", "KeyA", 65)
    );
    assert_eq!(key_def("?", false).unwrap().code, "Slash");
    assert_eq!(key_def("é", false).unwrap().key_code, 0);
    assert!(key_def("Hyper", false).is_none());

    let events = key_events(&key_def("a", false).unwrap(), &[]);
    assert_eq!(events[0]["type"], "keyDown");
    assert_eq!(events[0]["text"], "a");
    assert_eq!(events[1]["type"], "keyUp");
    // A command chord types nothing.
    let events = key_events(&key_def("a", false).unwrap(), &[Modifier::Ctrl]);
    assert_eq!(events[0]["type"], "rawKeyDown");
    assert!(events[0].get("text").is_none());
    assert_eq!(events[0]["modifiers"], 2);
    let events = key_events(&key_def("Tab", false).unwrap(), &[Modifier::Shift]);
    assert_eq!(events[0]["type"], "rawKeyDown");
    assert_eq!(events[0]["modifiers"], 8);
    assert_eq!(
        modifier_bits(&[
            Modifier::Alt,
            Modifier::Ctrl,
            Modifier::Meta,
            Modifier::Shift
        ]),
        15
    );
}

#[test]
fn eval_wraps_the_body_with_named_args() {
    let e = eval_expression("return a + b;", &json!({"a": 1, "b": "x"})).unwrap();
    assert!(e.starts_with(
        "(async function(a, b, args) {\nreturn a + b;\n}).call(globalThis, 1, \"x\", {\"a\":1,\"b\":\"x\"})"
    ));
    assert!(e.contains("JSON.stringify"));
    let e = eval_expression("return 1;", &Value::Null).unwrap();
    assert!(e.contains("(async function(args)"));
    assert!(e.contains(".call(globalThis, {})"));
    // A key named `args` keeps its own value.
    let e = eval_expression("return args;", &json!({"args": 3})).unwrap();
    assert!(e.starts_with("(async function(args) {\nreturn args;\n}).call(globalThis, 3)"));
    assert!(eval_expression("", &json!({"a-b": 1})).is_err());
    assert!(eval_expression("", &json!({"this": 1})).is_err());
    assert!(eval_expression("", &json!([1])).is_err());
}

#[test]
fn eval_results_and_exceptions() {
    let ok = json!({"result": {"type": "string", "value": "{\"n\":43}"}});
    assert_eq!(eval_result(&ok).unwrap(), json!({"n": 43}));
    let thrown = json!({
        "result": {"type": "object"},
        "exceptionDetails": {"text": "Uncaught (in promise)", "exception": {"description": "Error: boom\n    at x"}}
    });
    match eval_result(&thrown) {
        Err(BrowserError::Page(m)) => assert!(m.contains("Error: boom"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn rings_cap_entries_and_page_by_id() {
    let mut ring: Ring<ConsoleEntry> = Ring::default();
    for i in 0..(RING + 5) {
        ring.push(|id| ConsoleEntry {
            id,
            level: "log".into(),
            text: i.to_string(),
            at: 0.0,
        });
    }
    assert_eq!(ring.items.len(), RING);
    assert_eq!(ring.next, RING as u64 + 6);
    let all = ring.since(0);
    assert_eq!(all.first().unwrap().id, 6);
    let tail = ring.since(RING as u64 + 4);
    assert_eq!(tail.iter().map(|e| e.id).collect::<Vec<_>>(), [304, 305]);
    assert!(ring.since(ring.next).is_empty());
}

#[test]
fn console_and_exceptions_fill_the_console_ring() {
    let (s, _rx) = shared();
    s.on_event(ev(
        "Runtime.consoleAPICalled",
        json!({"type": "warning", "timestamp": 5.0, "args": [
            {"type": "string", "value": "count"},
            {"type": "number", "value": 3, "description": "3"},
            {"type": "object", "subtype": "null", "value": null},
            {"type": "object", "className": "Object", "description": "Object",
             "preview": {"properties": [{"name": "a", "type": "number", "value": "1"}, {"name": "b", "type": "string", "value": "x"}]}}
        ]}),
    ), None);
    s.on_event(
        ev(
            "Runtime.consoleAPICalled",
            json!({"type": "endGroup", "args": []}),
        ),
        None,
    );
    s.on_event(ev(
        "Runtime.exceptionThrown",
        json!({"timestamp": 6.0, "exceptionDetails": {"text": "Uncaught", "url": "http://h/app.js", "lineNumber": 9,
               "exception": {"description": "TypeError: x is undefined"}}}),
    ), None);
    s.on_event(
        ev(
            "Runtime.consoleAPICalled",
            json!({"type": "log", "args": [{"type": "string", "value": "é".repeat(2000)}]}),
        ),
        None,
    );
    let st = lock(&s.st);
    let c = st.console.since(0);
    assert_eq!(c.len(), 3);
    assert_eq!(
        (c[0].level.as_str(), c[0].text.as_str(), c[0].at),
        ("warn", "count 3 null {a: 1, b: \"x\"}", 5.0)
    );
    assert_eq!(c[1].level, "exception");
    assert_eq!(c[1].text, "TypeError: x is undefined (http://h/app.js:10)");
    assert!(c[2].text.len() <= TEXT_MAX);
    assert!(c[2].text.ends_with('…'));
}

#[test]
fn network_events_record_failures_slow_requests_and_inflight() {
    let (s, _rx) = shared();
    let send =
        |id: &str, url: &str, ts: f64, kind: &str| {
            s.on_event(ev(
            "Network.requestWillBeSent",
            json!({"requestId": id, "request": {"url": url, "method": "GET"}, "timestamp": ts,
                   "wallTime": 1000.0, "type": kind, "frameId": "T1"}),
        ), None);
        };
    send("1", "http://h/missing", 1.0, "Fetch");
    send("2", "http://h/slow", 1.0, "XHR");
    send("3", "http://h/down", 1.0, "Fetch");
    send("4", "http://h/gone", 1.0, "Fetch");
    send("5", "http://h/", 1.0, "Document");
    send("6", "data:text/plain,x", 1.0, "Fetch");
    s.on_event(
        ev(
            "Network.requestWillBeSent",
            json!({"requestId": "7", "request": {"url": "http://other/frame", "method": "GET"},
                   "timestamp": 1.0, "type": "Document", "frameId": "sub"}),
        ),
        None,
    );
    assert_eq!(lock(&s.st).requests.len(), 5);
    s.on_event(ev(
        "Network.responseReceived",
        json!({"requestId": "1", "timestamp": 1.2, "type": "Fetch", "frameId": "T1", "response": {"status": 404}}),
    ), None);
    s.on_event(
        ev(
            "Network.loadingFinished",
            json!({"requestId": "1", "timestamp": 1.3}),
        ),
        None,
    );
    s.on_event(
        ev(
            "Network.loadingFinished",
            json!({"requestId": "2", "timestamp": 2.5}),
        ),
        None,
    );
    s.on_event(
        ev(
            "Network.loadingFailed",
            json!({"requestId": "3", "timestamp": 1.1, "errorText": "net::ERR_CONNECTION_REFUSED"}),
        ),
        None,
    );
    s.on_event(ev(
        "Network.loadingFailed",
        json!({"requestId": "4", "timestamp": 1.1, "errorText": "net::ERR_ABORTED", "canceled": true}),
    ), None);
    s.on_event(ev(
        "Network.responseReceived",
        json!({"requestId": "5", "timestamp": 1.1, "type": "Document", "frameId": "T1", "response": {"status": 200}}),
    ), None);
    let st = lock(&s.st);
    let n = st.network.since(0);
    assert_eq!(n.len(), 3, "{n:?}");
    assert_eq!(
        (n[0].url.as_str(), n[0].status, n[0].ms),
        ("http://h/missing", Some(404), Some(200.0))
    );
    assert_eq!(
        (n[1].url.as_str(), n[1].ms),
        ("http://h/slow", Some(1500.0))
    );
    assert_eq!(n[2].error.as_deref(), Some("net::ERR_CONNECTION_REFUSED"));
    assert_eq!(n[0].at, 1_000_000.0);
    assert_eq!(st.doc_status, Some(200));
    // Only the main document is still loading.
    assert_eq!(st.requests.len(), 1);
}

#[test]
fn navigation_events_drive_the_settle_counters() {
    let (s, _rx) = shared();
    s.on_event(
        ev("Page.frameStartedLoading", json!({"frameId": "T1"})),
        None,
    );
    s.on_event(
        ev("Page.frameStartedLoading", json!({"frameId": "sub"})),
        None,
    );
    s.on_event(
        ev(
            "Page.frameNavigated",
            json!({"frame": {"id": "sub", "parentId": "T1", "url": "http://h/frame"}}),
        ),
        None,
    );
    s.on_event(
        ev(
            "Page.frameNavigated",
            json!({"frame": {"id": "T1", "url": "http://h/", "loaderId": "L"}}),
        ),
        None,
    );
    {
        let st = lock(&s.st);
        assert_eq!((st.load_seq, st.nav_seq, st.loading), (1, 1, true));
        assert_eq!(st.url, "http://h/");
    }
    s.on_event(
        ev("Page.frameStoppedLoading", json!({"frameId": "T1"})),
        None,
    );
    s.on_event(
        ev(
            "Page.navigatedWithinDocument",
            json!({"frameId": "T1", "url": "http://h/#x"}),
        ),
        None,
    );
    s.on_event(ev(
        "Runtime.executionContextCreated",
        json!({"context": {"id": 7, "name": WORLD, "auxData": {"frameId": "T1", "isDefault": false}}}),
    ), None);
    {
        let st = lock(&s.st);
        assert!(!st.loading);
        assert_eq!(st.nav_seq, 2);
        assert_eq!(st.url, "http://h/#x");
        assert_eq!(st.isolated_ctx, Some(7));
    }
    s.on_event(ev("Runtime.executionContextsCleared", json!({})), None);
    assert_eq!(lock(&s.st).isolated_ctx, None);
}

#[tokio::test]
async fn fetch_pauses_and_dialogs_go_to_the_page_task_and_waits_wake() {
    let (s, mut rx) = shared();
    let s = Arc::new(s);
    s.on_event(
        ev(
            "Fetch.requestPaused",
            json!({"requestId": "f1", "request": {"url": "http://x/"}}),
        ),
        None,
    );
    assert_eq!(rx.recv().await.unwrap().method, "Fetch.requestPaused");

    let waiter = {
        let s = Arc::clone(&s);
        tokio::spawn(async move {
            s.wait(Instant::now() + Duration::from_secs(5), |st| {
                st.blocked_seq > 0
            })
            .await
        })
    };
    tokio::task::yield_now().await;
    s.note_blocked("GET", "http://evil.example/", "outside the allowlist");
    assert!(waiter.await.unwrap());
    {
        let st = lock(&s.st);
        assert_eq!(
            st.blocked.as_deref(),
            Some("Blocked a navigation to http://evil.example/: outside the allowlist")
        );
        assert_eq!(
            st.network.since(0)[0].error.as_deref(),
            Some("blocked: outside the allowlist")
        );
    }
    assert!(
        !s.wait(Instant::now() + Duration::from_millis(30), |st| st.crashed)
            .await
    );
}

#[test]
fn startup_failures_are_explained() {
    let exe = Path::new("/opt/chrome/chrome");
    let msg = explain_startup(
        exe,
        "[0101/1:FATAL:zygote_host_impl_linux.cc(128)] No usable sandbox! Update your kernel",
        &CdpError::Gone("pipe closed".into()),
    );
    assert!(msg.contains("sandbox"), "{msg}");
    assert!(msg.contains("does not turn the sandbox off"));
    assert!(msg.contains("No usable sandbox"));
    let msg = explain_startup(
        exe,
        "/opt/chrome/chrome: error while loading shared libraries: libnss3.so: cannot open shared object file",
        &CdpError::Gone("pipe closed".into()),
    );
    assert!(msg.contains("missing: libnss3.so"), "{msg}");
    assert!(
        msg.contains("chrome-headless-shell, which needs fewer"),
        "{msg}"
    );
    // Already the headless shell: never "use chrome-headless-shell".
    let msg = explain_startup(
        Path::new("/t/chrome-headless-shell-linux64/chrome-headless-shell"),
        "chrome-headless-shell: error while loading shared libraries: libnss3.so: cannot open",
        &CdpError::Gone("pipe closed".into()),
    );
    assert!(msg.contains("missing: libnss3.so"), "{msg}");
    assert!(!msg.contains("use chrome-headless-shell"), "{msg}");
    let msg = explain_startup(
        exe,
        "Creating shared memory in /dev/shm/.org.chromium.Chromium.x failed",
        &CdpError::Timeout,
    );
    assert!(msg.contains("within 20 s"));
    assert!(msg.contains("/dev/shm"));
    let msg = explain_startup(
        exe,
        "[1105:ERROR:zygote_host_impl_linux.cc:117] Running as root without --no-sandbox is not supported.",
        &CdpError::Gone("x".into()),
    );
    assert!(msg.contains("runs as root"), "{msg}");
    let msg = explain_startup(exe, "", &CdpError::Gone("x".into()));
    assert!(!msg.contains("last output"));
}

#[test]
fn discovery_prefers_the_setting_then_path() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!(
        "chimaera-chromium-discovery-{}-{}",
        std::process::id(),
        now_ms() as u64
    ));
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let make = |p: &Path, mode: u32| {
        std::fs::write(p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    make(&bin.join("chromium"), 0o755);
    make(&bin.join("google-chrome"), 0o644); // not executable: skipped
    let custom = dir.join("my-chrome");
    make(&custom, 0o755);
    let path_var = std::env::join_paths([bin.clone()]).unwrap();

    assert_eq!(
        find_browser(None, &path_var, None).unwrap(),
        bin.join("chromium")
    );
    assert_eq!(
        find_browser(Some(custom.to_str().unwrap()), &path_var, None).unwrap(),
        custom
    );
    assert_eq!(
        find_browser(Some("chromium"), &path_var, None).unwrap(),
        bin.join("chromium")
    );
    assert_eq!(
        find_browser(Some("~/my-chrome"), &path_var, Some(&dir)).unwrap(),
        custom
    );
    let err = find_browser(Some("/nowhere/chrome"), &path_var, None).unwrap_err();
    assert!(err.contains("browser.path"), "{err}");
    if !cfg!(target_os = "macos") {
        let empty = std::env::join_paths([dir.join("empty")]).unwrap();
        let err = find_browser(None, &empty, None).unwrap_err();
        assert!(err.contains("set browser.path"), "{err}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn small_helpers() {
    assert_eq!(cap("abc", 10), "abc");
    let s = cap(&"é".repeat(10), 8);
    assert!(s.len() <= 8 && s.ends_with('…'), "{s}");
    assert_eq!(network_fs_name(0x6969), Some("NFS"));
    assert_eq!(network_fs_name(0x0BD0_0BD0), Some("Lustre"));
    assert_eq!(network_fs_name(0xEF53), None); // ext4
                                               // A 1×2 PNG header.
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    png.extend(1u32.to_be_bytes());
    png.extend(2u32.to_be_bytes());
    png.extend([8, 6, 0, 0, 0]);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
    assert_eq!(png_size(&b64), Some((1, 2)));
    assert_eq!(png_size("bm90IGEgcG5n"), None);
    assert_eq!(
        missing_library("x: error while loading shared libraries: libgbm.so.1: cannot open"),
        Some("libgbm.so.1".into())
    );
    let args = launch_args(Path::new("/run/p"));
    assert!(args.contains(&"--remote-debugging-pipe".to_string()));
    assert!(args.contains(&"--user-data-dir=/run/p".to_string()));
    assert!(!args.iter().any(|a| a.contains("no-sandbox")));
    assert!(!args
        .iter()
        .any(|a| a.starts_with("--remote-debugging-port")));
}

fn router() -> (Arc<Router>, mpsc::Receiver<PopupJob>) {
    let (to_popups, rx) = mpsc::channel(POPUP_QUEUE);
    (
        Arc::new(Router {
            conn: OnceLock::new(),
            sessions: Mutex::new(HashMap::new()),
            attached: Mutex::new(HashMap::new()),
            attached_tx: watch::channel(0).0,
            popups: Mutex::new(HashMap::new()),
            to_popups,
        }),
        rx,
    )
}

fn browser_ev(method: &str, params: Value) -> Event {
    Event {
        method: method.to_string(),
        params,
        session: None,
    }
}

#[tokio::test]
async fn popups_are_held_closed_and_reported_to_their_opener() {
    let (r, mut jobs) = router();
    let (page, _rx) = shared();
    let page = Arc::new(page);
    lock(&r.sessions).insert("S".into(), Arc::clone(&page));

    // Our own new target: parked until create_page claims it.
    r.on_event(browser_ev(
        "Target.attachedToTarget",
        json!({"sessionId": "S2", "waitingForDebugger": true,
               "targetInfo": {"targetId": "T2", "type": "page", "url": ""}}),
    ));
    assert_eq!(lock(&r.attached).get("T2").map(String::as_str), Some("S2"));
    assert!(jobs.recv().await.unwrap().opener.is_none());

    // A popup of T1: pending on the opener until its first document comes.
    r.on_event(browser_ev(
        "Target.attachedToTarget",
        json!({"sessionId": "P", "waitingForDebugger": true,
               "targetInfo": {"targetId": "TP", "type": "page", "url": "", "openerId": "T1"}}),
    ));
    let job = jobs.recv().await.unwrap();
    assert_eq!(
        (job.session.as_str(), job.opener.as_deref()),
        ("P", Some("T1"))
    );
    assert_eq!(lock(&page.st).popups_pending, 1);
    r.on_event(Event {
        method: "Fetch.requestPaused".into(),
        params: json!({"requestId": "r", "request": {"url": "http://elsewhere.example/x"}}),
        session: Some("P".into()),
    });
    assert!(lock(&r.popups).is_empty());
    let mut st = lock(&page.st);
    assert_eq!(st.popups_pending, 0);
    assert_eq!(
        st.take_notes().as_deref(),
        Some("A popup to http://elsewhere.example/x was blocked; use browser_navigate to open it.")
    );
    assert_eq!(st.take_notes(), None);
    drop(st);

    // Tabs that ran before auto-attach are left alone.
    r.on_event(browser_ev(
        "Target.attachedToTarget",
        json!({"sessionId": "S0", "waitingForDebugger": false,
               "targetInfo": {"targetId": "T0", "type": "page"}}),
    ));
    assert!(!lock(&r.attached).contains_key("T0"));
    // A page's session detaching marks it gone.
    r.on_event(browser_ev(
        "Target.detachedFromTarget",
        json!({"sessionId": "S"}),
    ));
    assert!(lock(&page.st).detached);
}

#[test]
fn notes_join_a_blocked_navigation_and_a_popup() {
    let (s, _rx) = shared();
    s.note_blocked("GET", "http://a.example/", "outside the allowlist");
    s.update(|st| st.popups_pending = 1);
    s.note_popup(None);
    let mut st = lock(&s.st);
    assert_eq!(
        st.take_notes().as_deref(),
        Some("Blocked a navigation to http://a.example/: outside the allowlist A popup window was blocked.")
    );
    assert_eq!(st.popups_pending, 0);
    assert_eq!(st.take_notes(), None);
}

#[tokio::test]
async fn a_full_fetch_queue_refuses_instead_of_growing() {
    let (tx, _rx) = mpsc::channel(1);
    let s = PageShared::new("T1".into(), tx);
    for id in ["a", "b"] {
        s.on_event(
            ev(
                "Fetch.requestPaused",
                json!({"requestId": id, "request": {"url": format!("http://h/{id}")}}),
            ),
            None,
        );
    }
    let n = lock(&s.st).network.since(0);
    assert_eq!(n.len(), 1);
    assert_eq!(n[0].url, "http://h/b");
    assert_eq!(
        n[0].error.as_deref(),
        Some("blocked: too many page loads at once")
    );
}

#[test]
fn subframe_blocks_stay_out_of_the_navigation_notes() {
    let (s, _rx) = shared();
    s.note_blocked_frame("GET", "http://evil.example/frame", "outside the allowlist");
    let mut st = lock(&s.st);
    assert_eq!(st.take_notes(), None);
    assert_eq!(st.network.since(0)[0].url, "http://evil.example/frame");
    assert!(local_document("about:srcdoc"));
    assert!(local_document("DATA:text/html,x"));
    assert!(local_document("blob:http://h/uuid"));
    assert!(!local_document("http://h/"));
}

#[test]
fn jpeg_sizes_come_from_the_frame_header() {
    // SOI, an APP0 segment, then SOF0 for 640×480.
    let mut jpg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00];
    jpg.extend([0xFF, 0xC0, 0x00, 0x11, 0x08, 0x01, 0xE0, 0x02, 0x80]);
    jpg.extend([0u8; 16]);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&jpg);
    assert_eq!(jpeg_size(&b64), Some((640, 480)));
    assert_eq!(image_size(&b64, ImageFormat::Jpeg), Some((640, 480)));
    assert_eq!(jpeg_size("bm90IGEganBlZw=="), None);
}
