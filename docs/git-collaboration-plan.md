# Git, worktrees and working together

Dated 2026-09-28. A plan, not a record: nothing here has shipped. It answers four
questions for a workspace, whether one person or several work in it. Who is doing
what, and on which branch? What is running where? Which conversation produced this
change, and what did it cost? And how do people, and their agents, see and reach each
other's work? It was built from a code map of the tree at `09c9ea12` (see
[what exists today](#what-exists-today)) and a survey of how other tools handle the
same problems: Claude Code, Codex, Cursor, Conductor, GitHub Copilot, Entire, git-ai,
Agent Trace and the kernel's trailer rules ([prior art](#prior-art)). It builds on the
plugin platform plan (`docs/plugin-platform-plan.md` on `claude/brave-hamilton-afhtt3`,
not yet on main), which decides what plugins can do.

## The short version

- **Git is the spine, and the repository is the only shared medium.** Core has no relay
  ([DESIGN.md](../DESIGN.md)). Anything that crosses between people crosses through
  git, the forge, or a folder they both reach on shared storage.
- **The unit of work is a branch in its worktree.** The main checkout counts as one.
  Each branch row shows who works there, which agents, terminals and jobs run there,
  how far it is from its base, and what it has cost. It grows out of the Branches
  section that the Source Control panel already has.
- **Agents do git; Chimaera shows it.** Read-only-first stays. A button such as "Commit
  this" or "Open a pull request" sends a visible prompt to that branch's agent, so every
  change to history happens in a conversation someone can read later. Creating and
  removing worktrees stays the one thing Chimaera does itself, as today.
- **Every session leaves a durable work record**: who started it, which agent and
  model, which branch, the commits it made, the files it touched, what it cost, and
  where its transcript is. Traceability, the audit trail and cost reports are all read
  from that one record.
- **Attribution comes from watching, not from asking.** The daemon sees HEAD move
  during an agent's turn and links those commits to the session. It needs no git hooks
  in the user's repository and no cooperation from the agent. Commit trailers are an
  opt-in extra that carries the link to other people.
- **Summaries travel, transcripts stay home.** Nothing leaves the machine without a
  click. A transcript leaves only as an explicit, redacted export.
- **Other people's work, without a server.** Several people can use one checkout on
  shared storage, which is common in labs. For that case, each daemon writes a small
  presence file inside `.git`, and each person sees the others' live agents there.
  With separate clones, commits, branches and pull requests already carry the story.
  A forge plugin adds pull-request state.
- **Messages across people are mail, not a phone call.** A note from someone else's
  agent is data. It waits for a person's click, is never taken as consent, and never
  starts a turn.
- **Vendors go in plugins, the spine stays in core.** GitHub or GitLab, and other
  tools' provenance formats (Entire, git-ai, Agent Trace), become plugins on the new
  platform. This plan asks that platform for four small generic pieces
  ([§6](#6-what-goes-in-plugins)).

## Principles

1. **Observe, don't require cooperation.** Facts come from what the daemon already
   sees: hook payloads, HEAD, `files_touched`, chat events. A convention that depends
   on an agent remembering it is an extra, never the source.
2. **Agents do git.** Chimaera does not grow a staging UI or a commit dialog. It shows
   the state, and its actions are prompts to agents. This keeps the read-only-first
   stance ([git feature page, Intent](features/git.md#intent--human-authored-ground-truth))
   and makes every history change traceable to a conversation.
3. **A worktree stays a dimension of one workspace** (same Intent section), never a
   peer workspace in the UI.
4. **Identity is the git identity.** A person is their `user.name` and `user.email` plus
   `user@host`. Core has no accounts and no roles.
5. **Private by default.** Nothing is written into the tracked tree, and nothing is
   pushed, without a switch the user turned on. Presence lives in `.git`, which is never
   committed.
6. **Mail, not a phone call, across people too** (the Agent notes rule). Anything from
   another person or their agent reaches an agent only through a person's click.
7. **Read other formats, write the smallest one.** Chimaera reads mycelium logs, Entire
   checkpoints, git-ai notes and Agent Trace records through plugins. What it writes is
   the smallest thing that links a commit to a session.
8. **Login-node discipline.** Records are append-only, capped JSONL files. No SQLite.
   Nothing polls unless a window is watching, and git runs through the existing limits
   (4 processes, 8 s, 8 MB). See [daemon rules](../.claude/rules/daemon.md).

## Three situations

| | Who | What they share | Served by |
|---|---|---|---|
| **A** | one person, many agents | one repository, several worktrees | §1–§3 |
| **B** | several people, one checkout on shared storage (a lab's group folder on HPC) | the files and the `.git` directory | §4, §5 |
| **C** | several people, separate clones, one remote | the remote and the forge | §4, §5, §6 |

Situation A is daily life and comes first. Situation B is common in science and rare in
software teams, and it is the one where the filesystem gives us live visibility with no
server. Situation C is the classic software team, where the forge is already the
meeting place.

## What exists today

The code map (2026-09-28) found a solid read side and no memory of the work.

- **Git service** (`crates/chimaera-server/src/git/`): porcelain-v2 status, diffs
  (unstaged, staged, head) returned as two whole blobs for the client to diff,
  `worktree list`, and worktree create and remove under
  `~/.chimaera/worktrees/<repo-key>/<branch>` behind four checks: the worktree is
  managed, not open, has no live session inside it, and is clean. Refresh is driven by
  the git epoch on `/ws/events`, with a 12 s fallback only while a window watches.
- **UI**: the Source Control panel with a Branches section (worktrees, their sessions,
  a "+ branch" composer), the diff view, a session's changes view, file-tree badges and
  a branch chip in the status strip.
- **Missing:**
  - No log, history, blame, branch list, remotes or pull-request state.
  - No session records its branch or a commit.
  - An agent's worktree is guessed from its spawn cwd, even though every claude hook
    payload carries the current `cwd`. `agents.rs` reads `transcript_path` from the
    payload but not `cwd`.
  - The ledger holds live sessions only. An ended session keeps a Recents row (20 per
    workspace) and nothing else.
  - Cost is the latest statusline value in memory for claude TUIs, and scattered
    through pruned journals for chats. No totals exist anywhere.
  - No record says who started a session: the user, the Mastermind, or which window.
    The Mastermind's act tools are audited only as `tracing` lines.
  - No notion of a person exists at any layer. One token, one daemon per OS user.

## 1. Branches: who works where (A)

### 1.1 Sessions know where they are

- **Use the `cwd` that every claude hook payload already carries** (next to
  `transcript_path`, `agents.rs` ~526), and the chat drivers' cwd. Map it to the
  worktree that contains it. This fixes the case of an agent that enters a worktree
  mid-session: claude's `EnterWorktree`, `claude -w`, a `.claude/worktrees/` folder, or a
  plain `cd`.
- **Record an anchor**, `{worktree, branch, head}`, at session start, at every turn end,
  and at session end. It costs one `rev-parse` per turn end through the existing git
  queue. Codex and Gemini TUIs without hooks keep their spawn cwd and say so, as
  provenance does today.
- **Show the branch** on rail rows, dashboard cards and the chat header. The chip was
  planned in `architecture.md` and deferred.

### 1.2 The branch rows

The Branches section becomes the place to look. One row per worktree, main checkout
first:

- **Who and what:** the agents working there, with their state, plus terminals and
  jobs whose working folder is inside the worktree. Once §4 lands, other people's
  agents appear here too.
- **Where it stands:** the base it started from (recorded at creation, otherwise the
  default branch), ahead and behind counts, `git diff --shortstat` against the merge
  base, and the number of commits since the base.
- **A state in plain words:** working · waiting on you · ready for review ·
  merged · idle for 5 days. Rows that need attention sort first.
- **What it cost** across all its sessions (§2.4).

### 1.3 Reviewing a branch

- **Diff against the base.** The plugin platform plan already adds `rev=` to
  `GET /git/diff`. Branch review uses the same parameter, with the merge base as the
  revision, and lists the changed files.
- **Comment on a line, and it goes to the branch's agent.** A comment in the diff
  becomes a message with a reference chip to the chat agent on that branch (the reference
  chip already exists for diff selections). A TUI cannot be typed into, so for a TUI the
  comment waits as a draft the user sends from its composer. This is the review loop
  Conductor, Codex and Copilot converged on.

### 1.4 Actions are prompts

Each action on a branch row sends an ordinary prompt, shown and editable before
sending, to the branch's agent, or starts an agent if the branch has none:

- **Commit this work**: commit with a message that explains the work.
- **Update from main**: rebase or merge, then resolve and explain any conflicts.
- **Open a pull request**: push the branch and open a PR that describes the work.
- **Finish up**: after a merge, the offer to remove the worktree (the existing remove,
  with its checks).

Chimaera never runs `commit`, `rebase`, `push` or `merge` itself. If committing from
the UI later earns its keep, the Intent section allows revisiting that. It is not part
of this plan.

### 1.5 Starting work on a branch

- The "+ branch" composer gains a **base** (the daemon already accepts one; the UI
  never sends it) and starts the agent in the new worktree, as today.
- **`spawn_agent` gains `branch` and `base`**, so the Mastermind can fan work out to
  parallel branches, then watch the rows and send review comments. Today it always
  spawns at the root.
- **`CreateSession` accepts a `cwd`** inside one of the workspace's worktrees.

### 1.6 Hygiene

- **Lock** a managed worktree while an agent runs in it
  (`git worktree lock --reason "chimaera: <session>"`). Claude Code and other tools'
  sweeps then leave it alone.
- **Honor `.worktreeinclude`**: Claude Code's file, gitignore syntax, copies ignored
  files such as `.env` into a new worktree. Using the same file adds no new convention.
  An optional setup command per workspace runs through the existing prelude seam.
- **Never remove a dirty or unpushed worktree without `force`.** Offer removal once a
  branch is merged.

### 1.7 Two agents, one file

When two live sessions in the same worktree touch the same file (`files_touched`
already has both lists), both rows get a quiet chip. Where the agent has hooks, it also
gets one context line through the existing hook answer: "session 'fix normalization'
edited src/qc.py 3 min ago". Agents avoid the clash themselves, and nothing is locked.

## 2. The work record: traceability and accounting (A, feeds B and C)

### 2.1 One record per session

`~/.chimaera/workspace/<ws>/work.jsonl` holds one record per session. It is
append-only: a session opens its record at start and closes it at end, and a daemon
restart closes any record left open.

| Field | Source |
|---|---|
| `id`, `agent`, `ui`, `title`, `models[]` | the session, chat events, statusline |
| `by` `{name, email, user_host}` | `git config user.name/email` in that worktree, plus `$USER@host` |
| `spawned_by` | `you` · `mastermind` · a session id · `restart` |
| `branch` `{worktree, branch, base}` and `head_start`, `head_end` | the anchors (§1.1) |
| `commits[]` (≤ 50 shas) | observed (§2.2) |
| `files` `{n, top[≤10]}` | `files_touched` |
| `usage` `{cost_usd, tokens_in, tokens_out, turns}` | `TurnCompleted` for chats; the last statusline value for claude TUIs (the session's cumulative cost); `null` where unknown |
| `started`, `ended`, `outcome` | exited · crashed · retired |
| `transcript` | the chat journal id, claude's jsonl path, or codex's rollout path |

Act-tool calls (`spawn_agent`, `message_agent`, `interrupt_agent`, note deliveries)
append `act` lines to the same file. That turns the audit trail from `tracing` lines
into something a view can read.

**Bounded.** A record is about 1 KiB. The file compacts at 4 MiB: dropped records fold
into one totals line per month (sessions, cost per agent and model). Totals survive
forever at a few hundred bytes a month, and details stay for the last few thousand
sessions. Only open records live in memory.

### 2.2 Commits, by observation

At each turn end, the anchor's HEAD is compared with the previous one. Commits reachable
from the new HEAD but not the old (`git rev-list old..new`, at most 50) are attributed to
that session.

- If two sessions on one branch had overlapping turns, the commit lists both. That is
  honest, not a guess.
- A commit made in a terminal with no agent turn in flight is attributed to the person.
- Force pushes, resets and rebases show up as a HEAD that is not a descendant of the
  old one. The record notes "history rewritten", and the old shas stay in the record.

This works for claude and codex, TUI and chat, and needs nothing in the repository.

### 2.3 History

A **History** list for a branch or a file. The platform plan adds
`GET /git/log?path=` (at most 50); this extends it to a branch range. Each commit shows
the person, an agent badge when a session made it, and that session's title. A click
opens the transcript at the turn that made the commit: the chat journal, or claude's
own transcript through the existing import. Blame-to-session comes later, from the same
data.

### 2.4 Cost

The work record answers cost questions: this week, per agent and model, per branch,
per person once §4 exists.

- A **Usage** section shows totals and a CSV export.
- Branch rows carry their own total.
- Where Chimaera cannot know a cost, it shows "—", not zero. A codex TUI has no cost
  telemetry.

No budgets or alerts in this plan.

### 2.5 Agents can ask

Two read tools on the chimaera MCP server:

- **`branches`**: the rows of §1.2 as data.
- **`history(path)`**: the commits that touched a path, with the session that made each
  one and its title and first prompt. An agent can then ask "why is this code like
  this?" and get the conversation that wrote it.

Both are local only. Who gets them is an [open decision](#8-open-decisions).

### 2.6 The Timeline

Episodes gain `evidence.git`: `{branch, from, to, commits}`. The Timeline then says
"committed 3 on `fix-qc`" beside the files, as `timeline-knowledge-plugins-plan.md`
promised.

## 3. Commits that carry their story (C, also A)

- **Opt-in trailers.** With a workspace switch on, agents' commits carry
  `Assisted-by: claude:claude-opus-5-5` (the Linux kernel and Fedora convention, now
  followed by several foundations) and `Chimaera-Session: <id>`.
  - For claude: its own `attribution.commit` setting, in the per-session settings
    Chimaera already generates. It must be merged with the user's own attribution, never
    replace it. Verify this on the pinned CLI before building.
  - For codex: a line in its developer instructions.
  - Chimaera installs no git hooks in the user's repository. Entire does, and a
    `prepare-commit-msg` hook collides with husky and pre-commit and surprises people.
- **Off by default**, offered once on the branch rows. Claude already adds its own
  `Co-Authored-By` unless told otherwise.
- **Squash merges drop trailers.** The fallback is matching: a squashed commit whose
  change matches a record's commits links back to it, which is how Anthropic's own
  analytics attributes pull requests. This is later work.

## 4. People, without a server (B, C)

### 4.1 Identity

`by` from §2.1 is the whole identity: name, email, `user@host`. A person shows as
initials and name. There is nothing to sign in to.

### 4.2 One checkout on shared storage (B)

Several people's daemons on a shared filesystem, each under its own Unix user, working
in one checkout.

- **Each daemon writes its own presence file** at
  `<git-common-dir>/chimaera/presence/<user>@<host>.json`. It is rewritten atomically
  at most every 10 s while that daemon has live sessions in the checkout, removed on
  exit, and treated as stale after 2 minutes.
- **The file contains** per live session: agent, title, branch, state, up to 20
  touched files, and the start time. It never holds prompts or transcripts.
- **No shared writes.** Each daemon writes only its own file, so there are no locks
  across NFS and no database. Readers list the directory only while a window shows the
  workspace, on the existing watch and 12 s fallback.
- **Opt-in per workspace:** "Show my agents to others working in this folder." If the
  directory is not group-writable, the switch says so in plain words and stays off.
- **What you see:** a quiet "2 others here" chip, their agents on the branch rows
  (read-only), and the §1.7 file clash chip across people ("Anna's claude edited
  src/qc.py 2 min ago"). This is where a clash costs the most.
- **Nothing leaks.** `.git` is never committed or pushed, so presence never reaches the
  remote.

### 4.3 Separate clones (C)

Most of what teammates did is already in the repository: commits, trailers, branches
and mycelium logs. The History list shows a teammate's commits with their name and,
when a trailer is present, the agent and model. Pull-request state comes from a forge
plugin (§6).

Publishing work-record summaries so teammates can read them is possible, but it is
**not in this plan's first phases**:

- It would use one ref per record under `refs/chimaera/work/<id>`. That is Entire's
  lesson: refs avoid contention between parallel writers, and a shared branch does not.
- It would push only with a switch, carry redacted summaries only, and warn when the
  remote is public.

Decide on it after §4.2 has been used.

### 4.4 Messages across people

- **Same checkout:** a note addressed to a person is one file in
  `<git-common-dir>/chimaera/mail/<user>/`. Their Chimaera shows it through Agent notes'
  existing **Deliver** click.
- **Separate clones:** the forge is the mailbox. Agents already comment on pull
  requests through `gh`. A forge plugin shows "3 new review comments on `fix-qc`" with
  **Hand to the branch's agent**.
- **Never:** one person's agent driving another person's agent directly. Claude Code's
  own cross-session messaging and agent teams take the same line. A message is not
  consent.

### 4.5 Sharing a conversation

**Share conversation** writes a redacted Markdown export: secrets scanned, including in
thinking blocks, which is where published agent logs have leaked keys. The user can then
attach it to a pull request, commit it, or send it. It happens only on an explicit
click, per session, and is never pushed automatically.

## 5. What each situation gets

| | A | B | C |
|---|---|---|---|
| Branch rows, anchors, review, actions as prompts | ✓ | ✓ | ✓ |
| Work record, History, cost | ✓ | yours, plus others' live presence | yours, plus others' commits |
| Others' live agents | — | presence files | out of scope (needs a relay) |
| Messages between people | — | mail folder in `.git` | pull-request comments through a forge plugin |
| A change's story travels | — | shared `.git` | opt-in trailers; published summaries later |

## 6. What goes in plugins

Core owns the generic spine: branches, anchors, the work record, presence and History.
It is not a vendor, and every surface needs it: the rail, the dashboard, the Timeline and
the Mastermind. Plugins on the new platform add the rest.

- **GitHub** and later **GitLab**: a privileged plugin that declares `gh` (or `glab`)
  under `[[programs]]` with `network = "github.com"`. It uses the user's own `gh auth`,
  which the job environment already reaches through HOME. It publishes, per branch: PR
  number and state, checks, review decision, and unresolved comments. Its actions are
  **Open in browser** and **Hand review comments to the branch's agent**. This is the
  "link your GitHub" feature, with no token handled by Chimaera.
- **Provenance readers:** Entire checkpoints, git-ai notes (`refs/notes/ai`), Agent
  Trace records. They are read-only, and their line attributions appear in History.
- **Mycelium** stays the lab notebook. Its session logs can name the work-record id,
  so a finding links to the conversation behind it.

**What this asks of the plugin platform**, all generic and none naming a vendor:

1. **A `branch/1` data surface.** Keyed by branch, it carries badges
   (`{label, tone, url}`), counts and actions, and core draws them on branch rows and in
   branch review. It fits the platform's rule that important, structured screens are
   core-drawn surfaces, as with `diagnostics/1`.
2. **A `git-changed` event**: HEAD moved, a branch appeared or vanished, a worktree was
   added or removed. It comes from the existing git epoch, so a plugin refreshes without
   polling.
3. **`[access] work = "read"`**: the work record through the host, in the same shape
   the History list reads.
4. **An ask-agent target of "the agent on this branch"**, generalizing the platform's
   **Ask agent** from diagnostics.

Plugins get git facts from the host (branches, log, work record) rather than by running
`git` themselves. That keeps `git.path`, the version gate and the limits in one place.

## 7. Phases

| Phase | Situation | What |
|---|---|---|
| **G1: memory** | A | Anchors from the hook `cwd`; the branch chip; `work.jsonl` (`by`, `spawned_by`, usage totals, observed commits, act lines); `evidence.git` on episodes. Almost no new UI, and every later phase reads from it. |
| **G2: branches** | A | Branch rows (§1.2); review against the base with comments to the agent; actions as prompts; `spawn_agent` and `CreateSession` with a branch; lock and `.worktreeinclude`. |
| **G3: history** | A | History for a branch or file; the `branches` and `history` tools; the Usage section; the file clash chip and hook line. |
| **G4: forge** | C | Opt-in trailers; the GitHub plugin once the platform's programs phase (P8) ships, plus `branch/1` and `git-changed`. |
| **G5: together** | B | Presence files; the mail folder; clashes across people. |
| **G6: maybe** | C | Published summaries under `refs/chimaera/work/`; the redacted conversation export. |

Each phase follows the repository's usual shipping rules: verified live, a feature
page, and captured intent for each `feat:`.

## 8. Open decisions

1. **Actions as prompts, or a commit button?** Recommended: prompts. The stance holds,
   and every history change stays in a conversation.
2. **Trailers:** off by default and offered once (recommended), or on by default?
3. **Presence on shared storage:** opt-in per workspace (recommended), or on whenever
   the folder is group-writable?
4. **Who gets `branches` and `history`:** every agent (recommended for `history`, which
   is repository facts), or the Mastermind only (recommended for `branches`, which
   names other sessions: the same line as the deferred "observe for all")?
5. **Where cost lives:** a panel on the workspace dashboard, or a Usage page in
   Settings covering all workspaces.
6. **Words:** the UI says "branch" everywhere, and "worktree" only where the folder
   matters. That avoids a new term.

## Out of scope

- **A git GUI**: staging hunks, a commit dialog, a branch switcher. Agents commit.
- **Seeing another person's agents live on another machine.** That needs a relay, and
  core has none ([DESIGN.md](../DESIGN.md)).
- **Accounts, roles and permissions.**
- **Working-tree checkpoints and rewind.** Agents have their own (claude's `/rewind`),
  and snapshots are a different project.
- **Jujutsu.** Worth watching, and nothing here precludes it. Claude Code's
  `WorktreeCreate` hooks show the seam.

## Prior art

Researched 2026-09-28. [V] means checked against primary docs, [S] a secondary source.

| Tool | What it does | What we take |
|---|---|---|
| **Claude Code worktrees** [V] | `claude -w`, `EnterWorktree`, subagent `isolation: worktree`, `.worktreeinclude`, a worktree lock while running, never sweeps dirty or unpushed work | `.worktreeinclude` as-is; the lock; the sweep rules |
| **Codex app** [V] | worktrees on a detached HEAD, about 15 kept, a snapshot before auto-delete, local↔worktree handoff | the cap-and-snapshot caution |
| **Conductor** [S/V] | "the workspace is the unit of delegation, the branch and PR the unit of integration"; inline diff comments go back to the agent | branch as the unit; review comments to the agent |
| **GitHub Copilot agent** [V] | pushes only to `copilot/*`, draft PRs, the starting human as co-author, logs linked from commits, sessions visible to repo readers | the PR as the integration point; visibility follows repository access |
| **Entire** [V] | hooks record turns to shadow branches; a `prepare-commit-msg` trailer; checkpoints moved from one branch to one ref per record; "if the repo is public, this data is visible to anyone" | one ref per record; trailer as a link; its privacy warning. Rejected: installing git hooks |
| **git-ai** [V] | line attribution as git notes under `refs/notes/ai`; transcripts removed from notes in favor of a URL pointer | read its notes; keep transcripts out of git |
| **Agent Trace** (Cursor RFC) [V] | a neutral JSON schema for line attribution; leaves storage to the tool | a possible export format |
| **Linux kernel, Fedora** [V] | `Assisted-by: AGENT:MODEL`; AI never signs off | the trailer format |
| **Claude Code agent teams, cross-session messaging** [V] | file inboxes and a locked task list on one machine; a message is never user consent | mail, not a phone call; messages as data |
| **Anthropic analytics, ccusage** [V] | PRs attributed by matching session edits to merged diffs; cost reports from local JSONL | squash fallback by matching; local cost totals |
| **Leaked agent logs** [S] | published agent transcripts leaked live keys, many in thinking blocks | redact before anything leaves, thinking included |

Sources: [Claude Code worktrees](https://code.claude.com/docs/en/worktrees) ·
[agent teams](https://code.claude.com/docs/en/agent-teams) ·
[cross-session messaging](https://code.claude.com/docs/en/cross-session-messaging) ·
[analytics](https://code.claude.com/docs/en/analytics) ·
[Codex worktrees](https://learn.chatgpt.com/docs/environments/git-worktrees) ·
[Conductor workflow](https://www.conductor.build/docs/workflow) ·
[Copilot agents](https://docs.github.com/en/copilot/how-tos/copilot-on-github/use-copilot-agents/manage-and-track-agents) ·
[Entire CLI](https://github.com/entireio/cli) ·
[Entire ref-based checkpoints](https://entire.io/blog/introducing-ref-based-checkpoint-storage) ·
[git-ai standard](https://github.com/git-ai-project/git-ai/blob/main/specs/git_ai_standard_v3.0.0.md) ·
[Agent Trace](https://agent-trace.dev/) ·
[kernel coding assistants](https://docs.kernel.org/process/coding-assistants.html) ·
[ccusage](https://github.com/ryoppippi/ccusage).
