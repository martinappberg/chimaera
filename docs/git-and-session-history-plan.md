# Git in the workbench, and session history

Dated 2026-09-28. A plan, not a record: nothing here has shipped. It has two parts:

- **Part 1: the core git pieces that are missing.** Several repositories in one
  workspace, history, diffs against any revision, sessions that know their branch,
  refresh after terminal commands, and worktree polish.
- **Part 2: session history and cost.** A lasting record of each agent session: who
  started it, what it changed, what it cost, and where its transcript is. It needs no
  git.

Working with other people (seeing a colleague's agents, messages between people,
forge integrations such as GitHub, commit trailers) is out of scope here and planned
separately.

The plan was built from a code map of the tree at `09c9ea12`
([what exists today](#what-exists-today)) and a survey of how Claude Code, Codex,
Cursor, Conductor and GitHub Copilot handle worktrees and agent history
([prior art](#prior-art)). It shares one route with the plugin platform plan
(`docs/plugin-platform-plan.md` on `claude/brave-hamilton-afhtt3`, not yet on main):
the log and `rev=` diff that plan adds for the editor's change bars
([§2](#2-history), [§3](#3-diffs-against-any-revision)).

## The stance

1. **Chimaera is for many kinds of work.** Code, analysis, writing and data work all
   happen in it. Many workspaces have no repository, and some have several.
   **Everything works without git. Git adds to a workspace when it is there.**
2. **Nothing asks you to use git.** No prompt to commit, branch or open a pull
   request. No "uncommitted changes" warnings. No automatic commits, and no commit per
   turn. No "initialize a repository?" offer.
3. **Read-only stays** ([git feature page, Intent](features/git.md#intent--human-authored-ground-truth)).
   Chimaera shows git. You and your agents change it, in a terminal or a conversation.
   Creating and removing worktrees stays the one change Chimaera makes itself.
4. **A worktree stays a dimension of its repository**, never a peer workspace (same
   Intent section). This plan adds that a workspace can hold several repositories.
5. **Login-node discipline.** Nothing scans a whole tree, nothing polls unless a window
   is watching, and every git call goes through the existing limits: 4 processes, 8 s,
   8 MB. Records are append-only, capped JSONL. See
   [daemon rules](../.claude/rules/daemon.md).

## What exists today

- **Git service** (`crates/chimaera-server/src/git/`):
  - porcelain-v2 status;
  - diffs (unstaged, staged, head) returned as two whole blobs that the client diffs;
  - `worktree list`;
  - worktree create and remove under `~/.chimaera/worktrees/<repo-key>/<branch>`,
    behind four checks (managed, not open, no live session inside, clean).
- **Refresh:** the git epoch on `/ws/events`, bumped by agent hooks, chat edits, saves
  and uploads, with a 12 s fallback only while a window watches.
- **UI:** the Source Control panel (changes, a Branches section of worktrees and their
  sessions, a "+ branch" composer), the diff view, a session's changes view,
  file-tree badges, and a branch chip in the status strip.
- **One repository per workspace.** The repository is found once, at the workspace
  root (`rev-parse --show-toplevel`). A root that only *contains* repositories shows
  "not a git repository", and repositories nested inside are invisible.
- **No history.** No log, no commit view, no diff against a commit or a branch base.
  [DESIGN.md](../DESIGN.md)'s M4 promised a log that was never built.
- **Sessions do not know their branch.** Every claude hook payload carries the current
  `cwd`, but `agents.rs` reads only `transcript_path` from it (~526). An agent that
  moves into a worktree is shown where it started.
- **A `git commit` typed in a terminal can take up to 12 s to show.** DESIGN says a
  finished terminal command refreshes git. The code does not.
- **Worktree composer gaps.** The UI never sends a base branch, though the daemon
  accepts one. Worktrees are not locked while an agent works in one. Ignored files
  such as `.env` are not copied into a new worktree.
- **No memory of sessions.** The ledger holds live sessions only. An ended session keeps
  a Recents row (20 per workspace) and nothing else.
  - Cost is the latest statusline value in memory for claude TUIs, and scattered
    through pruned journals for chats.
  - Nothing records who started a session, and the Mastermind's actions are logged
    only as `tracing` lines.

## Part 1: the missing core git pieces

### 1. Several repositories in one workspace

#### The cases

| Case | Example | Today |
|---|---|---|
| The root is a repository | a code project | works |
| The root is inside a repository | opening `analysis/` of a larger repo | works |
| The root holds several repositories | a project folder with a pipeline repo, an analysis repo and a cloned tool | "not a git repository" |
| A repository holds another | a submodule, or a tool cloned into `external/` (often gitignored) | the inner one is invisible |

The last two are common in research work, where a project folder collects repositories
rather than being one.

#### Finding them without scanning the tree

A workspace's repositories are the one enclosing the root, if any, plus those found
below it. There is no recursive walk. Discovery comes from four cheap sources:

1. **At open:** check for `.git` in the root's children and grandchildren. It skips
   the Quick Open ignore list, makes at most 2,000 checks, runs off the reactor, and
   happens once per workspace open (again on the panel's refresh button).
2. **The file tree:** when the tree lists a folder whose entries include `.git`, that
   folder is a repository. This is free, because the listing already happened.
3. **Agents:** when a session's `cwd` (§4) lands in a folder no known repository
   covers, one `rev-parse` runs there.
4. **Submodules:** the enclosing repository's porcelain-v2 status already marks them.

The cap is **32 repositories per workspace**. Past that, the panel says so, and the
rest appear as their folders are opened.

#### How they behave

- **A path belongs to the innermost repository that contains it.** Git already treats
  a nested repository as one untracked folder in the outer one, not as its files. The
  outer repository's list shows that folder as a link to the inner repository's
  section, not as a change.
- **Status is per repository** and single-flighted per repository.
  - `mark_path_dirty` already carries a path, so only the repository containing it
    refreshes.
  - The 12 s fallback covers only repositories whose section is open or that contain
    an open file.
- **The panel:** with one repository it looks exactly like today. With several, it
  lists them, each with its path in the workspace, branch, ↑↓, change count, and whether
  it is a submodule. Expanding one shows its changes, branches and history.
- **The status-strip chip follows focus**: the active file's, terminal's or session's
  repository. With nothing focused inside one, it says "3 repos".
- **File-tree badges** come from the innermost repository's status. Folder roll-ups
  stop at repository boundaries, and a repository folder gets a small mark.
- **Worktrees belong to a repository.** "+ branch" asks which repository when there
  is more than one.
- **Dubious ownership, per repository.** On shared group storage a repository owned by
  a colleague makes git refuse to work. The existing "dubious ownership" remediation
  has to work per repository, since this is exactly where it bites.

#### The wire

The change is additive.

- `GET /git/repos?workspace_id=` lists the repositories: top-level path, kind (root,
  enclosing, nested, submodule), and branch.
- The existing routes gain an optional `repo`, which must be one of that list, never an
  arbitrary path. Without it, a route uses the repository at or around the root, as
  today, so existing clients and the status shape are unchanged.
- The git epoch frame gains the repository that changed.

### 2. History

- **`GET /git/log?repo=&path=&rev=&skip=&limit=`**: at most 50 commits a page, each
  with sha, parents, author, date and subject. `path` narrows to one file, with
  `--follow`. This is the same route the plugin platform plan adds for change bars.
- **`GET /git/show?repo=&rev=`**: a commit's message and its files with added and
  removed line counts. Each file's diff opens in the existing diff view with `rev=`
  (§3).
- **UI:**
  - A **History** section per repository in the panel.
  - **File history** from the file tree and tab menus.
  - A commit opens its file list, and a file opens its diff.
  - A commit can be dropped into a chat as a reference, like a file.
- **The line:** Chimaera shows history and never checks out, reverts or resets.

### 3. Diffs against any revision

- **`rev=` on `GET /git/diff`:** the working tree against a revision, or one revision
  against another. The revision is validated with `check-ref-format` and resolved with
  `rev-parse --verify`, as the platform plan specifies.
- **"Changes on this branch"** for any branch row: everything since the branch left
  its base, meaning the merge-base diff plus uncommitted work. It is how you look at a
  worktree's work before handing it back, whether or not a pull request ever exists.

### 4. Sessions know their repository and branch

- **Use the `cwd`** that every claude hook payload carries, and the chat drivers' cwd.
  The session's repository is the innermost one containing that cwd. This covers an
  agent that enters a worktree mid-session (claude's `EnterWorktree`, `claude -w`, a
  plain `cd`).
- **Show the branch** above the chat input for chat sessions, and on dashboard cards,
  only when the session is in a repository; never on the rail. `architecture.md`
  planned this chip and deferred it.
- **Record an anchor** `{repo, worktree, branch, head}` at session start and end, for
  Part 2.
  - Commits made during the session show up as HEAD moving. Nothing requires or
    prompts for them.
  - When there are some, the session's changes view says "2 commits" and lists them.
- Codex and Gemini TUIs have no hooks, so they keep their start folder and say so, as
  provenance does today.

### 5. Refresh after terminal commands

Shell integration already knows when a command finishes (OSC 133 marks,
`chimaera-pty/src/marks.rs`). When one finishes, the daemon marks the terminal's current
folder dirty, which refreshes the repository containing it. A `git commit`, `checkout`
or `pull` typed in a terminal then shows at once. This is what DESIGN already
describes.

### 6. Worktree polish

- **Pick the base** in the "+ branch" composer, and the repository when there are
  several.
- **Lock** a managed worktree while an agent runs in it
  (`git worktree lock --reason "chimaera: <session>"`). Other tools' clean-up sweeps,
  Claude Code's included, then leave it alone.
- **Honor `.worktreeinclude`**: Claude Code's file, in gitignore syntax, copies ignored
  files such as `.env` into a new worktree. Using the same file means there is no new
  convention to learn.
- **Offer removal once a branch is merged**, through the existing remove and its checks.
  Never remove a dirty or unpushed worktree without `force`.
- **Start agents in a worktree:**
  - `CreateSession` accepts a `cwd` inside one of the workspace's worktrees.
  - The Mastermind's `spawn_agent` gains `branch` and `base`. Today it always starts at
    the root.
- **List local branches, not only worktrees**: name, last commit date and upstream.
  Read-only, with no checkout.

### Without git

- **No repository:** the Source Control panel says "not a git repository", and git's
  chips and sections do not appear. Everything else works as usual, including all of
  Part 2.
- **Git missing or older than 2.15:** today's diagnostic and the `git.path` field, per
  repository where it matters.

## Part 2: session history and cost

This part works the same with or without git. Git only adds fields.

### 7. One record per session

`~/.chimaera/workspace/<ws>/sessions.jsonl` holds one record per session. It is
append-only: a record opens when a session starts and closes when it ends, and a daemon
restart closes any record left open.

| Field | From |
|---|---|
| `id`, `agent`, `ui`, `title`, `first_prompt`, `models[]` | the session, chat events, the statusline |
| `started_by` | `you` · `mastermind` · a session id · `restart` |
| `started`, `ended`, `outcome` | exited · crashed · retired |
| `files` `{n, top[≤10]}` | `files_touched` |
| `usage` `{cost_usd, tokens_in, tokens_out, turns}` | chat turn results; the last statusline value for claude TUIs (a running total, `cost.total_cost_usd`); `null` where unknown |
| `transcript` | the chat journal id, claude's transcript path, or codex's rollout path |
| `git` (only in a repository) | the start and end anchors (§4), and up to 50 commit shas made during the session |

- **The Mastermind's actions** (`spawn_agent`, `message_agent`, `interrupt_agent`,
  note deliveries) append `act` lines to the same file. The audit trail becomes
  something a view can read, instead of `tracing` lines.
- **Bounded.**
  - A record is about 1 KiB, and the file compacts at 4 MiB.
  - Dropped records fold into one totals line per month: sessions, and cost and tokens
    per agent and model. Totals survive indefinitely at a few hundred bytes a month.
  - Details stay for the last few thousand sessions.
  - Only open records live in memory.

### 8. What a session changed, with or without git

- **The agent's own edits.** Claude's Edit and Write calls and codex's patches carry
  before and after text. Chat already draws them as diffs in tool cards. The changes
  view gathers them per file, in order.
  - Chat sessions read them from the journal.
  - Claude TUI sessions read them from claude's transcript, through the existing
    importer.
  - This needs no git.
- **The honest limit.** A change made by a shell command (a script, `sed`, a pipeline's
  outputs) has no before and after in the agent's records.
  - Without git, such a file shows as touched, where hooks name it, or not at all.
  - With git, the repository's status shows it anyway.
- **With git**, today's changes view (touched files crossed with status) stays, plus the
  session's commits.
- **Not used:** claude's own file backups for `/rewind` (`~/.claude/file-history/`).
  They are claude's private format and cover claude only.
- **Two sessions, one file.** When two live sessions edit the same file (both
  `files_touched` lists already exist), both rows get a quiet chip. Where the agent has
  hooks, it also gets one line of context through the existing hook answer: "session
  'fix normalization' edited src/qc.py 3 min ago". The agent can avoid the clash
  itself. Nothing is locked, and this works with or without git.

### 9. The session history list

- **Every past session of the workspace:** date, duration, agent and model, who started
  it, files changed, commits (in a repository) and cost.
- **Search** by title and first prompt, and filter by agent.
- **Open or resume** each session while its transcript still exists. Claude deletes
  transcripts after its `cleanupPeriodDays` (30 by default), and the chat journal
  directory is pruned at 100 MiB. When one is gone, the row says so in plain words.
- **Recents stays as it is** (the last 20 in the rail), with an **All sessions** entry
  that opens this list.
- **The Mastermind** can read an ended session's record through `read_session`.

### 10. Cost

- **Totals** per session, per workspace, per day and week, and per agent and model,
  with a CSV export.
- **Honest numbers.** Claude reports what the work would cost at API prices, which is
  not what a subscriber pays. The page says "estimated at API prices" and shows tokens
  beside it.
- **Unknown stays unknown.** A codex TUI has no cost telemetry, and codex chats report
  tokens but not cost. Those show "—", never zero.
- No budgets or alerts in this plan.

## Phases

Parts 1 and 2 are independent and can proceed in parallel.

| Phase | What |
|---|---|
| **G1: small fixes** | Sessions know their repository and branch from the hook `cwd` (§4); refresh after terminal commands (§5); worktree base, lock and `.worktreeinclude` (§6). |
| **G2: several repositories** | Discovery, per-repository status, the panel's repository list, `repo=` on the routes (§1). Before History, so History is per repository from its first version. |
| **G3: history** | Log, commit view, file history, `rev=` diffs, "changes on this branch", the local branch list (§2, §3, §6). Coordinated with the plugin platform's log route. |
| **H1: the record** | `sessions.jsonl`, `started_by`, the Mastermind's actions, the `git` fields when present (§7). |
| **H2: history list and changes** | The All sessions list, changes without git, the same-file warning (§8, §9). |
| **H3: cost** | Totals and the cost page (§10). |

Each phase follows the repository's shipping rules: verified live, a feature page,
and captured intent for each `feat:`.

## Open decisions

1. **Where the session list lives:** a pane surface opened from Recents' **All
   sessions** (recommended), or a section of the workspace dashboard.
2. **Where cost lives:** a Usage page in Settings covering all workspaces, plus one
   line per workspace on its dashboard (recommended), or the dashboard only.
3. **Discovery depth at open:** two levels below the root (recommended), one level,
   or only what the file tree and agents reveal.
4. **The local branch list:** read-only with no checkout (recommended), or leave
   branches to worktrees only.

## Out of scope

- **Committing, staging, pushing and pull requests from the UI**, automatic commits,
  and working-tree checkpoints. Agents and terminals change git, and agents keep their
  own rewind.
- **Working with other people**: seeing colleagues' agents, messages between people,
  forge integrations, commit trailers, shared session records. Planned separately.
- **Offering to initialize a repository.**

## Prior art

Researched 2026-09-28. [V] means checked against primary docs, [S] a secondary source.

| Tool | What it does | What we take |
|---|---|---|
| **Claude Code worktrees** [V] | `claude -w`, `EnterWorktree`, subagent `isolation: worktree`, `.worktreeinclude`, a worktree lock while an agent runs, never sweeps dirty or unpushed work | `.worktreeinclude` as-is; the lock; the removal rules |
| **Claude Code checkpoints** [V] | its own file snapshots per prompt, "not a replacement for version control" | not relied on; changes come from tool data |
| **Codex app** [V] | worktrees on a detached HEAD, about 15 kept, a snapshot before auto-delete | caution about automatic clean-up |
| **Cursor** [V] | `.cursor/worktrees.json` setup scripts, 25 worktrees per machine | a per-workspace setup step is enough; no new file |
| **Conductor** [S/V] | a branch in a worktree as the unit of work; diff review before merging | "changes on this branch" as the review view |
| **VS Code** | several repositories in one window, one section each, focus picks the status-bar repository | the panel's repository list and the chip that follows focus |
| **ccusage** [V] | cost reports from local session logs | local totals, no service |

Sources: [Claude Code worktrees](https://code.claude.com/docs/en/worktrees) ·
[checkpointing](https://code.claude.com/docs/en/checkpointing) ·
[Codex worktrees](https://learn.chatgpt.com/docs/environments/git-worktrees) ·
[Cursor worktrees](https://cursor.com/docs/configuration/worktrees) ·
[Conductor workflow](https://www.conductor.build/docs/workflow) ·
[ccusage](https://github.com/ryoppippi/ccusage).
