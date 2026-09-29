//! Programs and tools, the privileged tier, proven with its fixture
//! (`plugins/test-privileged`): a declared program runs as a job under the
//! host's limits and the plugin hears `job-finished`; a job past its time
//! loses its whole process group; the queue holds one per plugin and caps
//! the waiting; undeclared programs, the host's own variables and paths out
//! of the workspace are refused; a tool downloads from a local server,
//! checks its sha256, unpacks, runs its setup and serves its program; a
//! wrong checksum installs nothing; an agent's tool waits for its job; and
//! switching the plugin off, or a hard block, stops its jobs.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::plugin_updates::FakeReleases;
use super::support::*;
use crate::{lock, AppState};

const PID: &str = "test-privileged";

/// The tests that point the process-wide download override at their own
/// server run one at a time (in parallel, one would fetch from the other's
/// server after it stopped).
static TOOL_DOWNLOADS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
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
