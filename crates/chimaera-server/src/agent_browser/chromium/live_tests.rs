//! The Chromium engine against a real browser: ignored unless
//! `CHIMAERA_TEST_CHROMIUM` names a Chrome/Chromium/chrome-headless-shell.
//!
//! ```sh
//! CHIMAERA_TEST_CHROMIUM=/path/to/chrome cargo test -p chimaera-server \
//!   agent_browser::chromium::live_tests -- --ignored --nocapture
//! ```
//!
//! (An in-crate test, not `tests/`: the engine and `AppState` are
//! `pub(crate)`.)

use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;

const PAGE: &str = r#"<!doctype html>
<html><head><title>Fixture</title></head>
<body style="margin:0">
<button id="b" style="position:absolute;left:20px;top:20px;width:120px;height:40px"
  onclick="document.body.dataset.clicked='yes'; console.log('clicked')">Click me</button>
<input id="i" style="position:absolute;left:20px;top:100px;width:200px;height:30px">
<a id="away" href="http://blocked.invalid:81/" style="position:absolute;left:20px;top:160px">away</a>
<a id="next" href="/next" style="position:absolute;left:20px;top:200px">next</a>
<a id="pop" href="/popped" target="_blank" style="position:absolute;left:300px;top:20px">pop</a>
<iframe id="f1" src="http://blocked.invalid:81/frame" style="position:absolute;left:300px;top:100px;width:100px;height:60px"></iframe>
<iframe id="f2" src="http://localhost:{PORT}/framed" style="position:absolute;left:420px;top:100px;width:100px;height:60px"></iframe>
<div style="height:3000px"></div>
<script>
  window.pageVar = 42;
  console.log('hello from page', {a: 1});
  fetch('/missing');
  fetch('/slow');
</script>
</body></html>"#;

type Hits = Arc<std::sync::Mutex<Vec<String>>>;

/// A tiny HTTP/1.1 server on all loopback names: `/` the page, `/next` a
/// second page, `/framed` a frame, `/slow` after 1.2 s, `/redirect` to a
/// host off the allowlist, anything else 404. Every path asked is logged.
async fn serve() -> (u16, Hits) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits: Hits = Arc::default();
    let log = Arc::clone(&hits);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let log = Arc::clone(&log);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let mut n = 0;
                while !buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut buf[n..]).await {
                        Ok(0) | Err(_) => return,
                        Ok(k) => n += k,
                    }
                    if n == buf.len() {
                        return;
                    }
                }
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                log.lock().unwrap().push(path.clone());
                let (status, extra, body) = match path.as_str() {
                    "/" => ("200 OK", "", PAGE.replace("{PORT}", &port.to_string())),
                    "/framed" => ("200 OK", "", "<p>framed</p>".to_string()),
                    "/next" => (
                        "200 OK",
                        "",
                        "<!doctype html><title>Next</title><p>second page</p>".to_string(),
                    ),
                    "/slow" => {
                        tokio::time::sleep(Duration::from_millis(1200)).await;
                        ("200 OK", "", "slow".to_string())
                    }
                    "/redirect" => (
                        "302 Found",
                        "Location: http://example.invalid:8081/x\r\n",
                        String::new(),
                    ),
                    _ => ("404 Not Found", "", "nope".to_string()),
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (port, hits)
}

fn target(port: u16, path: &str) -> TargetUrl {
    TargetUrl {
        host: "127.0.0.1".into(),
        port,
        path: path.into(),
    }
}

async fn eval(c: &Chromium, s: &Arc<AppState>, world: World, body: &str, args: Value) -> Value {
    match c
        .act(
            s,
            None,
            "p",
            Action::Eval {
                world,
                body: body.into(),
                args,
                timeout_ms: 5000,
            },
        )
        .await
    {
        Ok(ActionOut::Value(v)) => v,
        other => panic!("eval {body}: {other:?}"),
    }
}

/// Center of the element `sel`, in viewport CSS px.
async fn center(c: &Chromium, s: &Arc<AppState>, sel: &str) -> (f64, f64) {
    let r = eval(
        c,
        s,
        World::Isolated,
        "const r = document.querySelector(sel).getBoundingClientRect(); return [r.x + r.width / 2, r.y + r.height / 2];",
        json!({"sel": sel}),
    )
    .await;
    (r[0].as_f64().unwrap(), r[1].as_f64().unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs CHIMAERA_TEST_CHROMIUM=<a Chrome or Chromium executable>"]
async fn drives_a_real_chromium() {
    let Some(exe) = std::env::var_os("CHIMAERA_TEST_CHROMIUM") else {
        eprintln!("CHIMAERA_TEST_CHROMIUM is not set; skipping");
        return;
    };
    let data = std::env::temp_dir().join(format!(
        "chimaera-chromium-live-{}-{}",
        std::process::id(),
        now_ms() as u64
    ));
    let config = data.join("config");
    let state = Arc::new(AppState::new(
        "test-token".into(),
        "testhost".into(),
        4242,
        0,
        data.clone(),
        config,
    ));
    let mut map = serde_json::Map::new();
    map.insert(
        "browser.path".into(),
        Value::String(exe.to_string_lossy().into_owned()),
    );
    lock(&state.settings).put(map).unwrap();

    let (port, hits) = serve().await;
    let c = Chromium::new();
    // The engine refuses while the login-node question is open; the core
    // runs this detection before it asks (tools.rs does).
    let _ = state.compute.detection().await;
    assert_eq!(
        c.available(&state, None).await.unwrap(),
        PathBuf::from(&exe),
        "discovery honours browser.path"
    );

    // An action before any Open: no page.
    assert!(matches!(
        c.act(
            &state,
            None,
            "p",
            Action::Rings {
                console_since: 0,
                network_since: 0
            }
        )
        .await,
        Err(BrowserError::Gone(_))
    ));

    let t0 = Instant::now();
    let out = c
        .act(
            &state,
            None,
            "p",
            Action::Open {
                url: target(port, "/"),
                width: 800,
                height: 600,
            },
        )
        .await
        .unwrap();
    eprintln!("open (incl. launch): {:?} -> {out:?}", t0.elapsed());
    let ActionOut::Value(v) = out else { panic!() };
    assert_eq!(v["url"], format!("http://127.0.0.1:{port}/"));
    assert_eq!(v["title"], "Fixture");
    assert_eq!(v["status"], 200);

    let (pgid, profile) = {
        let b = lock(&c.browser).clone().unwrap();
        (b.pgid, b.profile.clone())
    };
    assert!(profile.is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&profile).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    // Both worlds.
    let v = eval(
        &c,
        &state,
        World::Page,
        "return window.pageVar + n;",
        json!({"n": 1}),
    )
    .await;
    assert_eq!(v, json!(43));
    let v = eval(
        &c,
        &state,
        World::Isolated,
        "return [typeof window.pageVar, document.title];",
        json!({}),
    )
    .await;
    assert_eq!(v, json!(["undefined", "Fixture"]));
    let v = eval(
        &c,
        &state,
        World::Page,
        "await new Promise(r => setTimeout(r, 50)); return {ok: true};",
        Value::Null,
    )
    .await;
    assert_eq!(v, json!({"ok": true}));
    match c
        .act(
            &state,
            None,
            "p",
            Action::Eval {
                world: World::Page,
                body: "throw new Error('boom');".into(),
                args: Value::Null,
                timeout_ms: 2000,
            },
        )
        .await
    {
        Err(BrowserError::Page(m)) => assert!(m.contains("boom"), "{m}"),
        other => panic!("{other:?}"),
    }
    match c
        .act(
            &state,
            None,
            "p",
            Action::Eval {
                world: World::Page,
                body: "await new Promise(() => {});".into(),
                args: Value::Null,
                timeout_ms: 300,
            },
        )
        .await
    {
        Err(BrowserError::Timeout(_)) => {}
        other => panic!("{other:?}"),
    }

    // Click.
    let (x, y) = center(&c, &state, "#b").await;
    let out = c
        .act(
            &state,
            None,
            "p",
            Action::Click {
                x,
                y,
                clicks: 1,
                modifiers: vec![],
            },
        )
        .await
        .unwrap();
    assert_eq!(out, ActionOut::Done);
    let v = eval(
        &c,
        &state,
        World::Page,
        "return document.body.dataset.clicked;",
        Value::Null,
    )
    .await;
    assert_eq!(v, json!("yes"));

    // Type: inserted, then key by key, then a key chord.
    let (x, y) = center(&c, &state, "#i").await;
    c.act(
        &state,
        None,
        "p",
        Action::Click {
            x,
            y,
            clicks: 1,
            modifiers: vec![],
        },
    )
    .await
    .unwrap();
    c.act(
        &state,
        None,
        "p",
        Action::Text {
            text: "hello".into(),
            per_key: false,
        },
    )
    .await
    .unwrap();
    c.act(
        &state,
        None,
        "p",
        Action::Text {
            text: " wörld!".into(),
            per_key: true,
        },
    )
    .await
    .unwrap();
    c.act(
        &state,
        None,
        "p",
        Action::Key {
            key: "Backspace".into(),
            modifiers: vec![],
        },
    )
    .await
    .unwrap();
    let v = eval(
        &c,
        &state,
        World::Isolated,
        "return document.querySelector('#i').value;",
        Value::Null,
    )
    .await;
    assert_eq!(v, json!("hello wörld"));

    // Scroll and move.
    c.act(
        &state,
        None,
        "p",
        Action::Scroll {
            x: 300.0,
            y: 300.0,
            dx: 0.0,
            dy: 400.0,
        },
    )
    .await
    .unwrap();
    let scrolled = {
        let mut y = 0.0;
        for _ in 0..20 {
            y = eval(&c, &state, World::Page, "return scrollY;", Value::Null)
                .await
                .as_f64()
                .unwrap();
            if y > 0.0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        y
    };
    assert!(scrolled > 0.0, "the wheel scrolled the page");
    c.act(&state, None, "p", Action::Move { x: 10.0, y: 10.0 })
        .await
        .unwrap();

    // Screenshots: viewport downscaled, a clip, the full page.
    match c
        .act(
            &state,
            None,
            "p",
            Action::Screenshot {
                clip: None,
                full_page: false,
                max_width: 400,
                format: ImageFormat::Png,
                quality: None,
            },
        )
        .await
        .unwrap()
    {
        ActionOut::Png { b64, width, height } => {
            assert_eq!((width, height), (400, 300));
            assert!(b64.len() > 100);
        }
        other => panic!("{other:?}"),
    }
    match c
        .act(
            &state,
            None,
            "p",
            Action::Screenshot {
                clip: Some(Rect {
                    x: 10.0,
                    y: 10.0,
                    width: 100.0,
                    height: 50.0,
                }),
                full_page: false,
                max_width: 1600,
                format: ImageFormat::Png,
                quality: None,
            },
        )
        .await
        .unwrap()
    {
        ActionOut::Png { width, height, .. } => assert_eq!((width, height), (100, 50)),
        other => panic!("{other:?}"),
    }
    match c
        .act(
            &state,
            None,
            "p",
            Action::Screenshot {
                clip: None,
                full_page: true,
                max_width: 800,
                format: ImageFormat::Png,
                quality: None,
            },
        )
        .await
        .unwrap()
    {
        ActionOut::Png { width, height, .. } => {
            assert_eq!(width, 800);
            assert!(height >= 3000, "full page height {height}");
        }
        other => panic!("{other:?}"),
    }

    match c
        .act(
            &state,
            None,
            "p",
            Action::Screenshot {
                clip: None,
                full_page: false,
                max_width: 400,
                format: ImageFormat::Jpeg,
                quality: Some(50),
            },
        )
        .await
        .unwrap()
    {
        ActionOut::Png { b64, width, height } => {
            assert!(b64.starts_with("/9j/"), "a JPEG");
            assert_eq!((width, height), (400, 300));
        }
        other => panic!("{other:?}"),
    }

    // Frames: an allowlisted cross-site frame (localhost vs 127.0.0.1)
    // loaded; one off the allowlist was refused before it was sent.
    let mut framed = false;
    for _ in 0..40 {
        if hits.lock().unwrap().iter().any(|h| h == "/framed") {
            framed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(framed, "the allowlisted frame loaded");

    // Popups: a target=_blank click is held, refused unsent and reported.
    // (The page was scrolled above; the link sits at the top.)
    eval(
        &c,
        &state,
        World::Page,
        "scrollTo(0, 0); return 1;",
        Value::Null,
    )
    .await;
    let (x, y) = center(&c, &state, "#pop").await;
    match c
        .act(
            &state,
            None,
            "p",
            Action::Click {
                x,
                y,
                clicks: 1,
                modifiers: vec![],
            },
        )
        .await
        .unwrap()
    {
        ActionOut::Value(v) => {
            let note = v["blocked"].as_str().unwrap();
            assert!(
                note.contains(&format!(
                    "A popup to http://127.0.0.1:{port}/popped was blocked"
                )),
                "{note}"
            );
        }
        other => panic!("popup click: {other:?}"),
    }
    // One from script (no user gesture: Chrome's own blocker may stop it
    // first; the headless shell has none).
    eval(
        &c,
        &state,
        World::Page,
        "window.open('/popped2'); return 1;",
        Value::Null,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let pages = {
        let b = lock(&c.browser).clone().unwrap();
        let t = b
            .conn
            .call("Target.getTargets", json!({}), None, CALL)
            .await
            .unwrap();
        t["targetInfos"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["type"] == "page")
            .count()
    };
    assert_eq!(pages, 1, "no popup survives");
    {
        let h = hits.lock().unwrap();
        assert!(
            !h.iter().any(|p| p.starts_with("/popped")),
            "a popup's request was never sent: {h:?}"
        );
    }

    // Rings: the console messages, the 404 and the slow request.
    let mut rings = RingsOut::default();
    for _ in 0..40 {
        rings = match c
            .act(
                &state,
                None,
                "p",
                Action::Rings {
                    console_since: 0,
                    network_since: 0,
                },
            )
            .await
            .unwrap()
        {
            ActionOut::Rings(r) => r,
            other => panic!("{other:?}"),
        };
        if rings.network.iter().any(|n| n.url.ends_with("/slow")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    eprintln!("rings: {rings:#?}");
    assert!(rings
        .console
        .iter()
        .any(|e| e.text == "hello from page {a: 1}" && e.level == "log"));
    assert!(rings.console.iter().any(|e| e.text == "clicked"));
    assert!(rings
        .network
        .iter()
        .any(|n| n.url.ends_with("/missing") && n.status == Some(404)));
    let slow = rings
        .network
        .iter()
        .find(|n| n.url.ends_with("/slow"))
        .expect("slow entry");
    assert!(slow.ms.unwrap() >= 1000.0);
    assert_eq!(rings.inflight, 0);
    let frame = rings
        .network
        .iter()
        .find(|n| n.url == "http://blocked.invalid:81/frame")
        .expect("the refused frame is in the network ring");
    assert!(
        frame
            .error
            .as_deref()
            .unwrap()
            .starts_with("blocked: blocked.invalid:81 is outside the allowlist"),
        "{frame:?}"
    );
    let next = rings.console_next;
    match c
        .act(
            &state,
            None,
            "p",
            Action::Rings {
                console_since: next,
                network_since: rings.network_next,
            },
        )
        .await
        .unwrap()
    {
        ActionOut::Rings(r) => assert!(r.console.is_empty() && r.network.is_empty()),
        other => panic!("{other:?}"),
    }

    // The allowlist: a redirect off it, a direct navigation, and a link.
    match c
        .act(
            &state,
            None,
            "p",
            Action::Navigate {
                url: target(port, "/redirect"),
            },
        )
        .await
    {
        Err(BrowserError::Refused(m)) => {
            assert!(m.contains("example.invalid:8081"), "{m}");
            assert!(m.contains("allowlist"), "{m}");
        }
        other => panic!("redirect: {other:?}"),
    }
    let off = TargetUrl {
        host: "blocked.invalid".into(),
        port: 81,
        path: "/".into(),
    };
    match c
        .act(&state, None, "p", Action::Navigate { url: off })
        .await
    {
        Err(BrowserError::Refused(m)) => assert!(m.contains("blocked.invalid:81"), "{m}"),
        other => panic!("direct: {other:?}"),
    }
    c.act(
        &state,
        None,
        "p",
        Action::Navigate {
            url: target(port, "/"),
        },
    )
    .await
    .unwrap();
    let (x, y) = center(&c, &state, "#away").await;
    match c
        .act(
            &state,
            None,
            "p",
            Action::Click {
                x,
                y,
                clicks: 1,
                modifiers: vec![],
            },
        )
        .await
        .unwrap()
    {
        ActionOut::Value(v) => assert!(
            v["blocked"].as_str().unwrap().contains("blocked.invalid"),
            "{v}"
        ),
        other => panic!("link: {other:?}"),
    }

    // A link that's allowed navigates, and history goes back.
    c.act(
        &state,
        None,
        "p",
        Action::Navigate {
            url: target(port, "/"),
        },
    )
    .await
    .unwrap();
    let (x, y) = center(&c, &state, "#next").await;
    assert_eq!(
        c.act(
            &state,
            None,
            "p",
            Action::Click {
                x,
                y,
                clicks: 1,
                modifiers: vec![]
            }
        )
        .await
        .unwrap(),
        ActionOut::Done
    );
    let v = eval(
        &c,
        &state,
        World::Isolated,
        "return document.title;",
        Value::Null,
    )
    .await;
    assert_eq!(
        v,
        json!("Next"),
        "the click's navigation settled before the result"
    );
    match c
        .act(
            &state,
            None,
            "p",
            Action::History {
                op: HistoryOp::Back,
            },
        )
        .await
        .unwrap()
    {
        ActionOut::Value(v) => {
            assert_eq!(v["url"], format!("http://127.0.0.1:{port}/"));
            assert_eq!(v["title"], "Fixture");
        }
        other => panic!("{other:?}"),
    }
    match c
        .act(
            &state,
            None,
            "p",
            Action::History {
                op: HistoryOp::Reload,
            },
        )
        .await
        .unwrap()
    {
        ActionOut::Value(v) => assert_eq!(v["status"], 200),
        other => panic!("{other:?}"),
    }

    // A second page with its own viewport; then close it.
    match c
        .act(
            &state,
            None,
            "q",
            Action::Open {
                url: target(port, "/next"),
                width: 500,
                height: 400,
            },
        )
        .await
        .unwrap()
    {
        ActionOut::Value(v) => assert_eq!(v["title"], "Next"),
        other => panic!("{other:?}"),
    }
    let (w, h) = match c
        .act(
            &state,
            None,
            "q",
            Action::Eval {
                world: World::Page,
                body: "return [innerWidth, innerHeight];".into(),
                args: Value::Null,
                timeout_ms: 2000,
            },
        )
        .await
        .unwrap()
    {
        ActionOut::Value(v) => (v[0].as_u64().unwrap(), v[1].as_u64().unwrap()),
        other => panic!("{other:?}"),
    };
    assert_eq!((w, h), (500, 400));
    assert_eq!(
        c.act(&state, None, "q", Action::Close).await.unwrap(),
        ActionOut::Done
    );
    assert!(matches!(
        c.act(
            &state,
            None,
            "q",
            Action::Rings {
                console_since: 0,
                network_since: 0
            }
        )
        .await,
        Err(BrowserError::Gone(_))
    ));

    // Idle reaping: the page is closed, and with it the browser.
    c.reap_idle().await;
    assert!(lock(&c.browser).is_some(), "a fresh page survives a reap");
    {
        let b = lock(&c.browser).clone().unwrap();
        let pg = lock(&b.pages).get("p").cloned().unwrap();
        *lock(&pg.last_used) = Instant::now() - IDLE - Duration::from_secs(1);
    }
    c.reap_idle().await;
    assert!(
        lock(&c.browser).is_none(),
        "the browser closed with its last page"
    );
    let mut gone = false;
    for _ in 0..50 {
        if nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pgid), None).is_err()
            && !profile.exists()
        {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        gone,
        "the process group and the profile are gone after the reap"
    );

    // A new Open relaunches; shutdown kills the group at once.
    c.act(
        &state,
        None,
        "p",
        Action::Open {
            url: target(port, "/next"),
            width: 800,
            height: 600,
        },
    )
    .await
    .unwrap();
    let (pgid, profile) = {
        let b = lock(&c.browser).clone().unwrap();
        (b.pgid, b.profile.clone())
    };
    c.shutdown();
    let mut gone = false;
    for _ in 0..50 {
        if nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pgid), None).is_err() {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(gone, "shutdown killed the process group");
    for _ in 0..30 {
        if !profile.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!profile.exists(), "shutdown removed the profile");
    assert!(matches!(
        c.act(
            &state,
            None,
            "p",
            Action::Rings {
                console_since: 0,
                network_since: 0
            }
        )
        .await,
        Err(BrowserError::Gone(_))
    ));
    let _ = std::fs::remove_dir_all(&data);
}
