---
description: Hard constraints for the Svelte web UI — verify live (no browser/component tests), sanitize untrusted agent output, theme tokens, canonical agent vocabulary, runes discipline.
paths: ["web-ui/**"]
---

# Web-UI rules

Svelte 5 (runes). Build/check needs **Node 22** (`nvm use 22`; the nvm default 16
errors). Depth: [chat/AGENTS.md](../../web-ui/src/lib/chat/AGENTS.md),
[dashboard/AGENTS.md](../../web-ui/src/lib/dashboard/AGENTS.md), and
[settings/AGENTS.md](../../web-ui/src/lib/settings/AGENTS.md).

`src/lib/` is grouped by concern, not flat: **`net/`** (api, ws-adjacent transport,
reconnect, native bridge, the `/ws/events` socket) · **`layout/`** (the split/pane/tab
tree + view-state) · **`previews/`** (the file-preview surfaces + their loaders) ·
**`terminal/`** (the PTY stack: xterm, pool, socket, links) · **`browser/`** (the
reverse-proxied web-app pane + its proxy client) · **`workspace/`** (workbench
domain surfaces + their stores: files tree, git, sessions, launcher, home) · **`shared/`**
(cross-cutting leaf primitives: icons + glyphs, keys/keybindings, reference/provenance). The
`chat/`, `settings/`, and `dashboard/` subsystems keep their own folders (+ maps).
`App.svelte` stays at `src/`.

- **Component/UI behavior has no browser tests.** The chat store reducer has a
  targeted Vitest suite, and `svelte-check` covers types; a visible UI change is
  still only verified once you've driven it in the preview (see **verify-app** /
  **debug-live-app**). Don't claim a UI change works from unit/type checks alone.
- **Agent output is untrusted.** Anything the model emits (prose, tool output, echoed
  file contents) is attacker-influenced. Render it through `chat/Markdown.svelte`'s
  sanitizer; never `{@html}` raw agent text; never build a live external link without
  `rel="noopener"`.
- **Canonical agent vocabulary is sacred.** Never relabel agent-native terms — `xhigh`
  stays `xhigh`. The TUI↔chat mapping mirrors what the agent actually calls things.
- **Theme tokens, not hard-coded colors** (`--fg`, `--accent`, `--edge`, …) so the
  curated light + dark both hold. UI quality is an acceptance criterion.
- **Runes discipline.** Mutate `$state` only in the owning store's methods; give every
  timer/listener an `$effect` teardown; an `$effect` that reads and writes the same
  `$state` loops.
- **Never lose a user action to a closed socket** — `socket.send` returns `false` when
  not OPEN; respect it. Reconnect replays the gap; don't invent a client-side queue.
