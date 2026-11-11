//! Programs and tools, the privileged tier, proven with its fixture
//! (`plugins/test-privileged`): a declared program runs as a job under the
//! host's limits and the plugin hears `job-finished`; a job past its time
//! loses its whole process group; the queue holds one per plugin and caps
//! the waiting; undeclared programs, the host's own variables and paths out
//! of the workspace are refused; a tool downloads from a local server,
//! checks its sha256, unpacks, runs its setup and serves its program; a
//! wrong checksum installs nothing; an agent's tool waits for its job; and
//! switching the plugin off, or a hard block, stops its jobs.

// Most of these tests drive the fixture's POSIX programs and tool, so the
// helpers only they use are Unix-only with them.
#[cfg(unix)]
use std::time::{Duration, Instant};

use serde_json::{json, Value};

#[cfg(unix)]
use super::plugin_updates::FakeReleases;
use super::support::*;
#[cfg(unix)]
use crate::lock;
use crate::AppState;

const PID: &str = "test-privileged";

/// The tests that point the process-wide download override at their own
/// server run one at a time (in parallel, one would fetch from the other's
/// server after it stopped).
#[cfg(unix)]
static TOOL_DOWNLOADS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
#[cfg(unix)]
const TOOL: &[u8] = include_bytes!("../../../../plugins/test-privileged/fixture-tool-1.0.0.tar.gz");

async fn switch(state: &Arc<AppState>, ws: &str, on: bool) {
    let (status, body) = request(
        state,
        Method::PUT,
        &format!("/api/v1/workspaces/{ws}/plugins/{PID}"),
        Some(json!({"on": on})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn jobs_workspace(label: &str) -> (Arc<AppState>, String) {
    crate::plugins::test_catalog::privileged();
    let state = test_state();
    let ws = make_workspace(&state, label).await;
    switch(&state, &ws, true).await;
    (state, ws)
}

/// An action; the job id it started (the tree's text), or its error.
async fn act(
    state: &Arc<AppState>,
    ws: &str,
    action: &str,
    payload: Value,
) -> Result<String, String> {
    let (status, body) = request(
        state,
        Method::POST,
        &format!("/api/v1/workspaces/{ws}/plugins/{PID}/views/jobs/actions"),
        Some(json!({"action": action, "payload": payload})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    match body["tree"]["root"]["text"].as_str() {
        Some(id) => Ok(id.to_string()),
        None => Err(body["problems"].to_string()),
    }
}

#[cfg(unix)]
async fn query(state: &Arc<AppState>, ws: &str, name: &str, args: Value) -> Value {
    let (status, body) = request(
        state,
        Method::GET,
        &format!(
            "/api/v1/workspaces/{ws}/plugins/{PID}/query/{name}?args={}",
            urlencode(&args.to_string())
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["data"].clone()
}

/// Wait (≤ `limit`) for the fixture to record `finished <id> …`.
#[cfg(unix)]
async fn finished(state: &Arc<AppState>, ws: &str, id: &str, limit: Duration) -> String {
    let started = Instant::now();
    loop {
        let events = query(state, ws, "events", json!({})).await;
        if let Some(line) = events
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .find(|e| e.starts_with(&format!("finished {id} ")))
        {
            return line.to_string();
        }
        assert!(started.elapsed() < limit, "{id} never finished: {events}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn a_plugin_that_runs_programs_is_privileged_and_says_so() {
    let m = crate::plugins::test_catalog::privileged();
    assert_eq!(m.caps.tier().as_str(), "privileged");
    let lines: Vec<String> = m.caps.lines().into_iter().map(|l| l.text).collect();
    assert!(
        lines.contains(&"Runs sh: this plugin can run any command on this host".to_string()),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("Runs programs on this host: echo, fixture-tool, sleep")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("Downloads Fixture tool 1.0.0 from example.invalid")),
        "{lines:?}"
    );
}

#[cfg(unix)] // the fixture's programs and tool are POSIX stand-ins
#[tokio::test]
async fn a_declared_program_runs_as_a_job_and_the_plugin_hears_it_end() {
    let (state, ws) = jobs_workspace("jobs-echo").await;
    let id = act(&state, &ws, "echo", Value::Null).await.unwrap();
    let line = finished(&state, &ws, &id, Duration::from_secs(20)).await;
    assert!(line.contains("exit=0"), "{line}");
    assert!(line.contains("out=hello from a job"), "{line}");
    // Its status, through the plugin and through the route.
    let status = query(&state, &ws, "status", json!({"id": id})).await;
    assert_eq!(status["state"], "done");
    assert_eq!(status["stdout"], format!("output:.jobs/{id}/stdout.log"));
    assert_eq!(status["from"], "path");
    let (code, route) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/jobs/{id}"),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(route["plugin"], PID);
    // A job that ended before anyone waited on it (an agent's call that
    // got there late) is done at once, not after the whole hold.
    let asked = Instant::now();
    assert!(crate::plugins::jobs::wait(&state, PID, &id, Duration::from_secs(10)).await);
    assert!(asked.elapsed() < Duration::from_secs(1));
    // The activity log names the program, its arguments and how it ended.
    let entry = activity(&state, "job").await;
    assert_eq!(entry["program"], "echo");
    assert_eq!(entry["args"], json!(["hello", "from", "a", "job"]));
    assert_eq!(entry["exit"], 0);
    // The environment: the job's own variable, the host's TERM.
    let id = act(&state, &ws, "env", json!({"env": {"GREETING": "hi"}}))
        .await
        .unwrap();
    let line = finished(&state, &ws, &id, Duration::from_secs(20)).await;
    assert!(line.contains("out=hi dumb"), "{line}");
}

#[tokio::test]
async fn what_a_job_may_not_do_is_refused_before_it_runs() {
    let (state, ws) = jobs_workspace("jobs-refused").await;
    for (spec, why) in [
        (json!({"program": "curl", "args": []}), "not one of"),
        (json!({"program": "/bin/sh", "args": []}), "not one of"),
        (
            json!({"program": "echo", "env": {"PATH": "/tmp"}}),
            "the host's to set",
        ),
        (
            json!({"program": "echo", "env": {"LD_PRELOAD": "x.so"}}),
            "the host's to set",
        ),
        (json!({"program": "echo", "cwd": "../.."}), "`..`"),
        (
            json!({"program": "echo", "cwd": "output:../x"}),
            "stay inside",
        ),
        (json!({"program": "echo", "priority": "urgent"}), "priority"),
    ] {
        let err = act(&state, &ws, "spec", spec.clone()).await.unwrap_err();
        assert!(err.contains(why), "{spec}: {err}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_job_past_its_time_loses_its_whole_process_group() {
    let (state, ws) = jobs_workspace("jobs-timeout").await;
    let started = Instant::now();
    let id = act(&state, &ws, "sleep", json!({"wall_s": 1}))
        .await
        .unwrap();
    let line = finished(&state, &ws, &id, Duration::from_secs(20)).await;
    assert!(line.contains("timed_out=true"), "{line}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "stopped after {:?}",
        started.elapsed()
    );
    // The background child it left is gone too.
    let roots = query(&state, &ws, "roots", json!({})).await;
    let pid_file = PathBuf::from(roots["output"].as_str().unwrap()).join("child.pid");
    let pid: i32 = std::fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while running(pid) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!running(pid), "the job's child {pid} outlived it");
}

/// The newest activity entry of `kind` (it lands just after the job ends).
#[cfg(unix)]
async fn activity(state: &Arc<AppState>, kind: &str) -> Value {
    let started = Instant::now();
    loop {
        let log = crate::plugins::activity::recent(&state.plugin_catalog.root, PID, 50);
        if let Some(e) = log.into_iter().find(|e| e["kind"] == kind) {
            return e;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "no {kind} activity"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(unix)]
/// Whether `pid` still runs. A killed child whose parent died first is
/// reparented to init and stays a zombie until init reaps it (a container's
/// init may never do so), which `kill(pid, 0)` still finds.
fn running(pid: i32) -> bool {
    if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err() {
        return false;
    }
    let stat = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    !stat.is_empty() && !stat.starts_with('Z')
}

#[cfg(unix)] // the fixture's programs and tool are POSIX stand-ins
#[tokio::test]
async fn the_queue_runs_one_per_plugin_and_caps_the_waiting() {
    let (state, ws) = jobs_workspace("jobs-queue").await;
    let first = act(&state, &ws, "slow", Value::Null).await.unwrap();
    let mut waiting = Vec::new();
    for _ in 0..8 {
        waiting.push(act(&state, &ws, "slow", Value::Null).await.unwrap());
    }
    let over = act(&state, &ws, "slow", Value::Null).await.unwrap_err();
    assert!(over.contains("waiting"), "{over}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        query(&state, &ws, "status", json!({"id": first})).await["state"],
        "running"
    );
    assert_eq!(
        query(&state, &ws, "status", json!({"id": waiting[0]})).await["state"],
        "queued"
    );
    // Switched off, its jobs stop (a hard block does the same).
    switch(&state, &ws, false).await;
    let job = state.plugin_platform.jobs.get(&first).unwrap();
    let started = Instant::now();
    while !job.is_done() {
        assert!(started.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(job.json()["cancelled"], true);
    assert!(state
        .plugin_platform
        .jobs
        .get(&waiting[7])
        .unwrap()
        .is_done());
}

#[cfg(unix)] // the fixture's programs and tool are POSIX stand-ins
#[tokio::test]
async fn a_hard_block_stops_a_running_job_at_once() {
    use crate::plugins::revoke::{Entry, Level};
    let (state, ws) = jobs_workspace("jobs-block").await;
    let id = act(&state, &ws, "slow", Value::Null).await.unwrap();
    let job = state.plugin_platform.jobs.get(&id).unwrap();
    let started = Instant::now();
    while job.json()["state"] != "running" {
        assert!(started.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    crate::plugins::write(&state.plugin_guard.revoked).set_fetched_for_tests(vec![Entry {
        id: PID.into(),
        versions: vec!["0.1.0".into()],
        sha256: vec![],
        level: Level::Hard,
        reason: "it misbehaves".into(),
    }]);
    crate::plugins::revoke::apply(&state).await;
    let blocked = Instant::now();
    while !job.is_done() {
        assert!(blocked.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(job.json()["cancelled"], true);
    // And it can't start another.
    let (_, body) = request(
        &state,
        Method::POST,
        &format!("/api/v1/workspaces/{ws}/plugins/{PID}/views/jobs/actions"),
        Some(json!({"action": "echo", "payload": null})),
    )
    .await;
    assert!(body["tree"].is_null(), "{body}");
    // Nor did the blocked build hear its job end (it would start the next
    // one): with the block lifted, its record has no `finished` for it.
    tokio::time::sleep(Duration::from_millis(300)).await;
    crate::plugins::write(&state.plugin_guard.revoked).set_fetched_for_tests(vec![]);
    let events = query(&state, &ws, "events", json!({})).await;
    assert!(
        !events
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .any(|e| e.starts_with(&format!("finished {id} "))),
        "{events}"
    );
}

#[cfg(unix)] // the fixture's programs and tool are POSIX stand-ins
#[tokio::test]
async fn a_tool_downloads_checks_unpacks_sets_up_and_runs() {
    let _one = TOOL_DOWNLOADS.lock().await;
    let fake = FakeReleases::start().await;
    fake.put("/fixture-tool-1.0.0.tar.gz", TOOL.to_vec());
    crate::plugins::toolchain::set_downloads_for_tests("https://example.invalid", fake.base());
    let (state, ws) = jobs_workspace("jobs-tool").await;
    // Not installed: its program isn't found, and asking for it says so.
    let err = act(&state, &ws, "tool", Value::Null).await.unwrap_err();
    assert!(err.contains("not installed"), "{err}");
    let (status, listed) = request(
        &state,
        Method::GET,
        &format!("/api/v1/plugins/{PID}/tools"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(listed["tools"][0]["installed"].is_null());
    assert_eq!(listed["tools"][0]["download"]["host"], "example.invalid");

    let (status, done) = request(
        &state,
        Method::POST,
        &format!("/api/v1/plugins/{PID}/tools/fixture-tool/install"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["version"], "1.0.0");
    let tool = query(&state, &ws, "tool", json!({})).await;
    assert_eq!(tool["installed"]["version"], "1.0.0");
    assert_eq!(tool["current"], true);
    // The activity log keeps where it came from and the digest it matched.
    let entry = activity(&state, "tool-install").await;
    assert_eq!(
        entry["url"],
        "https://example.invalid/fixture-tool-1.0.0.tar.gz"
    );
    assert_eq!(entry["sha256"].as_str().map(str::len), Some(64));
    // Its setup ran, in its own folder; nothing outside changed.
    let dir = crate::plugins::toolchain::tools_root(&state)
        .join(PID)
        .join("fixture-tool");
    assert!(dir.join("1.0.0/setup-ran").is_file());
    assert_eq!(
        std::fs::read_link(dir.join("current")).unwrap(),
        PathBuf::from("1.0.0")
    );
    // Its program runs from it.
    let id = act(&state, &ws, "tool", Value::Null).await.unwrap();
    let line = finished(&state, &ws, &id, Duration::from_secs(20)).await;
    assert!(line.contains("out=fixture-tool hello"), "{line}");
    assert_eq!(
        query(&state, &ws, "status", json!({"id": id})).await["from"],
        "tool:fixture-tool"
    );
    // Remove.
    let (status, _) = request(
        &state,
        Method::DELETE,
        &format!("/api/v1/plugins/{PID}/tools/fixture-tool"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!dir.exists());
}

#[cfg(unix)] // the fixture's programs and tool are POSIX stand-ins
#[tokio::test]
async fn a_download_that_is_not_the_declared_file_installs_nothing() {
    let _one = TOOL_DOWNLOADS.lock().await;
    let fake = FakeReleases::start().await;
    let mut other = TOOL.to_vec();
    *other.last_mut().unwrap() ^= 1;
    fake.put("/fixture-tool-1.0.0.tar.gz", other);
    // The override is process-wide: this test names its own server.
    crate::plugins::toolchain::set_downloads_for_tests("https://example.invalid", fake.base());
    let (state, _ws) = jobs_workspace("jobs-badsum").await;
    let (status, body) = request(
        &state,
        Method::POST,
        &format!("/api/v1/plugins/{PID}/tools/fixture-tool/install"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("checksum"),
        "{body}"
    );
    let dir = crate::plugins::toolchain::tools_root(&state)
        .join(PID)
        .join("fixture-tool");
    assert!(!dir.join("current").exists());
    assert!(!dir.join("1.0.0").exists());
}

#[cfg(unix)] // the fixture's programs and tool are POSIX stand-ins
#[tokio::test]
async fn an_agents_tool_waits_for_its_job_and_gets_the_final_answer() {
    crate::plugins::test_catalog::privileged();
    let state = test_state();
    let ws = make_workspace(&state, "jobs-agent").await;
    let sid = inject_agent(&state, "kj1");
    lock(&state.session_workspaces).insert(sid.clone(), ws.clone());
    lock(&state.workspaces)
        .set_plugin_on(&ws, PID, true)
        .unwrap();
    let (is_err, text) = mcp_tool_call(&state, &sid, "kj1", "build", json!({})).await;
    assert!(!is_err, "{text}");
    assert_eq!(text, "exit 0 · building");
    state.sessions.kill(&sid).ok();
}

#[cfg(unix)] // the fixture's programs and tool are POSIX stand-ins
#[tokio::test]
async fn a_tool_answer_shows_pictures_from_its_own_output_folder() {
    crate::plugins::test_catalog::privileged();
    let state = test_state();
    let ws = make_workspace(&state, "jobs-picture").await;
    let sid = inject_agent(&state, "kp1");
    lock(&state.session_workspaces).insert(sid.clone(), ws.clone());
    lock(&state.workspaces)
        .set_plugin_on(&ws, PID, true)
        .unwrap();
    let (status, out) = mcp_post(
        &state,
        &sid,
        "kp1",
        json!({
            "jsonrpc": "2.0", "id": 9, "method": "tools/call",
            "params": {"name": "build", "arguments": {"picture": true}},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let content = out["result"]["content"].as_array().unwrap();
    assert_eq!(content.len(), 3, "{out}");
    let folder = crate::plugins::output::folder(&state.plugin_platform.output_root, PID, &ws);
    let caption = content[0]["text"].as_str().unwrap();
    assert!(
        caption.contains(&format!(
            "![one]({})",
            folder.join("pictures/one.png").display()
        )),
        "{caption}"
    );
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[1]["mimeType"], "image/png");
    let missing = content[2]["text"].as_str().unwrap();
    assert!(missing.starts_with("none: "), "{missing}");
    assert!(missing.contains("(not shown: "), "{missing}");
    state.sessions.kill(&sid).ok();
}

#[cfg(unix)] // the fixture's programs and tool are POSIX stand-ins
#[tokio::test]
async fn a_tools_folder_setting_moves_where_tools_install() {
    let _one = TOOL_DOWNLOADS.lock().await;
    let fake = FakeReleases::start().await;
    fake.put("/fixture-tool-1.0.0.tar.gz", TOOL.to_vec());
    crate::plugins::toolchain::set_downloads_for_tests("https://example.invalid", fake.base());
    let (state, _ws) = jobs_workspace("jobs-tools-dir").await;
    let set = |value: &str| {
        let mut map = serde_json::Map::new();
        map.insert("plugins.toolsDir".into(), value.into());
        crate::lock(&state.settings).put(map).unwrap();
    };
    let url = format!("/api/v1/plugins/{PID}/tools/fixture-tool/install");
    let install = || request(&state, Method::POST, &url, None);
    // A value that isn't a folder refuses, naming the setting.
    set("relative/tools");
    let (status, body) = install().await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("Plugin Tools Folder"),
        "{body}"
    );
    // A folder elsewhere (a cluster's $SCRATCH) is where it goes.
    let elsewhere = std::env::temp_dir().join(format!(
        "chimaera-tools-dir-{}",
        &chimaera_core::generate_token()[..8]
    ));
    set(&elsewhere.to_string_lossy());
    let (status, body) = install().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(crate::plugins::toolchain::tools_root(&state), elsewhere);
    assert!(elsewhere
        .join(PID)
        .join("fixture-tool/1.0.0/setup-ran")
        .is_file());
    let _ = std::fs::remove_dir_all(&elsewhere);
}

/// A guest call in flight on a build an update replaced, or in a workspace
/// the plugin was just switched off in, can't start a job (D5).
#[tokio::test]
async fn a_job_starts_only_for_the_current_build_where_it_is_on() {
    let (state, ws) = jobs_workspace("jobs-current").await;
    let m = crate::plugins::test_catalog::privileged();
    let spec = r#"{"program": "echo", "args": ["hi"]}"#;
    let mut stale = (*m).clone();
    stale.wasm.sha256 = std::sync::Arc::from("0".repeat(64));
    let err = crate::plugins::jobs::start(&state, &std::sync::Arc::new(stale), &ws, spec)
        .await
        .unwrap_err();
    assert!(err.contains("was updated"), "{err}");
    switch(&state, &ws, false).await;
    let err = crate::plugins::jobs::start(&state, &m, &ws, spec)
        .await
        .unwrap_err();
    assert!(err.contains("not switched on"), "{err}");
}

/// An update whose build no longer declares a program stops that program's
/// running job; a job of a program it still declares keeps going (D6).
#[cfg(unix)] // the fixture's programs and tool are POSIX stand-ins
#[tokio::test]
async fn an_update_that_drops_a_program_stops_its_jobs() {
    let state = test_state();
    let src = test_dir("jobs-update-src");
    let toml = crate::plugins::test_catalog::dist_test_text("test-privileged/plugin.toml");
    std::fs::write(src.join("plugin.toml"), &toml).unwrap();
    std::fs::write(
        src.join("plugin.wasm"),
        crate::plugins::test_catalog::dist_test_bytes("test-privileged/plugin.wasm"),
    )
    .unwrap();
    let install = || {
        request_trusting(
            &state,
            Method::POST,
            "/api/v1/plugins/install",
            Some(json!({"path": src})),
        )
    };
    let (status, body) = install().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ws = make_workspace(&state, "jobs-update").await;
    switch(&state, &ws, true).await;
    let dropped = act(&state, &ws, "slow", Value::Null).await.unwrap();
    let kept = act(
        &state,
        &ws,
        "spec",
        json!({"program": "sh", "args": ["-c", "sleep 30"], "wall_s": 30}),
    )
    .await
    .unwrap();
    let job = state.plugin_platform.jobs.get(&dropped).unwrap();
    let started = Instant::now();
    while job.json()["state"] != "running" {
        assert!(started.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let next = toml
        .replacen("version = \"0.1.0\"", "version = \"0.1.1\"", 1)
        .replace("[[programs]]\nname = \"sleep\"\n\n", "");
    assert!(!next.contains("\"sleep\""), "{next}");
    std::fs::write(src.join("plugin.toml"), &next).unwrap();
    let (status, body) = install().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let updated = Instant::now();
    while !job.is_done() {
        assert!(
            updated.elapsed() < Duration::from_secs(10),
            "the dropped program's job kept running"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(job.json()["cancelled"], true);
    let kept = state.plugin_platform.jobs.get(&kept).unwrap();
    assert!(!kept.is_done(), "{}", kept.json());
    switch(&state, &ws, false).await;
}

#[cfg(unix)]
/// A tool's setup step is reachable like a job: a block, an untrust or a
/// remove (`cancel_where`) stops its process group, and a build that is no
/// longer current never starts one (D7).
#[tokio::test]
async fn a_tool_setup_step_stops_with_its_plugin() {
    let (state, _ws) = jobs_workspace("jobs-setup-stop").await;
    let m = crate::plugins::test_catalog::privileged();
    let dir = test_dir("jobs-setup-stop-tool");
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = bin.join("slow-setup");
    std::fs::write(&script, "#!/bin/sh\necho $$ > \"$1\"\nexec sleep 30\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let pid_file = dir.join("setup.pid");
    let args = vec![pid_file.to_string_lossy().into_owned()];
    let step = {
        let (state, m, bin, dir, args) = (
            state.clone(),
            m.clone(),
            bin.clone(),
            dir.clone(),
            args.clone(),
        );
        tokio::spawn(async move {
            crate::plugins::jobs::run_setup(&state, &m, &bin, &dir, "slow-setup", &args).await
        })
    };
    let started = Instant::now();
    let pid: i32 = loop {
        if let Some(pid) = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|t| t.trim().parse().ok())
        {
            break pid;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the setup step never ran"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    crate::plugins::jobs::cancel_where(&state, PID, None);
    let ended = tokio::time::timeout(Duration::from_secs(8), step)
        .await
        .expect("the setup step outlived its plugin's cancel")
        .unwrap();
    let err = ended.unwrap_err();
    assert!(err.contains("stopped"), "{err}");
    let deadline = Instant::now() + Duration::from_secs(3);
    while running(pid) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!running(pid), "the setup step's process {pid} outlived it");
    let mut stale = (*m).clone();
    stale.wasm.sha256 = std::sync::Arc::from("0".repeat(64));
    let err = crate::plugins::jobs::run_setup(&state, &stale, &bin, &dir, "slow-setup", &args)
        .await
        .unwrap_err();
    assert!(err.contains("was updated"), "{err}");
}

/// A slow login shell: the job start inside a guest call is abandoned at
/// the call's budget, yet the capture finishes and is cached, so a later
/// start finds it (D8).
// Windows captures no login shell: a job gets the daemon's own environment.
#[cfg(unix)]
#[tokio::test]
async fn a_login_environment_capture_outlives_a_caller_that_gave_up() {
    let state = test_state();
    state
        .plugin_platform
        .jobs
        .set_capture_delay_for_tests(Duration::from_millis(1500));
    let gave_up = tokio::time::timeout(
        Duration::from_millis(200),
        crate::plugins::jobs::login_env(&state, None),
    )
    .await;
    assert!(gave_up.is_err());
    // Every later try gives up sooner than one capture takes: only a
    // capture that outlived its caller can answer one.
    let started = Instant::now();
    let env = loop {
        let attempt = tokio::time::timeout(
            Duration::from_millis(300),
            crate::plugins::jobs::login_env(&state, None),
        )
        .await;
        if let Ok(env) = attempt {
            break env.unwrap();
        }
        assert!(
            started.elapsed() < Duration::from_secs(25),
            "the abandoned capture was never cached"
        );
    };
    assert!(env.iter().any(|(k, _)| k == "PATH"));
}

/// A plugin that names one of its tools' programs as `provides.browser`
/// (the privileged fixture's component under another manifest).
#[cfg(unix)]
const BROWSER_PID: &str = "test-browser";
#[cfg(unix)]
fn browser_manifest() -> String {
    let artifacts: String = crate::plugins::platform::PLATFORMS
        .iter()
        .map(|p| {
            format!(
                "[[tools.artifacts]]\nplatform = \"{p}\"\n\
                 url = \"https://example.invalid/fixture-tool-1.0.0.tar.gz\"\n\
                 sha256 = \"6c79c8a92fe19b07bd7782b9b59b3520c5cb5fd84f1e8039a8b038535c49d6b9\"\n\
                 size = 325\nunpack = \"tar.gz\"\nbin = \"bin\"\n\n"
            )
        })
        .collect();
    format!(
        "id = \"{BROWSER_PID}\"\nname = \"Browser fixture\"\nversion = \"0.1.0\"\n\
         summary = \"Provides a stand-in browser, for chimaera's tests.\"\napi = \"0.2\"\n\n\
         [provides]\nmcp_tools = [\"build\"]\nbrowser = \"fixture-tool\"\n\n\
         [[programs]]\nname = \"fixture-tool\"\n\n\
         [[tools]]\nid = \"fixture-tool\"\nname = \"Fixture browser\"\nversion = \"1.0.0\"\n\
         programs = [\"fixture-tool\"]\n\n{artifacts}"
    )
}

/// `provides.browser`: refused unless a tool installs the program; on the
/// card's Can list; offered to the Chromium engine only where the plugin is
/// switched on and its tool is installed, and each missing step is named.
#[cfg(unix)] // the fixture's programs and tool are POSIX stand-ins
#[tokio::test]
async fn a_plugin_tool_supplies_the_agent_browser_where_it_is_on() {
    use crate::plugins::browser::{offer, Offer};
    let bad =
        browser_manifest().replace("browser = \"fixture-tool\"", "browser = \"google-chrome\"");
    let err = crate::plugins::parse_manifest(&bad).unwrap_err();
    assert!(err.contains("provides.browser"), "{err}");

    let _one = TOOL_DOWNLOADS.lock().await;
    let fake = FakeReleases::start().await;
    fake.put("/fixture-tool-1.0.0.tar.gz", TOOL.to_vec());
    crate::plugins::toolchain::set_downloads_for_tests("https://example.invalid", fake.base());
    let m = crate::plugins::test_catalog::add(
        &browser_manifest(),
        crate::plugins::test_catalog::dist_test_bytes("test-privileged/plugin.wasm"),
    );
    assert!(
        m.caps.lines().iter().any(|l| l.privileged
            && l.text
                .starts_with("Runs fixture-tool on this host as the browser")),
        "{:?}",
        m.caps.lines()
    );
    assert!(
        !m.caps
            .lines()
            .iter()
            .any(|l| l.text.contains("can't describe") || l.text.starts_with("Runs programs")),
        "{:?}",
        m.caps.lines()
    );
    let state = test_state();
    let ws = make_workspace(&state, "browser-plugin").await;
    let words = |o: Offer| match o {
        Offer::NotReady(why) => why,
        other => panic!("expected words, got {other:?}"),
    };

    // Installed here, off in this workspace: switch it on.
    let why = words(offer(&state, Some(&ws)).await);
    assert!(why.contains("not switched on in this workspace"), "{why}");
    // No workspace: never.
    let why = words(offer(&state, None).await);
    assert!(why.contains("has no workspace"), "{why}");

    let (status, body) = request(
        &state,
        Method::PUT,
        &format!("/api/v1/workspaces/{ws}/plugins/{BROWSER_PID}"),
        Some(json!({"on": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // On, nothing downloaded: the user's click is the next step.
    let why = words(offer(&state, Some(&ws)).await);
    assert!(why.contains("Install under Tools"), "{why}");
    assert!(why.contains("example.invalid"), "{why}");

    let (status, done) = request(
        &state,
        Method::POST,
        &format!("/api/v1/plugins/{BROWSER_PID}/tools/fixture-tool/install"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    let tools = crate::plugins::toolchain::tools_root(&state)
        .join(BROWSER_PID)
        .join("fixture-tool");
    match offer(&state, Some(&ws)).await {
        Offer::Ready(exe) => assert_eq!(exe, tools.join("1.0.0/bin/fixture-tool")),
        other => panic!("expected the installed browser, got {other:?}"),
    }

    // Installed but its program not runnable: Remove and Install again,
    // never "click Install" on a card that shows it installed.
    {
        use std::os::unix::fs::PermissionsExt;
        let exe = tools.join("1.0.0/bin/fixture-tool");
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o644)).unwrap();
        let why = words(offer(&state, Some(&ws)).await);
        assert!(why.contains("Remove it and click Install again"), "{why}");
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // Another workspace where it is off gets nothing from it.
    let other = make_workspace(&state, "browser-plugin-off").await;
    let why = words(offer(&state, Some(&other)).await);
    assert!(why.contains("not switched on"), "{why}");

    let (status, _) = request(
        &state,
        Method::DELETE,
        &format!("/api/v1/plugins/{BROWSER_PID}/tools/fixture-tool"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let why = words(offer(&state, Some(&ws)).await);
    assert!(why.contains("isn't downloaded"), "{why}");
}
