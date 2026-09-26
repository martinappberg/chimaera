//! `POST /api/v1/fs/validate`: the resolution ladder (bases, diff prefixes,
//! workspace-index basename and path-suffix fallbacks, `ambiguous`).

use super::support::*;
use crate::AppState;

async fn validate(state: &Arc<AppState>, body: serde_json::Value) -> serde_json::Value {
    let (status, answer) = request(state, Method::POST, "/api/v1/fs/validate", Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    answer
}

fn canon(path: &std::path::Path) -> String {
    std::fs::canonicalize(path)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

async fn workspace_at(state: &Arc<AppState>, root: &std::path::Path) -> String {
    let (status, ws) = request(
        state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ws}");
    ws["id"].as_str().unwrap().to_string()
}

/// `bases` are tried after `base`, in order; relative entries are ignored,
/// duplicates collapse, and `ambiguous` is always present.
#[tokio::test]
async fn fs_validate_tries_bases_in_order() {
    let state = test_state();
    let base = test_dir("v-base");
    let first = test_dir("v-first");
    let second = test_dir("v-second");
    std::fs::write(first.join("x.txt"), "1").unwrap();
    std::fs::write(second.join("x.txt"), "2").unwrap();
    std::fs::write(second.join("only-second.txt"), "2").unwrap();
    std::fs::write(base.join("here.txt"), "0").unwrap();

    let answer = validate(
        &state,
        serde_json::json!({
            "candidates": ["x.txt", "only-second.txt", "here.txt", "nowhere.txt"],
            "base": base.to_string_lossy(),
            "bases": ["relative/dir", base.to_string_lossy(), first.to_string_lossy(), second.to_string_lossy()],
        }),
    )
    .await;
    let valid = answer["valid"].as_object().unwrap();
    assert_eq!(valid["x.txt"]["path"], canon(&first.join("x.txt")));
    assert_eq!(
        valid["only-second.txt"]["path"],
        canon(&second.join("only-second.txt"))
    );
    assert_eq!(valid["here.txt"]["path"], canon(&base.join("here.txt")));
    assert!(!valid.contains_key("nowhere.txt"));
    assert_eq!(answer["ambiguous"], serde_json::json!({}));

    // At most eight extra bases count: a ninth is never tried.
    let mut bases: Vec<String> = (0..8)
        .map(|i| {
            test_dir(&format!("v-empty{i}"))
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    bases.push(first.to_string_lossy().into_owned());
    let answer = validate(
        &state,
        serde_json::json!({"candidates": ["x.txt"], "base": base.to_string_lossy(), "bases": bases}),
    )
    .await;
    assert!(answer["valid"].as_object().unwrap().is_empty(), "{answer}");
}

/// A git diff path (`a/src/x.rs`) resolves without its prefix — unless a
/// real `a/` directory holds it, which wins.
#[tokio::test]
async fn fs_validate_strips_git_diff_prefixes() {
    let state = test_state();
    let base = test_dir("v-diff");
    std::fs::create_dir_all(base.join("src")).unwrap();
    std::fs::write(base.join("src/lib.rs"), "x").unwrap();
    std::fs::create_dir_all(base.join("a/real")).unwrap();
    std::fs::write(base.join("a/real/f.rs"), "x").unwrap();
    std::fs::create_dir_all(base.join("real")).unwrap();
    std::fs::write(base.join("real/f.rs"), "x").unwrap();

    let answer = validate(
        &state,
        serde_json::json!({
            "candidates": ["a/src/lib.rs", "b/src/lib.rs", "a/real/f.rs", "c/src/lib.rs", "a/"],
            "base": base.to_string_lossy(),
        }),
    )
    .await;
    let valid = answer["valid"].as_object().unwrap();
    let lib = canon(&base.join("src/lib.rs"));
    assert_eq!(valid["a/src/lib.rs"]["path"], lib);
    assert_eq!(valid["b/src/lib.rs"]["path"], lib);
    assert_eq!(
        valid["a/real/f.rs"]["path"],
        canon(&base.join("a/real/f.rs"))
    );
    assert!(!valid.contains_key("c/src/lib.rs"));
    assert_eq!(valid["a/"]["kind"], "dir");
}

/// `strict` (document links) takes only the exact join per base: a broken
/// `b/spec.md` stays broken instead of opening `spec.md`, and no workspace
/// index fallback guesses either.
#[tokio::test]
async fn fs_validate_strict_takes_only_the_exact_join() {
    let state = test_state();
    let root = test_dir("v-strict");
    let docs = root.join("docs");
    std::fs::create_dir_all(docs.join("sub")).unwrap();
    std::fs::write(docs.join("spec.md"), "x").unwrap();
    std::fs::write(docs.join("sub/deep.md"), "x").unwrap();
    std::fs::write(root.join("top.md"), "x").unwrap();
    let ws = workspace_at(&state, &root).await;
    let candidates = [
        "spec.md",
        "b/spec.md",
        "deep.md",
        "sub/deep.md",
        "../top.md",
    ];

    // Lenient (chat, terminal): the prefix strip and the index both help.
    let answer = validate(
        &state,
        serde_json::json!({
            "candidates": candidates,
            "base": docs.to_string_lossy(),
            "workspace_id": ws,
        }),
    )
    .await;
    let valid = answer["valid"].as_object().unwrap();
    let spec = canon(&docs.join("spec.md"));
    assert_eq!(valid["b/spec.md"]["path"], spec, "{answer}");
    assert_eq!(
        valid["deep.md"]["path"],
        canon(&docs.join("sub/deep.md")),
        "{answer}"
    );

    let answer = validate(
        &state,
        serde_json::json!({
            "candidates": candidates,
            "base": docs.to_string_lossy(),
            "workspace_id": ws,
            "strict": true,
        }),
    )
    .await;
    let valid = answer["valid"].as_object().unwrap();
    assert_eq!(valid["spec.md"]["path"], spec, "{answer}");
    assert_eq!(
        valid["sub/deep.md"]["path"],
        canon(&docs.join("sub/deep.md"))
    );
    assert_eq!(valid["../top.md"]["path"], canon(&root.join("top.md")));
    assert!(!valid.contains_key("b/spec.md"), "{answer}");
    assert!(!valid.contains_key("deep.md"), "{answer}");
    assert_eq!(answer["ambiguous"], serde_json::json!({}));
}

/// Partial paths match workspace entries by whole trailing components: one
/// match is `valid`, several are `ambiguous` (shortest first, at most five).
#[tokio::test]
async fn fs_validate_matches_path_suffixes_in_the_workspace() {
    let state = test_state();
    let root = test_dir("v-suffix");
    for rel in [
        "results/figs/plot.png",
        "results/figs/models/model.safetensors",
        "one/x/y.rs",
        "two/deeper/x/y.rs",
        "lib/chat/view.ts",
    ] {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "x").unwrap();
    }
    for i in 0..7 {
        let path = root.join(format!("m{i}/common/util.py"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "x").unwrap();
    }
    let ws = workspace_at(&state, &root).await;
    // A cwd outside the tree: every hit below comes from the index.
    let cwd = test_dir("v-suffix-cwd");

    let answer = validate(
        &state,
        serde_json::json!({
            "candidates": [
                "figs/plot.png",      // unique suffix
                "s/plot.png",         // not a whole component
                "x/y.rs",             // two matches
                "common/util.py",     // seven matches, five listed
                "lib/chat/",          // a directory, trailing slash
                "./figs/plot.png",    // dot-relative: exact only
                "model.safetensors",  // long extension, bare basename
            ],
            "base": cwd.to_string_lossy(),
            "workspace_id": ws,
        }),
    )
    .await;
    let valid = answer["valid"].as_object().unwrap();
    let ambiguous = answer["ambiguous"].as_object().unwrap();
    assert_eq!(
        valid["figs/plot.png"]["path"],
        canon(&root.join("results/figs/plot.png"))
    );
    assert_eq!(valid["figs/plot.png"]["kind"], "file");
    assert!(!valid.contains_key("s/plot.png") && !ambiguous.contains_key("s/plot.png"));
    assert_eq!(
        ambiguous["x/y.rs"],
        serde_json::json!([
            {"path": canon(&root.join("one/x/y.rs")), "kind": "file"},
            {"path": canon(&root.join("two/deeper/x/y.rs")), "kind": "file"},
        ])
    );
    let common = ambiguous["common/util.py"].as_array().unwrap();
    assert_eq!(common.len(), 5, "{answer}");
    assert_eq!(common[0]["path"], canon(&root.join("m0/common/util.py")));
    assert_eq!(common[4]["path"], canon(&root.join("m4/common/util.py")));
    assert_eq!(valid["lib/chat/"]["path"], canon(&root.join("lib/chat")));
    assert_eq!(valid["lib/chat/"]["kind"], "dir");
    assert!(!valid.contains_key("./figs/plot.png"));
    assert_eq!(
        valid["model.safetensors"]["path"],
        canon(&root.join("results/figs/models/model.safetensors"))
    );
}

/// An ambiguous bare basename is listed, not dropped.
#[tokio::test]
async fn fs_validate_lists_ambiguous_basenames() {
    let state = test_state();
    let root = test_dir("v-ambig");
    for rel in ["a/dup.md", "bb/dup.md", "only/unique.md"] {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "x").unwrap();
    }
    let ws = workspace_at(&state, &root).await;
    let answer = validate(
        &state,
        serde_json::json!({
            "candidates": ["dup.md", "unique.md"],
            "base": root.to_string_lossy(),
            "workspace_id": ws,
        }),
    )
    .await;
    assert_eq!(
        answer["ambiguous"]["dup.md"],
        serde_json::json!([
            {"path": canon(&root.join("a/dup.md")), "kind": "file"},
            {"path": canon(&root.join("bb/dup.md")), "kind": "file"},
        ])
    );
    assert!(answer["valid"].get("dup.md").is_none());
    assert_eq!(
        answer["valid"]["unique.md"]["path"],
        canon(&root.join("only/unique.md"))
    );
}

/// Candidates longer than 1024 bytes are skipped even when they exist. The
/// candidates are long, the files are not: `./` hops keep the paths on disk
/// short. The cap is on the string an agent wrote, and macOS refuses any
/// path argument of 1024 bytes or more (its PATH_MAX; Linux's is 4096)
/// before a resolver sees it, so long real paths cannot even be created.
#[tokio::test]
async fn fs_validate_skips_candidates_over_1024_bytes() {
    let state = test_state();
    let base = test_dir("v-long");
    // 1000 bytes that resolve to `base` itself.
    let hops = "./".repeat(500);
    let at_cap = format!("{hops}{}", "f".repeat(24));
    let over_cap = format!("{hops}{}", "g".repeat(25));
    assert_eq!((at_cap.len(), over_cap.len()), (1024, 1025));
    std::fs::write(base.join("f".repeat(24)), "x").unwrap();
    std::fs::write(base.join("g".repeat(25)), "x").unwrap();
    // A shorter hop-built candidate proves the fixture resolves, so the
    // over-cap miss below is the cap's doing.
    let long = format!("{}{}", "./".repeat(100), "f".repeat(24));
    // The boundary itself is provable only where the OS takes
    // `base/<1024 bytes>` as a syscall argument: Linux (CI) does; macOS
    // answers ENAMETOOLONG whatever the path resolves to.
    let boundary_provable = match std::fs::metadata(base.join(&at_cap)) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::InvalidFilename => false,
        Err(e) => panic!("{e}"),
    };

    let answer = validate(
        &state,
        serde_json::json!({
            "candidates": [long.clone(), at_cap.clone(), over_cap.clone()],
            "base": base.to_string_lossy(),
        }),
    )
    .await;
    let valid = answer["valid"].as_object().unwrap();
    assert!(valid.contains_key(&long));
    assert_eq!(valid.contains_key(&at_cap), boundary_provable);
    assert!(!valid.contains_key(&over_cap));
}
