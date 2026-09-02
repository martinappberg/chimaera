# Tab-switch stall harness

Reproduces and measures the macOS WebKit tab-switch stall (2026-09-02: 1.3–2 s per switch between two terminals behind a few parked rendered documents; 23–37 ms after the fix) without screen control: Safari uses the same system WebKit as the app's WKWebView.

## What it measures

`harness.diff` adds a temporary driver to `App.svelte` (behind a `#stalldrive=` hash flag, read from `window.__bootHash` because the app rewrites the hash at boot). After the layout loads it opens the given sessions and files in the focused pane, activates each document once (parking it), runs a set of isolation probes (focus flip, `inert`, the active class, a body custom property, a stylesheet insert — the "full document restyle" reference), then switches between the two terminals 24 times, logging per switch:

- `switch` — `activateTab` to the second `requestAnimationFrame` (the whole commit).
- `walk fwd/back` — `Selection.modify` from a caret at the active layer's last text node: the engine's own visually-distinct-candidate search, the same walk WebKit's editor-state update runs on every rendering commit.
- finally the reveal cost of the first parked document and whether its scroll position survived.

Timings are written with `PUT /api/v1/fs/file` into the workspace, so no devtools are needed.

## Run

```sh
git apply scripts/perf/tab-switch/harness.diff   # temporary — never commit it
npm --prefix web-ui run build                     # a debug daemon serves dist from disk
# register a workspace holding a few large documents (>1 MB markdown opens in reading mode) and two shell sessions via the API, then:
scripts/perf/tab-switch/run-safari.sh http://127.0.0.1:<port> <token> <ws-id> <sessA>,<sessB> <ws>/a.md,<ws>/b.md,<ws>/c.md <ws>/timings.txt
git checkout web-ui/src/App.svelte web-ui/index.html
```

Node-dense documents (many inline elements) are what hurt — `**bold**`, `` `code` `` and `*em*` on every other word; ~235k inline elements per file reproduced the live numbers. Read the sample with `sample`'s call graph: the layout branch tells you whether parked content is being re-laid out, `Style::TreeResolver` whether it is being re-resolved, `Style::Scope::createDocumentResolver` that a stylesheet changed.

## The three causes it found

1. `visibility:hidden` on parked layers inherits → every switch re-resolved both layers' subtrees and re-shaped their text (now `opacity`).
2. xterm 6 keeps a `<style>` inside the terminal element → each re-parent into the warm stash was a stylesheet remove + insert → a document-wide resolver rebuild (now hoisted to `<head>` by `termPoolRuntime`).
3. WebKit's QuickType candidate walk from a caret crosses every unselectable parked node (now stopped by the `.sel-stop` spans around each layer).
