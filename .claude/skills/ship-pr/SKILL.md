---
name: ship-pr
description: Open a pull request for Chimaera correctly — the CI gates that must pass, the Conventional-Commit prefix that drives the auto version bump on merge, and the [skip release] marker for docs/chore PRs that shouldn't ship a version. Use when creating a PR, choosing a commit/PR title, or deciding whether a change should cut a release.
---

# Shipping a PR on Chimaera

Merges to `main` use an automated release decision over every unreleased merge:
shipping prefixes request a release, while docs/chore/refactor-only prefixes do
not request one. A docs merge can still trigger an earlier pending release. The PR *title* and
commit prefix are therefore load-bearing. See [AGENTS.md](../../../AGENTS.md) →
"Releases" for the full rules.

## Before opening

1. **Rebase on latest main.** `git fetch origin && git rebase origin/main`. (Once
   the branch is pushed, merge `origin/main` in instead — the Claude guard hook
   blocks force-pushes to the canonical repo, PR branches included.)
2. **Applicable gates are green:** for Rust changes, build `web-ui/dist` first on
   a fresh checkout, then `just check` (plugin assets, fmt, clippy, and both Rust
   workspaces' tests). If you touched `web-ui/**`, run its `check`, `test`, and
   `build` scripts before Rust checks. If you touched
   `crates/chimaera-app/**`, run `just app-check`; `app.yml` also builds the
   Tauri bundle on the PR.
3. **Runtime changes verified live**, not just tested (see the **verify-app**
   skill). Pure documentation changes need the doc-link and agent-asset checks;
   public-site changes also need light/dark visual verification. The PR body
   should say what you ran and observed.
4. **Shipping a `feat:`? It carries its docs.** A new user-facing capability must update
   its [feature-catalog](../../../docs/features/README.md) page — the **document-feature**
   skill — and capture the human's *why* via the **capture-feature-intent** skill. Only
   `feat:` triggers the intent questionnaire (never `fix:`/`refactor:`/`chore:`/`docs:`);
   "feature" is defined once, in [`scripts/version-bump.sh`](../../../scripts/version-bump.sh).

## Choose the title deliberately — it becomes the squash commit

On squash-merge the commit subject defaults to the **PR title**, and
`scripts/version-bump.sh` reads that **subject** (never the body) to decide the
bump. Full rules + rationale: [docs/agent-guides/releases.md](../../../docs/agent-guides/releases.md).

| PR title starts with | Result on merge |
|---|---|
| `feat:` | **minor** — a genuinely new user-facing capability |
| `fix:` / `perf:` / `revert:` | **patch** |
| conventional type with `!:` (e.g. `feat!:`), or `BREAKING CHANGE` / `BREAKING-CHANGE` in the subject | **major** |
| `refactor:` / `chore:` / `docs:` / `test:` / `ci:` / `build:` / `style:` | **no release requested** by this merge |
| anything else / no prefix | **patch** (safe default) |

`feat` is reserved for new capability — mislabeling a fix or refactor as `feat` is
what makes the minor version run away. Since `refactor:`/`chore:`/`docs:` now cut
**no release request**, you rarely need `[skip release]` for those.

## Landing without a release

Two ways, both read from the **subject** (= PR title):

- **Use a no-release type** — `refactor:` / `chore:` / `docs:` / `test:` / `ci:` /
  `build:` / `style:`. These request no new version. Prefer this for docs,
  chores, tooling, CI tweaks, and pure refactors.
- **Add `[skip release]` to the PR title** when a normally-releasing type shouldn't
  ship yet, e.g. `feat: experimental thing [skip release]`.

Both are **subject-anchored**: a mention in the PR *body* no longer skips or flips a
release (the old gate matched the whole folded message). So you can safely describe
`[skip release]` or dangerous commands in the body. If the title was edited at merge
time, verify the type/marker survived into the squash subject.

## Open it

```sh
git push -u origin HEAD            # push the branch
gh pr create --title "chore: <what>" --body-file /absolute/path/to/pr-body.md
```

Write the PR description to that file with actual newlines. When the desktop
task exposes `attach_artifact`, attach the created PR URL to the task.

End the PR body with an accurate agent trailer. For Codex:

```
🤖 Generated with [Codex](https://openai.com/codex/)
```

Claude Code uses its corresponding Claude Code trailer instead.

**Landing it.** With squash auto-merge on, you don't merge `origin/main` in by
hand just to catch up: `main` requires branches to be up to date, and
`.github/workflows/pr-auto-update.yml` brings the oldest pull request with
auto-merge that fell behind up to date after each merge, one at a time, so it
lands on its own. Merge `origin/main` in yourself only to resolve a conflict (the
PR says it has conflicts), which the workflow can't. A PR whose required checks
fail is skipped until it's fixed.

## After merge

Watch that the intended workflow ran: for a normal PR, `release.yml` should
publish a release with the bumped version; for a no-release type or
`[skip release]` PR, the
`version` job should report `release=false`, all build/publish jobs should be
skipped, and no release should be cut — unless an earlier merge since the last
tag still wants one: the decision reads every merge since that tag, so a
`[skip release]` or `docs:` merge then ships the pending release. If it cut one
nothing asked for, the marker didn't make it into the squash message.
