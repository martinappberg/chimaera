# Agent communication

Claude, Codex, Antigravity, Grok and Cursor chats, plus Claude/Codex terminals, can see which other agents
are running and what they're doing, read one's recent work, and send them messages that
actually arrive. The workspace **Mastermind** is part of it: the one coordinator the user
appoints, whose messages carry direction. Built into Chimaera (it replaced the Agent notes
plugin), **on by default**, switchable off in Settings → Agents. Plan and wire contract:
[docs/design/agent-communication-plan.md](../design/agent-communication-plan.md) (§12).

**Where it lives (shared):** daemon `crates/chimaera-server/src/comms.rs` (the tools, delivery,
wakes, read state, routes), `mcp.rs` (tool lists, instructions, the Mastermind tier),
`agents.rs` (the hook carrier), `chat.rs` (turn ends and steer fates reach
`comms::on_chat_event`), `timeline.rs` (the `note` fields), `ws.rs` (the `comms` frame). Engine
`crates/chimaera-agent/src/model.rs` (`AgentEvent::AgentMessage`, `AgentCommand::SendIfRunning`,
the `agent` / `mastermind` origins, `comms_title_suffix`, `BackgroundTool.for_message`),
`lib.rs` (`ChatManager::annotate`, `main_wait`), `codex.rs` (`agent_steers`), `claude.rs`
(the background-for-a-message notice); `agent_state.rs` (`note_main_call`, a claude
terminal's open calls). UI `web-ui/src/lib/workspace/comms.svelte.ts` (the store),
`web-ui/src/lib/chat/agentMessages.ts` + `AgentMessageCards.svelte` (message cards),
`web-ui/src/lib/dashboard/WakeRequestCard.svelte`, `AgentCard.svelte`, `MastermindDock.svelte`,
`web-ui/src/lib/settings/schema.ts`, the Timeline rows. Wire (bearer-authed):
`GET /api/v1/workspaces/{id}/comms`, `POST …/comms/wakes/{wid}`, `POST …/comms/deliver`,
`POST …/timeline/{seq}/deliver`; the `/ws/events` frame `{"type":"comms","epochs":{…}}`; tools
on the per-session MCP endpoint ([linked-terminals.md](linked-terminals.md#the-mcp-server)).

## The tools

- **What & when.** Four tools on the chimaera MCP server for every agent while the feature is
  on: `workspace_agents {include_exited?}` (who is here: id, name, vendor, chat or terminal,
  working / idle / waiting on the user, its now-line, branch and worktree, recent files, the
  Mastermind flag, and how a message reaches it), `read_agent {agent, lines?}` (a chat's
  transcript tail, a terminal's screen, an ended session's record — costs the other agent
  nothing), `message_agent {to, text, reply_to?, expect_reply?}` (`to` = an id, a unique name,
  `"mastermind"` or `"everyone"`), `read_messages {all?}` (the inbox).
- **How it's used.** Agents use them on their own; the MCP instructions give each agent its
  own session id and say when to message (a finding another needs, a blocker, a heads-up, a
  question) and when not (progress chatter). The UI names the calls in words — "Message to
  analyst", "Listed agents" — from the drivers' titles, which carry the target
  (`message_agent (chimaera) → s-…`, `chimaera.message_agent → s-…`).
- **Key behaviors.** Pre-allowed for workers (claude `permissions.allow`, codex
  `mcp_auto_approve`, the codex-terminal `approval_mode`), because the setting is the
  standing permission. Antigravity, Grok and Cursor retain native ACP approval requests; an ask-first Mastermind's `message_agent` keeps its prompt. The names
  avoid Claude Code's own `ListAgents` / `SendMessage` (machine-wide, other Claude sessions):
  asked to "use list_agents", a live haiku loaded those instead, so the instructions also say
  the harness's own agent tools never reach this workspace's agents. Those tools stay the
  agent's — never denied, never intercepted; a mix-up fails harmlessly (no Claude session has
  a workspace agent's id), and only haiku ever made one live. Workspace-scoped:
  another workspace's agents are neither listed nor reachable. A message is ≤ 2 KB (bigger:
  write a file and send its path); 10 posts per session per minute (shared with plugins'
  Timeline appends).

## Delivery: how a message arrives

- **What & when.** A message is first a Timeline `note` whose `delivery` field is set (a
  plugin's own note has none and is never delivered); its seq is its id (`#12`). Then it takes
  the best carrier that does **not** start a turn.
- **How it's used.**
  - **Claude, chat or terminal:** the next hook that fires answers with it as
    `additionalContext` — at its next step (PostToolUse), with the user's next prompt
    (UserPromptSubmit), or at start. A hook fired inside one of its subagents never carries it
    (it would be read there and never by the agent it was sent to). A chat session also journals an `agent_message` event so
    its transcript shows the message where the agent read it.
  - **Claude chat waiting on a subagent:** a turn blocked on a foreground subagent has no
    next step until it returns, so by default the message is read then, and the sender is
    told so ("waiting on a subagent for 4 min (…); it reads this when that subagent
    returns"). With **Messages may move a waiting subagent to the background** on
    (`agents.communication.freeSubagents`, default off), chimaera first moves that subagent to
    the background (the CLI's Ctrl-B, `background_tasks`): the Agent call returns at once, the
    agent's own PostToolUse carries the message, and the subagent keeps working. The chat
    shows "subagent moved to the background to read a message from another agent; it keeps
    working". It is off by default because it changes the flow of the user's turn, and the
    subagent's result then arrives in a separate turn (Claude Code's own task notification) —
    billed, and after the turn the user started has ended. Broadcasts never move anyone's
    subagent.
  - **Codex chat, mid-turn:** `send_if_running` — steered into the running turn (a queued
    user message with `origin: "agent"`), read (`sent`) at its next step. While the agent
    waits on its spawned agents (`wait`), the steer ends that wait at once: it reads and
    answers, then waits again. One that misses its turn settles `dropped` and goes back to
    the inbox; it never opens a turn.
  - **Antigravity, Grok and Cursor chats, mid-turn:** messages stay in the inbox. Unread direct
    messages meet the wake policy after the current turn; these agents do not support
    Codex steering.
  - **Codex terminals:** the inbox only — it sees messages when it calls
    `read_messages` (`workspace_agents` says so, and says how many wait).
  - **Antigravity, Grok and Cursor terminals:** automatic Chimaera MCP injection is not available
    in the verified CLIs. Use Chat for Chimaera messages; no global configuration is changed.
  - **An idle chat:** the wake policy decides (below).
- **Key behaviors.** What the agent reads: a header line
  `[message #12 from "loader refactor" (s-1a2b, claude) to you, re #10 — information … Reply
  with chimaera's message_agent tool (mcp__chimaera__message_agent — not SendMessage) to s-1a2b,
  reply_to 12.]`, then the body quoted (`> `) — the reply instruction names the exact tool
  because a live haiku otherwise answered with Claude Code's own `SendMessage`; the Mastermind's
  header says it is direction and its body is unquoted (a line posing as a header is
  escaped). Every delivery path claims its messages atomically (claude fires parallel hooks
  for parallel tool calls; a message is carried once). Up to 5 messages / 8 KB per hook
  answer; the rest wait. Read state persists per workspace (`<data>/workspace/<ws>/comms.json`,
  atomic rewrite) — a restart neither re-delivers nor loses one. Broadcasts reach every other
  agent's carrier and never wake anyone.
- **What the sender is told.** `message_agent` names what a working reader waits on and for
  how long, and when it will read the message; a message that reaches a waiting agent at once
  waits up to 8 s for the read and says whether it happened (a Codex reader in its `wait`, or
  a Claude reader with `freeSubagents` on: "Sent (#12) to builder — it was waiting on a
  subagent for 4 min (audit the loader); chimaera moved that subagent to the background (it
  keeps working); it has read it"). `workspace_agents` shows the same per agent
  ("working: running a command for 12 min (cargo test) · reads messages when its command
  ends"). The Timeline's delivery chip reads "reached it mid-wait" for those.

**While the reader's turn waits on something** (live, claude 2.1.300 and codex 0.162.1,
[PROTOCOL.md Pass 44](../../crates/chimaera-agent/PROTOCOL.md)):

| Reader | Waiting on a subagent | Running a long command | The sender is told |
|---|---|---|---|
| Claude chat | read when the subagent returns; with `freeSubagents` on, read now: the subagent moves to the background and keeps working | read when the command ends | what it waits on, for how long, and when (or, freed, whether) it read it |
| Codex chat | read now: the steer ends the `wait`, the subagent keeps working | read when the command ends | the same |
| Claude terminal | read when the subagent returns: nothing can move a terminal's subagent | read when the command ends | "waiting on a subagent for N min (…); a terminal can't be interrupted, so it reads this when that subagent returns" |
| Antigravity / Grok / Cursor chat | no subagent surface; nothing reaches a running ACP turn | read after the turn, by the wake policy | "it waits in its inbox (its current turn is still running; it is …) until that turn ends, when the user's wake setting decides whether it reads it then" |
| Codex terminal | inbox only (`read_messages`) | inbox only | "sees messages when it calls read_messages" |

Claude Code's own queued input (stdin messages, its cross-session peer messages, its task
notifications) is read only between main-thread steps, and its Send now (`priority: "now"`,
`interrupt {send_now}`) stops the subagent the turn waits on, so neither is used. A command
is not moved to the background: its result may be what the next step needs.

## Wakes and wake requests

- **What & when.** A message to an idle chat agent may start a turn — billed — only as
  Settings → Agents → **Agents may wake each other** says: **Never** (it waits for the agent's
  next turn or the user's hand-over), **Ask me** (default: a wake request in the dashboard's
  Needs you), **Within limits** (it wakes, inside the caps).
- **How it's used.** A wake request card — "analyst wants to wake reviewer: '…'" — offers
  **Wake** (delivers every unread message as one send) and **Leave in inbox**
  (`POST /comms/wakes/{wid} {wake}`). A reply to a question the reader asked
  (`expect_reply`, then `reply_to` it) wakes the asker without asking — the question-and-answer
  that works without a blocking call. A chat that ends its turn with messages still unread
  meets the same policy then. The user can also hand an agent its inbox
  (`POST /comms/deliver`, the Mastermind panel's inbox chip) or one Timeline message
  (`POST /timeline/{seq}/deliver`).
- **Key behaviors.** Caps: a fresh message wakes at most once per sender per 3 minutes, 10
  wakes per workspace per hour, 4 per conversation (a reply chain; a reply isn't held to the
  3 minutes — a quick second question's answer would otherwise sit unread) — past the conversation's limit the user is asked "X and Y have
  been going back and forth — let them continue?" (Continue resets it). A wake starts with a
  line saying why ("[chimaera delivered this while you were idle: it answers a question you
  asked]"). Terminal agents are never woken, whatever the policy. Wake requests are one per
  reader, at most 16 per workspace, and vanish once the reader has nothing unread.

## The Mastermind inside it

- **What & when.** The workspace Mastermind ([dashboard.md](dashboard.md#the-mastermind-panel))
  uses the same four tools; its extra tier is `workspace_status`, `list_changed_files`,
  `read_timeline`, `spawn_agent`, `spawn_terminal`, `interrupt_agent`.
- **Key behaviors.** Its `message_agent` carries direction (`origin: "mastermind"`, the
  Mastermind header): to a chat worker an ordinary send — Claude/Codex read it at their next
  step while working (with `freeSubagents` on, a Claude chat waiting on a foreground
  subagent has it moved to the background first, as for a peer's message), ACP workers queue it for the next turn, and an idle worker starts a turn —
  gated by the Mastermind's own ask-first/auto mode, not the wake policy; to a claude
  terminal the hook carrier. Messages to it (`to: "mastermind"`) wake an auto-mode Mastermind
  within the caps and wait in the panel's inbox in ask-first mode.

## The switch

- **What & when.** Settings → Agents → **Agents can see and message each other**
  (`agents.communication.enabled`, default on), **Agents may wake each other**
  (`agents.communication.wakes`: `never` · `ask` · `auto`, default `ask`) and **Messages may
  move a waiting subagent to the background** (`agents.communication.freeSubagents`, default
  off; Claude chats — see Delivery).
- **Key behaviors.** Off: new sessions get none of the four tools, no paragraph and no
  Mastermind tier, and a codex terminal no MCP injection unless a plugin needs one — the
  worker view is byte-for-byte the one from before the feature
  (`crates/chimaera-server/src/tests/fixtures/agent_view/*_off`). Running sessions stop at
  once (calls refused with "Settings → Agents", no carriers, no wakes); their tools stay listed
  until they restart. An appointed Mastermind goes dormant — kept, its panel saying so — and
  appointing one is refused (409). Messages already sent stay on the Timeline and are carried
  when it comes back on.

**Verified live (2026-09-30, claude 2.1.284 + codex 0.157.1, billed):** a haiku chat, asked
in plain words, listed the workspace with `workspace_agents` and asked a codex chat a question
with `expect_reply`; the request showed in Needs you, **Wake** clicked in the UI woke codex,
its `reply_to` answer woke the claude chat with no second request; a message sent while the
claude chat ran a 75 s command reached it at its next step (the journaled `agent_message`
card) in the same turn; one sent while codex ran a 60 s command was steered (queued →
`sent`) and answered in that turn; an auto-mode claude Mastermind directed codex, whose report
woke it; the switch off in Settings refused a running agent's `message_agent` at once and
emptied its tool list to the base six, and the Mastermind panel showed its off banner.
Later the same day: asked in plain words, a sonnet chat and an opus chat each put a question
to the codex chat through `workspace_agents` / `read_agent` / `message_agent` and were woken
by its answers; two haiku chats in a counting relay under **Within limits**
woke each other four times (a reply within the 3 minutes included), the fifth asked the user,
**Continue** gave four more; a codex **terminal** read its inbox with `read_messages` when
prompted and answered with `message_agent`, pre-approved, which woke the asker; a codex chat,
asked in plain words, started a conversation itself (`workspace_agents` → `read_agent` →
`message_agent`) and read the haiku's answer, steered into its still-running turn.

**Verified live (2026-10-14, claude 2.1.300 + codex 0.162.1, billed, isolated daemon over
its HTTP/WS API):** a haiku chat asked a chat that was waiting on a 150 s foreground subagent
a yes/no question with `expect_reply`, 24 s into the subagent's run. Claude reader (moving
the subagent, as `freeSubagents` on does now — the runs predate the setting): read
0.2 s after the send (the subagent moved to the background), answered with `message_agent`
4.1 s later, which woke the asker; the subagent finished at 165 s with its own result
("PONG") and Claude Code relayed it in a turn of its own. Codex reader (gpt-5.6-sol, collab
`wait`): read 0.4 s after the send, answered 10.4 s later (codex auto-reviewed the
pre-approved call), waited again, and its subagent finished on schedule.
`message_agent` answered "… — it has read it" in both.

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Agent communication — why it exists
_Captured 2026-09-29/30 from the maintainer, in the design conversation that produced it._

- **Problem it solves.** Agent notes made no sense as a plugin: agents should be able to talk
  to each other over Chimaera's MCP, and every agent should be able to query which agents are
  running — "extend it to be better and have agents be able to talk to each other".
- **Deliberate calls (settled by the maintainer):** built in, not a plugin — the Agent notes
  plugin is deleted; **enabled by default**, and "it is important one can turn it off"; the
  **Mastermind is part of it**; waking an idle agent is a policy defaulting to **Ask me**;
  peers reading each other's work (`read_agent`) — "yes most definitely".
- **How settled it is (intended vs provisional):** the calls above are the maintainer's.
  The mechanics — the carriers, the caps, the tool names, the header format — are how it
  works now.
- **Deliberately open / where it may go:** a per-workspace "off here" override (later, if
  someone needs it); visibility across workspaces (not in v1).
- **Do not change (or: open to change):** an agent keeps every tool its harness gave it —
  Chimaera never denies or hides Claude Code's own `ListAgents` / `SendMessage` in the agents
  it spawns, even to stop them being mistaken for the workspace's tools: "no we can absolutely
  not do that" (2026-09-30). Beyond that, pending — not asked.
