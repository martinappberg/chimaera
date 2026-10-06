---
name: ship-pr
description: Open a pull request for Chimaera correctly — the CI gates that must pass, the Conventional-Commit prefix that drives the automatic version bump, and the [skip release] marker for docs/chore PRs that shouldn't ship a version. Use when creating a PR, choosing a commit/PR title, or deciding whether a change should cut a release.
---

# Shipping a PR on Chimaera

A merge to `main` publishes nothing by itself. Releases ship in daily batches:
`release.yml` reads every merge since the last release, and shipping prefixes
request a release while docs/chore/refactor-only prefixes do not. The PR *title*
and commit prefix are therefore load-bearing. See
[docs/agent-guides/releases.md](../../../docs/agent-guides/releases.md) for the
full rules.

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

| PR title starts with | What the merge requests of the next release |
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

**Landing it.** `main` doesn't require branches to be up to date, so a PR with
squash auto-merge on lands as soon as its required checks (`ui`, `rust`, `cla`)
pass, however far behind it is. Merge `origin/main` in yourself only to resolve a
conflict (the PR says it has conflicts). The merged tree is tested by `main`'s own
CI run, and a release only ships a commit that run passed on, so a merge that
breaks `main` holds releases back until a fix lands; fix forward. That run needs
every `ci.yml` job green, not just the required ones: auto-merge doesn't wait
for `scripts` or `musl`, and a PR that lands with either red breaks `main` the
same way, so watch them pass too.

## After merge

Stop the previews you started for the PR and clean up its worktree using the
[worktree-lifecycle workflow](../worktree-lifecycle/SKILL.md). This is part of
completing the merge: stop owned previews gracefully, release build output,
and remove the idle checkout from another checkout (or archive a managed Codex
worktree). If another session still uses it, leave it intact and report why.

Nothing publishes on merge. The next scheduled `release.yml` run ships the
merge with everything else merged since the last release, once `main`'s CI has
passed on it. A fix that shouldn't wait: after its `ci.yml` run on `main` passes
(or, when a merge right behind it replaced that run before it started, the later
merge's run), `gh workflow run release.yml --ref main`. The run's `version` job
lists the merges it read and the version it chose; a no-release type or
`[skip release]` merge
contributes no bump, so a batch of only those reports `release=false` and cuts
nothing. If a release came out that nothing asked for, a marker didn't make it
into a squash message.
