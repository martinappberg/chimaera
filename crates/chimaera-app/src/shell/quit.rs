//! Quitting while an agent is working on this computer asks whether that work
//! keeps going here or continues in the cloud.
//!
//! **When it asks.** A quit (⌘Q, the menu or tray, Dock › Quit, logging out)
//! or closing the last window when that ends the app, once the unsaved-edits
//! guard has let it through (`unsaved` always runs first; the two dialogs stay
//! separate), while the account's cloud could run the work and the daemon
//! reports a project whose agents are running work that the cloud could take
//! now (`working_agents` and `cloud_handoff` on `GET /pro/status`), each of
//! those agents' own provider signed in there (`Pro::cloud_agents`, from the
//! last provider catalog read). A project whose agent is not is left out of
//! the question; when none is left, nothing changes: the app
//! exits and the daemon, which outlives the app by design, keeps the agents
//! running here. Closing any other window never asks.
//!
//! **The question** is a native dialog: "Claude is still working in
//! <project>" (or "Agents are still working in 2 projects") and "Keep working
//! on this Mac, or continue in the cloud?", with **Keep working here** (the
//! default: today's quit), **Continue in the cloud** and **Cancel** (nothing
//! happens).
//!
//! **Continue in the cloud** reuses the sleep handoff: `POST /pro/sleep` with
//! `park` and the named projects, within [`BUDGET`], while a small window
//! (`assets/handoff.html`) says "Sending your work to the cloud…". Done, or
//! the budget spent (the daemon's flush is an owned task and finishes by
//! itself), and the app quits. A handover that failed says so in the same
//! window ("The cloud couldn't take over. Your work continues on this Mac.");
//! its Quit leaves the daemon running that work here. Closing that window, or
//! quitting again, quits at once. The daemon keeps a handed-over project in
//! the cloud (parked) until the app's next launch posts `/pro/wake`
//! ([`welcome_back`]).
//!
//! The decisions (which projects, the words, the daemon's answer) are pure
//! and unit-tested; the glue below them only moves windows and dialogs.

use std::io::Read;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{
    DialogExt, MessageDialogButtons, MessageDialogKind, MessageDialogResult,
};

use super::{lock, Shell};

/// The shell-local window shown while work moves to the cloud.
pub(crate) const HANDOFF_WINDOW: &str = "cloud-handoff";

/// How long the app waits for the handover before quitting anyway: the
/// macOS sleep budget, which the daemon's flush is already shaped for.
const BUDGET: Duration = Duration::from_secs(25);

/// Extra wait for the daemon's answer past its own deadline.
const REPLY_GRACE: Duration = Duration::from_secs(5);

/// The quit must not hang on the daemon: an answer this late asks nothing.
const STATUS_TIMEOUT: Duration = Duration::from_millis(1500);

/// Cap on the status body read at quit.
const STATUS_MAX: u64 = 2 * 1024 * 1024;

/// How long a failure waits for the handover page to load before saying so.
const PAGE_LOAD_WAIT: Duration = Duration::from_secs(3);

const KEEP: &str = "Keep working here";
const CLOUD: &str = "Continue in the cloud";
const CANCEL: &str = "Cancel";

/// Where the work runs when it does not move, in the user's words.
const HERE: &str = if cfg!(target_os = "macos") {
    "this Mac"
} else {
    "this computer"
};

/// A project with agents running work that the cloud could take now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Working {
    pub(crate) workspace_id: String,
    pub(crate) name: String,
    /// Agent kinds as the daemon names them (`claude`, `codex`, ...).
    pub(crate) agents: Vec<String>,
}

/// The cloud provider an agent kind signs in with; `None` for one the cloud
/// cannot run.
fn cloud_provider(kind: &str) -> Option<&'static str> {
    match kind {
        "claude" => Some("claude"),
        "codex" => Some("codex"),
        _ => None,
    }
}

/// The projects the question is about, from the daemon's `/pro/status`:
/// working, takeable by the cloud, and every working agent's own provider
/// among `connected` (the providers signed in in the cloud). Nothing when the
/// daemon is not set up for Pro.
pub(crate) fn working_projects(status: &Value, connected: &[String]) -> Vec<Working> {
    if status["configured"] != true {
        return Vec::new();
    }
    let continues = |kind: &String| {
        cloud_provider(kind).is_some_and(|provider| connected.iter().any(|id| id == provider))
    };
    let rows = status["workspaces"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    rows.iter()
        .take(128)
        .filter(|row| row["cloud_handoff"] == true)
        .filter_map(|row| {
            let workspace_id = row["workspace_id"].as_str()?.to_owned();
            let agents: Vec<String> = row["working_agents"]
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .take(8)
                .map(str::to_owned)
                .collect();
            // An agent the cloud has no sign-in for would only wait there.
            (!agents.is_empty() && agents.iter().all(continues)).then(|| Working {
                workspace_id,
                name: project_name(row["name"].as_str()),
                agents,
            })
        })
        .collect()
}

/// Whether the daemon holds any project in the cloud for the app's return.
pub(crate) fn any_parked(status: &Value) -> bool {
    status["workspaces"]
        .as_array()
        .is_some_and(|rows| rows.iter().any(|row| row["parked"] == true))
}

fn project_name(name: Option<&str>) -> String {
    let name = name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("a project");
    if name.chars().count() > 60 {
        format!("{}…", name.chars().take(59).collect::<String>())
    } else {
        name.to_owned()
    }
}

/// An agent kind's product name; `None` for one this app does not know.
fn agent_name(kind: &str) -> Option<&'static str> {
    match kind {
        "claude" => Some("Claude"),
        "codex" => Some("Codex"),
        "gemini" => Some("Gemini"),
        "agy" => Some("Antigravity"),
        _ => None,
    }
}

/// The dialog's title: which agents are working, and where.
pub(crate) fn title(working: &[Working]) -> String {
    let [project] = working else {
        return format!("Agents are still working in {} projects", working.len());
    };
    let names: Option<Vec<&str>> = project.agents.iter().map(|kind| agent_name(kind)).collect();
    let who = match names.as_deref() {
        Some([one]) => format!("{one} is"),
        Some([first, second]) => format!("{first} and {second} are"),
        _ if project.agents.len() == 1 => "An agent is".to_owned(),
        _ => "Agents are".to_owned(),
    };
    format!("{who} still working in {}", project.name)
}

/// The dialog's question.
pub(crate) fn question(here: &str) -> String {
    format!("Keep working on {here}, or continue in the cloud?")
}

/// The handover window's words when the cloud could not take over: which
/// work stays here, and that it keeps running here.
pub(crate) fn failure(working: &[Working], failed: &[String], here: &str) -> (String, String) {
    let stuck: Vec<&str> = working
        .iter()
        .filter(|project| failed.contains(&project.workspace_id))
        .map(|project| project.name.as_str())
        .collect();
    if stuck.is_empty() || stuck.len() == working.len() {
        return (
            "The cloud couldn't take over.".to_owned(),
            format!("Your work continues on {here}."),
        );
    }
    (
        format!("The cloud couldn't take over {}.", list(&stuck)),
        format!("That work continues on {here}. The rest is in the cloud."),
    )
}

fn list(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// How the handover ended, from the daemon's `/pro/sleep` answer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Every named project is in the cloud.
    Moved,
    /// The budget ran out with the flushes still going: they finish (or keep
    /// the work here) on their own.
    Continuing,
    /// These projects (all of them when empty) stay here.
    Failed(Vec<String>),
}

pub(crate) fn outcome(reply: &Value) -> Outcome {
    if reply["handoff"] == true {
        return Outcome::Moved;
    }
    let failed: Vec<String> = reply["failed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["workspace_id"].as_str())
        .take(128)
        .map(str::to_owned)
        .collect();
    if reply["reason"] == "deadline" && failed.is_empty() {
        return Outcome::Continuing;
    }
    Outcome::Failed(failed)
}

/// The quit handover request: the sleep flush's own body, parked, for
/// exactly these projects.
fn handoff_body(working: &[Working]) -> Value {
    let mut body = super::power::sleep_body(BUDGET);
    body["park"] = json!(true);
    body["workspace_ids"] = working
        .iter()
        .map(|project| project.workspace_id.as_str())
        .collect::<Vec<_>>()
        .into();
    body
}

#[derive(Debug, PartialEq, Eq)]
enum Choice {
    Keep,
    Cloud,
    Cancel,
}

fn choice(result: &MessageDialogResult) -> Choice {
    match result {
        MessageDialogResult::Custom(text) if text == KEEP => Choice::Keep,
        MessageDialogResult::Custom(text) if text == CLOUD => Choice::Cloud,
        // Platforms that answer with the button's role, not its text.
        MessageDialogResult::Yes => Choice::Keep,
        MessageDialogResult::No => Choice::Cloud,
        _ => Choice::Cancel,
    }
}

/// What finishes a quit once the question is settled.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Trigger {
    /// A quit: through `finish_quit` (the window set is kept for next launch).
    Quit,
    /// Closing this window, the last one: it is destroyed (a closed window
    /// stays closed), and the app ends as it always did.
    Close(String),
}

/// Where the question stands for the quit (or close) in progress.
#[derive(Default)]
pub(crate) struct Gate(Phase);

#[derive(Debug, Default, PartialEq, Eq)]
enum Phase {
    /// The next quit or last close checks.
    #[default]
    Idle,
    /// Reading the daemon, or waiting for the user's answer.
    Asking,
    /// Handing work to the cloud; the trigger finishes when it ends.
    Sending(Trigger),
    /// The cloud couldn't take over; the trigger finishes on Quit.
    Failed(Trigger),
    /// Decided: the quit goes ahead without asking again.
    Settled,
}

/// What a new quit or last close does in `phase`.
#[derive(Debug, PartialEq, Eq)]
enum Request {
    /// Nothing to ask (any more): go ahead.
    Pass,
    /// Check whether to ask.
    Check,
    /// A question is on screen: bring it forward, and wait.
    Hold,
    /// Work is moving (or could not): quit now without waiting.
    Finish,
}

fn request(phase: &Phase) -> Request {
    match phase {
        Phase::Idle => Request::Check,
        Phase::Asking => Request::Hold,
        Phase::Sending(_) | Phase::Failed(_) => Request::Finish,
        Phase::Settled => Request::Pass,
    }
}

// --- Tauri glue ---------------------------------------------------------------

/// What the question needs from the shell, taken before any wait.
struct Probe {
    port: u16,
    token: String,
    /// The agent providers signed in in the cloud.
    connected: Vec<String>,
}

impl Probe {
    /// `None` when the cloud could not continue any agent for this account
    /// (the daemon is not even asked): the quit is what it always was.
    fn new(shell: &Shell) -> Option<Self> {
        let connected = shell.pro.cloud_agents();
        if connected.is_empty() {
            return None;
        }
        let local = lock(&shell.local).clone();
        Some(Self {
            port: local.port,
            token: local.token,
            connected,
        })
    }

    /// Blocking and bounded ([`STATUS_TIMEOUT`]); no answer asks nothing.
    fn working(&self) -> Vec<Working> {
        read_status(self.port, &self.token)
            .map(|status| working_projects(&status, &self.connected))
            .unwrap_or_default()
    }
}

fn read_status(port: u16, token: &str) -> Option<Value> {
    let url = format!("http://127.0.0.1:{port}/api/v1/pro/status");
    let authorization = format!("Bearer {token}");
    let mut response = crate::http::agent()
        .get(&url)
        .header("Authorization", &authorization)
        .config()
        .timeout_global(Some(STATUS_TIMEOUT))
        .max_redirects(0)
        .build()
        .call()
        .ok()?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(STATUS_MAX + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > STATUS_MAX {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

/// `finish_quit`, past the unsaved-edits guard. True = held: the question
/// (or the handover) owns this quit and finishes it through `finish_quit`.
pub(crate) fn hold_quit(app: &AppHandle) -> bool {
    hold(app, Trigger::Quit)
}

/// A close the unsaved-edits guard let through, of the window whose close
/// ends the app. True = held (the caller prevents the close).
pub(crate) fn hold_last_close(app: &AppHandle, label: &str) -> bool {
    hold(app, Trigger::Close(label.to_owned()))
}

fn hold(app: &AppHandle, trigger: Trigger) -> bool {
    let Some(shell) = app.try_state::<Shell>() else {
        return false;
    };
    let probe = Probe::new(&shell);
    let mut gate = lock(&shell.quit_gate);
    match request(&gate.0) {
        Request::Pass => false,
        Request::Hold => {
            drop(gate);
            raise(app, &trigger);
            true
        }
        Request::Finish => {
            drop(gate);
            finish(app);
            true
        }
        Request::Check => {
            let Some(probe) = probe else {
                return false;
            };
            gate.0 = Phase::Asking;
            drop(gate);
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let working = tauri::async_runtime::spawn_blocking(move || probe.working())
                    .await
                    .unwrap_or_default();
                decide(app, trigger, working).await;
            });
            true
        }
    }
}

/// macOS's terminate hook (Dock › Quit, logging out, restarting) needs its
/// answer before it returns, so this reads the daemon right here, bounded by
/// [`STATUS_TIMEOUT`]. True = held (AppKit is told to cancel; the question
/// finishes the quit itself).
#[cfg(target_os = "macos")]
pub(crate) fn hold_os_quit(app: &AppHandle) -> bool {
    let Some(shell) = app.try_state::<Shell>() else {
        return false;
    };
    let probe = Probe::new(&shell);
    let mut gate = lock(&shell.quit_gate);
    match request(&gate.0) {
        // Already moving work: let this quit go; the daemon finishes the
        // handover by itself.
        Request::Pass | Request::Finish => false,
        Request::Hold => {
            drop(gate);
            raise(app, &Trigger::Quit);
            true
        }
        Request::Check => {
            let Some(probe) = probe else {
                return false;
            };
            gate.0 = Phase::Asking;
            drop(gate);
            let working = probe.working();
            if working.is_empty() {
                lock(&shell.quit_gate).0 = Phase::Idle;
                return false;
            }
            tauri::async_runtime::spawn(decide(app.clone(), Trigger::Quit, working));
            true
        }
    }
}

async fn decide(app: AppHandle, trigger: Trigger, working: Vec<Working>) {
    if working.is_empty() {
        return proceed(&app, trigger);
    }
    raise(&app, &trigger);
    let (answer, answered) = tokio::sync::oneshot::channel();
    app.dialog()
        .message(question(HERE))
        .title(title(&working))
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::YesNoCancelCustom(
            KEEP.into(),
            CLOUD.into(),
            CANCEL.into(),
        ))
        .show_with_result(move |result| {
            let _ = answer.send(result);
        });
    match answered
        .await
        .map_or(Choice::Cancel, |result| choice(&result))
    {
        Choice::Keep => proceed(&app, trigger),
        Choice::Cloud => send(app, trigger, working).await,
        Choice::Cancel => lock(&app.state::<Shell>().quit_gate).0 = Phase::Idle,
    }
}

/// Finish the quit or close the way it would have gone without the question.
fn proceed(app: &AppHandle, trigger: Trigger) {
    let shell = app.state::<Shell>();
    match trigger {
        Trigger::Quit => {
            lock(&shell.quit_gate).0 = Phase::Settled;
            super::finish_quit(app);
        }
        Trigger::Close(label) => {
            lock(&shell.quit_gate).0 = Phase::Idle;
            // Destroyed, not closed: this close already passed both guards.
            if let Some(window) = app.get_webview_window(&label) {
                let _ = window.destroy();
            }
        }
    }
}

async fn send(app: AppHandle, trigger: Trigger, working: Vec<Working>) {
    let shell = app.state::<Shell>();
    lock(&shell.quit_gate).0 = Phase::Sending(trigger);
    let window = open_window(&app, &working);
    let reply = tokio::time::timeout(
        BUDGET + REPLY_GRACE,
        super::pro::daemon_request(&shell, "POST", "/pro/sleep", Some(handoff_body(&working))),
    )
    .await;
    let failed = match reply {
        Err(_) => return finish(&app),
        // The daemon refused or could not be reached: nothing moved.
        Ok(Err(_)) => Vec::new(),
        Ok(Ok(reply)) => match outcome(&reply) {
            Outcome::Moved | Outcome::Continuing => return finish(&app),
            Outcome::Failed(failed) => failed,
        },
    };
    {
        // The user may have quit meanwhile (the window, or another quit).
        let mut gate = lock(&shell.quit_gate);
        match std::mem::take(&mut gate.0) {
            Phase::Sending(trigger) => gate.0 = Phase::Failed(trigger),
            other => {
                gate.0 = other;
                return;
            }
        }
    }
    let (heading, detail) = failure(&working, &failed, HERE);
    let call = format!(
        "window.chimaeraHandoffFailed({}, {})",
        json!(heading),
        json!(detail)
    );
    if let Some((window, mut loaded)) = window {
        // A refusal can come back before the page has loaded; told earlier,
        // it would never show its Quit button.
        let _ = tokio::time::timeout(PAGE_LOAD_WAIT, loaded.wait_for(|loaded| *loaded)).await;
        if window.eval(call).is_ok() {
            return;
        }
    }
    // No window to say it in: the same words in a native dialog.
    let (done, closed) = tokio::sync::oneshot::channel();
    app.dialog()
        .message(detail)
        .title(heading)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCustom("Quit".into()))
        .show(move |_| {
            let _ = done.send(());
        });
    let _ = closed.await;
    finish(&app);
}

/// Quit now: the handover ended, its window closed, or the user quit again.
/// The daemon finishes (or already ended) the handover by itself.
fn finish(app: &AppHandle) {
    let Some(shell) = app.try_state::<Shell>() else {
        return;
    };
    let trigger = {
        let mut gate = lock(&shell.quit_gate);
        match std::mem::take(&mut gate.0) {
            Phase::Sending(trigger) | Phase::Failed(trigger) => trigger,
            other => {
                gate.0 = other;
                return;
            }
        }
    };
    if let Some(window) = app.get_webview_window(HANDOFF_WINDOW) {
        let _ = window.destroy();
    }
    proceed(app, trigger);
}

/// The handover window's close (its Quit button, or the title bar): quit now.
pub(crate) fn handoff_window_closing(app: &AppHandle) {
    finish(app);
    // A window with no handover behind it simply goes.
    if let Some(window) = app.get_webview_window(HANDOFF_WINDOW) {
        let _ = window.destroy();
    }
}

/// The handover window, and whether its page has finished loading.
fn open_window(
    app: &AppHandle,
    working: &[Working],
) -> Option<(tauri::WebviewWindow, tokio::sync::watch::Receiver<bool>)> {
    let names: Vec<&str> = working
        .iter()
        .map(|project| project.name.as_str())
        .collect();
    let script = format!(
        "window.__CHIMAERA_HANDOFF__ = {};",
        json!({ "projects": names })
    );
    let (loaded, loading) = tokio::sync::watch::channel(false);
    tauri::WebviewWindowBuilder::new(
        app,
        HANDOFF_WINDOW,
        tauri::WebviewUrl::App("handoff.html".into()),
    )
    .title("Continue in the cloud")
    .inner_size(440.0, 280.0)
    .resizable(false)
    .minimizable(false)
    .maximizable(false)
    .always_on_top(true)
    .center()
    .focused(true)
    .initialization_script(script)
    .on_page_load(move |_, payload| {
        if payload.event() == tauri::webview::PageLoadEvent::Finished {
            loaded.send_replace(true);
        }
    })
    .build()
    .inspect_err(|error| tracing::warn!(%error, "could not open the handover window"))
    .ok()
    .map(|window| (window, loading))
}

/// Bring the question forward: the handover window, the window being
/// closed, or the most recently used one.
fn raise(app: &AppHandle, trigger: &Trigger) {
    let target = app
        .get_webview_window(HANDOFF_WINDOW)
        .or_else(|| match trigger {
            Trigger::Close(label) => app.get_webview_window(label),
            Trigger::Quit => None,
        })
        .or_else(|| {
            let label = lock(&app.state::<Shell>().last_focused_window).clone();
            label.and_then(|label| app.get_webview_window(&label))
        });
    #[cfg(target_os = "macos")]
    let _ = app.show();
    if let Some(window) = target {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// At launch: projects handed to the cloud on the last quit may come home
/// again by the usual rules, so the daemon is told the app is back
/// (`/pro/wake`). Only when something is parked: a wake also restarts the
/// daemon's awake-on-power clock for moving live cloud work home.
pub(crate) fn welcome_back(app: &AppHandle) {
    if !app.state::<Shell>().pro.has_endpoint() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let shell = app.state::<Shell>();
        // A daemon just started may still be settling: a few tries, then
        // the next launch tries again.
        for attempt in 0..3u32 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            let Ok(status) = super::pro::daemon_request(&shell, "GET", "/pro/status", None).await
            else {
                continue;
            };
            if !any_parked(&status) {
                return;
            }
            if super::pro::daemon_request(&shell, "POST", "/pro/wake", None)
                .await
                .is_ok()
            {
                return;
            }
        }
        tracing::warn!("could not tell the daemon the app is back");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(rows: Value) -> Value {
        json!({"configured": true, "workspaces": rows})
    }

    fn working(id: &str, name: &str, agents: &[&str]) -> Working {
        Working {
            workspace_id: id.into(),
            name: name.into(),
            agents: agents.iter().map(|agent| agent.to_string()).collect(),
        }
    }

    fn providers(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn asks_only_about_working_projects_the_cloud_could_take() {
        let reply = status(json!([
            {"workspace_id": "w-a", "name": "atlas", "cloud_handoff": true, "working_agents": ["claude"]},
            // Idle: nothing to ask about.
            {"workspace_id": "w-b", "name": "idle", "cloud_handoff": true, "working_agents": []},
            // Working, but the cloud could not take it (kept here, no lease, ...).
            {"workspace_id": "w-c", "name": "private", "cloud_handoff": false, "working_agents": ["codex"]},
            // An older daemon that knows neither field.
            {"workspace_id": "w-d", "name": "old"},
            {"workspace_id": "w-e", "name": "  ", "cloud_handoff": true, "working_agents": ["codex", "claude"]},
        ]));
        assert_eq!(
            working_projects(&reply, &providers(&["claude", "codex"])),
            vec![
                working("w-a", "atlas", &["claude"]),
                working("w-e", "a project", &["codex", "claude"]),
            ]
        );
    }

    #[test]
    fn a_project_is_offered_only_when_every_working_agent_is_signed_in_in_the_cloud() {
        let reply = status(json!([
            {"workspace_id": "w-a", "name": "atlas", "cloud_handoff": true, "working_agents": ["claude"]},
            {"workspace_id": "w-b", "name": "borealis", "cloud_handoff": true, "working_agents": ["codex"]},
            // One of its two agents could only wait in the cloud.
            {"workspace_id": "w-c", "name": "cirrus", "cloud_handoff": true, "working_agents": ["claude", "codex"]},
            // An agent the cloud cannot run at all.
            {"workspace_id": "w-d", "name": "delta", "cloud_handoff": true, "working_agents": ["gemini"]},
        ]));
        // Only Claude is signed in there: only its project is offered.
        assert_eq!(
            working_projects(&reply, &providers(&["claude"])),
            vec![working("w-a", "atlas", &["claude"])]
        );
        assert_eq!(
            working_projects(&reply, &providers(&["codex", "claude"])),
            vec![
                working("w-a", "atlas", &["claude"]),
                working("w-b", "borealis", &["codex"]),
                working("w-c", "cirrus", &["claude", "codex"]),
            ]
        );
        // No agent signed in there (or never seen): no question, quit as today.
        assert!(working_projects(&reply, &[]).is_empty());
        // A repository sign-in is not an agent.
        assert!(working_projects(&reply, &providers(&["github"])).is_empty());
    }

    #[test]
    fn nothing_to_ask_without_pro_setup_or_an_answer() {
        let connected = providers(&["claude"]);
        let rows = json!([{"workspace_id": "w-a", "name": "atlas", "cloud_handoff": true, "working_agents": ["claude"]}]);
        assert!(working_projects(
            &json!({"configured": false, "workspaces": rows}),
            &connected
        )
        .is_empty());
        assert!(working_projects(&Value::Null, &connected).is_empty());
        assert!(working_projects(&json!({"configured": true}), &connected).is_empty());
    }

    #[test]
    fn the_title_names_the_real_agent_or_counts_projects() {
        assert_eq!(
            title(&[working("w", "atlas", &["claude"])]),
            "Claude is still working in atlas"
        );
        assert_eq!(
            title(&[working("w", "atlas", &["codex", "claude"])]),
            "Codex and Claude are still working in atlas"
        );
        assert_eq!(
            title(&[working("w", "atlas", &["claude", "codex", "gemini"])]),
            "Agents are still working in atlas"
        );
        assert_eq!(
            title(&[working("w", "atlas", &["something-new"])]),
            "An agent is still working in atlas"
        );
        assert_eq!(
            title(&[
                working("w-a", "atlas", &["claude"]),
                working("w-b", "borealis", &["claude"]),
            ]),
            "Agents are still working in 2 projects"
        );
        assert_eq!(
            question("this Mac"),
            "Keep working on this Mac, or continue in the cloud?"
        );
    }

    #[test]
    fn long_project_names_are_shortened() {
        let name = "x".repeat(80);
        let shown = project_name(Some(&name));
        assert_eq!(shown.chars().count(), 60);
        assert!(shown.ends_with('…'));
    }

    #[test]
    fn the_daemons_answer_decides_quit_or_explain() {
        assert_eq!(
            outcome(&json!({"handoff": true, "failed": []})),
            Outcome::Moved
        );
        assert_eq!(
            outcome(
                &json!({"handoff": false, "reason": "deadline", "pending": ["w-a"], "failed": []})
            ),
            Outcome::Continuing
        );
        assert_eq!(
            outcome(
                &json!({"handoff": false, "failed": [{"workspace_id": "w-a", "error": "transfer"}]})
            ),
            Outcome::Failed(vec!["w-a".into()])
        );
        // A listed project the daemon could not take, beside ones still going.
        assert_eq!(
            outcome(
                &json!({"handoff": false, "reason": "deadline", "failed": [{"workspace_id": "w-b", "error": "unavailable"}]})
            ),
            Outcome::Failed(vec!["w-b".into()])
        );
        // Refused before starting (no cloud hours, busy), or not set up (204).
        assert_eq!(
            outcome(&json!({"handoff": false, "reason": "cloud_hours_exhausted"})),
            Outcome::Failed(Vec::new())
        );
        assert_eq!(outcome(&Value::Null), Outcome::Failed(Vec::new()));
    }

    #[test]
    fn a_failure_says_which_work_stays_here() {
        let both = [
            working("w-a", "atlas", &["claude"]),
            working("w-b", "borealis", &["codex"]),
        ];
        assert_eq!(
            failure(&both, &[], "this Mac"),
            (
                "The cloud couldn't take over.".into(),
                "Your work continues on this Mac.".into()
            )
        );
        assert_eq!(
            failure(&both, &["w-a".into(), "w-b".into()], "this Mac").1,
            "Your work continues on this Mac."
        );
        assert_eq!(
            failure(&both, &["w-b".into()], "this computer"),
            (
                "The cloud couldn't take over borealis.".into(),
                "That work continues on this computer. The rest is in the cloud.".into()
            )
        );
        assert_eq!(list(&["a", "b", "c"]), "a, b and c");
    }

    #[test]
    fn the_request_is_the_sleep_handoff_parked_for_the_named_projects() {
        let body = handoff_body(&[
            working("w-a", "atlas", &["claude"]),
            working("w-b", "borealis", &["codex"]),
        ]);
        assert_eq!(body["park"], true);
        assert_eq!(body["workspace_ids"], json!(["w-a", "w-b"]));
        assert_eq!(body["deadline_ms"], 23_000);
    }

    #[test]
    fn dialog_buttons_map_to_choices() {
        assert_eq!(
            choice(&MessageDialogResult::Custom(KEEP.into())),
            Choice::Keep
        );
        assert_eq!(
            choice(&MessageDialogResult::Custom(CLOUD.into())),
            Choice::Cloud
        );
        assert_eq!(
            choice(&MessageDialogResult::Custom(CANCEL.into())),
            Choice::Cancel
        );
        // Escape, or a dialog that could not be shown.
        assert_eq!(choice(&MessageDialogResult::Cancel), Choice::Cancel);
        assert_eq!(choice(&MessageDialogResult::Yes), Choice::Keep);
    }

    #[test]
    fn a_quit_in_each_phase() {
        assert_eq!(request(&Phase::Idle), Request::Check);
        // The question is on screen: a second ⌘Q only brings it forward.
        assert_eq!(request(&Phase::Asking), Request::Hold);
        // Work is moving or could not move: quitting again quits now.
        assert_eq!(request(&Phase::Sending(Trigger::Quit)), Request::Finish);
        assert_eq!(
            request(&Phase::Failed(Trigger::Close("home".into()))),
            Request::Finish
        );
        assert_eq!(request(&Phase::Settled), Request::Pass);
    }

    #[test]
    fn parked_projects_are_noticed_at_launch() {
        assert!(any_parked(&status(
            json!([{"workspace_id": "w-a", "parked": false}, {"workspace_id": "w-b", "parked": true}])
        )));
        assert!(!any_parked(&status(
            json!([{"workspace_id": "w-a", "parked": false}])
        )));
        assert!(!any_parked(&status(json!([{"workspace_id": "w-a"}]))));
    }
}
