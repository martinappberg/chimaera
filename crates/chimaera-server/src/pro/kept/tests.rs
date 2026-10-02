//! The kept-version review over the real router (bearer auth included):
//! listing, both texts, each choice, `resolve_all`, where a discarded copy
//! goes (a fixture home Trash, never the real one), and the path fences —
//! only recorded siblings, only inside the project, never through a link.
use super::*;
use crate::pro::{trash::in_trash, Ownership};
use axum::{body::Body, http::Method, http::Request};
use http_body_util::BodyExt;
use std::sync::atomic::Ordering;
use tower::ServiceExt;

const STAMP: &str = "mine-20260929-1412";

struct Fixture {
    root: PathBuf,
    state: Arc<AppState>,
    project: PathBuf,
    workspace: String,
    /// The fixture's home Trash (under `root`); `None`: this computer has
    /// no Trash, so a discarded copy is deleted.
    trash: Option<PathBuf>,
}
impl Fixture {
    fn new() -> Self {
        Self::build(true)
    }
    fn without_trash() -> Self {
        Self::build(false)
    }
    fn build(with_trash: bool) -> Self {
        let root =
            std::env::temp_dir().join(format!("chimaera-kept-{}", chimaera_core::generate_token()));
        std::fs::create_dir_all(root.join("project")).unwrap();
        std::fs::create_dir_all(root.join("outside")).unwrap();
        let root = root.canonicalize().unwrap();
        let mut state = AppState::new(
            "fixture-token".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        );
        let trash = with_trash.then(|| crate::pro::trash::fixture_home_trash(&root));
        state.pro.trash = trash.clone();
        let state = Arc::new(state);
        state.stopping.store(true, Ordering::Release);
        let project = root.join("project");
        let workspace = crate::lock(&state.workspaces)
            .add(project.clone())
            .unwrap()
            .id;
        Self {
            root,
            state,
            project,
            workspace,
            trash,
        }
    }
    /// A discarded copy's text in the fixture's Trash.
    fn trashed(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(in_trash(self.trash.as_ref()?, name)).ok()
    }
    fn write(&self, relative: &str, body: &str) {
        let path = self.project.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    fn read(&self, relative: &str) -> Option<String> {
        std::fs::read_to_string(self.project.join(relative)).ok()
    }
    /// What a return writes: the kept copies on disk (by the caller) and the
    /// recorded report.
    fn keep(&self, files: usize, paths: &[&str]) {
        crate::pro::report_return(
            &self.state,
            &self.workspace,
            (files, paths.iter().map(PathBuf::from).collect()),
            &[],
        );
    }
    async fn call(&self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let uri = format!("/api/v1/pro/projects/{}/{uri}", self.workspace);
        let response = crate::app(self.state.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("Authorization", "Bearer fixture-token")
                    .header("Content-Type", "application/json")
                    .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }
    async fn list(&self) -> Value {
        let (status, value) = self.call(Method::GET, "kept", None).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        value
    }
    async fn resolve(&self, mine_path: &str, choice: &str) -> (StatusCode, Value) {
        self.call(
            Method::POST,
            "kept/resolve",
            Some(json!({"mine_path": mine_path, "choice": choice})),
        )
        .await
    }
    fn open(&self) -> Option<usize> {
        crate::lock(&self.state.pro.status)
            .get(&self.workspace)
            .and_then(|status| status.kept_both)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn kept(name: &str) -> String {
    format!("{name}.{STAMP}")
}
fn mine_paths(listing: &Value) -> Vec<String> {
    listing["pairs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pair| pair["mine_path"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn each_choice_settles_its_pair_and_the_report_ends_with_the_last() {
    let fx = Fixture::new();
    fx.write("notes.md", "the cloud's line\n");
    fx.write(&kept("notes.md"), "this computer's line\n");
    fx.write("data/table.csv", "a,b\n1,2\n");
    fx.write(&kept("data/table.csv"), "a,b\n1,3\n");
    // The cloud deleted plan.txt; this computer's edit is all that is left.
    fx.write(&kept("plan.txt"), "keep me\n");
    fx.keep(
        3,
        &[
            &kept("notes.md"),
            &kept("data/table.csv"),
            &kept("plan.txt"),
        ],
    );

    let listing = fx.list().await;
    assert_eq!(listing["files"], 3);
    assert_eq!(listing["total"], 3);
    assert_eq!(listing["unlisted"], 0);
    assert_eq!(listing["here"], true);
    assert_eq!(listing["trash"], true);
    assert_eq!(listing["branches"], json!([]));
    assert!(listing["returned_at"].as_u64().is_some_and(|at| at > 0));
    let pairs = listing["pairs"].as_array().unwrap();
    assert_eq!(pairs.len(), 3);
    assert_eq!(pairs[0]["path"], "notes.md");
    assert_eq!(pairs[0]["mine_path"], kept("notes.md").as_str());
    assert_eq!(pairs[0]["size"], 17);
    assert_eq!(pairs[0]["mine_size"], 21);
    assert!(pairs[0]["changed_at"].as_u64().is_some());
    assert_eq!(pairs[2]["path"], "plan.txt");
    assert_eq!(pairs[2]["size"], Value::Null, "the cloud deleted it");

    let (status, both) = fx
        .call(
            Method::GET,
            &format!("kept/file?mine_path={}", kept("notes.md")),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{both}");
    assert_eq!(both["mine"]["text"], "this computer's line\n");
    assert_eq!(both["cloud"]["text"], "the cloud's line\n");
    assert_eq!(both["mine"]["binary"], false);
    let (_, deleted) = fx
        .call(
            Method::GET,
            &format!("kept/file?mine_path={}", kept("plan.txt")),
            None,
        )
        .await;
    assert_eq!(deleted["cloud"], Value::Null);
    assert_eq!(deleted["mine"]["text"], "keep me\n");

    // This computer's version replaces the file; its copy is gone.
    let (status, after) = fx.resolve(&kept("notes.md"), "use_mine").await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_eq!(
        fx.read("notes.md").as_deref(),
        Some("this computer's line\n")
    );
    assert_eq!(fx.read(&kept("notes.md")), None);
    assert_eq!(after["files"], 2);
    assert_eq!(after["total"], 3, "the return's own count never drops");
    assert_eq!(after["failed"], json!([]));
    assert_eq!(after["discarded"], json!({"trash": 0, "deleted": 0}));

    // The cloud's version stays; this computer's copy leaves the project
    // for the Trash, under its own name.
    let (status, after) = fx.resolve(&kept("data/table.csv"), "use_cloud").await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_eq!(fx.read("data/table.csv").as_deref(), Some("a,b\n1,2\n"));
    assert_eq!(fx.read(&kept("data/table.csv")), None);
    assert!(!fx.project.join(kept("data/table.csv")).exists());
    assert_eq!(
        fx.trashed(&kept("table.csv")).as_deref(),
        Some("a,b\n1,3\n")
    );
    assert_eq!(after["discarded"], json!({"trash": 1, "deleted": 0}));
    assert_eq!(mine_paths(&after), vec![kept("plan.txt")]);

    // Keep both: nothing moves, and the last pair ends the report.
    let (status, after) = fx.resolve(&kept("plan.txt"), "keep_both").await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_eq!(fx.read(&kept("plan.txt")).as_deref(), Some("keep me\n"));
    assert_eq!(fx.read("plan.txt"), None);
    assert_eq!(after["files"], 0);
    assert_eq!(after["pairs"], json!([]));
    assert_eq!(after["returned_at"], Value::Null);
    assert_eq!(fx.open(), None);

    // Settled for good: a restart restores no report.
    let restored = crate::pro::ProState::new(fx.root.join("pro"));
    assert!(crate::lock(&restored.status)
        .get(&fx.workspace)
        .is_none_or(|status| status.kept_both.is_none()));
    // And a settled pair cannot be chosen again.
    let (status, again) = fx.resolve(&kept("plan.txt"), "use_mine").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(again["error_code"], "not_kept");
}

#[tokio::test]
async fn a_shortened_kept_name_never_overwrites_an_existing_prefix_neighbor() {
    let fx = Fixture::new();
    let name = "a".repeat(250);
    fx.write(&name, "local version");
    let kept = canonical::KeptCopies::new()
        .keep(&fx.project.join(&name), Path::new(&name))
        .unwrap();
    let mine = kept.to_str().unwrap();
    let neighbor = canonical::original_name(kept.file_name().unwrap().to_str().unwrap()).unwrap();
    assert_ne!(neighbor, name);
    fx.write(&name, "cloud version");
    fx.write(neighbor, "unrelated prefix neighbor");
    fx.keep(1, &[mine]);
    assert_eq!(fx.list().await["pairs"][0]["can_use_mine"], false);
    let (status, result) = fx.resolve(mine, "use_mine").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(result["error_code"], "unsafe_path");
    assert_eq!(
        fx.read(neighbor).as_deref(),
        Some("unrelated prefix neighbor")
    );
    assert_eq!(fx.read(&name).as_deref(), Some("cloud version"));
    assert_eq!(fx.read(mine).as_deref(), Some("local version"));
    assert_eq!(fx.open(), Some(1));
}

#[tokio::test]
async fn a_replaced_root_symlink_never_reads_or_changes_outside_pairs() {
    let fx = Fixture::new();
    fx.keep(1, &[&kept("a.txt")]);
    let outside = fx.root.join("outside");
    std::fs::write(outside.join("a.txt"), "outside original").unwrap();
    std::fs::write(outside.join(kept("a.txt")), "outside kept").unwrap();
    std::fs::rename(&fx.project, fx.root.join("previous-project")).unwrap();
    std::os::unix::fs::symlink(&outside, &fx.project).unwrap();
    let (status, _) = fx.resolve(&kept("a.txt"), "use_mine").await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = fx.call(Method::GET, "kept", None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        std::fs::read_to_string(outside.join("a.txt")).unwrap(),
        "outside original"
    );
    assert_eq!(
        std::fs::read_to_string(outside.join(kept("a.txt"))).unwrap(),
        "outside kept"
    );
}

#[tokio::test]
async fn owned_choices_hold_drainage_after_cancellation_and_refuse_changed_authority() {
    for case in 0..3 {
        let fx = Fixture::new();
        fx.write("a.txt", "cloud");
        fx.write(&kept("a.txt"), "mine");
        fx.keep(1, &[&kept("a.txt")]);
        crate::lock(&fx.state.pro.ownership)
            .insert(fx.workspace.clone(), Ownership::Local { epoch: 3 });
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let owner = fx.state.clone();
        let workspace = fx.workspace.clone();
        let task = tokio::spawn(async move {
            choose(
                &owner,
                &workspace,
                false,
                move |root, recorded, home, current| {
                    entered_tx.send(()).unwrap();
                    resume_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                    let path = recorded[0].clone();
                    let mut result = Outcome::default();
                    result.discarded(apply(root, &path, Choice::UseMine, home, current)?);
                    result.settled.push(path);
                    Ok(result)
                },
            )
            .await
        });
        entered_rx.await.unwrap();
        assert!(!crate::pro::execution::quiescent(&fx.state, &fx.workspace));
        let cache = fx.state.pro.cache(&fx.workspace).unwrap();
        assert!(cache.try_lock().is_err());
        assert!(fx.state.pro.configuration.try_lock().is_err());
        match case {
            0 => {
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
                assert!(!crate::pro::execution::quiescent(&fx.state, &fx.workspace));
                assert!(cache.try_lock().is_err());
                resume_tx.send(()).unwrap();
                let _configuration =
                    tokio::time::timeout(Duration::from_secs(5), fx.state.pro.configuration.lock())
                        .await
                        .unwrap();
                assert_eq!(fx.read("a.txt").as_deref(), Some("mine"));
                assert_eq!(fx.open(), None);
            }
            _ => {
                if case == 1 {
                    crate::lock(&fx.state.pro.ownership).insert(
                        fx.workspace.clone(),
                        Ownership::Remote {
                            epoch: 4,
                            holder: "other-device".into(),
                        },
                    );
                } else {
                    fx.state.pro.generation.fetch_add(1, Ordering::AcqRel);
                }
                resume_tx.send(()).unwrap();
                assert!(matches!(task.await.unwrap(), Err(Refusal::NotHere)));
                assert_eq!(fx.read("a.txt").as_deref(), Some("cloud"));
                assert_eq!(fx.read(&kept("a.txt")).as_deref(), Some("mine"));
                assert_eq!(fx.open(), Some(1));
            }
        }
        assert!(crate::pro::execution::quiescent(&fx.state, &fx.workspace));
    }
}

#[tokio::test]
async fn listing_waits_for_return_replacement_and_refuses_an_account_change() {
    for changed_account in [false, true] {
        let fx = Fixture::new();
        // The old report's sibling is absent. Scanning it without the return
        // reservation would settle a new report that reuses the same name.
        fx.keep(1, &[&kept("a.txt")]);
        let cache = fx.state.pro.cache(&fx.workspace).unwrap();
        let held = cache.lock().await;
        let owner = fx.state.clone();
        let workspace = fx.workspace.clone();
        let task = tokio::spawn(async move { listing(&owner, &workspace).await });
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        assert!(!task.is_finished(), "listing must wait for the return");
        assert_eq!(fx.open(), Some(1));
        {
            let _configuration = fx.state.pro.configuration.lock().await;
            fx.write("a.txt", "new cloud");
            fx.write(&kept("a.txt"), "new mine");
            fx.keep(1, &[&kept("a.txt")]);
            if changed_account {
                fx.state.pro.generation.fetch_add(1, Ordering::AcqRel);
            }
        }
        drop(held);
        let result = task.await.unwrap();
        if changed_account {
            assert!(matches!(result, Err(Refusal::NotHere)));
        } else {
            let value = result.unwrap();
            assert_eq!(value["files"], 1);
            assert_eq!(value["pairs"][0]["mine_path"], kept("a.txt"));
        }
        assert_eq!(fx.open(), Some(1));
        assert_eq!(fx.read(&kept("a.txt")).as_deref(), Some("new mine"));
    }
}

#[test]
fn a_root_swapped_away_and_back_cannot_supply_a_foreign_mutation_parent() {
    let fx = Fixture::new();
    fx.write("a.txt", "cloud");
    fx.write(&kept("a.txt"), "mine");
    let foreign = fx.root.join("outside");
    std::fs::write(foreign.join("a.txt"), "foreign cloud").unwrap();
    std::fs::write(foreign.join(kept("a.txt")), "foreign mine").unwrap();
    let pinned = open_dir(&fx.project, Path::new("")).unwrap().unwrap();
    let displaced = fx.root.join("displaced");
    let calls = std::cell::Cell::new(0);
    let current = || {
        if calls.get() == 0 {
            std::fs::rename(&fx.project, &displaced).unwrap();
            std::fs::rename(&foreign, &fx.project).unwrap();
        } else {
            std::fs::rename(&fx.project, &foreign).unwrap();
            std::fs::rename(&displaced, &fx.project).unwrap();
        }
        calls.set(calls.get() + 1);
        Ok(pinned.try_clone().unwrap())
    };
    apply(
        &fx.project,
        &PathBuf::from(kept("a.txt")),
        Choice::UseMine,
        None,
        &current,
    )
    .unwrap();
    assert_eq!(calls.get(), 2);
    assert_eq!(fx.read("a.txt").as_deref(), Some("mine"));
    assert_eq!(
        std::fs::read_to_string(foreign.join("a.txt")).unwrap(),
        "foreign cloud"
    );
    assert_eq!(
        std::fs::read_to_string(foreign.join(kept("a.txt"))).unwrap(),
        "foreign mine"
    );
}

#[tokio::test]
async fn a_restart_keeps_the_open_pairs_and_when_they_came_home() {
    let fx = Fixture::new();
    fx.write("a.txt", "cloud");
    fx.write(&kept("a.txt"), "mine");
    fx.write("b.txt", "cloud");
    fx.write(&kept("b.txt"), "mine");
    fx.keep(2, &[&kept("a.txt"), &kept("b.txt")]);
    let (status, _) = fx.resolve(&kept("a.txt"), "use_cloud").await;
    assert_eq!(status, StatusCode::OK);
    let at = fx.list().await["returned_at"].as_u64().unwrap();
    let restored = crate::pro::ProState::new(fx.root.join("pro"));
    let statuses = crate::lock(&restored.status);
    let status = statuses.get(&fx.workspace).expect("report restored");
    assert_eq!(status.kept_both, Some(1));
    assert_eq!(status.kept_paths, vec![PathBuf::from(kept("b.txt"))]);
    assert_eq!(status.kept_at, Some(at));
    assert_eq!(status.kept_total, Some(2));
}

#[tokio::test]
async fn only_recorded_siblings_inside_the_project_and_never_through_a_link() {
    let fx = Fixture::new();
    let outside = fx.root.join("outside");
    std::fs::write(outside.join("secret.txt"), "secret").unwrap();
    std::fs::write(outside.join(kept("secret.txt")), "planted").unwrap();
    // A folder in the project that is a link out of it.
    std::os::unix::fs::symlink(&outside, fx.project.join("link")).unwrap();
    // A kept copy that is itself a link out.
    fx.write("evil.txt", "cloud");
    std::os::unix::fs::symlink(
        outside.join("secret.txt"),
        fx.project.join(kept("evil.txt")),
    )
    .unwrap();
    // The file's own path is a link out.
    std::os::unix::fs::symlink(outside.join("secret.txt"), fx.project.join("aimed.txt")).unwrap();
    fx.write(&kept("aimed.txt"), "mine");
    // A real pair the return never recorded.
    fx.write("free.txt", "cloud");
    fx.write(&kept("free.txt"), "mine");
    // A recorded path that climbs out, and one into `.git`.
    let climbing = format!("../outside/{}", kept("secret.txt"));
    let git = format!(".git/{}", kept("config"));
    fx.keep(
        5,
        &[
            &format!("link/{}", kept("secret.txt")),
            &kept("evil.txt"),
            &kept("aimed.txt"),
            &climbing,
            &git,
        ],
    );

    for (path, choice, code) in [
        (
            format!("link/{}", kept("secret.txt")),
            "use_mine",
            "unsafe_path",
        ),
        (kept("evil.txt"), "use_mine", "unsafe_path"),
        (kept("evil.txt"), "use_cloud", "unsafe_path"),
        (kept("aimed.txt"), "use_mine", "unsafe_path"),
        (climbing.clone(), "use_cloud", "unsafe_path"),
        (git.clone(), "use_cloud", "unsafe_path"),
        (kept("free.txt"), "use_mine", "not_kept"),
        (format!("/{}", kept("etc/passwd")), "use_cloud", "not_kept"),
    ] {
        let (status, refusal) = fx.resolve(&path, choice).await;
        assert!(
            status.is_client_error(),
            "{path} {choice}: {status} {refusal}"
        );
        assert_eq!(refusal["error_code"], code, "{path} {choice}");
    }
    let (status, refusal) = fx
        .call(
            Method::GET,
            &format!("kept/file?mine_path={}", kept("free.txt")),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{refusal}");
    let (status, _) = fx
        .call(
            Method::GET,
            &format!("kept/file?mine_path={}", kept("evil.txt")),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a link is never read");

    // Nothing outside the project moved, and nothing inside either.
    assert_eq!(
        std::fs::read_to_string(outside.join("secret.txt")).unwrap(),
        "secret"
    );
    assert_eq!(
        std::fs::read_to_string(outside.join(kept("secret.txt"))).unwrap(),
        "planted"
    );
    assert!(fx.project.join(kept("evil.txt")).is_symlink());
    assert!(fx.project.join("aimed.txt").is_symlink());
    assert_eq!(fx.read(&kept("aimed.txt")).as_deref(), Some("mine"));
    assert_eq!(fx.read(&kept("free.txt")).as_deref(), Some("mine"));

    // None of them is a pair to review: the listing settles them all.
    let listing = fx.list().await;
    assert_eq!(listing["pairs"], json!([]));
    assert_eq!(listing["files"], 0);
    assert_eq!(fx.open(), None);
}

#[tokio::test]
async fn resolve_all_settles_every_pair_and_the_unnamed_rest() {
    let fx = Fixture::new();
    for name in ["one.txt", "two.txt"] {
        fx.write(name, "cloud");
        fx.write(&kept(name), "mine");
    }
    // A return that kept 40 files names only some of them.
    fx.keep(40, &[&kept("one.txt"), &kept("two.txt")]);
    let listing = fx.list().await;
    assert_eq!(listing["unlisted"], 38);
    assert_eq!(listing["files"], 40);

    // Settling the named ones one by one leaves the unnamed rest open.
    let (_, after) = fx.resolve(&kept("one.txt"), "keep_both").await;
    assert_eq!(after["files"], 39);
    assert_eq!(after["unlisted"], 38);

    let (status, after) = fx
        .call(
            Method::POST,
            "kept/resolve_all",
            Some(json!({"choice": "use_mine"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_eq!(fx.read("two.txt").as_deref(), Some("mine"));
    assert_eq!(fx.read(&kept("two.txt")), None);
    assert_eq!(
        fx.read(&kept("one.txt")).as_deref(),
        Some("mine"),
        "kept both"
    );
    assert_eq!(after["files"], 0);
    assert_eq!(after["unlisted"], 0);
    assert_eq!(fx.open(), None);

    // A pair that cannot take the choice stays, named, and holds the report
    // (a folder this computer may not write to; root writes anywhere).
    if rustix::process::geteuid().is_root() {
        return;
    }
    let fx = Fixture::new();
    fx.write("fine.txt", "cloud");
    fx.write(&kept("fine.txt"), "mine");
    fx.write("locked/stuck.txt", "cloud");
    fx.write(&kept("locked/stuck.txt"), "mine");
    fx.keep(2, &[&kept("fine.txt"), &kept("locked/stuck.txt")]);
    let locked = fx.project.join("locked");
    let mode = |bits| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(bits)).unwrap();
    };
    mode(0o555);
    let (status, after) = fx
        .call(
            Method::POST,
            "kept/resolve_all",
            Some(json!({"choice": "use_cloud"})),
        )
        .await;
    mode(0o755);
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_eq!(fx.read(&kept("fine.txt")), None);
    assert_eq!(fx.trashed(&kept("fine.txt")).as_deref(), Some("mine"));
    assert_eq!(after["discarded"], json!({"trash": 1, "deleted": 0}));
    assert_eq!(fx.read(&kept("locked/stuck.txt")).as_deref(), Some("mine"));
    assert_eq!(
        after["failed"],
        json!([{"mine_path": kept("locked/stuck.txt"), "error_code": "failed"}])
    );
    assert_eq!(mine_paths(&after), vec![kept("locked/stuck.txt")]);
    assert_eq!(fx.open(), Some(1));
}

#[tokio::test]
async fn use_the_clouds_for_all_sends_every_copy_to_the_trash() {
    let fx = Fixture::new();
    for name in ["one.txt", "two.txt", "deep/three.txt"] {
        fx.write(name, "cloud");
        fx.write(&kept(name), &format!("mine {name}"));
    }
    // Something already in the Trash under a copy's name stays as it was.
    let taken = in_trash(fx.trash.as_ref().unwrap(), &kept("one.txt"));
    std::fs::create_dir_all(taken.parent().unwrap()).unwrap();
    std::fs::write(&taken, "an older copy").unwrap();
    fx.keep(
        3,
        &[&kept("one.txt"), &kept("two.txt"), &kept("deep/three.txt")],
    );
    let (status, after) = fx
        .call(
            Method::POST,
            "kept/resolve_all",
            Some(json!({"choice": "use_cloud"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_eq!(after["discarded"], json!({"trash": 3, "deleted": 0}));
    assert_eq!(after["files"], 0);
    for name in ["one.txt", "two.txt", "deep/three.txt"] {
        assert_eq!(fx.read(name).as_deref(), Some("cloud"));
        assert_eq!(fx.read(&kept(name)), None, "{name}");
    }
    assert_eq!(
        fx.trashed(&kept("one.txt")).as_deref(),
        Some("an older copy")
    );
    assert_eq!(
        fx.trashed(&format!("{} 2", kept("one.txt"))).as_deref(),
        Some("mine one.txt")
    );
    assert_eq!(
        fx.trashed(&kept("two.txt")).as_deref(),
        Some("mine two.txt")
    );
    assert_eq!(
        fx.trashed(&kept("three.txt")).as_deref(),
        Some("mine deep/three.txt")
    );
}

#[tokio::test]
async fn without_a_trash_the_clouds_version_deletes_the_copy_and_says_so() {
    let fx = Fixture::without_trash();
    fx.write("a.txt", "cloud");
    fx.write(&kept("a.txt"), "mine");
    fx.write("b.txt", "cloud");
    fx.write(&kept("b.txt"), "mine");
    fx.keep(2, &[&kept("a.txt"), &kept("b.txt")]);
    assert_eq!(fx.list().await["trash"], false);
    let (status, after) = fx.resolve(&kept("a.txt"), "use_cloud").await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_eq!(fx.read(&kept("a.txt")), None);
    assert_eq!(fx.read("a.txt").as_deref(), Some("cloud"));
    assert_eq!(after["discarded"], json!({"trash": 0, "deleted": 1}));
    assert_eq!(after["trash"], false);
    // Keeping both discards nothing.
    let (_, after) = fx.resolve(&kept("b.txt"), "keep_both").await;
    assert_eq!(after["discarded"], json!({"trash": 0, "deleted": 0}));
    assert_eq!(fx.read(&kept("b.txt")).as_deref(), Some("mine"));
}

#[tokio::test]
async fn choices_wait_for_the_project_to_be_here() {
    let fx = Fixture::new();
    fx.write("a.txt", "cloud");
    fx.write(&kept("a.txt"), "mine");
    fx.keep(1, &[&kept("a.txt")]);
    crate::lock(&fx.state.pro.ownership).insert(
        fx.workspace.clone(),
        Ownership::Remote {
            epoch: 3,
            holder: "worker-a".into(),
        },
    );
    let listing = fx.list().await;
    assert_eq!(listing["here"], false);
    assert_eq!(listing["files"], 1);
    for choice in ["use_mine", "use_cloud", "keep_both"] {
        let (status, refusal) = fx.resolve(&kept("a.txt"), choice).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(refusal["error_code"], "not_here");
    }
    let (status, _) = fx
        .call(
            Method::POST,
            "kept/resolve_all",
            Some(json!({"choice": "use_cloud"})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(fx.read(&kept("a.txt")).as_deref(), Some("mine"));
    assert_eq!(fx.open(), Some(1));

    // An unknown project, and one with nothing kept.
    let response = crate::app(fx.state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v1/pro/projects/w-nope/kept")
                .header("Authorization", "Bearer fixture-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let other = Fixture::new();
    let listing = other.list().await;
    assert_eq!(listing["files"], 0);
    assert_eq!(listing["pairs"], json!([]));
    // Without the token, nothing.
    let response = crate::app(fx.state.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/pro/projects/{}/kept", fx.workspace))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn large_and_binary_versions_show_sizes_only() {
    let fx = Fixture::new();
    std::fs::write(fx.project.join("fig.png"), [0u8, 1, 2, 3]).unwrap();
    std::fs::write(fx.project.join(kept("fig.png")), [0u8, 9]).unwrap();
    let big = "x".repeat(TEXT_MAX as usize + 1);
    fx.write("big.log", &big);
    fx.write(&kept("big.log"), "short");
    fx.keep(2, &[&kept("fig.png"), &kept("big.log")]);
    let (_, fig) = fx
        .call(
            Method::GET,
            &format!("kept/file?mine_path={}", kept("fig.png")),
            None,
        )
        .await;
    assert_eq!(fig["cloud"]["binary"], true);
    assert_eq!(fig["cloud"]["text"], Value::Null);
    assert_eq!(fig["cloud"]["size"], 4);
    assert_eq!(fig["mine"]["size"], 2);
    let (_, log) = fx
        .call(
            Method::GET,
            &format!("kept/file?mine_path={}", kept("big.log")),
            None,
        )
        .await;
    assert_eq!(log["cloud"]["too_large"], true);
    assert_eq!(log["cloud"]["text"], Value::Null);
    assert_eq!(log["mine"]["text"], "short");
}

#[tokio::test]
async fn the_clouds_branches_are_read_live_from_the_projects_refs() {
    let fx = Fixture::new();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(&fx.project)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Kept fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Kept fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    fx.write("a.txt", "a");
    git(&["add", "a.txt"]);
    git(&["commit", "-q", "-m", "a"]);
    for branch in [
        "main@cloud-0123456789ab",
        "feature/x@cloud-abcdef012345",
        "almost@cloud-xyz",
        "plain",
    ] {
        git(&["branch", branch]);
    }
    // Only a project whose return kept branches runs Git for the listing.
    assert_eq!(fx.list().await["branches"], json!([]));
    crate::lock(&fx.state.pro.preferences)
        .entry(fx.workspace.clone())
        .or_default()
        .git_branches = vec!["main@cloud-0123456789ab".into()];
    let listing = fx.list().await;
    let mut branches: Vec<String> = serde_json::from_value(listing["branches"].clone()).unwrap();
    branches.sort();
    assert_eq!(
        branches,
        vec!["feature/x@cloud-abcdef012345", "main@cloud-0123456789ab"]
    );
    // Merged and deleted by the user: gone from the list.
    git(&["branch", "-D", "main@cloud-0123456789ab"]);
    assert_eq!(
        fx.list().await["branches"],
        json!(["feature/x@cloud-abcdef012345"])
    );
}
