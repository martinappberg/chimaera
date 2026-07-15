//! Shell integration: scripts that make interactive shells emit OSC 133
//! semantic prompts, OSC 633;E command-line reports, and OSC 7 cwd reports.
//!
//! Locally, [`shell_launch`] materializes the scripts under the runtime dir
//! and returns the argv + env that spawn the user's shell with the
//! integration injected (bash `--init-file`, zsh `ZDOTDIR` shim, fish
//! `vendor_conf.d`) while still sourcing their normal rc files. For remote
//! hosts (where nothing is installed), [`snippet`] returns a self-contained
//! bash+zsh script the user can append to a remote rc file.

use std::path::{Path, PathBuf};

use anyhow::Context;

pub const BASH: &str = include_str!("shellint/integration.bash");
pub const ZSH: &str = include_str!("shellint/integration.zsh");
pub const FISH: &str = include_str!("shellint/integration.fish");

/// The environment-prelude sourcing block, POSIX syntax (bash and zsh both
/// source it verbatim; fish gets an env-capture handler in its integration
/// instead). Appended after the user's own rc so the prelude can override
/// it; also embedded in the server's agent login-wrapper — one source of
/// truth for the guard semantics. The DONE guard stops nested integrated
/// shells from re-running the prelude; `[ -r ]` makes a missing file a
/// no-op rather than a startup error.
pub const PRELUDE_SNIPPET_POSIX: &str = "\
# Chimaera environment prelude: user-configured startup commands, run once\n\
# per session (the DONE guard covers nested shells; reconnect never re-runs\n\
# it because reattach is not a spawn).\n\
if [ -n \"${CHIMAERA_PRELUDE:-}\" ] && [ -z \"${CHIMAERA_PRELUDE_DONE:-}\" ] && [ -r \"$CHIMAERA_PRELUDE\" ]; then\n\
  export CHIMAERA_PRELUDE_DONE=1\n\
  . \"$CHIMAERA_PRELUDE\"\n\
fi\n";

/// How to spawn a shell with integration injected.
#[derive(Clone, Debug)]
pub struct ShellLaunch {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// The self-contained bash+zsh snippet for remote hosts.
pub fn snippet() -> String {
    format!(
        "# Chimaera shell integration (bash + zsh) — makes this shell report\n\
         # command boundaries, exit codes, and cwd to a chimaera terminal, so\n\
         # linked agents get a reliable command journal on this host too.\n\
         #\n\
         # Install on a remote host:\n\
         #   chimaera shell-integration | ssh HOST 'cat >> ~/.bashrc'   # bash\n\
         #   chimaera shell-integration | ssh HOST 'cat >> ~/.zshrc'    # zsh\n\
         if [ -n \"${{ZSH_VERSION:-}}\" ]; then\n{ZSH}\nelif [ -n \"${{BASH_VERSION:-}}\" ]; then\n{BASH}\nfi\n"
    )
}

/// Write `content` to `path` unless it already matches (keeps mtimes stable
/// and sourcing shells never see a half-written file thanks to the rename).
fn write_if_changed(path: &Path, content: &str) -> anyhow::Result<()> {
    if std::fs::read_to_string(path).is_ok_and(|cur| cur == content) {
        return Ok(());
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

/// A zsh ZDOTDIR-shim dotfile: source the user's counterpart (restoring
/// their ZDOTDIR while it runs, VS Code-style), then optionally the
/// integration.
fn zsh_shim(dotfile: &str, integration: Option<&Path>) -> String {
    let mut s = format!(
        "__chimaera_zdot=\"${{CHIMAERA_ORIG_ZDOTDIR:-$HOME}}\"\n\
         if [ -f \"$__chimaera_zdot/{dotfile}\" ]; then\n\
         \t__chimaera_own_zdotdir=\"$ZDOTDIR\"\n\
         \tZDOTDIR=\"$__chimaera_zdot\"\n\
         \t. \"$__chimaera_zdot/{dotfile}\"\n\
         \t# Respect an rc that intentionally re-homes ZDOTDIR; otherwise\n\
         \t# keep ours so the remaining startup files stay shimmed.\n\
         \tif [ \"$ZDOTDIR\" = \"$__chimaera_zdot\" ]; then\n\
         \t\tZDOTDIR=\"$__chimaera_own_zdotdir\"\n\
         \tfi\n\
         \tunset __chimaera_own_zdotdir\n\
         fi\n\
         unset __chimaera_zdot\n"
    );
    if let Some(integration) = integration {
        s.push_str(&format!(". \"{}\"\n", integration.display()));
    }
    s
}

/// Materialize the integration files under `base` (normally the runtime
/// dir); idempotent. Returns the shell-integration directory.
pub fn materialize_in(base: &Path) -> anyhow::Result<PathBuf> {
    let dir = base.join("shell-integration");
    let zsh_dir = dir.join("zsh");
    let fish_dir = dir.join("fish-xdg").join("fish").join("vendor_conf.d");
    std::fs::create_dir_all(&zsh_dir).with_context(|| format!("mkdir {}", zsh_dir.display()))?;
    std::fs::create_dir_all(&fish_dir).with_context(|| format!("mkdir {}", fish_dir.display()))?;

    let bash_script = dir.join("integration.bash");
    let zsh_script = dir.join("integration.zsh");
    write_if_changed(&bash_script, BASH)?;
    write_if_changed(&zsh_script, ZSH)?;
    write_if_changed(&fish_dir.join("chimaera.fish"), FISH)?;

    // bash --init-file replacement for ~/.bashrc: theirs first, then ours,
    // then the environment prelude (last, so it can override the rc).
    write_if_changed(
        &dir.join("bash-init.sh"),
        &format!(
            "# Generated by chimaera: the --init-file for integrated bash sessions.\n\
             if [ -f \"$HOME/.bashrc\" ]; then . \"$HOME/.bashrc\"; fi\n\
             . \"{}\"\n\
             {PRELUDE_SNIPPET_POSIX}",
            bash_script.display()
        ),
    )?;

    for dotfile in [".zshenv", ".zprofile", ".zlogin"] {
        write_if_changed(&zsh_dir.join(dotfile), &zsh_shim(dotfile, None))?;
    }
    // .zshrc is the interactive-startup shim, so the prelude belongs here
    // (not in .zshenv/.zprofile — those also run for scripts/login where
    // the user's interactive rc hasn't finished).
    write_if_changed(
        &zsh_dir.join(".zshrc"),
        &format!(
            "{}{PRELUDE_SNIPPET_POSIX}",
            zsh_shim(".zshrc", Some(&zsh_script))
        ),
    )?;

    Ok(dir)
}

/// Compose the launch for `shell` with integration injected, materializing
/// scripts under `base`. Unknown shells launch plain (no integration).
pub fn shell_launch_for(shell: &str, base: &Path) -> anyhow::Result<ShellLaunch> {
    let dir = materialize_in(base)?;
    let name = Path::new(shell)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut launch = ShellLaunch {
        argv: vec![shell.to_string()],
        env: Vec::new(),
    };
    match name.as_str() {
        "bash" => {
            launch.argv.push("--init-file".to_string());
            launch
                .argv
                .push(dir.join("bash-init.sh").to_string_lossy().into_owned());
        }
        "zsh" => {
            if let Some(orig) = std::env::var_os("ZDOTDIR") {
                launch.env.push((
                    "CHIMAERA_ORIG_ZDOTDIR".to_string(),
                    orig.to_string_lossy().into_owned(),
                ));
            }
            launch.env.push((
                "ZDOTDIR".to_string(),
                dir.join("zsh").to_string_lossy().into_owned(),
            ));
        }
        "fish" => {
            let ours = dir.join("fish-xdg").to_string_lossy().into_owned();
            let value = match std::env::var("XDG_DATA_DIRS") {
                Ok(cur) if !cur.is_empty() => format!("{ours}:{cur}"),
                // fish falls back to its builtin defaults only when the var
                // is unset, so re-supply them alongside ours.
                _ => format!("{ours}:/usr/local/share:/usr/share"),
            };
            launch.env.push(("XDG_DATA_DIRS".to_string(), value));
        }
        _ => {}
    }
    Ok(launch)
}

/// [`shell_launch_for`] on the user's real login shell under the runtime dir.
/// Resolved via [`crate::login_shell`] (passwd-backed) rather than raw
/// `$SHELL`, so a daemon launched from Finder/launchd — where `$SHELL` is
/// often absent — still opens the user's actual shell, not a `/bin/sh`/bash
/// stand-in.
pub fn shell_launch() -> anyhow::Result<ShellLaunch> {
    shell_launch_for(&crate::login_shell(), &crate::runtime_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_base(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("chimaera-shellint-{label}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn materialize_is_idempotent_and_complete() {
        let base = test_base("mat");
        let dir = materialize_in(&base).unwrap();
        let first = std::fs::metadata(dir.join("integration.bash"))
            .unwrap()
            .modified()
            .unwrap();
        for f in [
            "integration.bash",
            "integration.zsh",
            "bash-init.sh",
            "zsh/.zshenv",
            "zsh/.zprofile",
            "zsh/.zshrc",
            "zsh/.zlogin",
            "fish-xdg/fish/vendor_conf.d/chimaera.fish",
        ] {
            assert!(dir.join(f).is_file(), "{f} missing");
        }
        // Re-materializing rewrites nothing.
        materialize_in(&base).unwrap();
        let second = std::fs::metadata(dir.join("integration.bash"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(first, second);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn launch_shapes_per_shell() {
        let base = test_base("launch");

        let bash = shell_launch_for("/bin/bash", &base).unwrap();
        assert_eq!(bash.argv[0], "/bin/bash");
        assert_eq!(bash.argv[1], "--init-file");
        assert!(bash.argv[2].ends_with("bash-init.sh"));
        assert!(bash.env.is_empty());

        let zsh = shell_launch_for("/bin/zsh", &base).unwrap();
        assert_eq!(zsh.argv, vec!["/bin/zsh"]);
        assert!(zsh
            .env
            .iter()
            .any(|(k, v)| k == "ZDOTDIR" && v.ends_with("shell-integration/zsh")));

        let fish = shell_launch_for("/opt/homebrew/bin/fish", &base).unwrap();
        assert_eq!(fish.argv, vec!["/opt/homebrew/bin/fish"]);
        assert!(fish
            .env
            .iter()
            .any(|(k, v)| k == "XDG_DATA_DIRS" && v.contains("fish-xdg")));

        // Unknown shells spawn plain.
        let other = shell_launch_for("/bin/tcsh", &base).unwrap();
        assert_eq!(other.argv, vec!["/bin/tcsh"]);
        assert!(other.env.is_empty());

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn snippet_contains_both_shells_and_dispatch() {
        let s = snippet();
        assert!(s.contains("ZSH_VERSION"));
        assert!(s.contains("BASH_VERSION"));
        assert!(s.contains("add-zsh-hook"));
        assert!(s.contains("PROMPT_COMMAND"));
        assert!(s.contains("133;D"));
    }

    #[test]
    fn prelude_hook_reaches_every_shell() {
        // bash + zsh source the POSIX snippet after the user's rc; fish
        // carries its own env-capture handler. All three must be guarded.
        let base = test_base("prelude");
        let dir = materialize_in(&base).unwrap();

        let bash_init = std::fs::read_to_string(dir.join("bash-init.sh")).unwrap();
        let zshrc = std::fs::read_to_string(dir.join("zsh/.zshrc")).unwrap();
        for (label, content) in [("bash-init.sh", &bash_init), ("zsh/.zshrc", &zshrc)] {
            assert!(
                content.ends_with(PRELUDE_SNIPPET_POSIX),
                "{label} must end with the prelude block (after rc + integration)"
            );
        }
        // The user's rc sources BEFORE the prelude (order is the contract).
        assert!(bash_init.find(".bashrc").unwrap() < bash_init.find("CHIMAERA_PRELUDE").unwrap());
        assert!(zshrc.find(".zshrc").unwrap() < zshrc.find("CHIMAERA_PRELUDE").unwrap());

        for probe in ["CHIMAERA_PRELUDE", "CHIMAERA_PRELUDE_DONE"] {
            assert!(PRELUDE_SNIPPET_POSIX.contains(probe));
            assert!(FISH.contains(probe), "fish handler missing {probe}");
        }
        // fish must defer past config.fish (conf.d loads first) and
        // self-erase so the prelude runs exactly once.
        assert!(FISH.contains("--on-event fish_prompt"));
        assert!(FISH.contains("functions -e __chimaera_prelude"));

        std::fs::remove_dir_all(&base).ok();
    }

    /// Drive a real interactive bash (`-i`, piped stdin — PROMPT_COMMAND and
    /// the DEBUG trap run without a pty) with `prelude` sourced before the
    /// integration and `epilogue` after (both simulate a user rc), typing
    /// `commands`. Returns combined stdout+stderr (marks land on both).
    #[cfg(unix)]
    fn bash_probe(label: &str, prelude: &str, epilogue: &str, commands: &str) -> String {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let dir = test_base(label);
        std::fs::write(dir.join("integration.bash"), BASH).unwrap();
        let rc = dir.join("rc.sh");
        std::fs::write(
            &rc,
            format!(
                "{prelude}\n. \"{}\"\n{epilogue}\n",
                dir.join("integration.bash").display()
            ),
        )
        .unwrap();
        let mut child = Command::new("bash")
            .args(["--noprofile", "--init-file"])
            .arg(&rc)
            .arg("-i")
            .env_remove("PROMPT_COMMAND")
            .env_remove("CHIMAERA_INTEGRATION")
            .env_remove("BASH_ENV")
            .env("TERM", "xterm-256color")
            .env("HISTFILE", "")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn bash");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(commands.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        std::fs::remove_dir_all(&dir).ok();
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    }

    /// The command-start hook must be the DEBUG trap (works on every bash
    /// since 3.2, unlike PS0 which needs 4.4), installed at source time
    /// and re-armed from PROMPT_COMMAND — asserted against the running
    /// bash, whatever version the host has.
    #[test]
    #[cfg(unix)]
    fn bash_debug_hook_installed_and_rearmed() {
        let out = bash_probe(
            "hook",
            "",
            "",
            "trap -p DEBUG\nprintf 'PC=%s\\n' \"$PROMPT_COMMAND\"\n",
        );
        assert!(out.contains("__chimaera_preexec"), "hook missing: {out}");
        assert!(
            out.contains(r#"trap "$__chimaera_debug_chain" DEBUG"#),
            "re-arm missing from PROMPT_COMMAND: {out}"
        );
    }

    /// The Sherlock login-node scenario: a site rc has already installed a
    /// DEBUG trap whose handler calls functions (user-audit shells). The
    /// integration must still emit command-start/done marks — on bash < 4.4
    /// that takes the prompt-time re-arm, because trap changes made while an
    /// rc is sourced get silently reverted there — and the site's handler
    /// must keep running (chained).
    #[test]
    #[cfg(unix)]
    fn bash_marks_survive_hostile_audit_trap() {
        let prelude = r#"
user_audit() { local CMD; CMD=$(HISTTIMEFORMAT='' builtin history 1); : "$CMD"; }
debug_trap() { local _orig=$1; user_audit; echo AUDIT-RAN; : "$_orig"; }
trap 'debug_trap "$_"' DEBUG
PROMPT_COMMAND='RET=$?; : logger-standin'
"#;
        let out = bash_probe("hostile", prelude, "", "echo mark-test\n");
        assert!(out.contains("\x1b]133;C"), "no command-start mark: {out}");
        assert!(out.contains("\x1b]133;D;0"), "no done mark: {out}");
        assert!(
            out.contains("\x1b]633;E;echo mark-test"),
            "no command report: {out}"
        );
        assert!(out.contains("AUDIT-RAN"), "prior trap not chained: {out}");
    }

    /// A tool that re-traps DEBUG from PROMPT_COMMAND (bash-preexec style)
    /// steals the hook: on bash >= 4.4 the theft takes and the per-prompt
    /// re-arm must win it back by the next prompt; on older bash the shell
    /// itself reverts prompt-time trap changes (the same quirk that reverts
    /// rc-time installs). Either way the command after the theft must carry
    /// a command-start mark directly before its output.
    #[test]
    #[cfg(unix)]
    fn bash_rearm_survives_late_trap_clobber() {
        let epilogue = r#"PROMPT_COMMAND="$PROMPT_COMMAND; trap ':' DEBUG""#;
        let out = bash_probe("clobber", "", epilogue, "echo first\necho recovered\n");
        assert!(
            out.contains("\x1b]133;C\x07recovered"),
            "re-arm did not keep/recover the hook: {out:?}"
        );
    }
}
