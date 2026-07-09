# Git & source control

Read-only git for a workspace's repo — porcelain-v2 status and side-by-side diff — plus the
one class of mutation: creating/removing **worktrees** confined to a daemon-managed root
("spin a branch into its own openable window"). There is **no** stage / unstage / commit /
discard / push / pull endpoint anywhere; the panel reviews, it doesn't commit.

**Where it lives (shared):** UI `web-ui/src/lib/workspace/{GitView.svelte,git.ts,gitDeco.ts,
SessionChangesView.svelte}` + the diff surface `web-ui/src/lib/previews/DiffView.svelte`.
Daemon: `crates/chimaera-server/src/git/` (`http.rs` status/diff/worktrees, `worktree.rs`
create/remove, `resolve.rs`, `service.rs`, `parse.rs`). Wire: `GET /api/v1/git/status`,
`GET /api/v1/git/diff`, `GET/POST/DELETE /api/v1/git/worktrees`, and a git **epoch** on
`/ws/events`.

## Source-control panel

- **What & when.** A singleton pane surface: a branch header plus every changed path grouped
  into Conflicts / Staged / Changes / Untracked, click-to-diff.
- **How it's used.** The header names the branch (or `detached`+SHA / `(unborn)`) with `↑N`/`↓N`
  ahead/behind and a refresh button. Each changed row shows a file glyph, a mid-truncated
  repo-relative path, a rename `←` marker, and a letter badge. Clicking a row opens its diff in an
  **adjacent** pane (Cmd/Ctrl-click forces a fresh split) so the panel stays visible beside it.
- **Where it lives.** `GitView.svelte` (`groups`, `openDiff`), `git.ts` (`gitStatus` store,
  `fetchGitDiff`), `gitDeco.ts` (`decoFor`/`dirColor`). Routes `GET /api/v1/git/status?workspace_id=`,
  `GET /api/v1/git/diff?workspace_id=&path=&mode=`.
- **Key behaviors.** Diff mode per group: Staged rows open `staged` (index vs HEAD); everything else
  `unstaged` (working tree vs index). One path can appear in two groups (staged edit + further
  worktree edit) — VS Code semantics; the badge disambiguates. A clean repo shows "Working tree
  clean."; `status.truncated` appends a cap note. Rows only *open* diffs — no checkboxes/stage/commit
  controls exist server-side.

## The diff surface

- **What & when.** The side-by-side viewer the panel (and the session-changes view) opens. Full
  before/after review with a mode toggle.
- **How it's used.** Opens as a pane tab keyed by `(path, mode)`; a toolbar toggles Unstaged /
  Staged / All without changing the tab identity. Selecting text on the working-tree (right) side
  publishes a reference chip for a chat composer.
- **Where it lives.** `DiffView.svelte` (CodeMirror `MergeView`). The daemon returns two **full
  blobs**; the client computes the diff (`git/http.rs` maps modes: `staged`→`HEAD:rel` vs `:rel`;
  `head`→`HEAD:rel` vs worktree; default `unstaged`→`:rel` vs worktree).
- **Key behaviors.** Editors are strictly read-only (the diff can't be edited/committed). Binary and
  over-cap files degrade to a quiet message (each side capped at 2 MB; binary detected by NUL in the
  first 8000 bytes). The reference-chip bridge is armed only for working-tree comparisons. Reloads on
  a git epoch bump.

## Worktrees — create & remove (the only mutations)

- **What & when.** Make a new branch in its own worktree under chimaera's managed root and spawn a
  session into it, or delete a managed worktree checkout (keeping the branch).
- **How it's used.** In the Branches header, "+ branch" → pick agent/terminal, type a name,
  "create + open" → `POST /api/v1/git/worktrees {workspace_id, branch, base?}` creates the worktree,
  registers it as a workspace, and spawns the session there. Hover a *removable* worktree row → `×`
  → a `confirm()` → `DELETE /api/v1/git/worktrees {workspace_id, path}`.
- **Where it lives.** `git.ts` (`createWorktree`/`removeWorktree`), `GitView.svelte`
  (`spawnInNewBranch`/`remove`); server `git/worktree.rs`.
- **Key behaviors.** Create is additive — never touches an existing checkout; the daemon rejects
  names git would refuse (`check-ref-format`), 409s if the branch is already checked out, and asserts
  path containment under the managed root. Remove is **fenced four ways**: must be under the managed
  root, not the current workspace, hold no live session, and be clean unless `force` (the UI never
  sends force). The branch itself survives a remove.

## Branches / worktrees view (the agent↔branch map)

- **What & when.** Below the changes list: one block per worktree showing which live sessions run in
  each — jump to a session that lives in another worktree.
- **Where it lives.** `GitView.svelte` (`worktrees`, session rows), `git.ts` (`gitWorktrees`,
  `worktreeForPath`). Route `GET /api/v1/git/worktrees?workspace_id=`.
- **Key behaviors.** The agent↔branch edge is **derived** from each session's `cwd` (longest-root
  match, so a worktree nested inside the main checkout at `.claude/worktrees/…` attributes
  correctly). Only actionable worktrees are listed; the rest fold into an "N other worktrees" line.

## Session-scoped changes

- **What & when.** Per-agent review: the files *this* session touched, cross-referenced with live
  git status. Review exactly what one agent changed.
- **Where it lives.** `SessionChangesView.svelte`; data is `session.files_touched` × git status.
- **Key behaviors.** If the session lives in a *linked worktree* (different `workspace_id`), the view
  fetches that workspace's own status rather than mis-decorating every row "no change". A row with a
  git change opens the diff; a touched-but-unchanged row (a `·` dot) just opens the file. Read-only.

## Git-binary / repo remediation

- **What & when.** Turns the two common HPC dead-ends into fix flows: git too old/missing, or "dubious
  ownership"/permission on shared storage.
- **Where it lives.** `GitView.svelte` (`gitBad`/`repoError`/`saveGitPath`), `git.ts` (`gitEnv`); every
  `/git/status` response carries the git diagnostic + `repo_error`.
- **Key behaviors.** Git is resolved via the login shell and **gated at ≥ 2.15** (`MIN_GIT` — needs
  porcelain-v2 + `worktree`); too-old/missing git offers a `git.path` setting input naming how the path
  was resolved. A "dubious ownership" error extracts the path and prints the exact
  `git config --global --add safe.directory <path>` remedy. Heavily HPC-shaped. Every git invocation is
  bounded (a hard timeout that **kills** the child so a wedged NFS mount can't pin a thread, output/entry
  caps, a concurrency permit). Status publishing bumps a per-workspace epoch on `/ws/events`
  (invalidate-and-refetch — big path lists stay off the firehose).

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Why git is read-only-first
_Captured 2026-07-09 — drafted from DESIGN.md + code, confirmed live with the maintainer._

- **The stance.** Replace code-server's git panel. Read-only-first is a **deliberate** choice —
  "stage/commit stay in a terminal for now" (decision 2026-07-07) — not an unbuilt gap. A git
  worktree is treated as a *dimension of one workspace*, not a peer; refresh is event-driven, never a
  status poll; tree status is a client overlay, not baked into `fs::list`.
- **Core vs addition.** This is an **addition to the core**, so the read-only stance **can change if
  there's a clear improvement** — the maintainer's rule: don't be too strict about additions.
- **Do not change casually:** event-driven refresh (never poll); worktree-as-dimension. The
  read-only boundary itself is open to revisit if committing from the UI earns its keep.
