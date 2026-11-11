use crate::AppState;

/// The environment every chimaera-spawned session gets: the shim dir
/// prepended to PATH (theming + future adoption for typed agents; spawn env
/// only — user dotfiles are never touched), the session's own id, and the
/// client's color scheme. `CHIMAERA_SHIMS` lets the login-shell wrap
/// re-prepend the shim dir after profile init reorders PATH.
/// `prelude` (when a prelude applies — see `environment`) rides as
/// `CHIMAERA_PRELUDE`, sourced once by the shell-integration rc or the
/// agent login-wrapper; None sets nothing (zero delta without preludes).
pub(crate) fn session_env(
    state: &AppState,
    session_id: &str,
    theme: &str,
    prelude: Option<&std::path::Path>,
) -> Vec<(String, String)> {
    let shims = state.shims_dir.display().to_string();
    let inherited = std::env::var("PATH").unwrap_or_default();
    let mut env = vec![
        ("PATH".to_string(), spawn_path(&shims, &inherited)),
        ("CHIMAERA_SESSION".to_string(), session_id.to_string()),
        ("CHIMAERA_THEME".to_string(), theme.to_string()),
        ("CHIMAERA_SHIMS".to_string(), shims),
    ];
    if let Some(path) = prelude {
        env.push(("CHIMAERA_PRELUDE".to_string(), path.display().to_string()));
    }
    env
}

/// The remove-list for a spawn whose add-list is `env`: the
/// launcher-context scrub plus the prelude pair, minus anything `env`
/// explicitly sets. The prelude vars must go — a daemon started from
/// inside a chimaera terminal (dev loops do this all the time) inherits
/// `CHIMAERA_PRELUDE_DONE`, which would silently suppress preludes in
/// every session it spawns (the same bug class as the launcher-context
/// leak). The env/env_remove sets are kept DISJOINT here because the two
/// spawn layers order them oppositely: chimaera-pty applies removes
/// before the overlay (adds win), the chat driver transport applies them
/// after (removal wins — found live: it deleted the CHIMAERA_PRELUDE the
/// spawn had just set). Disjoint sets behave identically under both.
pub(crate) fn spawn_env_remove(env: &[(String, String)]) -> Vec<String> {
    let mut names = launcher_context_env();
    names.push("CHIMAERA_PRELUDE".to_string());
    names.push("CHIMAERA_PRELUDE_DONE".to_string());
    names.retain(|n| !env.iter().any(|(k, _)| k == n));
    names
}

/// Inherited env vars to REMOVE from every spawned session: markers that
/// describe the DAEMON's launcher, not the session. When the daemon was
/// started from inside a Claude Code session (dev loops do this all the
/// time), `CLAUDE_CODE_SESSION_ID`/`CLAUDE_CODE_CHILD_SESSION` leak through
/// and make every claude spawned under it believe it is a nested child
/// session — and a child-marked interactive claude persists NO transcript
/// (verified against claude 2.1.204; the two markers bisected live), so
/// conversations silently lose `--resume`. The whole CLAUDE* family goes:
/// none of it can describe a chimaera session truthfully, and anything the
/// user set in their own profile comes back through the login-shell wrap.
///
/// The native app's askpass wiring goes too, when a daemon inherited it from
/// an older app: a session's ssh would otherwise prompt through the app's
/// modal (or hang with none open), and any agent could raise a native prompt
/// for a real host. A user's own `SSH_ASKPASS` is kept.
pub(crate) fn launcher_context_env() -> Vec<String> {
    let vars: Vec<(String, String)> = std::env::vars().collect();
    // Fixed for the daemon's life; `runtime_dir()` creates the directory, so
    // not once per spawn.
    static APP_SHIM: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    let app_shim = APP_SHIM.get_or_init(|| chimaera_core::runtime_dir().join("askpass.sh"));
    let mut names: Vec<String> = vars
        .iter()
        .map(|(name, _)| name)
        .filter(|name| {
            *name == "CLAUDECODE"
                || *name == "CLAUDE_EFFORT"
                || *name == "AI_AGENT"
                || name.starts_with("CLAUDE_CODE_")
                || name.starts_with("CLAUDE_AGENT_")
        })
        .cloned()
        .collect();
    names.extend(app_askpass_env(&vars, app_shim));
    names
}

/// The app's askpass variables present in `vars`. Its own markers always;
/// `SSH_ASKPASS` (+ `_REQUIRE`) only when they are the app's — the shim it
/// writes, or set alongside its socket marker.
fn app_askpass_env(vars: &[(String, String)], app_shim: &std::path::Path) -> Vec<String> {
    let has = |name: &str| vars.iter().any(|(k, _)| k == name);
    let app_owned_askpass = has("CHIMAERA_ASKPASS_SOCK")
        || vars
            .iter()
            .any(|(k, v)| k == "SSH_ASKPASS" && std::path::Path::new(v) == app_shim);
    let mut names: Vec<String> = ["CHIMAERA_ASKPASS_SOCK", "CHIMAERA_ASKPASS_ALIAS"]
        .into_iter()
        .filter(|n| has(n))
        .map(str::to_string)
        .collect();
    if app_owned_askpass {
        for name in ["SSH_ASKPASS", "SSH_ASKPASS_REQUIRE"] {
            if has(name) {
                names.push(name.to_string());
            }
        }
    }
    names
}

/// The spawned session's PATH: shim dir first, then the daemon's inherited
/// PATH — or the fixed system default when that is empty. A bare
/// "{shims}:" tail is an empty final PATH member, which sh searches as
/// the cwd (measured: a repo-local ./curl ran inside an install session).
#[cfg(unix)]
pub(crate) fn spawn_path(shims: &str, inherited: &str) -> String {
    if inherited.is_empty() {
        format!("{shims}:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin")
    } else {
        format!("{shims}:{inherited}")
    }
}

/// Windows lists PATH with `;`, and its fixed default is the system
/// directories under `%SystemRoot%`.
#[cfg(windows)]
pub(crate) fn spawn_path(shims: &str, inherited: &str) -> String {
    if inherited.is_empty() {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
        format!(r"{shims};{root}\System32;{root};{root}\System32\WindowsPowerShell\v1.0")
    } else {
        format!("{shims};{inherited}")
    }
}

#[cfg(test)]
mod scrub_tests {
    /// The scrub list contains exactly the launcher-context markers present
    /// in the daemon's environment — nothing else (a session must keep the
    /// rest of its inherited env).
    #[test]
    fn launcher_context_env_matches_claude_markers_only() {
        // Unique-ish names; set_var is process-global but nothing else in
        // this test binary reads them.
        std::env::set_var("CLAUDE_CODE_CHILD_SESSION", "1");
        std::env::set_var("CLAUDE_AGENT_SDK_VERSION", "0.0.0");
        std::env::set_var("CHIMAERA_SCRUB_TEST_INNOCENT", "keep");
        let scrub = super::launcher_context_env();
        assert!(scrub.iter().any(|n| n == "CLAUDE_CODE_CHILD_SESSION"));
        assert!(scrub.iter().any(|n| n == "CLAUDE_AGENT_SDK_VERSION"));
        assert!(
            !scrub
                .iter()
                .any(|n| n.starts_with("CHIMAERA") && !n.starts_with("CHIMAERA_ASKPASS_")),
            "only claude-context markers are scrubbed: {scrub:?}"
        );
        assert!(!scrub.iter().any(|n| n == "PATH" || n == "HOME"));
        std::env::remove_var("CLAUDE_CODE_CHILD_SESSION");
        std::env::remove_var("CLAUDE_AGENT_SDK_VERSION");
        std::env::remove_var("CHIMAERA_SCRUB_TEST_INNOCENT");
    }

    #[test]
    fn the_apps_askpass_wiring_is_scrubbed_and_a_users_own_askpass_kept() {
        let shim = std::path::Path::new("/home/u/.chimaera/run/askpass.sh");
        let env = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let mut app = super::app_askpass_env(
            &env(&[
                ("SSH_ASKPASS", "/home/u/.chimaera/run/askpass.sh"),
                ("SSH_ASKPASS_REQUIRE", "force"),
                (
                    "CHIMAERA_ASKPASS_SOCK",
                    "/home/u/.chimaera/run/askpass.sock",
                ),
                ("PATH", "/usr/bin"),
            ]),
            shim,
        );
        app.sort();
        assert_eq!(
            app,
            [
                "CHIMAERA_ASKPASS_SOCK",
                "SSH_ASKPASS",
                "SSH_ASKPASS_REQUIRE"
            ]
        );
        // The app's shim alone (a socket marker lost) is still the app's.
        assert_eq!(
            super::app_askpass_env(
                &env(&[("SSH_ASKPASS", "/home/u/.chimaera/run/askpass.sh")]),
                shim
            ),
            ["SSH_ASKPASS"]
        );
        // The user's own askpass program stays.
        assert!(super::app_askpass_env(
            &env(&[
                ("SSH_ASKPASS", "/usr/libexec/ssh-askpass"),
                ("SSH_ASKPASS_REQUIRE", "prefer"),
            ]),
            shim
        )
        .is_empty());
    }
}
