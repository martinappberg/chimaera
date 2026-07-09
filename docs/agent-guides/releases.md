# Releases & versioning

The single source of truth for how Chimaera versions and ships. The root
[CLAUDE.md](../../CLAUDE.md), the [ship-pr skill](../../.claude/skills/ship-pr/SKILL.md),
[CONTRIBUTING.md](../../CONTRIBUTING.md), and `.github/workflows/release.yml` all
point here — change the policy here (and in the script + its test), not by editing
five copies.

## Every merge to `main` MAY cut a release

`release.yml` runs on every push to `main`. `scripts/version-bump.sh` decides the
next version — or `skip` — from the squash-commit **subject** (which defaults to the
PR title). The build + publish jobs run only when a release is actually due; a
`skip` sets `release=false` and they're gated off.

## The version mapping (read from the SUBJECT)

| Subject prefix | Result | When to use it |
|---|---|---|
| `feat:` | **minor** (0.3.2 → 0.4.0) | a genuinely new user-facing capability |
| `fix:` · `perf:` · `revert:` | **patch** (0.3.2 → 0.3.3) | a small change that ships |
| any `!:` (e.g. `feat!:`) | **major** (0.3.2 → 1.0.0) | a breaking change |
| `refactor:` · `chore:` · `docs:` · `test:` · `ci:` · `build:` · `style:` | **no release** | rebuilds, but ships no new version |
| `[skip release]` in the subject | **no release** | explicit opt-out (wins over everything) |
| anything else / no prefix | **patch** | safe default — never a silent skip |

`feat` is reserved for new capability. Don't label a fix, a refactor, or a chore as
`feat` — that's how the minor version runs away (the drift this policy fixes).

**A `feat:` carries its docs.** Because `feat:` *is* the definition of "new user-facing
capability" (this table is the single place that defines it), a `feat:` PR must also:
update the capability's [feature-catalog](../features/README.md) page (the
[document-feature](../../.claude/skills/document-feature/SKILL.md) skill), and record the
human's *why* via the [capture-feature-intent](../../.claude/skills/capture-feature-intent/SKILL.md)
skill. `fix:` / `refactor:` / `chore:` / `docs:` never trigger the intent questionnaire — that
gate is what keeps the Intent sections free of patch-level noise. The
[ship-pr](../../.claude/skills/ship-pr/SKILL.md) flow checks for the doc update.

## Subject-anchored, on purpose

The decision reads only the **subject** (`git log -1 --pretty=%s`), never the body.
On squash-merge GitHub folds the whole PR description into the commit body, so
reading the type / `!` / `[skip release]` from the entire message would let a stray
body line flip the bump or skip a release. Put the load-bearing bits in the **PR
title**. A breaking change must carry `!` in the subject (`feat!:`) — a
`BREAKING CHANGE` footer in the body is deliberately not honored, for the same reason.

## Skipping a release

Put `[skip release]` in the **PR title** (it becomes the squash subject), or simply
use a no-release type (`refactor:` / `chore:` / `docs:` / …). Both stop the release
without touching the build.

## The logic is tested (change the two together)

`scripts/version-bump.sh` is a pure function of `(latest-tag, subject)`. It is pinned
by `scripts/version-bump.test.sh` (a characterization matrix) which runs in
`ci.yml`'s `scripts` job. When you change the policy, change the script **and** the
test in the same commit.
