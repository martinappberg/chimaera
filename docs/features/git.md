# Git & source control

Read-only git for a workspace's repositories — porcelain-v2 status, side-by-side diffs, history —
plus the one class of mutation: creating/removing **worktrees** confined to a daemon-managed root
(and locking/unlocking them while an agent works inside). There is **no** stage / unstage / commit
/ discard / push / pull / checkout / reset endpoint anywhere; the panel reviews, it doesn't commit.
Git is **optional and ambient**: without a repository nothing git-shaped appears, and nothing ever
suggests using git.

**Where it lives (shared):** UI `web-ui/src/lib/workspace/{GitView.svelte (the panel shell + the
repository list), GitRepoSection.svelte (one repository's changes, Branches, History),
GitDetailView.svelte (the commit / history / "Changes on this branch" surface), GitHistoryList.svelte,
CommitRow.svelte, git.ts (stores + fetchers), gitFormat.ts, gitDeco.ts, SessionChangesView.svelte}`,
`shared/BranchChip.svelte`, and the diff surface `web-ui/src/lib/previews/DiffView.svelte`.
Daemon: `crates/chimaera-server/src/git/` (`http.rs` status/diff/worktrees/branches/repos, `history.rs`
log/show/compare, `repos.rs` discovery, `session.rs` the session tracker, `anchor.rs`, `worktree.rs`,
`include.rs`, `rev.rs`, `resolve.rs`, `service.rs`, `parse.rs`). Wire: `GET /api/v1/git/{status,diff,
repos,branches,log,show,compare}`, `GET/POST/DELETE /api/v1/git/worktrees`,
`GET /api/v1/sessions/{id}/git`, the additive `git` field on session rows, and a git **epoch** frame on
`/ws/events` (with per-repository epochs).

## Source-control panel

- **What & when.** A singleton pane surface. With one repository: a branch header plus every changed
  path grouped into Conflicts / Staged / Changes / Untracked (click-to-diff), then two collapsible
  sections, **Branches** and **History**. With several repositories: a compact list instead.
- **How it's used.** The header names the branch (`No branch (at 3f2a1c9)` when detached, `No commits
  yet` when unborn) with `↑N`/`↓N` (tooltip in words: "2 commits to push · 1 to pull from
  origin/main") and a refresh button. Each changed row shows a file glyph, a mid-truncated
  repo-relative path, a rename `←` marker, and a letter badge. Clicking a row opens its diff in an
  **adjacent** pane (Cmd/Ctrl-click forces a fresh split). A folder that is itself a repository shows
  under "Repositories inside" as a link to its own section, not as a change.
- **Where it lives.** `GitView.svelte`, `GitRepoSection.svelte` (`groups`, `openDiff`), `git.ts`
  (`gitStatus`, `gitRepos`, `gitRepoStatuses`, `fetchGitDiff`), `gitDeco.ts`.
- **Key behaviors.** Diff mode per group: Staged rows open `staged` (index vs HEAD); everything else
  `unstaged` (working tree vs index). One path can appear in two groups — VS Code semantics; the badge
  disambiguates. A clean repo shows "Working tree clean."; no repository shows "This folder isn't a git
  repository." and nothing else. Section open/closed state is remembered per workspace (browser storage).
  A repository git refuses to read (dubious ownership on shared storage) shows the exact
  `safe.directory` remedy — per repository.

## Several repositories in one workspace

- **What & when.** A folder that holds repositories (a pipeline repo, an analysis repo, a cloned tool),
  a repository with a nested clone or submodules. Each repository has its own status, branches and
  history.
- **How it's used.** The panel lists them — `name · branch · N changed` (↑↓ only when non-zero),
  nesting indented under the parent, submodules marked, repositories with changes first. Expanding a
  row shows that repository's section; the one holding the focused file or session expands by itself.
  The status-strip chip names the focused file's / terminal's / session's repository (`analysis main
  ●2`), or "N repos" when nothing focused is inside one; clicking it scrolls the panel to that
  repository. File-tree badges come from the innermost repository, roll-ups stop at a repository's
  folder, and repository folders get a small mark.
- **Where it lives.** Daemon `git/repos.rs` (`discover_all`, `note_listed_dir`, `note_agent_repo`,
  `note_submodules`), `git/service.rs` (`innermost`, `add_found`, per-repository keys/epochs/watchers),
  `git/http.rs` (`repos`, `pick_repo`). UI `git.ts` (`gitRepos`, `gitRepoStatuses`, `gitIndex`,
  `onGitNudge`, `gitFocus`), `App.svelte` (the strip chip, watched repositories), `FileTree.svelte`.
- **Key behaviors.** Discovery never walks the tree: a two-level `.git` probe at open (Quick Open's
  ignore list skipped, ≤2,000 checks, off the reactor; again on the panel's refresh), file-tree listings
  that show a `.git`, an agent's folder landing in an unknown repository (one `rev-parse`), and
  submodules (`.gitmodules`, porcelain-v2 `S` marks) — at most **32** per workspace ("capped" past
  that). A linked worktree of a known repository is never listed as a peer. Every route takes an
  optional `repo` (a known top level, or a worktree `git worktree list` reports for one — never an
  arbitrary path); without it the routes behave exactly as before, and `/git/diff` picks the innermost
  repository holding the path. A change refreshes only the repository containing it (plus a submodule's
  superproject); the 12 s backstop covers the primary and the nested repositories a window has open or
  holds a file of. Intent: see [the 2026-09-29 entry](#full-git-support--why-it-exists).

## History

- **What & when.** What happened in a repository, a file, or a branch — read-only.
- **How it's used.** The **History** section shows ~20 compact rows (subject, author, relative time;
  hover gives the message and a short sha) and loads more as you scroll (pages of 50). A commit opens
  its own tab: subject, body, `author · date · sha` (click the sha to copy), and the files with `+N −M`;
  a file opens that commit's diff against its parent. **File history** is in the file tree's and file
  tabs' menus and in Quick Open (with "Source Control" and "History"). A commit can be referenced in a
  chat — "Reference in chat", or dragged onto an agent like a file — as `commit 3f2a1c9 ("subject")`.
- **Where it lives.** Daemon `git/history.rs` (`log`, `show`, `compare`), `git/rev.rs`. UI
  `GitHistoryList.svelte`, `CommitRow.svelte`, `GitDetailView.svelte` (`gitx` surface), `layout.ts`
  (`GitDetailTab`, diff tabs' `rev`/`repo`/`orig`), `shared/reference.ts` (`referenceCommit`,
  `composeCommitReference`).
- **Key behaviors.** `GET /git/log?repo=&path=&rev=&skip=&limit=` (≤50 a page; `path` follows renames);
  `GET /git/show?repo=&rev=`; `rev=` on `GET /git/diff` (working tree vs the revision, or `mode=commit`
  for the commit vs its parent). Every revision is validated with `check-ref-format` and resolved with
  `rev-parse --verify` before use; flags, ranges, blob paths and reflog selectors are refused. Chimaera
  shows history and never checks out, reverts or resets. Intent: see [the 2026-09-29 entry](#full-git-support--why-it-exists).

## The diff surface

- **What & when.** The side-by-side viewer the panel, the session-changes view, commits and branch
  changes open.
- **How it's used.** Opens as a pane tab keyed by `(path, mode[, rev, repo])`; for status diffs a
  toolbar toggles Unstaged / Staged / All without changing the tab identity (revision diffs show what
  they were opened as). Selecting text on the working-tree (right) side publishes a reference chip.
- **Where it lives.** `DiffView.svelte` (CodeMirror `MergeView`). The daemon returns two **full
  blobs**; the client computes the diff.
- **Key behaviors.** Editors are strictly read-only. Binary and over-cap files degrade to a quiet
  message (each side capped at 2 MB; binary = NUL in the first 8000 bytes). Reloads when the epoch of
  the repository holding the file moves.

## Sessions know their branch

- **What & when.** Which repository and branch each session works in — including an agent that
  entered a worktree mid-session.
- **How it's used.** Chat sessions show one quiet line above the input (branch, and the worktree
  folder when it isn't the main checkout; click opens "Changes on this branch"); dashboard cards show
  the same label in their meta line. Nothing on the rail. The Branches section lists each session under
  the worktree it is in.
- **Where it lives.** Daemon `git/session.rs` (the tracker task, `note_hook_cwd`, `session_git`),
  `git/anchor.rs`, `agents.rs` (the hook's `cwd`), `session_view.rs` (the additive `git` row field and
  the agent's `cwd_current`). UI `shared/BranchChip.svelte`, `chat/ChatView.svelte`,
  `dashboard/AgentCard.svelte`.
- **Key behaviors.** A session's folder is its shell's polled cwd, the `cwd` every claude hook carries,
  else its spawn folder (hook-less agent TUIs keep their start folder). A hook fired inside a subagent
  (`agent_id` set) carries the subagent's folder — its own worktree when isolated — and never moves
  the session. The tracker
  resolves it once per folder (`rev-parse`), reads the branch from `HEAD` (no process), and recomputes
  only when a folder changes or a git epoch moves — never on a timer. It keeps anchors
  `{repo, worktree, branch, head}` at start, at claude turn ends and at end (in memory; ended sessions
  ≤128) and serves `GET /sessions/{id}/git` → `{start, current, commits[≤50], rewritten,
  branch_changed, repo_changed}`. `CreateSession` accepts a `cwd` inside the workspace or one of its
  worktrees; the Mastermind's `spawn_agent` takes `branch`/`base` (the worker stays in the Mastermind's
  workspace and runs in that branch's worktree). A command finishing in a terminal marks the terminal's
  folder dirty, so a `git commit` typed there shows at once. Intent: see [the 2026-09-29 entry](#full-git-support--why-it-exists).

## Worktrees — create & remove (the only mutations)

- **What & when.** Make a new branch in its own worktree under chimaera's managed root (optionally
  starting an agent there), or delete a managed worktree checkout (keeping the branch).
- **How it's used.** "+ New branch" in a repository's Branches section → a name, **From** (a local
  branch; the current one by default), and "Start an agent here" → `POST /api/v1/git/worktrees
  {workspace_id, branch, base?, repo?}`, then (with the box ticked) `POST /api/v1/sessions
  {workspace_id, kind:"agent", cwd:<worktree>}` — the agent starts in THIS workspace with the branch's
  worktree as its folder, and the window, file tree and panes stay exactly where they were. A muted
  line then says what `.worktreeinclude` copied ("Copied .env and 1 more"). A removable worktree offers "Remove worktree" on hover — always visible once
  merged — → an inline **Remove / Keep** confirmation → `DELETE /api/v1/git/worktrees {workspace_id, path, repo?}`.
- **Where it lives.** `git.ts` (`createWorktree`/`removeWorktree`), `GitRepoSection.svelte`
  (`spawnInNewBranch`/`remove`); server `git/worktree.rs`, `git/include.rs`.
- **Key behaviors.** A worktree is a dimension of the workspace it was made from, never a workspace of
  its own: create registers nothing (the answer's `workspace` is always null) and never moves a
  window; the branch shows as a Branches row, and "Changes on this branch" is how its files are
  reviewed. Opening it as its own window stays possible like any folder, and a removal drops such a
  registration. (Maintainer, 2026-09-29: the whole window jumping to a new branch "feels bad UI UX" —
  "that one agent could be in just that branch".) Create is additive; names go through `check-ref-format`, the base through
  `rev::resolve_commit`; 409 if the branch is already checked out; path containment under the managed
  root is asserted. `.worktreeinclude` (Claude Code's file, gitignore syntax) copies files that are both
  git-ignored and matched — ≤200 files / 64 MB, symlinks never followed or copied, nothing overwritten.
  Remove is **fenced five ways**: under the managed root, not the current workspace, no live session
  inside (any surface, including an agent's hook-reported folder), clean, and no commits that are
  neither pushed nor merged into the main checkout's branch — the last two unless `force` (the UI never
  sends force). While an agent runs inside a managed worktree it is locked (`chimaera: <session>`) so
  other tools' clean-up leaves it alone; the lock goes when the last agent leaves, stale chimaera locks
  are released (or adopted by restored sessions) at daemon start, and other tools' locks are never
  touched. Intent for the base picker, `.worktreeinclude`, locking and the unshared-commits
  fence (shipped 2026-09-29).

## Branches (the agent↔branch map)

- **What & when.** A repository's worktree branches (main checkout first) and which sessions work in
  each; the local branches without a worktree under a collapsed "Other branches (N)".
- **How it's used.** A row reads: branch, the worktree folder (muted, only when its name differs from
  the branch), "N ahead of main", "merged" (only once the branch had commits of its own and main holds
  them all — a brand-new branch says nothing; `http.rs` `branch_is_new` reads the branch's reflog), and
  the agents' glyphs with their state dots (click one to open that session). Clicking a row opens **Changes on this branch**: everything since the branch left its base
  — the merge-base diff plus uncommitted work — each file opening its diff against that point. An
  "Other branches" row opens that branch's history. Read-only: there is no checkout.
- **Where it lives.** `GitRepoSection.svelte`, `GitDetailView.svelte` (`view: "branch"`); routes
  `GET /git/worktrees` (additive `merged`, `ahead_of_main`, `behind_main`), `GET /git/branches` (≤100:
  name, last commit date, upstream, ahead/behind), `GET /git/compare?repo=&base=`.
- **Key behaviors.** The session↔worktree edge is the daemon's (`session.git.worktree`), not a client
  guess. Only actionable worktrees are listed (main, current, holding sessions, managed); the rest fold
  into "N other worktrees". Intent: see [the 2026-09-29 entry](#full-git-support--why-it-exists).

## Session-scoped changes

- **What & when.** Per-agent review: the files *this* session touched, cross-referenced with live
  git status.
- **Where it lives.** `SessionChangesView.svelte`; data is `session.files_touched` × git status.
- **Key behaviors.** If the session lives in a *linked worktree* (different `workspace_id`), the view
  fetches that workspace's own status rather than mis-decorating every row "no change". An agent working in a managed branch worktree uses that worktree's status even while it
  stays in the original workspace. With several repositories each row takes the status of
  the repository that contains it. A row with a git change
  opens the diff; a touched-but-unchanged row (a `·` dot) opens its recorded edits. Read-only. Without a
  repository the rows carry no git mark. One list with or without git: each file shows its edit
  count, and a file with no uncommitted git change opens the agent's own edits for it, in order
  ([session-history.md](session-history.md#what-a-session-changed-with-or-without-git)). The
  session's commits lead the view when it made any.

## Git-binary / repo remediation

- **What & when.** Turns the two common HPC dead-ends into fix flows: git too old/missing, or "dubious
  ownership"/permission on shared storage.
- **Where it lives.** `GitView.svelte` (`gitBad`/`repoError`/`saveGitPath`), `GitRepoSection.svelte`
  (per repository), `git.ts` (`gitEnv`); every `/git/status` response carries the git diagnostic +
  `repo_error`.
- **Key behaviors.** Git is resolved via the login shell and **gated at ≥ 2.15** (`MIN_GIT`);
  too-old/missing git offers a `git.path` setting input naming how the path was resolved. A "dubious
  ownership" error extracts the path and prints the exact `git config --global --add safe.directory
  <path>` remedy. Every git invocation is bounded (a hard timeout that **kills** the child, output/entry
  caps, a 4-process permit, `GIT_OPTIONAL_LOCKS=0`, `GIT_TERMINAL_PROMPT=0`). Status publishing bumps a
  per-workspace epoch (and a per-repository one) on `/ws/events` (invalidate-and-refetch — big path lists
  stay off the firehose).

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Why git is read-only-first
_Captured 2026-07-09 — drafted from docs/design/README.md + code, confirmed live with the maintainer._

- **The stance.** Replace code-server's git panel. Read-only-first is a **deliberate** choice —
  "stage/commit stay in a terminal for now" (decision 2026-07-07) — not an unbuilt gap. A git
  worktree is treated as a *dimension of one workspace*, not a peer; refresh is event-driven, never a
  status poll; tree status is a client overlay, not baked into `fs::list`.
- **Core vs addition.** This is an **addition to the core**, so the read-only stance **can change if
  there's a clear improvement** — the maintainer's rule: don't be too strict about additions.
- **Do not change casually:** event-driven refresh (never poll); worktree-as-dimension. The
  read-only boundary itself is open to revisit if committing from the UI earns its keep.

### Full git support — why it exists
_Captured 2026-09-29 from the maintainer, in the session that built it (his words quoted)._

- **Problem it solves:** "this is just so we have full git support in chimaera platform" — the core
  git pieces that were missing (several repositories in one workspace, history, diffs against any
  revision, sessions that know their branch, worktree polish).
- **How settled it is:**
  - **Core bet — git stays optional and never pushy.** "I dont want tooo much of git to become a
    thing, where you are like forced to write PRs etc … it is just a good natural extension but a lot
    of people will use chimaera for different things." Everything works without a repository; nothing
    prompts to commit, branch, open a pull request or `git init`; not every turn is committed.
  - **Core bet — a branch is where an agent works, not where the window goes.** Starting an agent on
    a new branch moving the whole window "feels bad UI UX … that one agent could be in just that
    branch". A worktree stays a dimension of its workspace.
  - **Additions (deliberate today, improvable):** "nested repos matter"; the branch shows on the line
    above a chat's input and on dashboard cards, and on the rail only for an agent working in a
    separate worktree ("if it is not in worktree then we don't display anything"); a brand-new branch
    says nothing rather than "merged"; Remove worktree says so in short prose ("not overly clear, same
    word length") and never deletes the branch; git views open as preview tabs.
- **Deliberately open / left out:** a commit, stage or push UI (agents and the terminal change git);
  deleting branches; working with other people, forge integrations and commit trailers, which are
  planned separately.
- **Do not change:** the two core bets above. The rest is open to improvement.
