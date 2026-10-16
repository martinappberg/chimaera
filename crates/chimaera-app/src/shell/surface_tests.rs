//! The shell's authority surface, pinned: which events reach which windows,
//! which windows may act on which host, the exact core grants, and the
//! command lockstep (handler ↔ manifest ↔ permission files ↔ `native.ts`).
//! Pure: the source-reading tests read files under `CARGO_MANIFEST_DIR`.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{
    caller_is_local, caller_may_target, close_all_within, dispatch_script, host_event_targets,
    local_event_targets, TunnelClose, WindowScope, DAEMON_UI_CORE_PERMISSIONS,
    DAEMON_UI_DEBUG_PERMISSIONS, TAURI_JS_DISPATCH, TAURI_JS_LISTENERS,
};
use crate::command_manifest::{
    Authority, COMMAND_AUTHORITY, DAEMON_UI_COMMANDS, WSL_SETUP_COMMANDS,
};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Every `.rs` file under `dir`, recursively.
fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The windows every routing test runs over.
fn windows() -> HashMap<String, WindowScope> {
    let mut pending = WindowScope::new(None, None, "pending".into());
    pending.begin_home_navigation(Some("host-a".into()), None, "host-a Home".into());
    let mut hub_on_b = WindowScope::new(None, None, "hub".into());
    hub_on_b.begin_home_navigation(Some("host-b".into()), None, "host-b Home".into());
    hub_on_b.complete_home_navigation();
    HashMap::from([
        ("home".into(), WindowScope::new(None, None, "home".into())),
        (
            "local-ws".into(),
            WindowScope::new(None, Some("ws".into()), "local-ws".into()),
        ),
        (
            "a-ws".into(),
            WindowScope::new(Some("host-a".into()), Some("ws".into()), "a-ws".into()),
        ),
        ("a-pending".into(), pending),
        (
            "b-ws".into(),
            WindowScope::new(Some("host-b".into()), Some("ws".into()), "b-ws".into()),
        ),
        ("hub-on-b".into(), hub_on_b),
        (
            "a-job".into(),
            WindowScope::new_compute(
                "host-a#job1".into(),
                "host-a".into(),
                Some("ws".into()),
                "a-job".into(),
            ),
        ),
    ])
}

fn labels(list: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = list.iter().map(|s| s.to_string()).collect();
    out.sort();
    out
}

#[test]
fn host_events_reach_that_hosts_windows_with_the_token_and_local_ones_without() {
    let windows = windows();
    let (full, public) = host_event_targets(&windows, "host-a");
    assert_eq!(full, labels(&["a-ws"]));
    // The window still navigating to host-a runs its old document: tokenless.
    assert_eq!(
        public,
        labels(&["home", "local-ws", "a-pending", "hub-on-b"])
    );
    for other in ["b-ws", "a-job"] {
        assert!(!full.contains(&other.to_string()) && !public.contains(&other.to_string()));
    }

    let (full, public) = host_event_targets(&windows, "host-a#job1");
    assert_eq!(full, labels(&["a-job"]));
    assert!(!public.contains(&"a-ws".to_string()), "{public:?}");

    let (full, _) = host_event_targets(&windows, "host-b");
    assert_eq!(full, labels(&["b-ws", "hub-on-b"]));

    assert_eq!(
        local_event_targets(&windows),
        labels(&["home", "local-ws"]),
        "the local daemon's token reaches local windows only"
    );
}

#[test]
fn a_window_may_target_only_hosts_its_scope_allows() {
    let windows = windows();
    let may = |label: &str, alias: Option<&str>| caller_may_target(&windows[label], alias);
    // (window, target, allowed)
    let table: &[(&str, Option<&str>, bool)] = &[
        ("home", None, true),
        ("home", Some("host-a"), true),
        ("local-ws", Some("host-b"), true),
        ("a-ws", Some("host-a"), true),
        ("a-ws", Some("host-b"), false),
        ("a-ws", None, false),
        ("a-ws", Some("host-a#job1"), false),
        ("a-job", Some("host-a#job1"), true),
        ("a-job", Some("host-a"), true),
        ("a-job", Some("host-b"), false),
        ("a-job", None, false),
        ("hub-on-b", Some("host-b"), true),
        ("hub-on-b", Some("host-a"), false),
        ("hub-on-b", None, false),
        // Mid-navigation the old document is still running: nothing.
        ("a-pending", Some("host-a"), false),
        ("a-pending", None, false),
    ];
    for (label, alias, allowed) in table {
        assert_eq!(may(label, *alias), *allowed, "{label} → {alias:?}");
    }
    // An ordinary alias shaped like a job key is not its prefix's window.
    let colliding = WindowScope::new(Some("host-a#job1".into()), None, "c".into());
    assert!(!caller_may_target(&colliding, Some("host-a")));

    for (label, local) in [
        ("home", true),
        ("local-ws", true),
        ("a-ws", false),
        ("a-job", false),
        ("hub-on-b", false),
        ("a-pending", false),
    ] {
        assert_eq!(caller_is_local(&windows[label]), local, "{label}");
    }
}

#[test]
fn every_command_has_exactly_one_authority_and_checks_it() {
    let classified: Vec<&str> = COMMAND_AUTHORITY.iter().map(|(c, _)| *c).collect();
    let unique: BTreeSet<&str> = classified.iter().copied().collect();
    assert_eq!(unique.len(), classified.len(), "a command classified twice");
    let daemon: BTreeSet<&str> = DAEMON_UI_COMMANDS.iter().copied().collect();
    assert_eq!(unique, daemon, "classification ≠ DAEMON_UI_COMMANDS");

    let sources: Vec<String> = ["src/shell/commands.rs", "src/shell/cluster.rs"]
        .iter()
        .map(|p| read(&manifest_dir().join(p)))
        .collect();
    for (command, authority) in COMMAND_AUTHORITY {
        let needle = match authority {
            Authority::AnyWindow => continue,
            Authority::OwnHost => "require_target(",
            Authority::LocalOnly => "require_local(",
        };
        let head = format!("fn {command}(");
        let body = sources
            .iter()
            .find_map(|src| {
                let at = src.find(&head)?;
                let rest = &src[at..];
                // A top-level fn ends at the first closing brace in column 0.
                Some(&rest[..rest.find("\n}\n").unwrap_or(rest.len())])
            })
            .unwrap_or_else(|| panic!("no fn {command} in commands.rs/cluster.rs"));
        assert!(
            body.contains(needle),
            "{command} is {authority:?} but its body never calls {needle}…)"
        );
    }
}

#[test]
fn daemon_windows_get_the_exact_core_grants_and_never_the_default_set() {
    // The devtools toggle exists only where devtools do.
    let debug: Vec<&str> = DAEMON_UI_DEBUG_PERMISSIONS.to_vec();
    if cfg!(debug_assertions) {
        assert_eq!(debug, ["core:webview:allow-internal-toggle-devtools"]);
    } else {
        assert!(debug.is_empty(), "{debug:?}");
    }
    let granted: BTreeSet<&str> = DAEMON_UI_CORE_PERMISSIONS.iter().copied().collect();
    let expected: BTreeSet<&str> = [
        "core:event:allow-listen",
        "core:event:allow-unlisten",
        "core:window:allow-close",
        "core:window:allow-start-dragging",
        "core:window:allow-internal-toggle-maximize",
        "core:window:allow-set-title",
    ]
    .into_iter()
    .collect();
    assert_eq!(granted, expected);
    for permission in DAEMON_UI_CORE_PERMISSIONS {
        for banned in [
            "core:default",
            "core:image",
            "core:menu",
            "core:tray",
            "core:path",
            "core:webview",
            "core:app",
            "core:event:allow-emit",
        ] {
            assert!(!permission.starts_with(banned), "{permission}");
        }
    }
}

/// `emit` on a window handle broadcasts to every webview, and `emit` /
/// `emit_to` reach every page's catch-all listener: neither may carry a
/// token or a prompt. Those go through `deliver_to` / `emit_host_scoped`.
#[test]
fn no_token_or_prompt_bearing_event_goes_through_the_broadcast_event_system() {
    let mut files = Vec::new();
    rust_sources(&manifest_dir().join("src"), &mut files);
    // `file!()` uses the host's separator (`\` on Windows).
    let this = Path::new(file!())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let secret_events = [
        "\"host-status\"",
        "\"local-daemon-updated\"",
        "\"ssh-askpass",
    ];
    let window_broadcasts = ["window.emit(", "webview.emit(", "w.emit("];
    let mut found = Vec::new();
    for file in files {
        if file
            .file_name()
            .is_some_and(|n| n.to_string_lossy() == this)
        {
            continue;
        }
        let source = read(&file);
        let name = file
            .strip_prefix(manifest_dir())
            .unwrap()
            .display()
            .to_string();
        // Whitespace-free, so a call split across lines still matches.
        let flat: String = source.chars().filter(|c| !c.is_whitespace()).collect();
        for pattern in window_broadcasts {
            if flat.contains(pattern) {
                found.push(format!("{name}: {pattern}"));
            }
        }
        for call in [".emit(", ".emit_to(", ".emit_filter(", "emit_str("] {
            for (at, _) in flat.match_indices(call) {
                let args: String = flat[at + call.len()..].chars().take(160).collect();
                let args = &args[..args.find(';').unwrap_or(args.len())];
                for event in secret_events {
                    if args.contains(event) {
                        found.push(format!("{name}: {call}{event}…"));
                    }
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "broadcast of a secret-bearing event:\n{}",
        found.join("\n")
    );
}

#[test]
fn the_dispatch_script_names_one_event_and_carries_its_payload_verbatim() {
    let script = dispatch_script("host-status", r#"{"alias":"a","token":"t"}"#);
    assert!(script.contains(&format!("window['{TAURI_JS_LISTENERS}']")));
    assert!(script.contains(&format!("window['{TAURI_JS_DISPATCH}']")));
    assert!(script.contains(r#"t["host-status"]"#), "{script}");
    assert!(
        script.contains(r#"payload:{"alias":"a","token":"t"}"#),
        "{script}"
    );
    // Entries whose callback was unregistered (Tauri's unlisten) are dropped
    // from the table, not dispatched.
    assert!(script.contains("!c.has(e.handlerId)"), "{script}");
    assert!(script.contains("delete l[id]"), "{script}");
    assert!(script.contains("if(ids.length)f("), "{script}");
    // An event name is quoted as data, never spliced as code.
    let odd = dispatch_script("x'];alert(1);//", "null");
    assert!(odd.contains(r#""x'];alert(1);//""#), "{odd}");
}

/// `deliver_to` calls Tauri's private JS dispatch by name. Pin the names and
/// the dispatch shape against the tauri version in Cargo.lock, so an upgrade
/// that changes them fails here instead of silently dropping every event.
#[test]
fn tauri_js_event_internals_match_the_locked_tauri_source() {
    let lock = read(&manifest_dir().join("Cargo.lock"));
    let version = lock
        .split("[[package]]")
        .find_map(|block| {
            let mut lines = block.lines().map(str::trim);
            lines.find(|l| *l == "name = \"tauri\"")?;
            block
                .lines()
                .map(str::trim)
                .find_map(|l| l.strip_prefix("version = \"")?.strip_suffix('"'))
        })
        .expect("tauri in Cargo.lock");
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap()).join(".cargo"));
    let crate_dir = std::fs::read_dir(cargo_home.join("registry/src"))
        .expect("cargo registry sources")
        .filter_map(|entry| Some(entry.ok()?.path().join(format!("tauri-{version}"))))
        .find(|dir| dir.is_dir())
        .unwrap_or_else(|| panic!("tauri-{version} sources not under {}", cargo_home.display()));
    let listener = read(&crate_dir.join("src/event/listener.rs"));
    assert!(listener.contains(&format!("function_name: \"{TAURI_JS_DISPATCH}\"")));
    assert!(listener.contains(&format!("listeners_object_name: \"{TAURI_JS_LISTENERS}\"")));
    let event = read(&crate_dir.join("src/event/mod.rs"));
    for shape in [
        "value: function (eventData, ids)",
        "const listener = listeners[id]",
        "Object.defineProperty(eventListeners, '{event_id}'",
        // Deletable, which `dispatch_script`'s stale-entry sweep relies on.
        "value: listener, configurable: true",
        // Unlisten drops only the callback, never the table entry.
        "window.__TAURI_INTERNALS__.unregisterCallback(listener.handlerId)",
    ] {
        assert!(
            event.contains(shape),
            "tauri-{version} dispatch changed: {shape}"
        );
    }
    // The callback registry the sweep reads (`callbacks.has(handlerId)`).
    let core = read(&crate_dir.join("scripts/core.js"));
    for shape in [
        "const callbacks = new Map()",
        "callbacks.delete(id)",
        "Object.defineProperty(window.__TAURI_INTERNALS__, 'callbacks'",
    ] {
        assert!(
            core.contains(shape),
            "tauri-{version} callbacks changed: {shape}"
        );
    }
}

#[test]
fn the_command_lockstep_holds() {
    let dir = manifest_dir();
    let shell = read(&dir.join("src/shell.rs"));
    let start = shell.find("generate_handler![").expect("generate_handler!") + 18;
    let end = start + shell[start..].find(']').unwrap();
    let handlers: Vec<&str> = shell[start..end]
        .split(',')
        .map(|item| item.trim().rsplit("::").next().unwrap())
        .filter(|name| !name.is_empty())
        .collect();
    let handler_set: BTreeSet<&str> = handlers.iter().copied().collect();
    assert_eq!(handler_set.len(), handlers.len(), "a handler listed twice");

    let daemon: BTreeSet<&str> = DAEMON_UI_COMMANDS.iter().copied().collect();
    let wsl: BTreeSet<&str> = WSL_SETUP_COMMANDS.iter().copied().collect();
    assert_eq!(daemon.len(), DAEMON_UI_COMMANDS.len());
    assert!(daemon.is_disjoint(&wsl), "{:?}", daemon.intersection(&wsl));
    let all: BTreeSet<&str> = daemon.union(&wsl).copied().collect();
    assert_eq!(handler_set, all, "generate_handler! ≠ the command manifest");

    let generated: BTreeSet<String> = std::fs::read_dir(dir.join("permissions/autogenerated"))
        .unwrap()
        .filter_map(|e| {
            let name = e.ok()?.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".toml").map(str::to_string)
        })
        .collect();
    let all_owned: BTreeSet<String> = all.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        generated, all_owned,
        "permissions/autogenerated ≠ the manifest"
    );

    let wizard: serde_json::Value =
        serde_json::from_str(&read(&dir.join("capabilities/wsl-setup.json"))).unwrap();
    assert_eq!(wizard["local"], serde_json::json!(true));
    assert_eq!(wizard["windows"], serde_json::json!(["wsl-setup*"]));
    assert!(
        wizard.get("remote").is_none(),
        "the wizard grant is local-only"
    );
    let granted: BTreeSet<String> = wizard["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap().to_string())
        .collect();
    let mut expected: BTreeSet<String> = WSL_SETUP_COMMANDS
        .iter()
        .map(|c| format!("allow-{}", c.replace('_', "-")))
        .collect();
    expected.insert("core:default".into());
    assert_eq!(granted, expected);

    let native = read(&dir.join("../../web-ui/src/lib/net/native.ts"));
    for command in DAEMON_UI_COMMANDS {
        let quoted = format!("\"{command}\"");
        assert!(
            native
                .lines()
                .any(|line| line.contains("invoke") && line.contains(&quoted)),
            "no native.ts invoke of {command}"
        );
    }
    assert!(!native.contains("wsl_"), "wsl_* commands are wizard-only");
}

#[tokio::test]
async fn quit_closes_every_tunnel_at_once_under_one_deadline() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let done = Arc::new(AtomicUsize::new(0));
    let mut closes: Vec<TunnelClose> = Vec::new();
    for _ in 0..4 {
        let done = done.clone();
        closes.push(Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            done.fetch_add(1, Ordering::SeqCst);
        }));
    }
    // A dead link that never finishes its teardown.
    closes.push(Box::pin(std::future::pending::<()>()));
    let started = std::time::Instant::now();
    close_all_within(closes, Duration::from_millis(400)).await;
    let took = started.elapsed();
    assert_eq!(
        done.load(Ordering::SeqCst),
        4,
        "concurrent: 4 × 150 ms fit in 400 ms"
    );
    assert!(
        took < Duration::from_secs(2),
        "bounded by the deadline: {took:?}"
    );
}
