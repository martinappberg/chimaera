# Tab-switch stall harness

Measures what a tab switch costs in macOS WebKit with documents parked behind it — the 2026-09-02 stall (1.3–2 s per switch between two terminals, seconds in a busy workspace) and its three causes are written up in [the field notes](../../../docs/history/field-notes.md#the-tab-switch-stall-reproduced-and-fixed-without-screen-control-2026-09-02-safari-harness). Safari is the same system WebKit as the app's WKWebView, so no screen control is needed.

## What it measures

The driver (`web-ui/src/lib/perf/tabSwitchDrive.ts`, compiled in only with `CHIMAERA_PERF=1`) opens the given sessions and files in the focused pane, parks each document scrolled, leaves a caret in the last document and switches away (the parked-caret case), runs isolation probes (focus flip, `inert` on the active and on a parked layer, the active class, a body custom property, a stylesheet insert — the document-wide restyle reference — a sibling insert, and a select-all to see whether parked text reaches the clipboard), then switches between the two terminals 24 times, logging per switch the commit time and the engine's own caret walk from each end of the active layer (`Selection.modify`, the search WebKit's editor-state update runs on every rendering commit). It ends by revealing the first document (cost + scroll position). Everything is written to `<log-path>` through the daemon.

Terminals are warmed before documents load, and the driver waits for Markdown/editor content
before scrolling. After the dormant reveal it also measures twelve document↔terminal switches,
full-document selection, document height with/without the reading window, scrolling to the end,
and restoring a scrolled tab. Set `CHIMAERA_PERF_QUICK=1` on the runner to reduce terminal cycles
to two while iterating; use the full run to measure the 30-second dormant case.

The 2026-09-07 reproduction used three 1.37 MB Markdown documents, each containing 1,500 dense
paragraphs (108,000 inline formatting elements). Warm mixed switches before reading-windowing
took 180–349 ms (median 262.5 ms); afterward, 32–50 ms (median 45.5 ms). The dormant reveal
fell from 1,266 ms to 80 ms. These are unsampled Safari timings including two animation frames,
not hardware-independent budgets. Selection restored over one million characters; measured
document height was identical (311,298 px), and the 5,000 px scroll position survived a tab round trip.
A follow-up through a localhost proxy adding 150 ms in each direction measured 27–70 ms
mixed switches, confirming warm tab selection does not wait for the simulated 300 ms RTT.
Browser verification also covered a disk refresh while parked, full-text Find after a font
resize (identical full/windowed height), and an unsaved editor draft surviving a tab round trip.

The 2026-10-20 run (issue #327; three 1.42 MB documents of 1,500 paragraphs, 108,000 inline
elements each, Safari 26.6, a debug daemon, load average ~9 from unrelated system work) found
document→terminal switches at 527–621 ms (median 548 ms) while terminal→document stayed at
28–41 ms. The driver's per-phase split showed the cost in the first animation frame, and the
departing layer growing from 2,110 to 109,534 elements during the commit: the pane's
`DocumentFind` closed an already-closed find when its layer went `inert`, that dispatched
`chimaera-find-visibility: false`, and the reading window answered with `schedule()` — every
block restored, a full layout of the document in the next frame, and the window rebuilt the
frame after. The same bundle built from `2ead70c0` (before #324) measured 518–593 ms, so the
cost arrived with find-in-pane (#237), not with the preview lifetime change. With both ends
fixed (the window ignores a no-change event, the find announces only a real close): first
document→terminal switch 39 ms, mixed document→terminal 24–40 ms (median 37), terminal→document
18–55 ms (median 37), 24 terminal cycles 20–124 ms (median 50), the 30-second dormant reveal
107 ms, selection over 1.13 M characters, identical full/windowed height (513,042 px) and the
5,000 px restore — all from a run with Safari frontmost and no sampler.

## Run

```sh
CHIMAERA_PERF=1 npm --prefix web-ui run build      # a debug daemon serves dist from disk
# register a workspace with a few large documents and two shell sessions through the API, then:
node scripts/perf/tab-switch/run-safari.mjs http://127.0.0.1:<port> <token> <ws-id> <sessA>,<sessB> <ws>/a.md,<ws>/b.md,<ws>/c.md <ws>/timings.txt
npm --prefix web-ui run build                      # back to a harness-free bundle
```

Node-dense documents are what hurt: markdown over 1 MB (so it opens in reading mode) with `**bold**`, `` `code` `` and `*em*` on every other word — ~235k inline elements per file reproduced the live numbers. Read the sample's call graph: a layout branch means parked content is being re-laid out, `Style::TreeResolver` that it is being re-resolved, `Style::Scope::createDocumentResolver` that a stylesheet changed. An `AXObjectCache::notificationPostTimerFired` branch means an accessibility client is attached (a screen reader, an activity recorder, an accessibility-tree read of Safari): it pulls the frame's layout forward into its own timer, so the cost it shows is the frame's, not its own.

Each switch line carries its split — `commit` (the layout op and Svelte's flush, including the pool's terminal attach), `f1` and `f2` (the two animation frames) — and `leftEls`, the departing layer's element count before the switch, after the commit, and after each frame: a jump there is a reading window restoring its blocks. `CHIMAERA_PERF_SAMPLE_AT=<secs>` moves the sampler's start (25 by default; the document↔terminal phase of a quick run starts around 18).

The driver waits on animation frames, which Safari stops for a window that is fully covered or minimized (a partly visible window keeps running): a run whose log stops mid-way was covered, not slow. On a shared machine, start the run when input has been idle for a while (`ioreg -c IOHIDSystem`'s `HIDIdleTime`) and record the frontmost app alongside it with `lsappinfo front` — not through System Events, which is itself an accessibility client.
