# Linked terminals (the "leash") & the MCP server

A user-granted edge that lets an **agent** session reach specific **terminal** sessions. The
agent reaches exactly those terminals — no more — through the daemon's own built-in MCP server
(`list_terminals` / `run_in_terminal` / `read_terminal`). This is Chimaera's bridge between a
coding agent and the long-lived shells a user actually works in (modules loaded, a conda env, an
ssh session to a cluster).

**Where it lives (shared):** UI `web-ui/src/lib/workspace/agentLinks.ts` + the `LinkCtrl` and
leash-drawing in `web-ui/src/App.svelte`; the drag band that arms a link is in
`web-ui/src/lib/layout/dnd.ts` (see [workbench.md](workbench.md)). Daemon:
`crates/chimaera-server/src/{links.rs,mcp.rs}`. Wire: `GET/PUT /api/v1/links`,
`DELETE /api/v1/links/{terminal_id}`, `POST /api/v1/mcp/{id}?key=`.

## Granting the leash

- **What & when.** Give an agent access to one or more terminals so it can run commands in your
  already-set-up shell instead of a fresh, unconfigured one.
- **How it's used.** Three ways, all user-initiated: drag a terminal's tab (or the pane-bar link
  icon) onto an agent pane/tab/rail-row; use the pane-bar "Link to agent…" menu; or type
  `@term:NAME` in a chat composer (which auto-links). Linking auto-reveals the target session as a
  split beside the agent.
- **Where it lives.** `agentLinks.ts` (`listLinks`/`putLink`/`deleteLink`, `termReference`,
  `agentHue`), `App.svelte` (`LinkCtrl`, `doLink`). Server `links.rs` (`put_link`/`delete_link`/
  `terminals_of`). Routes `GET/PUT /api/v1/links`, `DELETE /api/v1/links/{terminal_id}`.
- **Key behaviors.** **One agent per terminal** (the map is keyed by terminal id) — re-linking
  *moves* the leash (PUT overwrites). An agent may hold many. Unlink is idempotent and optimistic
  (drops locally, re-lists on failure). Each agent gets a **deterministic accent hue** (a curated
  5-hue palette clear of the semantic colors) painted on its chips, its linked-pane borders, and
  its exec pulses. Reference resolution prefers the leash: a selection from a *linked* terminal
  lands on its bound agent before any focused/MRU/newest agent. Links live **in memory only** — a
  link dies with either session, and sessions die with the daemon.

## The MCP server

- **What & when.** How a linked agent actually reaches its terminals: the daemon exposes a built-in
  MCP server per agent, offering three tools.
- **How it's used.** Wired into claude via the generated `--mcp-config` pointing at
  `POST /api/v1/mcp/{agent_id}?key={secret}` (JSON-RPC over MCP streamable-HTTP, stateless). Tools:
  `list_terminals` (the agent's granted terminals), `run_in_terminal` (type a command, await the
  outcome — the same [exec engine](terminals.md#the-exec-engine-and-command-journal)), `read_terminal`
  (the command journal).
- **Where it lives.** `mcp.rs` (`mcp`, `tool_defs`, `list_terminals`/`run_in_terminal`/`read_terminal`,
  `autolink_mentions`, `resolve_terminal`).
- **Key behaviors.** `/mcp/{id}` is **not** behind the bearer layer — it's authorized by the random
  per-session `key` in the URL (the agent's MCP client can't know the daemon token), re-checked
  against the `AgentRecord`. The links are the agent's **whole access scope** — it can't reach a
  terminal it wasn't granted. A `@term:NAME` mention resolves across *every* non-agent session (the
  point is granting a not-yet-linked terminal); the mention arriving through the `UserPromptSubmit`
  hook (TUI) or the protocol input (chat) **is** the consent. MCP is stateless streamable-HTTP
  (plain JSON, no SSE). Dead-session edges are pruned at read time so the `/ws/events` snapshot dedup
  doesn't flap.

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### The leash — why it exists
_Captured 2026-07-09 (from the maintainer). This is the catalog's first worked intent entry —
also used to pressure-test the questionnaire in the [capture-feature-intent](../../.claude/skills/capture-feature-intent/SKILL.md) skill._

- **Problem it solves.** Give an agent reach into the shell *you're actually working in*, instead
  of a fresh one it spawns cold. Concretely: you've set up particular conda environments; you're
  mid-task in a live shell and want the agent to read and act from exactly where you are; you've
  `ssh`'d somewhere you can't (or won't) expose to the agent directly and want it to work through
  that session. The leash is how the agent borrows your already-set-up context.
- **How settled it is (intended vs provisional).** Provisional — *"it's just a link."* The **why**
  above is the stable part. The current mechanics — one agent per terminal, links held **in memory
  only** and dropped with the session/daemon, the `@term:` mention as the consent, the accent-hue
  coloring — are how it works **for now**, not a promise. Treat them as the current implementation,
  not a contract.
- **Deliberately open / where it may go.** Nothing here is fixed. A direction the maintainer named:
  starting agents *and* terminals with **linked environments / configs** from the outset — the leash
  growing from an ad-hoc grant into a way to launch paired agent+terminal contexts. *"We'll see."*
- **Do not change:** nothing is off-limits — this is explicitly an area open to change. A future
  agent should preserve the **purpose** (an agent operating through the user's own live shell
  context) but is free to rework the grant model, the in-memory persistence, or the scope if there's
  a good reason. Don't treat the current mechanics as invariants.
