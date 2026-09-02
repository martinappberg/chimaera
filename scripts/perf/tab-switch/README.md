# Tab-switch stall harness

Measures what a tab switch costs in macOS WebKit with documents parked behind it — the 2026-09-02 stall (1.3–2 s per switch between two terminals, seconds in a busy workspace) and its three causes are written up in [the field notes](../../../docs/history/field-notes.md#the-tab-switch-stall-reproduced-and-fixed-without-screen-control-2026-09-02-safari-harness). Safari is the same system WebKit as the app's WKWebView, so no screen control is needed.

## What it measures

The driver (`web-ui/src/lib/perf/tabSwitchDrive.ts`, compiled in only with `CHIMAERA_PERF=1`) opens the given sessions and files in the focused pane, parks each document scrolled, leaves a caret in the last document and switches away (the parked-caret case), runs isolation probes (focus flip, `inert` on the active and on a parked layer, the active class, a body custom property, a stylesheet insert — the document-wide restyle reference — a sibling insert, and a select-all to see whether parked text reaches the clipboard), then switches between the two terminals 24 times, logging per switch the commit time and the engine's own caret walk from each end of the active layer (`Selection.modify`, the search WebKit's editor-state update runs on every rendering commit). It ends by revealing the first document (cost + scroll position). Everything is written to `<log-path>` through the daemon.

## Run

```sh
CHIMAERA_PERF=1 npm --prefix web-ui run build      # a debug daemon serves dist from disk
# register a workspace with a few large documents and two shell sessions through the API, then:
node scripts/perf/tab-switch/run-safari.mjs http://127.0.0.1:<port> <token> <ws-id> <sessA>,<sessB> <ws>/a.md,<ws>/b.md,<ws>/c.md <ws>/timings.txt
npm --prefix web-ui run build                      # back to a harness-free bundle
```

Node-dense documents are what hurt: markdown over 1 MB (so it opens in reading mode) with `**bold**`, `` `code` `` and `*em*` on every other word — ~235k inline elements per file reproduced the live numbers. Read the sample's call graph: a layout branch means parked content is being re-laid out, `Style::TreeResolver` that it is being re-resolved, `Style::Scope::createDocumentResolver` that a stylesheet changed.
