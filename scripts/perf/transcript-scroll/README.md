# Transcript scroll harness

Measures what scrolling a long chat transcript feels like, frame by frame, in the engines that matter: macOS WebKit (the native app's WKWebView) driven by **real trackpad-style wheel events**, and Chromium driven by its compositor. It reproduced the 2026-09-25 "jumps and isn't smooth" report and verified the fix — written up in [the field notes](../../../docs/history/field-notes.md#long-transcripts-scroll-without-jumps-2026-09-25-webkit-wheel-harness). Neither a unit test nor the Browser pane can see this: the bug lives in WebKit's scrolling thread, which only real wheel events with gesture and momentum phases exercise, and a hidden pane stops rendering frames entirely.

## Pieces

- `wkscroll.swift` — a floating WKWebView that loads the app, runs a page script, then feeds wheel events through `WKWebView.scrollWheel(with:)` built from `CGEvent` scroll events carrying scroll and momentum phases (creating events needs no Accessibility permission; posting them globally would). Scenario items: `up:<events>:<px>` / `down:…` (a trackpad drag), `flingup:<px>` / `flingdown:<px>` (a flick plus a decaying momentum tail), `wait:<ms>`, `js:<expr>` (no commas — the scenario is comma-separated).
- `cdp-scroll.mjs` — the Chromium twin: headless Chrome over raw CDP with `Input.synthesizeScrollGesture`. Scenario items `up:<px>:<px/s>`, `down:…`, `wait:`, `js:`.
- `page/setup.js` — opens the session named by `?scrollSession=` (workspace `?scrollWorkspace=`), pins it to the live bottom, and logs each frame's scroll offset, window start, spacer, and every visible row's position. Set `window.__dir` per phase (`js:window.__dir=-1` before scrolling down; `0` = sitting still; `9` = following the tail).
- `page/jumps.js` (jumps, lost positions, edge hits, frame intervals, scroll-handler cost), `page/stream.js` (follow cost, yanks, reader drift), `page/drag.js` (far jump: blank frames, time to content), `page/top.js` (the end of history: blank space, where block 0 landed).
- `fake-agent.mjs` — a billing-free stream-json Claude: every turn gets 0–3 tool calls plus markdown of random length (paragraphs, lists, fenced code, tables); a prompt containing "slow" streams a long reply over ~15 s. `send-slow.mjs` sends that prompt.
- `ab.sh` — runs one scenario against two built UIs (`BUILD_A`, `BUILD_B`) on the same debug daemon, which reads `web-ui/dist` from disk.

## Run

```sh
bash .claude/skills/develop/serve-isolated.sh            # note the port + #token URL
printf '#!/bin/bash\nexec node %s "$@"\n' "$PWD/scripts/perf/transcript-scroll/fake-agent.mjs" > /tmp/fake-agent.sh && chmod +x /tmp/fake-agent.sh
# PUT {"agents.claude.path":"/tmp/fake-agent.sh"} to /api/v1/settings, POST a workspace,
# POST /api/v1/sessions {"workspace_id":…,"kind":"agent","agent":"claude","ui":"chat","name":"scroll-test"}
CHIMAERA_PORT=<port> CHIMAERA_TOKEN=<token> node scripts/perf/pump-turns.mjs <sessionId> 150
URL='http://127.0.0.1:<port>/?scrollSession=scroll-test#token=<token>' BUILD_A=<old dist copy> BUILD_B=<new dist copy> \
  bash scripts/perf/transcript-scroll/ab.sh "$(printf 'flingup:90,wait:120,%.0s' {1..10})js:window.__dir=-1,$(printf 'flingdown:90,wait:120,%.0s' {1..10})wait:300"
```

The window is a real on-screen (floating, accessory) window: WebKit only reports the page visible, and only paints at display rate, under a real `NSApp.run()` loop. Streams grow the fixture's tail, so interleave A/B runs (`B A B A`) when comparing streaming numbers. WebKit's `performance.now()` is 1 ms-granular; use the Chromium runner for sub-millisecond handler costs.
