use super::*;
use std::os::unix::fs::PermissionsExt;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-resolution-{}-{}",
            std::process::id(),
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn executable(&self, name: &str, script: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn installs(&self) -> (PathBuf, PathBuf) {
        (
            self.executable(
                ".nvm/versions/node/v23.8.0/bin/codex",
                "echo codex-cli 0.162.1",
            ),
            self.executable("homebrew/bin/codex", "echo codex-cli 0.158.0"),
        )
    }

    fn search(&self, path: &[&Path], known: &[&Path]) -> BinarySearch {
        BinarySearch {
            shell: "/fixture/login-shell".into(),
            path: Some(std::env::join_paths(path).unwrap()),
            well_known: known.iter().map(|p| p.to_path_buf()).collect(),
        }
    }

    fn state(&self) -> AppState {
        AppState::new(
            "test-token".into(),
            "testhost".into(),
            4242,
            0,
            self.0.join("data"),
            self.0.join("config"),
        )
    }

    async fn resolve(
        &self,
        search: &BinarySearch,
        shell_result: Result<Option<PathBuf>, String>,
    ) -> Result<PathBuf, String> {
        resolve_with_search(
            AgentKind::Codex,
            &self.0.join("managed"),
            None,
            search,
            std::future::ready(shell_result),
        )
        .await
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn explicit_and_login_shell_choices_outrank_newer_installations() {
    let f = Fixture::new();
    let (nvm, brew) = f.installs();
    let search = f.search(&[nvm.parent().unwrap()], &[&nvm]);
    let selected = resolve_with_search(
        AgentKind::Codex,
        &f.0.join("managed"),
        Some(brew.to_str().unwrap().into()),
        &search,
        async { panic!("a valid explicit path must not run the shell") },
    )
    .await;
    assert_eq!(selected.unwrap(), brew);
    assert_eq!(
        f.resolve(&search, Ok(Some(brew.clone()))).await.unwrap(),
        brew
    );

    // Retain the documented behavior for a stale explicit path.
    assert_eq!(
        resolve_with_search(
            AgentKind::Codex,
            &f.0.join("managed"),
            Some(f.0.join("gone").to_str().unwrap().into()),
            &search,
            std::future::ready(Ok(Some(nvm.clone()))),
        )
        .await
        .unwrap(),
        nvm
    );
}

#[tokio::test]
async fn inherited_path_order_beats_fixed_prefixes_without_ranking_versions() {
    let f = Fixture::new();
    let (nvm, brew) = f.installs();
    for (first, second) in [(&nvm, &brew), (&brew, &nvm)] {
        let search = f.search(
            &[first.parent().unwrap(), second.parent().unwrap()],
            &[second],
        );
        assert_eq!(f.resolve(&search, Ok(None)).await.unwrap(), *first);
    }
    // Symlinks and paths with spaces retain the user-facing path.
    let link = f.0.join("a space/codex");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&nvm, &link).unwrap();
    let nonexec = f.executable("nonexec/codex", "exit 1");
    std::fs::set_permissions(&nonexec, std::fs::Permissions::from_mode(0o644)).unwrap();
    let search = f.search(
        &[
            Path::new(""),
            Path::new("."),
            Path::new("relative"),
            nonexec.parent().unwrap(),
            link.parent().unwrap(),
        ],
        &[&brew],
    );
    assert_eq!(f.resolve(&search, Ok(None)).await.unwrap(), link);
}

#[tokio::test]
async fn installer_prefix_order_and_managed_last_resort_are_preserved() {
    let f = Fixture::new();
    let (_, brew) = f.installs();
    let local = f.executable(".local/bin/codex", "echo local");
    let managed = f.executable("managed/codex", "echo managed");
    let search = f.search(&[], &[&local, &brew]);
    assert_eq!(f.resolve(&search, Ok(None)).await.unwrap(), local);
    std::fs::remove_file(local).unwrap();
    assert_eq!(f.resolve(&search, Ok(None)).await.unwrap(), brew);
    std::fs::remove_file(brew).unwrap();
    assert_eq!(f.resolve(&search, Ok(None)).await.unwrap(), managed);
    std::fs::remove_file(managed).unwrap();
    assert!(f
        .resolve(&search, Ok(None))
        .await
        .unwrap_err()
        .contains("not found"));
}

#[tokio::test]
async fn failed_shell_never_authorizes_a_different_installation() {
    let f = Fixture::new();
    let (nvm, brew) = f.installs();
    f.executable("managed/codex", "echo managed");
    let search = f.search(&[nvm.parent().unwrap()], &[&brew]);
    for failure in ["timed out", "could not start", "exited with status 1"] {
        let error = f.resolve(&search, Err(failure.into())).await.unwrap_err();
        assert!(error.contains(failure));
        assert!(error.contains("Retry") && error.contains("agents.codex.path"));
    }
    // A vanished shell-selected binary is also an error, not a fallback.
    assert!(f
        .resolve(&search, Ok(Some(f.0.join("gone"))))
        .await
        .is_err());
}

#[tokio::test]
async fn shell_probe_distinguishes_miss_banners_and_startup_failure() {
    let f = Fixture::new();
    let (nvm, _) = f.installs();
    let shell = f.executable(
        "shell",
        &format!(
            "test \"$1\" = -ilc || exit 2\nprintf '/startup/banner\\n'\nPATH='{}' /bin/sh -ec \"$2\"\nprintf '/logout/banner\\n'",
            nvm.parent().unwrap().display()
        ),
    );
    let probe = |bin| probe_login_shell(shell.to_str().unwrap(), bin, Duration::from_secs(10));
    assert_eq!(probe("codex").await.unwrap(), Some(nvm));
    assert_eq!(probe("missing_agent").await.unwrap(), None);

    for script in ["exit 1", "echo /misleading/path; exit 0"] {
        let shell = f.executable("failed-shell", script);
        assert!(
            probe_login_shell(shell.to_str().unwrap(), "codex", Duration::from_secs(10))
                .await
                .is_err()
        );
    }
    assert!(probe_login_shell(
        f.0.join("no-shell").to_str().unwrap(),
        "codex",
        Duration::from_secs(10)
    )
    .await
    .unwrap_err()
    .contains("could not start"));
}

#[tokio::test]
async fn shell_probe_timeout_kills_startup_children_and_does_not_become_a_miss() {
    let f = Fixture::new();
    let leaked = f.0.join("leaked-child");
    let shell = f.executable(
        "slow-shell",
        &format!(
            "(/bin/sleep 0.4; echo leaked > '{}') &\nwait",
            leaked.display()
        ),
    );
    let error = probe_login_shell(shell.to_str().unwrap(), "codex", Duration::from_millis(100))
        .await
        .unwrap_err();
    assert!(error.contains("timed out"));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !leaked.exists(),
        "a startup child survived the probe timeout"
    );
}

#[tokio::test]
async fn shell_probe_caps_noisy_startup_output() {
    let f = Fixture::new();
    let shell = f.executable("noisy-shell", "exec /usr/bin/yes noisy");
    assert!(
        probe_login_shell(shell.to_str().unwrap(), "codex", Duration::from_secs(10))
            .await
            .unwrap_err()
            .contains("output limit")
    );
}

#[tokio::test]
async fn daemon_retries_failed_discovery_and_never_caches_an_alternative_from_it() {
    let f = Fixture::new();
    let (nvm, brew) = f.installs();
    let search = f.search(&[nvm.parent().unwrap()], &[&brew]);
    let state = f.state();
    let kind = AgentKind::Codex;
    let failure = || f.resolve(&search, Err("login shell timed out".into()));
    let miss = detect_with_resolver(&state, kind, false, false, |_, _| failure()).await;
    assert!(miss.path.is_err());
    assert!(!crate::lock(&state.agent_bins).contains_key(&kind));
    let catalog = detect_with_resolver(&state, kind, false, true, |_, _| async {
        panic!("catalog polling should serve the short-lived miss")
    })
    .await;
    assert_eq!(catalog.path, miss.path);

    // A new/restored chat's detect(false) retries even during the miss TTL.
    let recovered = detect_with_resolver(&state, kind, false, false, |_, _| {
        f.resolve(&search, Ok(Some(nvm.clone())))
    })
    .await;
    assert_eq!(recovered.path.unwrap(), nvm);
    assert!(!crate::lock(&state.agent_misses).contains_key(&kind));
    let cached = detect_with_resolver(&state, kind, false, false, |_, _| async {
        panic!("a validated successful detection should be cached")
    })
    .await;
    assert_eq!(cached.path.unwrap(), nvm);

    // A refresh cannot replace nvm with Homebrew when the shell times out.
    let retained = detect_with_resolver(&state, kind, true, true, |_, _| failure()).await;
    assert_eq!(retained.path.unwrap(), nvm);
    assert!(!crate::lock(&state.agent_misses).contains_key(&kind));

    // Once that executable vanishes, a failed refresh must not serve it.
    std::fs::remove_file(&nvm).unwrap();
    let gone = detect_with_resolver(&state, kind, false, false, |_, _| failure()).await;
    assert!(gone.path.is_err());
    assert!(!crate::lock(&state.agent_bins).contains_key(&kind));
    let recovered = detect_with_resolver(&state, kind, false, false, |_, _| {
        f.resolve(&search, Ok(None))
    })
    .await;
    assert_eq!(recovered.path.unwrap(), brew);
}

#[tokio::test]
async fn managed_detection_after_a_confirmed_miss_is_reported_in_the_catalog() {
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let f = Fixture::new();
    let state = f.state();
    let fake = f.executable("data/agents/bin/codex", "echo codex-cli 0.162.1");
    let search = f.search(&[], &[]);
    let detected = detect_with_resolver(
        &state,
        AgentKind::Codex,
        true,
        false,
        |managed, explicit| async move {
            resolve_with_search(
                AgentKind::Codex,
                &managed,
                explicit,
                &search,
                std::future::ready(Ok(None)),
            )
            .await
        },
    )
    .await;
    assert_eq!(detected.path.as_deref().unwrap(), fake);
    assert!(detected.managed);

    // Keep unrelated catalog rows independent of the test host's installs.
    for kind in AgentKind::ALL {
        if kind != AgentKind::Codex {
            crate::lock(&state.agent_misses).insert(kind, ("fixture miss".into(), Instant::now()));
        }
    }
    let response = crate::app(Arc::new(state))
        .oneshot(
            Request::get("/api/v1/agents")
                .header("authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let rows: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "codex")
        .unwrap();
    assert_eq!(row["installed"], true);
    assert_eq!(row["managed"], true);
    assert_eq!(row["path"], fake.to_str().unwrap());
}
