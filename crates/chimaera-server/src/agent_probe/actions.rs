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

pub(crate) async fn run(
    State(state): State<Arc<AppState>>,
    AxPath(ws): AxPath<String>,
    Json(body): Json<Action>,
) -> Response {
    let Some(workspace) = crate::lock(&state.workspaces).get(&ws) else {
        return not_found();
    };
    let Some(kind) = AgentKind::parse(&body.agent).filter(|k| AgentKind::ALL.contains(k)) else {
        return crate::plugins::bad_request("Unknown agent.");
    };
    let args = match crate::launcher::extension_action(kind, &body.action, body.target.as_deref()) {
        Ok(args) => args,
        Err(message) => return crate::plugins::bad_request(message),
    };
    let (bin, _) = match bin_of(&state, kind).await {
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
        let prelude = crate::environment::materialize_prelude(
            &prepare_state,
            &prepare_sid,
            &prepare_ws,
            None,
        );
        Ok::<_, std::io::Error>((completion, prelude))
    })
    .await;
    let Ok(Ok((completion, prelude))) = prepared else {
        return crate::plugins::bad_request("Couldn't prepare extension management.");
    };
    let env = crate::api::session_env(&state, &sid, "dark", prelude.as_deref());
    let env_remove = crate::api::spawn_env_remove(&env);
    let mut argv = vec![
        "/bin/bash".into(),
        "-c".into(),
        SCRIPT.into(),
        "chimaera-extension-action".into(),
        completion.display().to_string(),
        if kind == AgentKind::Grok && body.action == "install_plugin" {
            "grok-install".into()
        } else {
            "native".into()
        },
        bin.display().to_string(),
    ];
    argv.extend(args);
    let result = state.sessions.spawn(chimaera_pty::SpawnOpts {
        cwd: workspace.root,
        name: Some(format!("{} extensions", body.agent)),
        cols: 100,
        rows: 28,
        command: Some(crate::launcher::wrap_login_shell(
            &crate::launcher::login_shell(),
            argv,
        )),
        id: Some(sid.clone()),
        env,
        env_remove,
        scrollback: crate::lock(&state.settings).scrollback_lines(),
    });
    match result {
        Ok(info) => {
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

#[cfg(test)]
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
