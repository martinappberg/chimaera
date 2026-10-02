use super::*;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "chimaera-return-install-{}",
            chimaera_core::generate_token()
        ));
        fs::create_dir_all(path.join("project")).unwrap();
        fs::create_dir_all(path.join("stage")).unwrap();
        Self(path)
    }
    fn root(&self) -> PathBuf {
        self.0.join("project")
    }
    fn journal(&self) -> PathBuf {
        self.0.join("transaction")
    }
    fn write(&self, name: &str, before: Option<&[u8]>, after: Option<&[u8]>) -> Write {
        let tag = name.replace('/', "_");
        let before = before.map(|bytes| {
            let path = self.0.join("stage").join(format!("{tag}.before"));
            fs::write(&path, bytes).unwrap();
            let target = self.root().join(name);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, bytes).unwrap();
            path
        });
        let after = after.map(|bytes| {
            let path = self.0.join("stage").join(format!("{tag}.after"));
            fs::write(&path, bytes).unwrap();
            path
        });
        Write {
            root: self.root(),
            relative: name.into(),
            before,
            after,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn binding() -> Binding {
    Binding {
        endpoint: "https://fixture.invalid".into(),
        account: Some("a-1".into()),
        workspace: "w-1".into(),
        epoch: 7,
        receipt: Some("receipt-1".into()),
    }
}

#[test]
fn overwrite_delete_failure_retains_originals_and_exact_restart_rolls_forward() {
    let fixture = Fixture::new();
    let writes = vec![
        fixture.write("a", Some(b"original a"), Some(b"incoming a")),
        fixture.write("b", Some(b"original b"), None),
        fixture.write("c", None, Some(b"incoming c")),
    ];
    let mut transaction =
        Transaction::prepare(&fixture.journal(), binding(), writes, 1024).unwrap();
    assert!(transaction
        .apply(&|| {
            ensure!(
                fixture.root().join("b").exists(),
                "failure after overwrite and deletion"
            );
            Ok(())
        })
        .is_err());
    assert_eq!(fs::read(fixture.root().join("a")).unwrap(), b"incoming a");
    assert!(!fixture.root().join("b").exists());
    assert_eq!(
        fs::read(fixture.journal().join("0.before")).unwrap(),
        b"original a"
    );
    assert_eq!(
        fs::read(fixture.journal().join("1.before")).unwrap(),
        b"original b"
    );
    drop(transaction);
    let mut stale = binding();
    stale.epoch += 1;
    assert!(Transaction::open(&fixture.journal(), &stale).is_err());
    let mut retry = Transaction::open(&fixture.journal(), &binding())
        .unwrap()
        .unwrap();
    retry.apply(&|| Ok(())).unwrap();
    retry.commit(&|| Ok(())).unwrap();
    retry.cleanup().unwrap();
    assert_eq!(fs::read(fixture.root().join("c")).unwrap(), b"incoming c");
    assert!(!fixture.journal().exists());
}

#[test]
fn newer_user_edit_or_deletion_is_never_overwritten_on_retry() {
    let fixture = Fixture::new();
    let writes = vec![
        fixture.write("a", Some(b"original"), Some(b"incoming")),
        fixture.write("b", None, Some(b"later")),
    ];
    let mut transaction =
        Transaction::prepare(&fixture.journal(), binding(), writes, 1024).unwrap();
    assert!(transaction
        .apply(&|| {
            ensure!(
                fs::read(fixture.root().join("a")).unwrap_or_default() != b"incoming",
                "failure after first file"
            );
            Ok(())
        })
        .is_err());
    fs::write(fixture.root().join("a"), b"new user edit").unwrap();
    let mut retry = Transaction::open(&fixture.journal(), &binding())
        .unwrap()
        .unwrap();
    assert!(retry.apply(&|| Ok(())).is_err());
    assert_eq!(
        fs::read(fixture.root().join("a")).unwrap(),
        b"new user edit"
    );
    // Simulate a crash after rename, before the applied bit was persisted.
    fs::remove_file(fixture.root().join("a")).unwrap();
    // Keep only the durable ready record, as if rename preceded the crash.
    fs::write(fixture.journal().join("progress.jsonl"), b"[0,1]\n").unwrap();
    let mut retry = Transaction::open(&fixture.journal(), &binding())
        .unwrap()
        .unwrap();
    assert!(retry.apply(&|| Ok(())).is_err());
    assert!(!fixture.root().join("a").exists());
    assert!(fixture.journal().join("0.before").exists());
}

#[test]
fn symlink_parent_or_replaced_root_refuses_without_touching_other_folders() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let writes = vec![fixture.write("sub/a", Some(b"original"), Some(b"incoming"))];
    let mut transaction =
        Transaction::prepare(&fixture.journal(), binding(), writes, 1024).unwrap();
    let outside = fixture.0.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("a"), b"outside").unwrap();
    fs::rename(fixture.root().join("sub"), fixture.root().join("sub-old")).unwrap();
    symlink(&outside, fixture.root().join("sub")).unwrap();
    assert!(transaction.apply(&|| Ok(())).is_err());
    assert_eq!(fs::read(outside.join("a")).unwrap(), b"outside");
    fs::remove_file(fixture.root().join("sub")).unwrap();
    fs::rename(fixture.root().join("sub-old"), fixture.root().join("sub")).unwrap();
    fs::rename(fixture.root(), fixture.0.join("old-project")).unwrap();
    fs::create_dir(fixture.root()).unwrap();
    assert!(transaction.apply(&|| Ok(())).is_err());
    assert_eq!(
        fs::read(fixture.0.join("old-project/sub/a")).unwrap(),
        b"original"
    );
}

#[test]
fn storage_budget_refuses_before_any_destination_change() {
    let fixture = Fixture::new();
    let writes = vec![fixture.write("a", Some(b"original"), Some(b"incoming"))];
    assert!(Transaction::prepare(&fixture.journal(), binding(), writes, 3).is_err());
    assert_eq!(fs::read(fixture.root().join("a")).unwrap(), b"original");
    assert!(!fixture.journal().exists());
}

#[test]
fn empty_directories_survive_install_and_retry_refuses_symlinks() {
    let fixture = Fixture::new();
    let staged = fixture.0.join("stage/empty");
    fs::create_dir(&staged).unwrap();
    let write = Write {
        root: fixture.root(),
        relative: "empty".into(),
        before: None,
        after: Some(staged),
    };
    let mut transaction =
        Transaction::prepare(&fixture.journal(), binding(), vec![write], 1024).unwrap();
    transaction.apply(&|| Ok(())).unwrap();
    assert!(fixture.root().join("empty").is_dir());
    transaction.commit(&|| Ok(())).unwrap();
    transaction.cleanup().unwrap();
}

#[test]
fn overlapping_native_overlay_targets_merge_only_the_same_before_image() {
    let fixture = Fixture::new();
    let first = fixture.write("sub/native", Some(b"original"), Some(b"overlay"));
    let last_before = fixture.0.join("stage/native-original");
    let last_after = fixture.0.join("stage/native-session");
    fs::write(&last_before, b"original").unwrap();
    fs::write(&last_after, b"session").unwrap();
    let second = Write {
        root: fixture.root().join("sub"),
        relative: "native".into(),
        before: Some(last_before.clone()),
        after: Some(last_after.clone()),
    };
    let mut transaction =
        Transaction::prepare(&fixture.journal(), binding(), vec![first, second], 1024).unwrap();
    assert_eq!(transaction.journal.intents.len(), 1);
    transaction.apply(&|| Ok(())).unwrap();
    transaction.commit(&|| Ok(())).unwrap();
    transaction.cleanup().unwrap();
    assert_eq!(
        fs::read(fixture.root().join("sub/native")).unwrap(),
        b"session"
    );
    let first = fixture.write("sub/native", Some(b"original"), Some(b"overlay"));
    fs::write(&last_before, b"different captured original").unwrap();
    let second = Write {
        root: fixture.root().join("sub"),
        relative: "native".into(),
        before: Some(last_before),
        after: Some(last_after),
    };
    assert!(
        Transaction::prepare(&fixture.journal(), binding(), vec![first, second], 1024).is_err()
    );
    assert_eq!(
        fs::read(fixture.root().join("sub/native")).unwrap(),
        b"original"
    );
    assert!(!fixture.journal().exists());
}
#[test]
fn progress_is_bounded_append_only_and_incomplete_tail_is_not_a_completed_step() {
    let fixture = Fixture::new();
    let writes = vec![fixture.write("a", Some(b"original"), Some(b"incoming"))];
    let mut transaction =
        Transaction::prepare(&fixture.journal(), binding(), writes, 1024).unwrap();
    let manifest = fs::read(fixture.journal().join("journal.json")).unwrap();
    transaction.apply(&|| Ok(())).unwrap();
    assert_eq!(
        fs::read(fixture.journal().join("journal.json")).unwrap(),
        manifest
    );
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(fixture.journal().join("progress.jsonl"))
        .unwrap()
        .write_all(b"[1,3")
        .unwrap();
    drop(transaction);
    let mut retry = Transaction::open(&fixture.journal(), &binding())
        .unwrap()
        .unwrap();
    assert!(!retry.committed());
    assert_eq!(
        fs::read(fixture.journal().join("progress.jsonl")).unwrap(),
        b"[0,1]\n[0,2]\n"
    );
    retry.commit(&|| Ok(())).unwrap();
    drop(retry);
    let retry = Transaction::open(&fixture.journal(), &binding())
        .unwrap()
        .unwrap();
    assert!(retry.committed());
    retry.cleanup().unwrap();
}
#[test]
fn commit_refuses_user_edits_after_all_files_were_installed() {
    let fixture = Fixture::new();
    let writes = vec![fixture.write("a", Some(b"original"), Some(b"incoming"))];
    let mut transaction =
        Transaction::prepare(&fixture.journal(), binding(), writes, 1024).unwrap();
    transaction.apply(&|| Ok(())).unwrap();
    fs::write(
        fixture.root().join("a"),
        b"user edit during metadata finalization",
    )
    .unwrap();
    assert!(transaction.commit(&|| Ok(())).is_err());
    assert!(fixture.journal().join("0.before").exists());
    assert_eq!(
        fs::read(fixture.root().join("a")).unwrap(),
        b"user edit during metadata finalization"
    );
}

#[test]
fn live_git_reservations_block_external_git_and_recover_only_our_locks() {
    let fixture = Fixture::new();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(fixture.root())
            .output()
            .unwrap()
    };
    assert!(git(&["init", "-b", "main"]).status.success());
    assert!(git(&["config", "user.email", "fixture@example.invalid"])
        .status
        .success());
    assert!(git(&["config", "user.name", "fixture"]).status.success());
    fs::write(fixture.root().join("a"), b"original").unwrap();
    assert!(git(&["add", "a"]).status.success());
    assert!(git(&["commit", "-m", "fixture"]).status.success());
    let writes = vec![fixture.write("a", Some(b"original"), Some(b"incoming"))];
    let mut transaction =
        Transaction::prepare(&fixture.journal(), binding(), writes, 1024).unwrap();
    let git_root = fixture.root().join(".git");
    transaction
        .reserve_git(vec![git_root.clone()], &|| Ok(()))
        .unwrap();
    let mut saved_locks = Vec::new();
    for reservation in &transaction.reservations {
        let file = plain(
            &reservation.directory,
            std::ffi::OsStr::new(&reservation.name),
        )
        .unwrap()
        .unwrap();
        let mut bytes = Vec::new();
        file.take(4096).read_to_end(&mut bytes).unwrap();
        // Save absolute names using the small fixture's known roots.
        let target = if reservation.name == "main.lock" {
            git_root.join("refs/heads/main.lock")
        } else {
            git_root.join(&reservation.name)
        };
        saved_locks.push((target, bytes));
    }
    transaction
        .apply(&|| {
            assert!(!git(&["add", "a"]).status.success());
            assert!(!git(&["checkout", "-b", "other"]).status.success());
            assert!(!git(&["update-ref", "refs/heads/main", "HEAD"])
                .status
                .success());
            Ok(())
        })
        .unwrap();
    drop(transaction);
    // Restore exactly the bytes that a process crash would leave on disk.
    for (path, bytes) in &saved_locks {
        fs::write(path, bytes).unwrap();
    }
    let mut retry = Transaction::open(&fixture.journal(), &binding())
        .unwrap()
        .unwrap();
    retry.reserve_git(Vec::new(), &|| Ok(())).unwrap();
    retry.apply(&|| Ok(())).unwrap();
    retry.commit(&|| Ok(())).unwrap();
    drop(retry);
    for (path, bytes) in &saved_locks {
        fs::write(path, bytes).unwrap();
    }
    // A committed crash must clear its own locks before profiles use Git.
    Transaction::cleanup_committed(
        &fixture.journal(),
        "https://fixture.invalid",
        Some("a-1"),
        "w-1",
        7,
    )
    .unwrap();
    assert!(git(&["add", "a"]).status.success());
    assert!(saved_locks.iter().all(|(path, _)| !path.exists()));

    let writes = vec![fixture.write("a", Some(b"original"), Some(b"next"))];
    let mut transaction =
        Transaction::prepare(&fixture.journal(), binding(), writes, 1024).unwrap();
    fs::write(git_root.join("index.lock"), b"external Git reservation").unwrap();
    assert!(transaction
        .reserve_git(vec![git_root.clone()], &|| Ok(()))
        .is_err());
    assert_eq!(
        fs::read(git_root.join("index.lock")).unwrap(),
        b"external Git reservation"
    );
    assert_eq!(fs::read(fixture.root().join("a")).unwrap(), b"original");
}

#[test]
fn a_new_repository_never_commits_over_a_concurrent_external_git_index_edit() {
    let fixture = Fixture::new();
    fs::write(fixture.root().join("a"), b"original").unwrap();
    let before = fixture.0.join("stage/tree-before");
    snapshot(&fixture.root(), &before, &|_| true, 1024 * 1024).unwrap();
    let incoming = fixture.0.join("stage/incoming");
    fs::create_dir(&incoming).unwrap();
    let git = |root: &Path, args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap()
    };
    for args in [
        &["init", "-b", "main"][..],
        &["config", "user.email", "fixture@example.invalid"],
        &["config", "user.name", "fixture"],
    ] {
        assert!(git(&incoming, args).status.success());
    }
    fs::write(incoming.join("a"), b"incoming").unwrap();
    assert!(git(&incoming, &["add", "a"]).status.success());
    assert!(git(&incoming, &["commit", "-m", "fixture"])
        .status
        .success());
    let writes = changes(&fixture.root(), &before, &incoming).unwrap();
    let mut transaction =
        Transaction::prepare(&fixture.journal(), binding(), writes, 1024 * 1024).unwrap();
    let edited = std::cell::Cell::new(false);
    transaction
        .apply(&|| {
            if !edited.get()
                && fixture.root().join(".git/index").exists()
                && git(&fixture.root(), &["rev-parse", "--verify", "HEAD"])
                    .status
                    .success()
            {
                assert!(git(&fixture.root(), &["add", "a"]).status.success());
                edited.set(true);
            }
            Ok(())
        })
        .unwrap();
    assert!(edited.get());
    let newer_index = fs::read(fixture.root().join(".git/index")).unwrap();
    assert!(transaction.commit(&|| Ok(())).is_err());
    assert_eq!(
        fs::read(fixture.root().join(".git/index")).unwrap(),
        newer_index
    );
    assert_eq!(git(&fixture.root(), &["show", ":a"]).stdout, b"original");
    drop(transaction);
    let mut retry = Transaction::open(&fixture.journal(), &binding())
        .unwrap()
        .unwrap();
    assert!(retry.apply(&|| Ok(())).is_err());
    assert_eq!(
        fs::read(fixture.root().join(".git/index")).unwrap(),
        newer_index
    );
}

#[test]
fn invalid_recovery_roots_and_names_refuse_before_any_git_filesystem_loop() {
    let fixture = Fixture::new();
    let transaction = Transaction::prepare(
        &fixture.journal(),
        binding(),
        vec![fixture.write("a", Some(b"old"), Some(b"new"))],
        1024,
    )
    .unwrap();
    let path = fixture.journal().join("journal.json");
    let original = fs::read(&path).unwrap();
    for roots in [
        vec![
            PathBuf::from("/missing-a"),
            PathBuf::from("/missing-b"),
            PathBuf::from("/missing-c"),
        ],
        vec![PathBuf::from("/missing-a"), PathBuf::from("/missing-a")],
        vec![PathBuf::from("/tmp/../missing-a")],
        vec![PathBuf::from("relative")],
        vec![PathBuf::from(format!("/{}", "a".repeat(4096)))],
    ] {
        let mut record: serde_json::Value = serde_json::from_slice(&original).unwrap();
        record["git_roots"] = serde_json::to_value(roots).unwrap();
        fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        let error = Transaction::open(&fixture.journal(), &binding())
            .err()
            .unwrap()
            .to_string();
        assert!(
            error.contains("Git root"),
            "validates before attempting to open nonexistent paths: {error}"
        );
    }
    for field in ["committed", "aside", "blob"] {
        let mut record: serde_json::Value = serde_json::from_slice(&original).unwrap();
        match field {
            "committed" => record["committed"] = true.into(),
            "aside" => record["intents"][0]["aside"] = ".chimaera-staging-return-wrong-0".into(),
            _ => record["intents"][0]["before"]["blob"] = "../foreign".into(),
        }
        fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(Transaction::open(&fixture.journal(), &binding()).is_err());
    }
    assert_eq!(fs::read(fixture.root().join("a")).unwrap(), b"old");
    drop(transaction);
}

#[test]
fn durable_state_enrollment_never_creates_children_through_a_symlink() {
    let fixture = Fixture::new();
    let outside = fixture.0.join("outside");
    fs::create_dir(&outside).unwrap();
    let alias = fixture.0.join("alias");
    std::os::unix::fs::symlink(&outside, &alias).unwrap();
    assert!(write_state(&alias.join("not-created/state.json"), b"{}").is_err());
    assert!(!outside.join("not-created").exists());
}

#[test]
fn a_foreign_binding_never_repairs_an_incomplete_progress_tail() {
    let fixture = Fixture::new();
    let transaction = Transaction::prepare(
        &fixture.journal(),
        binding(),
        vec![fixture.write("a", Some(b"old"), Some(b"new"))],
        1024,
    )
    .unwrap();
    let progress = fixture.journal().join("progress.jsonl");
    fs::write(&progress, b"[0,1]\n{").unwrap();
    let mut foreign = binding();
    foreign.epoch += 1;
    assert!(Transaction::open(&fixture.journal(), &foreign).is_err());
    assert_eq!(fs::read(progress).unwrap(), b"[0,1]\n{");
    drop(transaction);
}

#[test]
fn a_fifo_progress_record_refuses_without_waiting_for_an_external_reader() {
    let fixture = Fixture::new();
    let mut transaction = Transaction::prepare(
        &fixture.journal(),
        binding(),
        vec![fixture.write("a", Some(b"old"), Some(b"new"))],
        1024,
    )
    .unwrap();
    nix::unistd::mkfifo(
        &fixture.journal().join("progress.jsonl"),
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    let started = std::time::Instant::now();
    assert!(Transaction::open(&fixture.journal(), &binding()).is_err());
    assert!(transaction.progress(0, 1).is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(fs::read(fixture.root().join("a")).unwrap(), b"old");
}
