# AGENTS.md — the index

Fast orientation for coding agents. This file is a lean **index**: what Chimaera
is, how to run and check it, the handful of conventions you can't infer from the
code, and pointers into the deep docs. Read the pointer for what you're touching
rather than front-loading everything.

> **Docs drift — verify before you trust.** Treat every path, command, and claim
> here as possibly stale: confirm it against the actual repo before relying on it,
> and when you find a doc wrong, **fix it in the same change**. That "verify →
> trust → update" loop is what keeps this orientation trustworthy instead of
> rotting. A Claude Code `Stop` hook nudges when an area's code changed without
> its map or feature page being touched.

## What Chimaera is

**A workspace for everything you do with agents.** Chimaera is an agent
workbench that brings conversations, terminals, files and artifacts, git review,
workspace context, and extensions into one workspace on the host that owns the
work — locally, over SSH, or inside a Slurm allocation. One Rust binary
(`chimaera`, statically linked on Linux) runs the daemon and serves the Svelte UI;
a native Tauri app wraps the same UI in real windows.

Chimaera launches the user’s own Claude Code, Codex, Antigravity and Grok Build
runtimes with the account, skills, and connections on the workspace host. Structured
chat adapters drive their native protocols; interactive agent TUIs run in daemon-owned PTYs. Windows are views onto
the daemon: closing a view or losing its connection leaves remote work running
while its host and daemon stay alive. Restart restoration resumes supported
conversations and restarts shells; it does not preserve the old processes.

Lowercase **chimaera** for the binary/product/dock label; "Chimaera" as the
capitalized proper noun in prose.

## Where things are (and what to read next)

A Rust workspace (the daemon) + a Svelte 5 UI it embeds + a separate Tauri app.
Each area carries its own `AGENTS.md` map (file table + the invariants that bite)
— read the most specific one; it wins over this index on local detail.

| Area | What it is | Map |
|---|---|---|
| `crates/chimaera` | the binary: CLI + daemon entrypoint (delegation-only) | [map](crates/chimaera/AGENTS.md) |
| `crates/chimaera-core` | shared types, version/build-id, shell integration | [map](crates/chimaera-core/AGENTS.md) |
| `crates/chimaera-pty` | the persistent PTY / terminal engine | [map](crates/chimaera-pty/AGENTS.md) |
| `crates/chimaera-agent` | the structured-agent engine (drivers, journal) | [map](crates/chimaera-agent/AGENTS.md) · [PROTOCOL](crates/chimaera-agent/PROTOCOL.md) |
| `crates/chimaera-remote` | SSH orchestration for `connect` (thorough in-code docs) | — |
| `crates/chimaera-server` | the daemon: every route + WS + business logic; embeds `web-ui/dist` and the plugins lock (no plugin bytes) | [map](crates/chimaera-server/AGENTS.md) |
| `crates/chimaera-plugin-api` | the plugin interface: the `chimaera:plugin` WIT world + the Rust side plugins implement | [map](crates/chimaera-plugin-api/AGENTS.md) |
| `plugins/` | `plugins.lock` (the curated first-party plugins: the release of each that installs, and its sha256s; each plugin lives in its own repository) + the host's test fixture | [map](plugins/AGENTS.md) |
| `crates/chimaera-app` | the Tauri 2 native shell (its own standalone workspace) | [map](crates/chimaera-app/AGENTS.md) |
| `web-ui/` | the Svelte 5 client the daemon serves | [chat](web-ui/src/lib/chat/AGENTS.md) · [dashboard](web-ui/src/lib/dashboard/AGENTS.md) · [settings](web-ui/src/lib/settings/AGENTS.md) · [knowledge](web-ui/src/lib/knowledge/AGENTS.md) · [plugins](web-ui/src/lib/plugins/AGENTS.md) |

The maps above tell you how the code is *structured*. For what the app **does** — feature
by feature, with how each is used and where it's wired — see the **[feature catalog](docs/features/README.md)**
(an index → lean per-feature pages; read the one you're touching, don't front-load them).

To build an extension, read the **[plugin authoring guide](docs/agent-guides/plugins.md)**:
it covers extension boundaries, a compilable starter, local installation, and validation.

Deep docs, read on demand: the **[architecture guide](docs/agent-guides/architecture.md)**
(the source of truth for how it's built and why), the design spine
[docs/design/README.md](docs/design/README.md), and the dated [field notes](docs/history/field-notes.md).
Before editing, read each `.claude/rules/*.md` whose frontmatter `paths` match the
files you will touch. Claude Code loads these automatically; other agents must do
this explicitly.

## Run it

Node 22 (`.nvmrc`). The Rust toolchain is pinned in
`rust-toolchain.toml` — **format with `cargo +1.96.0 fmt`** (a differing default
`cargo fmt` can pass locally yet fail CI's pinned check).

```sh
npm --prefix web-ui run check      # svelte-check
npm --prefix web-ui run test       # targeted Vitest suites (not browser/component tests)
npm --prefix web-ui run build      # build before Rust checks: the daemon embeds web-ui/dist (rust-embed)
just check                         # plugin test assets + fmt/clippy/test in daemon and plugin workspaces
bash scripts/build-plugins.sh      # tests only: the fixture + the locked releases → plugins/dist-test; not needed to build or run the daemon; `just plugins`
node scripts/check-doc-links.mjs   # every relative markdown link + #anchor resolves
node scripts/check-site.mjs        # public-site links, search/sharing metadata, and sitemap
node scripts/check-agent-assets.mjs # Claude/Codex skill + agent bridges stay in sync
node scripts/check-workflow-security.mjs # immutable Actions pins + explicit permissions
scripts/worktree-gc                # which worktrees are idle + what cleanup frees (dry run)
bash scripts/worktree-gc.test.sh   # worktree-gc's deletion rules, on a throwaway repo
```

**Isolated preview — use this in a worktree.** A debug daemon on its own state dir
+ an auto-assigned port, so parallel worktrees don't clobber each other's
`~/.chimaera`: `preview_start` the **chimaerad-isolated** config, read the
`#token=` URL from `preview_logs`, open it. A debug daemon reads `web-ui/dist` from
disk, so after a UI change just rebuild the UI and reload — no daemon restart. Full
loop, manual launcher when preview tools are unavailable, and gotchas:
the **[develop](.claude/skills/develop/SKILL.md)** skill.

**Anthropic-hosted Claude Code cloud sessions** (claude.ai/code, `claude --cloud`) have no browser
pane and no HPC access; a SessionStart hook installs the web-UI deps and builds
`web-ui/dist` for you. Setup + what "verify live" means there:
[cloud sessions guide](docs/agent-guides/cloud-sessions.md).

## Conventions you can't infer from the code

- **Verify live, don't just unit-test.** Terminal state, reconnect, resize, and
  agent integrations have all had bugs that pass tests but break against the real
  thing. Drive the flow (the **[verify-app](.claude/skills/verify-app/SKILL.md)**
  skill); the PR says what you ran and observed. The web UI has targeted Vitest
  coverage, but no browser/component tests — the live preview remains its runtime net.
- **Keep the daemon light on shared hosts.** Target ~150 MB RSS, no unbounded
  buffers, no busy loops, hard preview ceilings. Cluster workspaces default to
  Slurm jobs; a login-node daemon requires `--login-node`. The same resource
  constraints apply in either placement. **No SQLite near NFS/Lustre**; durable logs
  are append-only, size-capped JSONL under `~/.chimaera` (small whole-file state is
  capped JSON rewritten atomically); hot state is reconstructible.
- **Worktrees fill the disk.** Isolated sessions build in their own worktrees, and one
  worktree's cargo `target/` dirs have reached 35–60 GB. Follow the
  **[worktree-lifecycle](.claude/skills/worktree-lifecycle/SKILL.md)** skill: remove
  your worktree once its PR merges, run `scripts/worktree-gc` before a big build when
  disk is low, and never touch a worktree it calls ACTIVE.
- **The daemon↔UI wire is a stable public interface.** Core structs serialize
  straight to it — don't let its shape drift as a side effect of a refactor.
- **Agent wire formats are pinned, not trusted** — a driver or agent-CLI change
  needs `just chat-smoke` for Claude/Codex or `just chat-smoke-acp` for ACP
  adapters (live, billed). **Terminal state is
  server-side** (never serialize the `alacritty` `Term` grid). **UI quality is an
  acceptance criterion** (curated light/dark, the brand mark, a real workbench feel).
- **Comments state constraints and *why*, not narration.** `cargo +1.96.0 fmt` and
  `clippy -D warnings` must pass clean.
- Area-specific constraints live in `.claude/rules/*.md`; read every rule whose
  frontmatter matches the files being changed. The nested `AGENTS.md` maps carry
  the depth. When you add a substantial subsystem, add its map in the same style.

## Releases

CI + releases are automatic. `ci.yml` (fmt/clippy/test + UI + musl cross-builds)
gates every PR; `app.yml` build-checks the Tauri bundle when the app or UI change;
`release.yml` evaluates every merge to `main` and publishes when any unreleased
squash-commit subject requests a release. `pr-auto-update.yml` keeps pull requests with auto-merge up
to date with `main` (which requires it), the oldest behind first, one at a time.
Get the prefix right — or add `[skip release]` — via the
**[ship-pr](.claude/skills/ship-pr/SKILL.md)** skill, which owns the exact version
mapping and the no-release path.

## PR checklist

1. Applicable checks green: `just check` for Rust changes; UI checks for UI changes;
   documentation checks for pure docs (+ `app.yml` if you touched `web-ui/**` or the app).
2. Runtime changes verified live — note what you ran and observed; pure docs need documentation checks.
3. Right Conventional-Commit prefix for the version bump, or `[skip release]`.
4. Shipping a `feat:`? Update its [feature-catalog](docs/features/README.md) page
   (**document-feature**) and capture the human's *why* (**capture-feature-intent**).
5. First-time contributors: CLA sign-off ([.github/CONTRIBUTING.md](.github/CONTRIBUTING.md)).

## Skills, rules, subagents, hooks

- **Skills** (`/name`): Codex discovers lightweight bridges in `.agents/skills/`;
  Claude Code discovers the canonical workflows in `.claude/skills/`.
  **audit-repo** (parallel repository health pass), **develop** (run + iterate),
  **verify-app** (drive a change
  live), **debug-live-app** (read daemon/UI logs, reproduce, common failure modes),
  **ship-pr** (open a PR + version bump), **chat-mode** (the structured chat stack),
  **document-feature** (add/update a docs/features page), **capture-feature-intent**
  (the `feat:`-gated intent questionnaire), **worktree-lifecycle** (worktree and
  `target/` cleanup, low disk).
- **Rules**: path-scoped constraints in `.claude/rules/`; see the explicit-read
  requirement above for agents that do not auto-load them.
- **Subagents**: Claude definitions live in `.claude/agents/`; Codex definitions
  live in `.codex/agents/`. Both provide `area-implementer` (scoped edits + live
  verify) and `diff-reviewer` (read-only invariant check vs `origin/main`).
- **Claude hooks**: `.claude/settings.json` — fmt-on-save, destructive-command + generated-
  file guards, session orientation, the cloud-only bootstrap, the doc-drift warn, and
  worktree-gc (low-disk flag + idle-worktree cleanup at start, stale-object sweep at end)
  (personal hooks go in the gitignored `.claude/settings.local.json`).
- **Codex hooks**: `.codex/hooks.json` — the same two worktree-gc hooks. Codex runs
  project hooks only in a trusted project, after each is approved once in `/hooks`.
