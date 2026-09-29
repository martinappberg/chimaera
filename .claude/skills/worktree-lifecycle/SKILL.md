---
name: worktree-lifecycle
description: Keep Chimaera's git worktrees and their Rust build output from filling the disk — when to remove your worktree, how to run scripts/worktree-gc (dry run, --apply, --self), what the SessionStart/SessionEnd hooks do, and the hard rule never to touch an ACTIVE worktree. Use when your PR has merged, when disk space is low or a session-start hook flagged it, before a big cargo build (just check, cargo test --workspace, an app build), or when asked to clean up worktrees or target/ dirs.
---

# Worktree lifecycle

Every agent session works in its own git worktree: Claude Code under
`.claude/worktrees/<name>`, the Codex app under `~/.codex/worktrees/<id>/chimaera`.
Each one grows its own cargo `target/` (the daemon workspace, plus
`crates/chimaera-app/target` and `plugins/target`), and single worktrees have reached
35–60 GB. On macOS most of that was stale debug objects: a dev build keeps each
executable's object files in `target/debug/deps` for the debugger, and every
incremental rebuild writes new ones without deleting the old. Left alone, worktrees
filled the maintainer's disk to 99%.

`scripts/worktree-gc` sorts every worktree of the repo (`git worktree list`) into one
group and prints why:

| Group | Meaning | What `--apply` does |
|---|---|---|
| **ACTIVE** | a process has its cwd or an open file inside it, it is `git worktree lock`ed, the script runs from it, or something in it changed in the last 6 h (unless its PR is merged — see REMOVE) | nothing, ever |
| **REMOVE** | PR merged or closed (or HEAD already in `origin/main`), no uncommitted changes, no unpushed commits. A merged PR skips the 6 h wait when the checkout *is* that PR (HEAD is its head, or its own commits all sit behind it) | `git worktree remove` (never `--force`) + `git worktree prune`; the branch is kept |
| **TRIM** | not removable, idle for more than 24 h | deletes its git-ignored `target/` and `node_modules/` dirs; source is never touched |
| **KEEP** | everything else (an open PR idle < 24 h, unpushed commits, uncommitted changes) | nothing |

It judges a worktree only by its branch, that branch's PR (`gh`), git state and live
processes (`lsof`, or `/proc`) — **never by its folder name**. Both apps reuse folders
for unrelated sessions, so `.claude/worktrees/chimera-pro-cloud-…` may hold a
branch about something else entirely.

Why merged skips the wait: merged, clean and pushed leaves nothing in progress, and
archiving a session in the Claude app detaches its branch — a fresh `HEAD` and
reflog write that otherwise reads as "changed 0m ago" and holds the folder for 6 h.
A detached worktree finds its PR by commit, or through a local branch still pointing
at its HEAD (the PR's head may have moved on since the checkout last pushed). A
closed PR (it may be reopened) and a fresh worktree branched from main still wait.
The cost: a merged session you return to after its folder went has no worktree.

## The lifecycle

1. **Before a big build**, check `df -h ~`. Under ~50 GiB free (the session-start
   hook says so when it is), run `scripts/worktree-gc` and read the table. Its
   `--apply` only ever acts on REMOVE and TRIM, which the hooks already apply
   automatically, so running it is safe; nothing else is yours to delete.
2. **After your PR merges**, your worktree should go. A session can't remove the
   worktree it runs in (it is ACTIVE: "this session runs here"), so:
   - drop its build output now with `scripts/worktree-gc --self --apply`;
   - end the session. The next gc run (any session's start, at most hourly) removes
     the worktree once nothing uses it — merged, clean and pushed is REMOVE — unless
     automatic cleanup is off on that machine (`worktree-gc.auto`, below); then say
     so and leave the removal to the human.
   - Working from another checkout? `git worktree remove <path>` — never `--force`,
     and never on a worktree the table calls ACTIVE.
3. **At session end** the SessionEnd hook runs `--self --sweep`: it deletes only the
   stale debug objects no binary in `target/` references, so the next build stays warm.
   `--self` without `--sweep` deletes this worktree's whole `target/` (the next build
   starts cold) and refuses while anything runs from it.

## Never

- touch an ACTIVE worktree: no `rm -rf` of its `target/`, no `git worktree remove`,
  no unlocking a lock you didn't take. Codex app-servers keep their cwd in a worktree
  for hours, and `chimaera-dev.app`, `chimaera serve` and test binaries run straight
  out of `target/debug`.
- `git worktree remove --force`, delete branches, or clean another worktree by hand —
  run the script, which re-checks each worktree right before acting.
- trust `du` for what a delete frees. Cargo hardlinks objects between `deps/` and
  `incremental/`, so `du` of the parts overcounts. `--apply` reports the change in
  `df`'s free space instead.
- keep anything you need in a worktree's git-ignored files: REMOVE deletes the whole
  folder, and only committed-and-pushed work survives it.

## Commands

```sh
scripts/worktree-gc                    # dry run: the table + what --apply would do
scripts/worktree-gc --markdown         # the same, as a markdown table (PR bodies)
scripts/worktree-gc --apply            # remove REMOVE, trim TRIM; reports df before/after
scripts/worktree-gc --self [--sweep] [--apply]   # this worktree's own target/
bash scripts/worktree-gc.test.sh       # the safety rules, against a throwaway repo
```

Knobs: `git config worktree-gc.auto false` stops the hooks from deleting anything
(they still flag low disk; `git config --get worktree-gc.auto` shows whether a
machine opted out), `WORKTREE_GC=off` silences them entirely, and
`WORKTREE_GC_ACTIVE_HOURS` (6), `WORKTREE_GC_IDLE_HOURS` (24), `WORKTREE_GC_LOW_GB`
(50), `WORKTREE_GC_INTERVAL_MIN` (60) tune them. It acts only on the repo it lives
in, whatever the cwd. It fails safe: without `lsof`/`/proc`, or when it can't read a
worktree's files, that worktree is ACTIVE; without `gh` nothing is removable by PR
state.

## Hooks

- **Claude Code** (`.claude/settings.json`): SessionStart runs `--hook session-start`
  — prints one line into context when disk is low, and starts a detached
  `--apply` run at most once an hour. SessionEnd runs `--hook session-end` — a
  detached `--self --sweep --apply` (skipped on `/clear`).
- **Codex** (`.codex/hooks.json`): the same two hooks, in Codex's documented format.
  Codex runs project hooks only in a trusted project, after each is approved in
  `/hooks` — and it records that approval per `hooks.json` path, so each Codex worktree
  may ask again. `codex exec` 0.157.1 ran no project SessionStart hook at all in
  testing. So for Codex, treat this skill and the AGENTS.md rule as the mechanism
  and the hooks as a bonus.
- Background runs log to `<main checkout>/.git/worktree-gc/log`.

## Why the build is smaller now

The dev profile (root `Cargo.toml`, mirrored in `crates/chimaera-app/Cargo.toml`)
builds workspace crates with `debug = "line-tables-only"` and dependencies with
`debug = false`: backtraces and debugger breakpoints keep file:line, but locals are
not inspectable. When you need them, rebuild with full debug info:
`CARGO_PROFILE_DEV_DEBUG=full cargo build`.

On macOS, `export CARGO_PROFILE_DEV_SPLIT_DEBUGINFO=packed` in your own shell stops
the stale-object pile-up at the source (dsymutil writes a `.dSYM` per binary and the
objects are deleted). It is not the repo default: the profile key would change Linux
builds too, and a macOS-only rustflag would silently disable any `build.rustflags`
you set yourself.
