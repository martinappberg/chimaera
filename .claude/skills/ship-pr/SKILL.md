---
name: ship-pr
description: Open a pull request for Chimaera correctly — the CI gates that must pass, the Conventional-Commit prefix that drives the auto version bump on merge, and the [skip release] marker for docs/chore PRs that shouldn't ship a version. Use when creating a PR, choosing a commit/PR title, or deciding whether a change should cut a release.
---

# Shipping a PR on Chimaera

Merges to `main` are automated: **every merge cuts a published release** unless
you opt out. So the PR *title* and *commit prefix* are load-bearing. See
[CLAUDE.md](../../../CLAUDE.md) → "Releases & how to skip one" for the full rules.

## Before opening

1. **Rebase on latest main.** The remote is `upstream`:
   `git fetch upstream && git rebase upstream/main`.
2. **Gate is green:** `just check` (fmt + clippy + test). If you touched
   `web-ui/**` or `crates/chimaera-app/**`, `app.yml` will also build the Tauri
   bundle on the PR — make sure the UI builds (`npm --prefix web-ui run check`).
3. **Verified live**, not just tested (see the **verify-app** skill). The PR body
   should say what you ran and observed.

## Choose the title deliberately — it becomes the squash commit

On squash-merge the commit subject defaults to the **PR title**, and
`release.yml` reads that subject to decide the version bump:

| PR title starts with | Version bump on merge |
|---|---|
| `feat: ...` | **minor** (0.1.0 → 0.2.0) |
| `fix:` / `chore:` / `refactor:` / etc. | **patch** (0.1.0 → 0.1.1) |
| anything with `BREAKING CHANGE` or `!:` | **major** (0.1.0 → 1.0.0) |

So a user-facing feature must be `feat:` to release as a minor; don't mislabel.

## Landing without a release — `[skip release]`

Docs, chores, tooling, CI tweaks — anything that shouldn't ship a new version:
put **`[skip release]`** in the **PR title** (and optionally the body). The
release workflow's `version` job is gated on
`!contains(head_commit.message, '[skip release]')`, so the entire release is
skipped. Example title:

```
docs: add CLAUDE.md and dev skills [skip release]
```

Because the squash subject = PR title by default, tagging the title is the
reliable way to make it stick — verify the marker survives into the squash
message at merge time if the title was edited.

## Open it

```sh
git push -u upstream HEAD          # push the branch
gh pr create --title "docs: <what> [skip release]" --body "<what changed; what you ran/observed>"
```

End the PR body with the standard trailer:

```
🤖 Generated with [Claude Code](https://claude.com/claude-code)
```

## After merge

Watch that the intended workflow ran: for a normal PR, `release.yml` should
publish a release with the bumped version; for a `[skip release]` PR, the
`version` job should be **skipped** and no release cut. If it cut one you didn't
intend, the marker didn't make it into the squash message.
