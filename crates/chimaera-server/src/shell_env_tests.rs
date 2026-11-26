use super::*;
use std::os::unix::fs::PermissionsExt;

const START: &str = "chimaera-env-nonce-start";
const END: &str = "chimaera-env-nonce-end";

fn dump(chatter: &str, entries: &[&str]) -> Vec<u8> {
    let mut out = format!("{chatter}\n{START}\n").into_bytes();
    for entry in entries {
        out.extend_from_slice(entry.as_bytes());
        out.push(0);
    }
    out.extend_from_slice(format!("\n{END}\n").as_bytes());
    out
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn the_environment_is_read_between_the_markers_only() {
    // An rc that prints: a banner, something shaped like a variable, and
    // the marker's own words without its line.
    let chatter = format!("Welcome back!\nFAKE=from-a-banner\nlooking for {START} here");
    let parsed = parse_capture(
        &dump(&chatter, &["HOME=/home/u", "EDITOR=vim", "EMPTY="]),
        START,
        END,
    )
    .unwrap();
    assert_eq!(
        parsed,
        pairs(&[("HOME", "/home/u"), ("EDITOR", "vim"), ("EMPTY", "")])
    );
}

#[test]
fn values_keep_newlines_equals_signs_and_marker_lookalikes() {
    // NUL separates entries, so an exported bash function or a PEM key
    // arrives whole — and a value that itself holds the end line can't
    // cut the dump short (the last end marker closes it).
    let function = "BASH_FUNC_ml%%=() {  module \"$@\"\n}";
    let tricky = format!("TRICKY=a=b\n{END}\nstill the value");
    let parsed = parse_capture(&dump("", &[function, &tricky, "LAST=1"]), START, END).unwrap();
    assert_eq!(parsed[0].0, "BASH_FUNC_ml%%");
    assert_eq!(parsed[0].1, "() {  module \"$@\"\n}");
    assert_eq!(parsed[1].1, format!("a=b\n{END}\nstill the value"));
    assert_eq!(parsed[2], ("LAST".to_string(), "1".to_string()));
}

#[test]
fn entries_that_are_not_variables_are_skipped() {
    let mut bytes = format!("\n{START}\n").into_bytes();
    bytes.extend_from_slice(b"GOOD=1\0no-equals-sign\0=no-name\0BAD=\xff\xfe\0ALSO=2\0");
    bytes.extend_from_slice(format!("\n{END}\n").as_bytes());
    assert_eq!(
        parse_capture(&bytes, START, END).unwrap(),
        pairs(&[("GOOD", "1"), ("ALSO", "2")])
    );
}

#[test]
fn a_capture_that_cannot_be_trusted_is_refused() {
    // Startup never reached the dump (an rc that exits, a shell that
    // rejects `-ilc`).
    assert_eq!(
        parse_capture(b"some banner\n", START, END),
        Err(CaptureError::Incomplete)
    );
    // It started and was cut off.
    let mut cut = dump("", &["A=1"]);
    cut.truncate(cut.len() - END.len() - 2);
    assert_eq!(
        parse_capture(&cut, START, END),
        Err(CaptureError::Incomplete)
    );
    // `env` printed nothing (no `-0`: it failed on stderr).
    assert_eq!(
        parse_capture(&dump("", &[]), START, END),
        Err(CaptureError::Incomplete)
    );
    // `env` ignored `-0` and printed lines: splitting them on NUL would
    // make one variable out of all of them.
    let lines = format!("\n{START}\nA=1\nB=2\n\n{END}\n");
    assert_eq!(
        parse_capture(lines.as_bytes(), START, END),
        Err(CaptureError::Malformed)
    );
    // Past the cap nothing is parsed, however well-formed.
    let huge = "X=".to_string() + &"y".repeat(OUTPUT_CAP);
    assert_eq!(
        parse_capture(&dump("", &[&huge]), START, END),
        Err(CaptureError::TooLarge)
    );
}

fn base(items: &[(&str, &str)]) -> HashMap<String, String> {
    pairs(items).into_iter().collect()
}

#[test]
fn only_what_the_startup_files_set_is_kept() {
    let env = ShellEnv::from_capture(
        pairs(&[
            ("HOME", "/home/u"),
            ("LANG", "en_US.UTF-8"),
            ("OPENAI_API_KEY", "sk-test"),
            ("SSH_AUTH_SOCK", "/home/u/.1password/agent.sock"),
            ("PATH", "/home/u/.local/bin:/usr/bin:/bin"),
            // The capturing shell's own state, the terminal, chimaera's own.
            ("SHLVL", "1"),
            ("PWD", "/somewhere"),
            ("OLDPWD", "/"),
            ("_", "/usr/bin/env"),
            ("TERM", "dumb"),
            ("GPG_TTY", "not a tty"),
            ("CHIMAERA_SESSION", "s-stale"),
            ("CHIMAERA_PRELUDE_DONE", "1"),
            // Scrubbed from what the shell started with, so only an rc can
            // have set it: the user's own setting comes back.
            ("CLAUDE_CODE_USE_BEDROCK", "1"),
        ]),
        &base(&[
            ("HOME", "/home/u"),
            ("PATH", "/usr/bin:/bin"),
            // The launchd socket the rc replaced: the rc's wins.
            ("SSH_AUTH_SOCK", "/private/tmp/launchd/Listeners"),
        ]),
    );
    assert_eq!(
        env.vars,
        pairs(&[
            ("LANG", "en_US.UTF-8"),
            ("OPENAI_API_KEY", "sk-test"),
            ("SSH_AUTH_SOCK", "/home/u/.1password/agent.sock"),
            ("CLAUDE_CODE_USE_BEDROCK", "1"),
        ])
    );
    assert_eq!(
        env.path.as_deref(),
        Some("/home/u/.local/bin:/usr/bin:/bin")
    );
    // Values never reach a log through `{:?}`.
    let shown = format!("{env:?}");
    assert!(
        !shown.contains("sk-test") && !shown.contains("1password"),
        "{shown}"
    );
}

fn shell_env(vars: &[(&str, &str)], path: Option<&str>) -> ShellEnv {
    ShellEnv {
        vars: pairs(vars),
        path: path.map(str::to_string),
        zdotdir: None,
    }
}

#[test]
fn a_spawns_own_variables_win_over_the_capture() {
    let captured = shell_env(
        &[
            ("OPENAI_API_KEY", "sk-test"),
            ("AGY_CLI_DISABLE_AUTO_UPDATE", "false"),
            ("NO_OPEN_BROWSER", "0"),
        ],
        None,
    );
    let mut env = pairs(&[
        ("CHIMAERA_SESSION", "s-1"),
        ("AGY_CLI_DISABLE_AUTO_UPDATE", "true"),
        ("NO_OPEN_BROWSER", "1"),
    ]);
    captured.overlay(&mut env, "/usr/bin:/bin");
    assert_eq!(
        env,
        pairs(&[
            ("CHIMAERA_SESSION", "s-1"),
            ("AGY_CLI_DISABLE_AUTO_UPDATE", "true"),
            ("NO_OPEN_BROWSER", "1"),
            ("OPENAI_API_KEY", "sk-test"),
        ])
    );
    // Laying it over twice adds nothing.
    captured.overlay(&mut env, "/usr/bin:/bin");
    assert_eq!(env.len(), 4);
}

#[test]
fn the_captured_path_rides_beside_path_for_the_wrap_to_restore() {
    let captured = shell_env(
        &[],
        Some("/home/u/.nvm/bin:/opt/homebrew/bin:/usr/bin:/bin"),
    );
    // A plain session: shims first, then the daemon's PATH. PATH itself is
    // left alone (the login profile reorders it anyway); nothing of the
    // spawn's own needs carrying.
    let mut env = pairs(&[
        ("PATH", "/data/shims:/usr/bin:/bin"),
        ("CHIMAERA_SHIMS", "/data/shims"),
    ]);
    captured.overlay(&mut env, "/usr/bin:/bin");
    assert_eq!(env[0].1, "/data/shims:/usr/bin:/bin");
    assert_eq!(
        env[2],
        (
            SHELL_PATH_ENV.to_string(),
            "/home/u/.nvm/bin:/opt/homebrew/bin:/usr/bin:/bin".to_string()
        )
    );

    // A sign-in put its no-browser directory in front: it stays in front of
    // the captured PATH, or the restore would drop it.
    let mut env = pairs(&[
        ("PATH", "/run/no-browser:/data/shims:/usr/bin:/bin"),
        ("CHIMAERA_SHIMS", "/data/shims"),
    ]);
    captured.overlay(&mut env, "/usr/bin:/bin");
    assert_eq!(
        env[2].1,
        "/run/no-browser:/home/u/.nvm/bin:/opt/homebrew/bin:/usr/bin:/bin"
    );

    // A probe command sets no PATH at all.
    let mut env = Vec::new();
    captured.overlay(&mut env, "/usr/bin:/bin");
    assert_eq!(
        env,
        pairs(&[(
            SHELL_PATH_ENV,
            "/home/u/.nvm/bin:/opt/homebrew/bin:/usr/bin:/bin"
        )])
    );
}

/// The scrub and the capture together: a launcher marker the daemon
/// inherited stays removed, the same family set by the user's rc arrives,
/// and the two lists stay disjoint (the chat transport removes last).
#[test]
fn a_scrubbed_name_the_rc_sets_is_not_removed_again() {
    std::env::set_var("CLAUDE_CODE_SHELL_ENV_TEST_MARKER", "daemon");
    std::env::set_var("CLAUDE_CODE_SHELL_ENV_TEST_SETTING", "daemon");
    let mut env = pairs(&[("CHIMAERA_SESSION", "s-1")]);
    shell_env(&[("CLAUDE_CODE_SHELL_ENV_TEST_SETTING", "from-rc")], None)
        .overlay(&mut env, "/usr/bin:/bin");
    let remove = crate::api::spawn_env_remove(&env);
    assert!(remove
        .iter()
        .any(|n| n == "CLAUDE_CODE_SHELL_ENV_TEST_MARKER"));
    assert!(!remove
        .iter()
        .any(|n| n == "CLAUDE_CODE_SHELL_ENV_TEST_SETTING"));
    // A stale restore variable is scrubbed unless this spawn set one.
    assert!(remove.iter().any(|n| n == SHELL_PATH_ENV));
    std::env::remove_var("CLAUDE_CODE_SHELL_ENV_TEST_MARKER");
    std::env::remove_var("CLAUDE_CODE_SHELL_ENV_TEST_SETTING");
}

struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-shell-env-{}-{}",
            std::process::id(),
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn write(&self, name: &str, content: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    fn executable(&self, name: &str, script: &str) -> PathBuf {
        let path = self.write(name, &format!("#!/bin/sh\n{script}\n"));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn source(&self, shell: &str) -> Source {
        Source {
            shell: shell.to_string(),
            base: Arc::new(base(&[
                ("HOME", self.0.to_str().unwrap()),
                ("PATH", "/usr/bin:/bin"),
            ])),
        }
    }

    fn lines(&self, name: &str) -> usize {
        std::fs::read_to_string(self.0.join(name))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Generous: a loaded host can take seconds to start a real shell.
const TEST_BUDGET: Duration = Duration::from_secs(60);

fn value<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    env.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

#[tokio::test]
async fn zsh_reports_what_zshrc_exports_whatever_it_prints() {
    if !std::path::Path::new("/bin/zsh").exists() {
        return;
    }
    let home = Home::new();
    home.write(".zprofile", "export FROM_ZPROFILE=profile\n");
    home.write(
        ".zshrc",
        "echo 'a banner on stdout'\nprintf 'FAKE=1\\0'\nexport FROM_ZSHRC=rc\nexport MULTI=$'line one\\nline two'\nexport PATH=\"$HOME/rc-bin:$PATH\"\n",
    );
    let env = capture(&home.source("/bin/zsh"), TEST_BUDGET)
        .await
        .unwrap();
    assert_eq!(value(&env, "FROM_ZPROFILE"), Some("profile"));
    assert_eq!(value(&env, "FROM_ZSHRC"), Some("rc"));
    assert_eq!(value(&env, "MULTI"), Some("line one\nline two"));
    assert_eq!(value(&env, "FAKE"), None);
    let path = value(&env, "PATH").unwrap();
    assert!(
        path.starts_with(&format!("{}/rc-bin:", home.0.display())),
        "{path}"
    );
}

#[tokio::test]
async fn bash_reports_bashrc_even_when_the_profile_does_not_source_it() {
    if !std::path::Path::new("/bin/bash").exists() {
        return;
    }
    // Only a `.bashrc`: a login bash reads no file of the user's at all.
    let home = Home::new();
    home.write(
        ".bashrc",
        "echo sourced >> \"$HOME/rc-runs\"\necho 'a banner'\nexport FROM_BASHRC=rc\n",
    );
    let env = capture(&home.source("/bin/bash"), TEST_BUDGET)
        .await
        .unwrap();
    assert_eq!(value(&env, "FROM_BASHRC"), Some("rc"));
    assert_eq!(home.lines("rc-runs"), 1);

    // A profile that doesn't mention it (a comment doesn't count).
    let home = Home::new();
    home.write(
        ".bash_profile",
        "# .bashrc is for interactive shells\nexport FROM_PROFILE=profile\n",
    );
    home.write(
        ".bashrc",
        "echo sourced >> \"$HOME/rc-runs\"\nexport FROM_BASHRC=rc\n",
    );
    let env = capture(&home.source("/bin/bash"), TEST_BUDGET)
        .await
        .unwrap();
    assert_eq!(value(&env, "FROM_PROFILE"), Some("profile"));
    assert_eq!(value(&env, "FROM_BASHRC"), Some("rc"));
    assert_eq!(home.lines("rc-runs"), 1);
}

#[tokio::test]
async fn bash_sources_bashrc_once_when_the_profile_already_does() {
    if !std::path::Path::new("/bin/bash").exists() {
        return;
    }
    let home = Home::new();
    home.write(
        ".bash_profile",
        "if [ -f ~/.bashrc ]; then . ~/.bashrc; fi\n",
    );
    home.write(
        ".bashrc",
        "echo sourced >> \"$HOME/rc-runs\"\nexport FROM_BASHRC=rc\n",
    );
    let env = capture(&home.source("/bin/bash"), TEST_BUDGET)
        .await
        .unwrap();
    assert_eq!(value(&env, "FROM_BASHRC"), Some("rc"));
    assert_eq!(home.lines("rc-runs"), 1);
}

#[tokio::test]
async fn a_shell_that_never_answers_is_stopped_with_its_children() {
    let home = Home::new();
    let leaked = home.0.join("leaked-child");
    let shell = home.executable(
        "slow-shell",
        &format!(
            "(/bin/sleep 0.4; echo leaked > '{}') &\nwait",
            leaked.display()
        ),
    );
    let result = capture(
        &home.source(shell.to_str().unwrap()),
        Duration::from_millis(100),
    )
    .await;
    assert_eq!(result, Err(CaptureError::Timeout));
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(!leaked.exists(), "a startup child survived the timeout");
}

#[tokio::test]
async fn a_shell_that_floods_its_output_is_cut_off() {
    let home = Home::new();
    let shell = home.executable("noisy-shell", "exec /usr/bin/yes noisy");
    assert_eq!(
        capture(&home.source(shell.to_str().unwrap()), TEST_BUDGET).await,
        Err(CaptureError::TooLarge)
    );
}

#[tokio::test]
async fn a_background_job_holding_the_pipe_does_not_delay_the_answer() {
    // An rc that leaves a job running with the shell's stdout: the answer
    // is complete at the end marker, long before the pipe closes.
    let home = Home::new();
    let shell = home.executable(
        "lingering-shell",
        "/bin/sleep 30 &\nexport FROM_RC=1\nexec /bin/sh -c \"$2\"",
    );
    let started = Instant::now();
    let env = capture(&home.source(shell.to_str().unwrap()), TEST_BUDGET)
        .await
        .unwrap();
    assert_eq!(value(&env, "FROM_RC"), Some("1"));
    assert!(started.elapsed() < Duration::from_secs(20));
}

/// A stand-in login shell: counts its runs, "reads" `~/.profile`, then runs
/// the capture script like a shell would.
fn counting_shell(home: &Home) -> Source {
    let shell = home.executable(
        "counting-shell",
        "echo run >> \"$HOME/shell-runs\"\n[ -r \"$HOME/.profile\" ] && . \"$HOME/.profile\"\nexec /bin/sh -c \"$2\"",
    );
    home.source(shell.to_str().unwrap())
}

fn cached(cache: &ShellEnvCache, name: &str) -> Option<String> {
    let env = cache.current()?;
    value(&env.vars, name).map(str::to_string)
}

#[tokio::test]
async fn one_capture_serves_every_spawn_until_a_startup_file_changes() {
    let home = Home::new();
    home.write(".profile", "export MARK=one\n");
    let source = counting_shell(&home);
    let cache = Arc::new(ShellEnvCache::default());

    // Spawns arriving together share one shell.
    tokio::join!(
        cache.ready(source.clone(), TEST_BUDGET, TEST_BUDGET),
        cache.ready(source.clone(), TEST_BUDGET, TEST_BUDGET),
        cache.ready(source.clone(), TEST_BUDGET, TEST_BUDGET),
    );
    assert_eq!(cached(&cache, "MARK").as_deref(), Some("one"));
    assert_eq!(home.lines("shell-runs"), 1);

    // Later spawns start no shell at all.
    cache.ready(source.clone(), TEST_BUDGET, TEST_BUDGET).await;
    assert_eq!(home.lines("shell-runs"), 1);

    // The user edits a startup file: the next spawn has the new value.
    home.write(".profile", "export MARK=two-edited\n");
    cache.ready(source.clone(), TEST_BUDGET, TEST_BUDGET).await;
    assert_eq!(cached(&cache, "MARK").as_deref(), Some("two-edited"));
    assert_eq!(home.lines("shell-runs"), 2);

    // An explicit re-check reads again even with every file unchanged (a
    // file the rc sources may be what changed).
    cache.invalidate();
    cache.ready(source.clone(), TEST_BUDGET, TEST_BUDGET).await;
    assert_eq!(home.lines("shell-runs"), 3);
    cache.ready(source, TEST_BUDGET, TEST_BUDGET).await;
    assert_eq!(home.lines("shell-runs"), 3);
}

#[tokio::test]
async fn a_failed_capture_keeps_the_last_good_one_and_is_not_retried_per_spawn() {
    let home = Home::new();
    home.write(".profile", "export MARK=good\n");
    let source = counting_shell(&home);
    let cache = Arc::new(ShellEnvCache::default());
    cache.ready(source.clone(), TEST_BUDGET, TEST_BUDGET).await;
    assert_eq!(cached(&cache, "MARK").as_deref(), Some("good"));

    // The profile now ends the shell before it can answer.
    home.write(".profile", "export MARK=never-seen\nexit 0\n");
    cache.ready(source.clone(), TEST_BUDGET, TEST_BUDGET).await;
    assert_eq!(home.lines("shell-runs"), 2);
    assert_eq!(cached(&cache, "MARK").as_deref(), Some("good"));

    // Spawn after spawn: no new shell until something changes.
    cache.ready(source.clone(), TEST_BUDGET, TEST_BUDGET).await;
    cache.ready(source.clone(), TEST_BUDGET, TEST_BUDGET).await;
    assert_eq!(home.lines("shell-runs"), 2);

    // Fixed: read at the next spawn.
    home.write(".profile", "export MARK=fixed\n");
    cache.ready(source, TEST_BUDGET, TEST_BUDGET).await;
    assert_eq!(cached(&cache, "MARK").as_deref(), Some("fixed"));
}

#[tokio::test]
async fn a_spawn_waits_only_so_long_and_the_capture_still_lands() {
    let home = Home::new();
    home.write(".profile", "/bin/sleep 1\nexport MARK=late\n");
    let source = counting_shell(&home);
    let cache = Arc::new(ShellEnvCache::default());

    let started = Instant::now();
    cache
        .ready(source.clone(), Duration::from_millis(50), TEST_BUDGET)
        .await;
    assert!(started.elapsed() < Duration::from_millis(900));
    assert!(cache.current().is_none());

    // The same shell, joined rather than started again, answers for the
    // next spawn.
    cache.ready(source, TEST_BUDGET, TEST_BUDGET).await;
    assert_eq!(cached(&cache, "MARK").as_deref(), Some("late"));
    assert_eq!(home.lines("shell-runs"), 1);
}

#[test]
fn the_startup_files_watched_follow_the_shell() {
    let home = Home::new();
    let names = |source: &Source, zdotdir: Option<&str>| -> Vec<String> {
        startup_files(source, zdotdir.map(PathBuf::from))
            .into_iter()
            .map(|p| {
                p.strip_prefix(&home.0)
                    .map(|rel| format!("~/{}", rel.display()))
                    .unwrap_or_else(|_| p.display().to_string())
            })
            .collect()
    };
    assert_eq!(
        names(&home.source("/bin/zsh"), None),
        ["~/.zshenv", "~/.zprofile", "~/.zshrc", "~/.zlogin"]
    );
    // A `ZDOTDIR` the user's `.zshenv` set is where the rest lives.
    let zsh = names(&home.source("/usr/local/bin/zsh"), Some("/cfg/zsh"));
    assert_eq!(zsh.len(), 8);
    assert!(zsh.contains(&"~/.zshenv".to_string()) && zsh.contains(&"/cfg/zsh/.zshrc".to_string()));
    assert_eq!(
        names(&home.source("/bin/bash"), None),
        [
            "~/.bash_profile",
            "~/.bash_login",
            "~/.profile",
            "~/.bashrc"
        ]
    );
    assert_eq!(
        names(&home.source("/opt/homebrew/bin/fish"), None),
        ["~/.config/fish/config.fish"]
    );
    assert_eq!(names(&home.source("/bin/ksh"), None), ["~/.profile"]);
}

#[test]
fn bash_alone_gets_the_bashrc_step() {
    assert!(capture_script("/bin/bash", "s", "e").starts_with(BASHRC_STEP));
    for shell in ["/bin/zsh", "/opt/homebrew/bin/fish", "/bin/sh"] {
        assert_eq!(
            capture_script(shell, "s", "e"),
            "printf '\\ns\\n'; command env -0; printf '\\ne\\n'"
        );
    }
}
