//! Transient connector sign-in jobs. The real CLI owns OAuth and credential
//! storage; we retain only a bounded authorization URL and status in memory.
use super::*;
use tokio::sync::{mpsc, watch};

const AUTH_LIMIT: usize = 16;
const AUTH_LIFETIME: Duration = Duration::from_secs(600);
const OUTPUT_LIMIT: usize = 64 * 1024;
const URL_LIMIT: usize = 8192;

#[derive(Default)]
pub(crate) struct AuthState(Mutex<HashMap<String, Arc<Attempt>>>);

struct Attempt {
    workspace: String,
    kind: AgentKind,
    created: Instant,
    view: Mutex<AuthView>,
    input: mpsc::Sender<Input>,
    cancel: watch::Sender<bool>,
}

#[derive(Clone, Serialize)]
struct AuthView {
    id: String,
    agent: String,
    name: String,
    state: String,
    authorization_url: Option<String>,
    message: Option<String>,
}

impl AuthView {
    fn finished(&self) -> bool {
        matches!(self.state.as_str(), "succeeded" | "failed" | "cancelled")
    }
}

impl Attempt {
    fn snapshot(&self) -> AuthView {
        crate::lock(&self.view).clone()
    }
    fn update(&self, state: &str, message: Option<&str>) {
        let mut view = crate::lock(&self.view);
        view.state = state.into();
        view.message = message.map(str::to_string);
        if view.finished() {
            view.authorization_url = None;
        }
    }
}

enum Input {
    Callback(String),
    Check,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Login {
    agent: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Callback {
    url: String,
}

fn refusal(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error":message}))).into_response()
}

pub(crate) async fn login(
    State(state): State<Arc<AppState>>,
    AxPath(ws): AxPath<String>,
    Json(body): Json<Login>,
) -> Response {
    let Some(root) = workspace_root(&state, &ws) else {
        return not_found();
    };
    let Some(kind @ (AgentKind::Claude | AgentKind::Codex)) = AgentKind::parse(&body.agent) else {
        return refusal(
            StatusCode::BAD_REQUEST,
            "Connections are supported for Claude and Codex.",
        );
    };
    if !valid_name(&body.name) {
        return refusal(StatusCode::BAD_REQUEST, "Invalid connection name.");
    }
    let (sender, receiver) = mpsc::channel(1);
    let (cancel, cancellation) = watch::channel(false);
    let attempt = {
        let mut attempts = crate::lock(&state.probes.connection_auth.0);
        attempts.retain(|_, a| a.created.elapsed() < AUTH_LIFETIME || !a.snapshot().finished());
        for existing in attempts.values() {
            let view = existing.snapshot();
            if existing.kind == kind && view.name == body.name && !view.finished() {
                if existing.workspace != ws {
                    return refusal(
                        StatusCode::CONFLICT,
                        "This connection is being signed in from another workspace.",
                    );
                }
                return auth_response(view);
            }
        }
        if attempts.len() >= AUTH_LIMIT {
            let oldest = attempts
                .iter()
                .filter(|(_, a)| a.snapshot().finished())
                .min_by_key(|(_, a)| a.created)
                .map(|(id, _)| id.clone());
            if let Some(id) = oldest {
                attempts.remove(&id);
            }
        }
        if attempts.len() >= AUTH_LIMIT {
            return refusal(
                StatusCode::CONFLICT,
                "Finish an existing connection sign-in first.",
            );
        }
        let id = crate::agents::fresh_session_id();
        let attempt = Arc::new(Attempt {
            workspace: ws,
            kind,
            created: Instant::now(),
            view: Mutex::new(AuthView {
                id: id.clone(),
                agent: body.agent,
                name: body.name,
                state: "starting".into(),
                authorization_url: None,
                message: None,
            }),
            input: sender,
            cancel,
        });
        attempts.insert(id, attempt.clone());
        attempt
    };
    let answer = attempt.snapshot();
    tokio::spawn(run(state, attempt, root, receiver, cancellation));
    auth_response(answer)
}

fn lookup(state: &AppState, ws: &str, id: &str) -> Option<Arc<Attempt>> {
    workspace_root(state, ws)?;
    crate::lock(&state.probes.connection_auth.0)
        .get(id)
        .filter(|a| a.workspace == ws)
        .cloned()
}

fn auth_response(view: AuthView) -> Response {
    let mut response = Json(view).into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

pub(crate) async fn status(
    State(state): State<Arc<AppState>>,
    AxPath((ws, id)): AxPath<(String, String)>,
) -> Response {
    let Some(attempt) = lookup(&state, &ws, &id) else {
        return not_found();
    };
    auth_response(attempt.snapshot())
}

pub(crate) async fn cancel(
    State(state): State<Arc<AppState>>,
    AxPath((ws, id)): AxPath<(String, String)>,
) -> Response {
    let Some(attempt) = lookup(&state, &ws, &id) else {
        return not_found();
    };
    let _ = attempt.cancel.send(true);
    StatusCode::NO_CONTENT.into_response()
}

pub(crate) async fn input(
    State(state): State<Arc<AppState>>,
    AxPath((ws, id)): AxPath<(String, String)>,
    Json(body): Json<Callback>,
) -> Response {
    let Some(attempt) = lookup(&state, &ws, &id) else {
        return not_found();
    };
    let url = body.url.trim();
    if !safe_url(url) {
        return refusal(
            StatusCode::BAD_REQUEST,
            "Paste the complete http or https callback URL from your browser.",
        );
    }
    let mut view = crate::lock(&attempt.view);
    if view.state != "awaiting_callback" {
        return refusal(
            StatusCode::CONFLICT,
            "This sign-in isn't waiting for a callback URL.",
        );
    }
    if attempt.input.try_send(Input::Callback(url.into())).is_err() {
        return refusal(StatusCode::CONFLICT, "A callback is already being checked.");
    }
    view.state = "verifying".into();
    StatusCode::NO_CONTENT.into_response()
}

pub(crate) async fn check(
    State(state): State<Arc<AppState>>,
    AxPath((ws, id)): AxPath<(String, String)>,
) -> Response {
    let Some(attempt) = lookup(&state, &ws, &id) else {
        return not_found();
    };
    let mut view = crate::lock(&attempt.view);
    if view.state != "awaiting_browser" {
        return refusal(
            StatusCode::CONFLICT,
            "This sign-in isn't waiting for browser authorization.",
        );
    }
    if attempt.input.try_send(Input::Check).is_err() {
        return refusal(
            StatusCode::CONFLICT,
            "The connection is already being checked.",
        );
    }
    view.state = "verifying".into();
    StatusCode::NO_CONTENT.into_response()
}

fn safe_url(url: &str) -> bool {
    if url.len() > URL_LIMIT
        || url.chars().any(char::is_whitespace)
        || url.chars().any(char::is_control)
    {
        return false;
    }
    let Ok(uri) = url.parse::<axum::http::Uri>() else {
        return false;
    };
    matches!(uri.scheme_str(), Some("https" | "http"))
        && uri.authority().is_some_and(|a| !a.as_str().contains('@'))
}

fn authorization_url(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let marker = lower.find("authoriz")?;
    text[marker..].split_whitespace().find_map(|part| {
        let url = part.split('\x1b').next()?.trim_matches(['\'', '"']);
        safe_url(url).then(|| url.to_string())
    })
}

async fn verify(state: &Arc<AppState>, attempt: &Attempt, root: &Path) -> bool {
    state.probes.invalidate();
    let report = probe(state, &attempt.workspace, root, attempt.kind).await;
    let name = attempt.snapshot().name;
    report["connections"].as_array().is_some_and(|rows| {
        rows.iter().any(|r| {
            r["name"] == name
                && r["kind"] == "mcp"
                && matches!(r["status"].as_str(), Some("connected" | "authenticated"))
        })
    })
}

async fn run(
    state: Arc<AppState>,
    attempt: Arc<Attempt>,
    root: PathBuf,
    mut input: mpsc::Receiver<Input>,
    mut cancel: watch::Receiver<bool>,
) {
    let work = async {
        state.probes.invalidate();
        let report = probe(&state, &attempt.workspace, &root, attempt.kind).await;
        let name = attempt.snapshot().name;
        let matches: Vec<&Value> = report["connections"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| r["kind"] == "mcp" && r["name"] == name)
            .collect();
        if matches.len() != 1 || matches[0]["login"] != true {
            return Err("This connection no longer offers sign-in. Check its status again.");
        }
        if attempt.kind == AgentKind::Claude && name.starts_with("claude.ai ") {
            // Hosted "needs authentication" also covers unconnected or broken
            // configurations. The CLI's start-auth URL forces OAuth, which can
            // fail registration for public connectors that need no OAuth at all.
            // Claude's settings own activation, repair and any required sign-in.
            {
                let mut view = crate::lock(&attempt.view);
                view.authorization_url =
                    Some("https://claude.ai/customize/connectors/yours".into());
                view.state = "awaiting_browser".into();
            }
            while let Some(command) = input.recv().await {
                if matches!(command, Input::Check) {
                    if verify(&state, &attempt, &root).await {
                        return Ok(());
                    }
                    attempt.update("awaiting_browser", Some("Not connected yet. Review this connector's setup or error in Claude, then check again."));
                }
            }
            return Err("The connection setup was closed.");
        }
        let (bin, _) = bin_of(&state, attempt.kind)
            .await
            .map_err(|_| "The agent is no longer available.")?;
        let prelude = ProbePrelude::write(&state, Some(&attempt.workspace)).await;
        let mut command = base_command(
            &wrapped(&bin, &["mcp", "login", "--no-browser", "--", &name]),
            Some(&root),
            prelude.path(),
        );
        command.stdin(Stdio::piped()).process_group(0);
        let mut child = command
            .spawn()
            .map_err(|_| "Couldn't start the agent's sign-in flow.")?;
        let group = GroupKill(child.id().map(|pid| nix::unistd::Pid::from_raw(pid as i32)));
        let mut stdin = child.stdin.take().ok_or("Couldn't open sign-in input.")?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or("Couldn't read sign-in progress.")?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or("Couldn't read sign-in progress.")?;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let (mut out_done, mut err_done) = (false, false);
        let (mut out_buf, mut err_buf) = ([0u8; 2048], [0u8; 2048]);
        while !out_done || !err_done {
            tokio::select! {
                read = stdout.read(&mut out_buf), if !out_done => {
                    let n = read.map_err(|_| "Couldn't read sign-in progress.")?;
                    out_done = n == 0; out.extend_from_slice(&out_buf[..n]);
                }
                read = stderr.read(&mut err_buf), if !err_done => {
                    let n = read.map_err(|_| "Couldn't read sign-in progress.")?;
                    err_done = n == 0; err.extend_from_slice(&err_buf[..n]);
                }
                Some(Input::Callback(url)) = input.recv() => {
                    stdin.write_all(url.as_bytes()).await.map_err(|_| "The sign-in flow stopped accepting input.")?;
                    stdin.write_all(b"\n").await.map_err(|_| "The sign-in flow stopped accepting input.")?;
                }
            }
            if out.len() + err.len() > OUTPUT_LIMIT {
                return Err("The agent returned too much sign-in output.");
            }
            let mut view = crate::lock(&attempt.view);
            if view.authorization_url.is_none() {
                // Only complete output tokens are considered; a URL split over
                // pipe reads must not become an incomplete authorization link.
                for bytes in [&out, &err] {
                    let text = String::from_utf8_lossy(bytes);
                    let complete = text
                        .rfind(char::is_whitespace)
                        .map_or("", |end| &text[..end]);
                    if let Some(url) = authorization_url(complete) {
                        view.authorization_url = Some(url);
                        view.state = "awaiting_callback".into();
                        break;
                    }
                }
            }
        }
        let exit = child
            .wait()
            .await
            .map_err(|_| "Couldn't finish the sign-in flow.")?;
        drop(group);
        if !exit.success() {
            return Err("The provider sign-in did not complete. Try again, or check the connection in the agent.");
        }
        attempt.update("verifying", None);
        if verify(&state, &attempt, &root).await {
            Ok(())
        } else {
            Err("The agent hasn't confirmed this connection. Check again or retry sign-in.")
        }
    };
    let stopping = async {
        loop {
            if state.stopping.load(Ordering::Relaxed) {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    };
    tokio::select! {
        result = tokio::time::timeout(AUTH_LIFETIME, work) => match result {
            Ok(Ok(())) => attempt.update("succeeded", None),
            Ok(Err(message)) => attempt.update("failed", Some(message)),
            Err(_) => attempt.update("failed", Some("Sign-in expired after ten minutes. Try again.")),
        },
        _ = cancel.changed() => attempt.update("cancelled", None),
        _ = stopping => attempt.update("cancelled", None),
    }
    state.probes.changed();
    state.changes.notify_waiters();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn auth_links_are_urls_and_callbacks_cannot_inject_input_lines() {
        assert_eq!(
            authorization_url(
                "Visit this URL to authorize:\n https://example.test/oauth?state=a\n"
            )
            .as_deref(),
            Some("https://example.test/oauth?state=a")
        );
        assert!(authorization_url("docs: https://example.test/help\n").is_none());
        assert!(!safe_url("javascript:alert(1)"));
        assert!(!safe_url("https://a.test/cb\nnext command"));
        assert!(!safe_url("https://user:secret@a.test/cb"));
        assert!(safe_url(
            "http://localhost:1234/callback?code=test&state=test"
        ));
    }
}
