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

_No intent captured yet — pending the next `feat:` in this area._ This is a strong candidate for
the first intent capture: the *why* of the leash (why an agent should reach a user's live shell
rather than spawn its own), and what about the "the mention is the consent" grant model is a
deliberate security stance vs an implementation detail, are not derivable from code.
