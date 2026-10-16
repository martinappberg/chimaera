---
name: verify-app
description: Verify a Chimaera change end-to-end against the real daemon, UI, and PTY layer before claiming it works. Use after changing daemon/server/PTY/UI behavior — especially terminal, reconnect, resize, previews, or agent-launch flows, which have all had bugs that only reproduce live. Skip for pure docs/comment changes.
---

# Verifying a Chimaera change

Chimaera's verification culture is explicit: **features are driven live before
they land, not just unit-tested.** Terminal state, reconnect semantics, resize,
and agent integrations have repeatedly had bugs that pass tests but break against
the real thing. Your PR should say what you ran and what you observed.

## 1. Tests + lints first (necessary, not sufficient)

```sh
npm --prefix web-ui run check      # svelte-check, if you touched the UI
npm --prefix web-ui run test       # targeted reducer/unit coverage
npm --prefix web-ui run build      # production bundle and generated assets
just check                        # plugin assets, then fmt/clippy/test in both Rust workspaces
```

Build `web-ui/dist` before workspace Rust checks on a fresh checkout; the server
embeds it. For a daemon-only change, an existing bundle satisfies that prerequisite.

`crates/chimaera-pty` has real PTY tests (`tests.rs`, `snapshot.rs`) — extend
them when you change terminal state or snapshotting.

## 2. Select scenarios from the changed contracts

Do not pick scenarios from memory. Read the diff, then find the contracts it
touches: the area `AGENTS.md` map for each changed path, the
[feature page](../../../docs/features/README.md) for the capability, and each
`.claude/rules/*.md` bullet whose `paths` match. Every contract you find
selects at least one scenario from section 4. If the diff touches a contract that
no scenario covers, write one. Name the new scenario in the evidence.

## 3. Drive the real flow

Bring up the dev loop (see the **develop** skill), then exercise the *actual*
path you changed. When available, use the browser-pane tooling to observe:
`read_page` for structure/content, `read_console_messages` + `preview_logs` for
errors, `read_network_requests` for API/WS traffic, a screenshot for proof.

If those tools are absent, use the develop skill's manual isolated launcher,
daemon terminal output, and the available browser or native-app inspection tools.
Report any visual or network surface your session could not inspect.

**In a Claude cloud session** (`CLAUDE_CODE_REMOTE=true`) there is no preview
tooling: run the daemon headless and drive it over HTTP/WS instead, starting from
`scripts/smoke-daemon.mjs`, and name in the PR what you could not check (see the
[cloud sessions guide](../../../docs/agent-guides/cloud-sessions.md)).

## 4. Scenarios

Match the exercise to what you touched:

- **Terminal / PTY** — spawn a session, type, scroll. Then **kill the socket and
  reattach**: the exact screen must come back (scrollback, colors, cursor, title),
  no lost bytes. Reattach from a second tab at a **different window size**.
- **Resize / resync** — change the divider drag or font size on the initiator:
  it must reflow natively with **no flicker or scroll reset**. A *foreign* client
  repaints after a coalescing window. Read the
  [architecture guide](../../../docs/agent-guides/architecture.md) ("Resize repaint
  refinement") for the invariants before trusting your eyes.
  (Known accepted gap: a resync while on the alternate screen can't restore the
  primary screen's scrollback.)
- **Previews** — open the file types you touched (image, markdown, csv/tsv incl.
  gzip, pdf, sandboxed html). Confirm that ranged/paged formats stay incremental,
  whole-file formats enforce their size caps, and the client renders; watch
  daemon and browser memory on a big file.
- **Agent launch / links** — launch an agent runtime, confirm attention badges
  (needs-attention / finished / errored) derive from protocol events in chat
  and Claude hooks in its TUI, and that a linked terminal grant is scoped and
  audited in scrollback.
- **Reconnect** — reload and reattach from another machine/tab. Chat sockets
  (`/ws/chat/{id}`) replay the seq-numbered gap; the session-list bus
  (`/ws/events`) sends the current full roster; PTY sockets rebuild the screen
  from an escape-sequence snapshot.

## 5. Resource discipline is part of "works"

The daemon must stay light on shared hosts; cluster workspaces normally run
inside allocations. While exercising, sanity-check it stays within budget (~150 MB RSS target, <1 core steady-state, no runaway buffers) —
a preview of a huge Parquet/HTML file must never balloon memory. A change that
works but leaks or busy-loops is not done.

## 6. Report with structured evidence

Record one entry per scenario, in the PR body:

```
Scenario:  <name, and the contract that selected it>
Steps:     <the exact actions, in order>
Observed:  <what happened, including numbers such as RSS or window size>
Evidence:  <artifact: log excerpt, screenshot path, request id, command output>
Result:    pass | fail | blocked (<what was missing>)
```

Use `blocked` when a check could not run: a tool is unavailable, credentials are
missing, or there is no HPC allocation. Do not report a blocked check as a
failure. Do not report it as a pass. Name what it would take to unblock it.

Example: "spawned a claude session, killed the ws, reattached from a 2nd tab at
80×24 → identical screen; `just check` green." That sentence, with its evidence,
is the deliverable reviewers look for.
