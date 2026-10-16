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
merge's `ci.yml` run on `main` has passed (or, if a merge right behind it replaced
that run while it waited to start, the later merge's run), run

```sh
gh workflow run release.yml --ref main
```

Run it sooner and it releases the newest commit that *has* passed, which leaves the
fix for the next run. The run's `version` job logs the merges it read and its
decision.

## Linux runner migration

`ci.yml` and the four release jobs formerly using `ubuntu-latest` are temporarily
pinned to `ubuntu-24.04`. GitHub's [runner migration notice](https://github.com/actions/runner-images/issues/14748)
sets the Ubuntu 26.04 rollout window to October 19–November 19, 2026. This is a
validation hold, not a confirmed Ubuntu 26 incompatibility. An OS label still
receives image updates; it does not freeze every installed package.

The October 15 [successful CI run](https://github.com/martinappberg/chimaera/actions/runs/38783104803)
used Ubuntu 24.04 image `20261012.334.1` and Zig `0.17.0` for both musl targets.
The workflow and toolchain files inspected locally matched remote `main` during
the audit. Both workflows now explicitly select that Zig version: the pinned
[`setup-zig` action](https://github.com/mlugg/setup-zig/blob/d1434d08867e3ee9daa34448df10607b98908d29/action.yml)
otherwise falls back to the latest release when there is no `build.zig.zon`.
Keep these two Zig pins aligned. `cargo-zigbuild` is now at `0.23.4` with
`--locked`, Rust at `1.99.1` via `rust-toolchain.toml`, and Node at `26.12.0` via
`.nvmrc` and `setup-node`.

The exposure is the native host environment: C compiler/linker, glibc, shell
utilities, certificates, and downloaded tools running on that host. The published
[Ubuntu 24 image](https://github.com/actions/runner-images/blob/ubuntu24/20261012.334/images/ubuntu/Ubuntu2404-Readme.md)
and [Ubuntu 26 image](https://github.com/actions/runner-images/blob/ubuntu26/20260927.149/images/ubuntu/Ubuntu2604-Readme.md)
have different GCC/Clang, CMake, Bash, jq, curl, and OpenSSL versions. Root-workspace
native dependencies include `ring` and `libmimalloc-sys` through `cc`; it uses
rustls/ring rather than `openssl-sys` or `aws-lc-sys`, so the image's OpenSSL/CMake
changes alone do not establish a daemon build failure. `gh`, jq, rustup, and Node
22 remain available in the published Ubuntu 26 inventory. No affected command
has been shown to fail on Ubuntu 26 in this audit.

The Linux Tauri app in both `app.yml` and `release.yml` already builds on
`ubuntu-22.04`. Its apt prerequisites (`libwebkit2gtk-4.1-dev`, `libxdo-dev`,
`libssl-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`, and build utilities)
are therefore outside this label migration. Changing that runner also changes
the AppImage's glibc floor and needs a separate compatibility decision. The
static musl daemon builds do not inherit the host's glibc floor in the same way.

Before lifting the temporary pin, use a validation PR with explicit
`ubuntu-26.04` runners and retain the run URLs and image versions:

1. Run every `ci.yml` job, including dependency review on a PR event, UI
   install/check/test/build, script tests, the Rust/plugin checks, and both musl
   targets. Keep Node, Rust, Zig, and cargo-zigbuild unchanged to isolate the OS.
2. Test once without restored build caches, then with caches created on Ubuntu
   26. The pinned [Rust cache action](https://github.com/Swatinem/rust-cache/blob/6323deb102c322ba6fcbdcafc7e3dddab59af2b6/src/config.ts)
   explicitly keys by OS family/architecture, not Ubuntu release or Zig version;
   other hashed inputs may differ, but do not rely on that for isolation. Give
   the validation caches a distinct `prefix-key`, include the Ubuntu label in
   the eventual cache namespace, and separate musl targets and Zig versions.
   Isolate or disable `setup-zig`'s build cache too.
3. Exercise the release daemon build steps in a build-only validation job with
   read-only permissions and no publishing/signing steps. Remove CI's
   `CARGO_PROFILE_RELEASE_LTO` and `CARGO_PROFILE_RELEASE_CODEGEN_UNITS` overrides
   so this also tests the release profile's fat LTO and single codegen unit.
   Check both binaries with `file` and `readelf`: correct architecture, no ELF
   interpreter, and no dynamic `NEEDED` libraries. Run `--version` and an
   isolated daemon smoke on x86_64 and an actual arm64 runner (or documented
   emulation), including an older supported Linux environment.
4. Check release coordination separately: `gh run list` and release reads with
   a read-only token, `scripts/version-bump.test.sh`, and the plugin-pins tests.
   Exercise artifact upload/download in the validation workflow. A successful
   scheduled release may have skipped every build because no release was due;
   it is not build evidence. Dispatching `release.yml` from a feature branch
   also skips its `version` job and cannot validate these paths. Validate actual
   release writes only in a disposable repository, never by removing main's
   publishing guard.

After those checks pass, move the pinned Ubuntu 24 job definitions in CI/release to
explicit `ubuntu-26.04` together and record the evidence here. Review this hold
before November 19, 2026; the date alone is not a reason to lift it. Local
`actionlint`, workflow-security, and documentation checks validate the patch,
but cannot establish hosted-runner compatibility.

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

## CI build reuse

CI's `ui-build` job produces the embedded web assets before the separate `ui`
validation job. Rust checks and both musl builds can then run alongside the UI's
check/test steps. `ui` explicitly fails if the asset build failed, preserving
its required-check behavior. Main's complete CI run remains the release gate.

Release builds produce the UI once at `needs.version.outputs.sha`, then download
that artifact into each Unix build. Tauri's release steps disable only the
redundant frontend build command; signing, plugin smoke, and publishing gates
remain intact. Daemon attachment downloads only `chimaera-*` artifacts.

Rust CI caches include the plugin workspace. Musl Rust and Zig caches separate
Ubuntu image families and target architectures; Rust caches also name the Zig
version. Keep CI and release pins in sync when upgrading. Node uses the exact
`.nvmrc` version, and Rust uses the minimal profile with explicit components and
targets in `rust-toolchain.toml`. App PR checks also run for shared Rust crates
and toolchain changes, so a compiler upgrade gets all three desktop build gates.

TypeScript stays on 6 while `svelte-check`'s peer dependency excludes 7; update
that constraint only once the checker supports it. The frozen plugin API 0.1
keeps its existing guest bindings; the current API uses the latest bindings.

Marp remains on its stable v4 line (v5 is still a release candidate). Its KaTeX
dependency is overridden to the UI's current version to address
[GHSA-238p-pmpm-9mq7](https://github.com/advisories/GHSA-238p-pmpm-9mq7).
Keep slide rendering and untrusted-math checks in the upgrade validation; remove
the override once Marp's stable dependency range includes the patched version.

The October 15, 2026 dependency refresh uses Rust 1.99.1 and Node 26.12.0
(Node's Current line, not LTS). Registry checks covered every direct Rust
dependency and both npm manifests. `npm audit` reports zero vulnerabilities
for both npm projects. `cargo audit` reports no vulnerability-class findings
in the daemon, plugins, or app lockfiles; the app still has two informational
warnings through Tauri 2.12.4's Linux GTK3 stack:

- `glib 0.18.5`: [RUSTSEC-2024-0429](https://rustsec.org/advisories/RUSTSEC-2024-0429.html),
  an unsound `VariantStrIter`, fixed in GLib bindings 0.20 and later.
- `proc-macro-error 1.0.4`: [RUSTSEC-2024-0370](https://rustsec.org/advisories/RUSTSEC-2024-0370.html),
  unmaintained, pulled in by `glib-macros` and `gtk3-macros`.

These are existing upstream constraints, not Ubuntu 26 failures. Recheck them
when Tauri updates its GTK dependency chain; forcing GLib 0.20 into dependencies
requiring 0.18 is not a compatible lockfile update. No advisory is suppressed.
