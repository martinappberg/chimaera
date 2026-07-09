---
description: Hard constraints for the Svelte web UI — verify live (no JS tests), sanitize untrusted agent output, theme tokens, canonical agent vocabulary, runes discipline.
paths: ["web-ui/**"]
---

# Web-UI rules

Svelte 5 (runes). Build/check needs **Node 22** (`nvm use 22`; the nvm default 16
errors). Depth: [chat/CLAUDE.md](../../web-ui/src/lib/chat/CLAUDE.md),
[settings/CLAUDE.md](../../web-ui/src/lib/settings/CLAUDE.md).

- **There are no JS tests** — only `svelte-check` (types) + the live preview. So a UI
  change is only "verified" once you've driven it in the preview (see **verify-app** /
  **debug-live-app**). Don't claim a UI change works off `svelte-check` alone.
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
