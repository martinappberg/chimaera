//! Explicit user actions through the provider's native manager. Long-lived
//! interactive management belongs to the PTY layer, never the probe semaphore.
use super::*;
use axum::{
    extract::{Path as AxPath, State},
    response::{IntoResponse, Response},
    Json,
};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Action {
    pub(crate) agent: String,
    pub(crate) action: String,
    pub(crate) target: Option<String>,
}

#[cfg(unix)]
const SCRIPT: &str = r#"completion=$1
trust=$2
shift 2
finish() {
    result=$?
    trap - EXIT
    printf '\n'
    if [ "$result" -eq 0 ]; then
        printf '%s\n' 'Finished. Return to Extensions to check the result.' 'Start a new agent session to load changed extensions.'
    else
        printf 'The agent command failed (exit %s). Review its message above.\n' "$result"
    fi
    printf '1' > "$completion"
    if [ -t 0 ]; then printf '\nPress Enter to close.'; IFS= read -r acknowledgement; fi
    exit "$result"
}
trap finish EXIT
if [ "$trust" = grok-install ]; then
    printf '%s\n' 'Grok plugins can run hooks, tools, and connections on this machine.'
    printf 'Install and trust %s? [y/N] ' "${@: -1}"
    IFS= read -r answer
    case "$answer" in y|Y|yes|YES) set -- "$@" --trust ;; *) printf '%s\n' 'Installation cancelled.'; exit 0 ;; esac
fi
"$@"
"#;

/// [`SCRIPT`] for Windows PowerShell 5.1 and later. Fixed text: the
/// completion file, the trust mode and the command (a JSON array) arrive in
/// the environment, never as source.
#[cfg(windows)]
const SCRIPT_PS: &str = r#"$cmd = $env:CHIMAERA_ACTION_ARGV | ConvertFrom-Json
$cmd = @($cmd)
$result = 0
$run = $true
if ($env:CHIMAERA_ACTION_TRUST -eq 'grok-install') {
    Write-Output 'Grok plugins can run hooks, tools, and connections on this machine.'
    $answer = Read-Host ('Install and trust ' + $cmd[-1] + '? [y/N]')
    if ($answer -cmatch '^(y|Y|yes|YES)$') { $cmd += '--trust' } else { Write-Output 'Installation cancelled.'; $run = $false }
}
if ($run) {
    $rest = @()
    if ($cmd.Count -gt 1) { $rest = $cmd[1..($cmd.Count - 1)] }
    try {
        & $cmd[0] @rest
        $result = $LASTEXITCODE
        if ($null -eq $result) { $result = 0 }
    } catch {
        Write-Output $_.Exception.Message
        $result = 1
    }
}
Write-Output ''
if ($result -eq 0) {
    Write-Output 'Finished. Return to Extensions to check the result.'
    Write-Output 'Start a new agent session to load changed extensions.'
} else {
    Write-Output ('The agent command failed (exit ' + $result + '). Review its message above.')
}
Set-Content -NoNewline -Path $env:CHIMAERA_ACTION_DONE -Value '1'
if (-not [Console]::IsInputRedirected) { Write-Output ''; [void](Read-Host 'Press Enter to close') }
exit $result
"#;

/// The PTY command that runs `bin args` as the action, and the environment
/// it reads. Unix: [`SCRIPT`] in bash, through the login shell.
#[cfg(unix)]
fn action_command(
    completion: &Path,
    trust: &str,
    bin: &Path,
    args: Vec<String>,
) -> (Vec<String>, Vec<(String, String)>) {
    let mut argv = vec![
        "/bin/bash".into(),
        "-c".into(),
        SCRIPT.into(),
        "chimaera-extension-action".into(),
        completion.display().to_string(),
        trust.to_string(),
        bin.display().to_string(),
    ];
    argv.extend(args);
    (
        crate::launcher::wrap_login_shell(&crate::launcher::login_shell(), argv),
        Vec::new(),
    )
}

/// Windows: [`SCRIPT_PS`], passed encoded so no quoting rule touches it.
#[cfg(windows)]
fn action_command(
    completion: &Path,
    trust: &str,
    bin: &Path,
    args: Vec<String>,
) -> (Vec<String>, Vec<(String, String)>) {
    use base64::Engine as _;
    let utf16: Vec<u8> = SCRIPT_PS
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let mut command = vec![bin.display().to_string()];
    command.extend(args);
    let argv = vec![
        "powershell.exe".to_string(),
        "-NoProfile".to_string(),
        "-ExecutionPolicy".to_string(),
        "Bypass".to_string(),
        "-EncodedCommand".to_string(),
        base64::engine::general_purpose::STANDARD.encode(utf16),
    ];
    let env = vec![
        (
            "CHIMAERA_ACTION_DONE".to_string(),
            completion.display().to_string(),
        ),
        ("CHIMAERA_ACTION_TRUST".to_string(), trust.to_string()),
        (
            "CHIMAERA_ACTION_ARGV".to_string(),
            serde_json::to_string(&command).unwrap_or_default(),
        ),
    ];
    (argv, env)
}

pub(crate) async fn run(
    State(state): State<Arc<AppState>>,
    AxPath(ws): AxPath<String>,
    Json(body): Json<Action>,
) -> Response {
    let Some(workspace) = crate::lock(&state.workspaces).get(&ws) else {
        return not_found();
    };
    let Some(kind) = crate::operation::kind(&body.agent) else {
        return crate::plugins::bad_request("Unknown agent.");
    };
    let args = match crate::launcher::extension_action(kind, &body.action, body.target.as_deref()) {
        Ok(args) => args,
        Err(message) => return crate::plugins::bad_request(message),
    };
    // A terminal sign-in names one of the servers the last report listed as
    // needing one — never an arbitrary id typed into argv.
    if body.action == "login_connection" {
        let listed = state
            .probes
            .get(&format!("connections:{}:{ws}", body.agent))
            .and_then(|report| {
                report["connections"].as_array().map(|rows| {
                    rows.iter().any(|r| {
                        r["name"].as_str() == body.target.as_deref() && r["login_terminal"] == true
                    })
                })
            })
            .unwrap_or(false);
        if !listed {
            return crate::plugins::bad_request(
                "Check connections again, then sign in to a server that asks for it.",
            );
        }
    }
    let (bin, _, usage) = match bin_of(&state, kind).await {
        Ok(found) => found,
        Err(_) => return crate::plugins::bad_request("Install this agent first."),
    };
    let sid = crate::agents::fresh_session_id();
    let prepare_state = state.clone();
    let prepare_sid = sid.clone();
    let prepare_ws = ws.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        let completion = chimaera_core::runtime_dir().join(format!(
            "extension-action-{}",
            chimaera_core::generate_token()
        ));
        std::fs::File::create_new(&completion)?;
        let startup = crate::environment::job_startup_blocking();
        let prelude = crate::environment::materialize_prelude(
            &prepare_state,
            &prepare_sid,
            &prepare_ws,
            None,
            startup.as_deref(),
        );
        Ok::<_, std::io::Error>((completion, prelude))
    })
    .await;
    let Ok(Ok((completion, prelude))) = prepared else {
        return crate::plugins::bad_request("Couldn't prepare extension management.");
    };
    let mut env = crate::api::session_env(&state, &sid, "dark", prelude.as_deref());
    let trust = if kind == AgentKind::Grok && body.action == "install_plugin" {
        "grok-install"
    } else {
        "native"
    };
    let (command, action_env) = action_command(&completion, trust, &bin, args);
    env.extend(action_env);
    let env_remove = crate::api::spawn_env_remove(&env);
    let result = state.sessions.spawn(chimaera_pty::SpawnOpts {
        cwd: workspace.root,
        name: Some(format!("{} extensions", body.agent)),
        cols: 100,
        rows: 28,
        command: Some(command),
        id: Some(sid.clone()),
        env,
        env_remove,
        scrollback: crate::lock(&state.settings).scrollback_lines(),
    });
    match result {
        Ok(info) => {
            crate::runtime_retention::watch(state.clone(), info.id.clone(), usage);
            crate::lock(&state.session_workspaces).insert(sid.clone(), ws);
            let watch = state.clone();
            tokio::spawn(async move {
                while watch.sessions.get(&sid).is_some_and(|s| s.alive) {
                    if tokio::fs::metadata(&completion)
                        .await
                        .is_ok_and(|m| m.len() > 0)
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                let _ = tokio::fs::remove_file(&completion).await;
                if let Some(path) = prelude {
                    let _ = tokio::fs::remove_file(path).await;
                }
                watch.probes.changed();
                watch.changes.notify_waiters();
            });
            state.changes.notify_waiters();
            Json(json!({"session_id":info.id})).into_response()
        }
        Err(_) => {
            let _ = tokio::fs::remove_file(completion).await;
            if let Some(path) = prelude {
                let _ = tokio::fs::remove_file(path).await;
            }
            crate::plugins::bad_request("Couldn't open the agent's extension manager.")
        }
    }
}

// The trust prompt is the bash script's.
#[cfg(all(test, unix))]
mod tests {
    use super::SCRIPT;
    use std::io::Write;
    use std::process::{Command, Stdio};

    #[test]
    fn grok_install_trust_requires_an_explicit_answer() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-extension-trust-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir(&root).unwrap();
        for (answer, approved) in [("\n", false), ("no\n", false), ("yes\n", true)] {
            let mut child = Command::new("/bin/bash")
                .args(["-c", SCRIPT, "test"])
                .arg(root.join("done"))
                .args([
                    "grok-install",
                    "/bin/bash",
                    "-c",
                    "printf '%s' \"$*\" > \"$1\"",
                    "fixture",
                ])
                .arg(root.join("called"))
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(answer.as_bytes())
                .unwrap();
            assert!(child.wait().unwrap().success());
            assert_eq!(root.join("called").exists(), approved);
            if approved {
                assert!(std::fs::read_to_string(root.join("called"))
                    .unwrap()
                    .ends_with(" --trust"));
            }
            assert_eq!(std::fs::read_to_string(root.join("done")).unwrap(), "1");
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
