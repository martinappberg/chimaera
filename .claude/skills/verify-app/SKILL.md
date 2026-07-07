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
just check         # cargo fmt --check + clippy -D warnings + cargo test --workspace
npm --prefix web-ui run check      # svelte-check, if you touched the UI
```

`crates/chimaera-pty` has real PTY tests (`tests.rs`, `snapshot.rs`) — extend
them when you change terminal state or snapshotting.

## 2. Drive the real flow

Bring up the dev loop (see the **develop** skill), then exercise the *actual*
path you changed. Use the `preview_*` tooling to observe: `preview_snapshot`
for structure/content, `preview_console_logs` + `preview_logs` for errors,
`preview_network` for API/WS traffic, `preview_screenshot` for proof.

Match the exercise to what you touched:

- **Terminal / PTY** — spawn a session, type, scroll. Then **kill the socket and
  reattach**: the exact screen must come back (scrollback, colors, cursor, title),
  no lost bytes. Reattach from a second tab at a **different window size**.
- **Resize / resync** — change the divider drag or font size on the initiator:
  it must reflow natively with **no flicker or scroll reset**. A *foreign* client
  repaints after a coalescing window. Read DESIGN.md `## Architecture` →
  "Resize repaint refinement" for the invariants before trusting your eyes.
  (Known accepted gap: a resync while on the alternate screen can't restore the
  primary screen's scrollback.)
- **Previews** — open the file types you touched (image, markdown, csv/tsv incl.
  gzip, pdf, sandboxed html). Confirm the server streams (never loads whole files)
  and the client renders; watch RSS stays bounded on a big file.
- **Agent launch / links** — launch an agent runtime, confirm attention badges
  (needs-attention / finished / errored) fire from hooks, and that a linked
  terminal grant is scoped and audited in scrollback.
- **Reconnect** — reload the page and reattach from another machine/tab; the
  event bus should replay only the gap (seq-numbered), not the whole stream.

## 3. Resource discipline is part of "works"

The daemon runs on shared login nodes. While exercising, sanity-check it stays
within budget (~150 MB RSS target, <1 core steady-state, no runaway buffers) —
a preview of a huge Parquet/HTML file must never balloon memory. A change that
works but leaks or busy-loops is not done.

## 4. Report

State what you ran and observed, alongside the tests — e.g. "spawned a claude
session, killed the ws, reattached from a 2nd tab at 80×24 → identical screen;
`just check` green." That sentence is the deliverable reviewers look for.
