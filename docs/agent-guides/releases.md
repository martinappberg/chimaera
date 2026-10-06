# Releases & versioning

The single source of truth for how Chimaera versions and ships. The root
[AGENTS.md](../../AGENTS.md), the [ship-pr skill](../../.claude/skills/ship-pr/SKILL.md),
[.github/CONTRIBUTING.md](../../.github/CONTRIBUTING.md), and `.github/workflows/release.yml` all
point here — change the policy here (and in the script + its test), not by editing
five copies.

## Releases ship in batches, not per merge

A merge to `main` publishes nothing. `release.yml` runs once a day (its `schedule`)
and whenever a maintainer runs it by hand. Each run:

1. picks the newest `main` commit whose `ci.yml` push run **succeeded**, which is
   not necessarily `main`'s tip, since the tip may still be under test or red;
2. reads the squash-commit **subjects** (each defaults to its PR title) of **every
   merge since the last `v*` tag** up to that commit, and `scripts/version-bump.sh`
   decides the next version or `skip`; the largest bump any of them asks for wins;
3. builds and publishes only when a release is actually due; a `skip` sets
   `release=false` and the build + publish jobs are gated off.

Why batched: every installed app auto-updates to each release, and a release per
merge meant up to six updates a day, each a 40–95 minute build. Merging stays cheap;
users get at most one scheduled update a day.

Why the newest *green* commit and not the tip: a PR's checks ran on its branch, not
necessarily on the tree its squash-merge produced, so `main`'s own CI run is the gate.
A red `main` holds releases at the last green commit until a fix lands and passes.
One exception: a run's steps come from `release.yml` at the commit it ran on, so
when that file differs at the green commit, the run skips with a warning rather
than build a tree with steps written for another; a run whose copy agrees ships
the batch.

**Ship now** (a hotfix, or anything that shouldn't wait for the schedule): once the
merge's `ci.yml` run on `main` has passed, run

```sh
gh workflow run release.yml --ref main
```

Run it sooner and it releases the newest commit that *has* passed, which leaves the
fix for the next run. The run's `version` job logs the merges it read and its
decision.

## The version mapping (read from the SUBJECT)

| Subject prefix | Result | When to use it |
|---|---|---|
| `feat:` | **minor** (0.3.2 → 0.4.0) | a genuinely new user-facing capability |
| `fix:` · `perf:` · `revert:` | **patch** (0.3.2 → 0.3.3) | a small change that ships |
| conventional type with `!:` (e.g. `feat!:`), or `BREAKING CHANGE` / `BREAKING-CHANGE` in the subject | **major** (0.3.2 → 1.0.0) | a breaking change |
| `refactor:` · `chore:` · `docs:` · `test:` · `ci:` · `build:` · `style:` | **no release requested** | contributes no bump; CI still checks the merge |
| `[skip release]` in the subject | **no release** for that merge | explicit opt-out (wins over its own type; its code still ships with the next release) |
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

The decision reads only **subjects** (`git log --first-parent --format=%s <tag>..HEAD`),
never a body.
On squash-merge GitHub folds the whole PR description into the commit body, so
reading the type / `!` / `[skip release]` from the entire message would let a stray
body line flip the bump or skip a release. Put the load-bearing bits in the **PR
title**. Use `!` in the subject for a breaking change (`feat!:`). The script also
recognizes `BREAKING CHANGE` or `BREAKING-CHANGE` in the subject; a footer in the
body is deliberately not honored, for the same reason.

## Skipping a release

Put `[skip release]` in the **PR title** (it becomes the squash subject), or simply
use a no-release type (`refactor:` / `chore:` / `docs:` / …). Both request no
release for that merge; CI still checks it. An earlier unreleased merge can still
request a release in the same run.

## The logic is tested (change the two together)

`scripts/version-bump.sh` is a pure function of `(latest-tag, subjects since it)`. It is pinned
by `scripts/version-bump.test.sh` (a characterization matrix) which runs in
`ci.yml`'s `scripts` job. When you change the policy, change the script **and** the
test in the same commit.
