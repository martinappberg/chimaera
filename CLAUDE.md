# CLAUDE.md — the index

Fast orientation for coding agents. This file is a lean **index**: what Chimaera
is, how to run and check it, the handful of conventions you can't infer from the
code, and pointers into the deep docs. Read the pointer for what you're touching
rather than front-loading everything.

> **Docs drift — verify before you trust.** Treat every path, command, and claim
> here as possibly stale: confirm it against the actual repo before relying on it,
> and when you find a doc wrong, **fix it in the same change**. That "verify →
> trust → update" loop is what keeps this orientation trustworthy instead of
> rotting. A `Stop` hook nudges you when an area's code changed without its map/
> DESIGN being touched.

## What Chimaera is

An **agent workbench, not an IDE**. One static Rust binary (`chimaera`) is the
whole server: it owns your agent sessions as persistent, daemon-owned PTY
processes on whatever host owns the work — laptop, dev server, or HPC login node —
and serves a workspace-first Svelte web UI around them (file previews, terminals,
git state). Agents run as the real interactive TUIs (`claude`, `codex`) in
daemon-owned PTYs, so they look, behave, and bill like any terminal; a structured
**chat mode** drives the same agents over their JSON protocols. Close the laptop
mid-run and nothing dies — windows are just views onto the daemon. A native Tauri
app wraps the same UI in real windows.

Lowercase **chimaera** for the binary/product/dock label; "Chimaera" as the
capitalized proper noun in prose.

## Where things are (and what to read next)

A Rust workspace (the daemon) + a Svelte 5 UI it embeds + a separate Tauri app.
Each area carries its own `CLAUDE.md` map (file table + the invariants that bite)
— read the most specific one; it wins over this index on local detail.

| Area | What it is | Map |
|---|---|---|
| `crates/chimaera` | the binary: CLI + daemon entrypoint (delegation-only) | [map](crates/chimaera/CLAUDE.md) |
| `crates/chimaera-core` | shared types, version/build-id, shell integration | [map](crates/chimaera-core/CLAUDE.md) |
| `crates/chimaera-pty` | the persistent PTY / terminal engine | [map](crates/chimaera-pty/CLAUDE.md) |
| `crates/chimaera-agent` | the structured-agent engine (drivers, journal) | [map](crates/chimaera-agent/CLAUDE.md) · [PROTOCOL](crates/chimaera-agent/PROTOCOL.md) |
| `crates/chimaera-remote` | SSH orchestration for `connect` (thorough in-code docs) | — |
| `crates/chimaera-server` | the daemon: every route + WS + business logic; embeds `web-ui/dist` | [map](crates/chimaera-server/CLAUDE.md) |
| `crates/chimaera-app` | the Tauri 2 native shell (its own standalone workspace) | [map](crates/chimaera-app/CLAUDE.md) |
| `web-ui/` | the Svelte 5 client the daemon serves | [chat](web-ui/src/lib/chat/CLAUDE.md) · [settings](web-ui/src/lib/settings/CLAUDE.md) |

Deep docs, read on demand: the **[architecture guide](docs/agent-guides/architecture.md)**
(the source of truth for how it's built and why), the design spine
[DESIGN.md](DESIGN.md), and the dated [field notes](docs/history/field-notes.md).
Area "rules that bite" auto-load from `.claude/rules/` when you open matching files.

## Run it

Node 22 (`.nvmrc`; the nvm default 16 errors). The Rust toolchain is pinned in
`rust-toolchain.toml` — **format with `cargo +1.96.0 fmt`** (a differing default
`cargo fmt` can pass locally yet fail CI's pinned check).

```sh
just check                         # fmt --check + clippy -D warnings + test (pinned toolchain)
npm --prefix web-ui run check      # svelte-check — the UI has no other automated tests
npm --prefix web-ui run build      # emits web-ui/dist, which the daemon embeds (rust-embed)
node scripts/check-doc-links.mjs   # every relative markdown link + #anchor resolves
```

**Isolated preview — use this in a worktree.** A debug daemon on its own state dir
+ an auto-assigned port, so parallel worktrees don't clobber each other's
`~/.chimaera`: `preview_start` the **chimaerad-isolated** config, read the
`#token=` URL from `preview_logs`, open it. A debug daemon reads `web-ui/dist` from
disk, so after a UI change just rebuild the UI and reload — no daemon restart. Full
loop + gotchas: the **[develop](.claude/skills/develop/SKILL.md)** skill.

## Conventions you can't infer from the code

- **Verify live, don't just unit-test.** Terminal state, reconnect, resize, and
  agent integrations have all had bugs that pass tests but break against the real
  thing. Drive the flow (the **[verify-app](.claude/skills/verify-app/SKILL.md)**
  skill); the PR says what you ran and observed. The web UI has **no JS tests** —
  the live preview is its only runtime net.
- **The daemon runs on shared HPC login nodes.** ~150 MB RSS, no unbounded buffers,
  no busy loops, hard preview ceilings. **No SQLite near NFS/Lustre**; durable state
  is append-only, size-capped JSONL under `~/.chimaera`; hot state is reconstructible.
- **The daemon↔UI wire is a stable public interface.** Core structs serialize
  straight to it — don't let its shape drift as a side effect of a refactor.
- **Agent wire formats are pinned, not trusted** — a driver or agent-CLI change
  needs `just chat-smoke` (live, bills a few cents). **Terminal state is
  server-side** (never serialize the `alacritty` `Term` grid). **UI quality is an
  acceptance criterion** (curated light/dark, the brand mark, a real workbench feel).
- **Comments state constraints and *why*, not narration.** `cargo +1.96.0 fmt` and
  `clippy -D warnings` must pass clean.
- Area-specific constraints live in `.claude/rules/*.md` (auto-load with matching
  files); the nested `CLAUDE.md` maps carry the depth. When you add a substantial
  subsystem, add its map in the same style.

## Releases

CI + releases are automatic. `ci.yml` (fmt/clippy/test + UI + musl cross-builds)
gates every PR; `app.yml` build-checks the Tauri bundle when the app or UI change;
**every merge to `main` cuts a published GitHub release** whose version the
squash-commit prefix decides. Get the prefix right — or add `[skip release]` — via
the **[ship-pr](.claude/skills/ship-pr/SKILL.md)** skill, which owns the exact
version mapping and the no-release path.

## PR checklist

1. `just check` green (+ `app.yml` if you touched `web-ui/**` or the app).
2. Verified live, not just tests — note what you ran and observed.
3. Right Conventional-Commit prefix for the version bump, or `[skip release]`.
4. First-time contributors: CLA sign-off ([CONTRIBUTING.md](CONTRIBUTING.md)).

## Skills, rules, subagents, hooks

- **Skills** (`/name`): **develop** (run + iterate), **verify-app** (drive a change
  live), **debug-live-app** (read daemon/UI logs, reproduce, common failure modes),
  **ship-pr** (open a PR + version bump), **chat-mode** (the structured chat stack).
- **Rules**: path-scoped constraints in `.claude/rules/` load with matching files.
- **Subagents**: `.claude/agents/` — `area-implementer` (scoped edits + live verify)
  and `diff-reviewer` (read-only invariant check vs `upstream/main`).
- **Hooks**: `.claude/settings.json` — fmt-on-save, destructive-command + generated-
  file guards, session orientation, and the doc-drift warn (personal hooks go in the
  gitignored `.claude/settings.local.json`).
