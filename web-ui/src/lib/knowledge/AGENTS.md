# web-ui/src/lib/knowledge — the Knowledge view

Orientation for coding agents. The read-only view over what agents recorded
through a knowledge plugin (design:
[docs/knowledge-redesign-plan.md](../../../../docs/knowledge-redesign-plan.md),
feature page [timeline-and-knowledge.md](../../../../docs/features/timeline-and-knowledge.md)).
Parent map: repo-root [AGENTS.md](../../../../AGENTS.md). The store it reads
lives next door in `../workspace/knowledge.ts` (`GET /workspaces/{id}/knowledge`,
refetched off the Timeline epoch nudge and on visibility return — never polled).

## File map

| File | What it owns |
|---|---|
| `KnowledgeView.svelte` | The tab: header (counts, the plugin's source chip, one search box), the section nav (the plugin's section names; Tidy up muted with its count), and the main area — `Overview`, a section's `EntryList` + `EntryReader`, search results across kinds, or `TidyList`. Selection, back/forward history, keyboard (`j`/`k`, `/`, `[`/`]`, `Esc`), narrow-pane list/reader swap, the hover previews for chips, and taking `knowledgeFocus` requests. Without a provider: one line and the attach action for the plugin with `provides.knowledge`. |
| `Overview.svelte` | Where we left off (the handoff's slots drawn as markdown, older handoffs) · Waiting on you (`asks`) · What changed (7 days, bad news first) · Open work. |
| `EntryList.svelte` | A section's filter chips and rows (the shared `Row`): findings grouped by topic, to-dos by status (done folded), the rest newest first. |
| `EntryReader.svelte` | One entry: nav + crumb + open in file, standing / amends / reused-id callouts, the status as written, facts (to-do, session), What backs it (`FileCard`s: resolving paths open), the body (`EntryBody`), follow-ups, Referenced by. |
| `EntryBody.svelte` | Fetches the entry's file (`render.ts`, cached per snapshot), draws its `span` through the reading renderer, links ids (`linkReferences`); a fallback markdown without a span. |
| `StatusMark.svelte` | A status exactly as written, with the plugin's ladder glyph only for the plugin's own status words. |
| `TidyList.svelte` | The plugin's `tidy` rows; "Ask an agent" drafts the row's `ask` via `shared/askAgent.ts`. |
| `entries.ts` | `buildIndex`: every entry of every kind as one `Entry` envelope (`ekey` unique), `byId`, backlinks from `refs`; `resolveRef` (same topic first), `searchEntries`, `qualifiedId`. Vitest. |
| `overview.ts` | `providerLabels` (the plugin's `labels` over neutral defaults), `statusWord`, `kindWord`, `stateLabel`, `whatChanged`, `waitingOnYou`, `openWork`, to-do grouping and priorities. Vitest. |
| `list.ts` | `filtersFor`, `listGroups`, `todoRest`. Vitest. |
| `render.ts` | `entrySource` (file text per snapshot), `entryMarkdown` (a span's lines, heading dropped), `drawMarkdown` (the reading renderer into an element). |
| `store.ts` | `knowledgeLookup`: the active snapshot + its index, derived once per snapshot (Knowledge view, dashboard). |
| `references.ts` | Registers the snapshot as the first source of `shared/references.ts` (its `id_shapes`; targets open Knowledge at the entry). Imported once by `App.svelte`. |
| `snapshot.fixture.ts` | The `knowledge/1`-shaped snapshot the pure tests share (test-only). |

## Invariants / gotchas

- **Agents write, Chimaera reads — and never rates.** Nothing here writes a
  file; "open in file" and "Ask an agent" are the correction paths. A status is
  shown exactly as the agent wrote it (`stated`); rank and tone only from the
  plugin's `labels.status_words`. Never compute, map or "fix" a status.
- **Core names no plugin.** No plugin id, file name or status word in this
  folder: words come from `labels` (`providerLabels` supplies neutral kind
  names only), the provider is "the active plugin with `provides.knowledge`"
  (`plugins/store.ts` `knowledgePlugin`). Tones are the `ui/1` set
  (`shared/ui/tone.ts`).
- **Other surfaces ask the registry, not this store.** Chat, previews and the
  Timeline resolve ids through `shared/references.ts`; only this view and the
  dashboard's Knowledge card read `knowledgeLookup`.
- **Everything shown is agent/file text.** One-liners through
  `shared/inlineMarkdown.ts`; bodies through the reading renderer
  (`previews/doc`), whose pipeline sanitizes. Never raw `{@html}` of file text.
  A chip's target lives in a `ReferenceChips` WeakMap, never in DOM attributes.
- **Ids repeat; keys don't.** Every list and element is keyed by `ekey`
  (`<kind>:<normalized key>`); a finding's key is the provider's `<topic>/<id>`.
  A repeated `{#each}` key throws and used to freeze the whole window, so
  `normalizeKnowledge` lists a provider's amends, guidance files and handoff
  sources once each (the daemon also dedupes the guidance it merges).
- **A body is as fresh as the snapshot object.** `render.ts` caches each
  file per snapshot; the daemon's `read` digest (the provider's stamp:
  every read file's path, mtime and length) makes the response bytes differ
  whenever any file did, so a reworded paragraph that leaves the snapshot's
  fields alone still yields a new object and a re-read.
- **Spans are `{path, line, end_line}`**, workspace-relative, 1-based and
  inclusive; `end_line` 0 means the whole file (a session log); a one-line
  span (a to-do table row) has no body of its own.
- **A failed refresh is quiet:** the daemon keeps `provider` and adds `error`
  to the last snapshot; the view shows one muted line.
