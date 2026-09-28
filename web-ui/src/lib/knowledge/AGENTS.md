# web-ui/src/lib/knowledge — the Knowledge view

Orientation for coding agents. The read-only view over what the agents
recorded (design: [docs/timeline-knowledge-plugins-plan.md](../../../../docs/timeline-knowledge-plugins-plan.md)
§5 and §9). Parent map: repo-root [AGENTS.md](../../../../AGENTS.md). The
store it reads lives next door in `../workspace/knowledge.ts`
(`GET /workspaces/{id}/knowledge`, refetched off the Timeline epoch nudge
and on visibility return — never polled).

## File map

| File | What it owns |
|---|---|
| `KnowledgeView.svelte` | The tab: header (counts, the source chip, the one client-side search box), the sticky section nav with counts + the "How sure" legend, and the sections in their fixed order — Where we left off · What we found (topics → `FindingRow`) · What we decided · Watch out for · Open · Guidance & memory. An empty section doesn't render; with no structured provider it is Guidance & memory plus the one attach card. |
| `FindingRow.svelte` | One finding: id · claim · meta + a Timeline-derived badge · the ladder + status word · the evidence strip (one mark per ledger row: filled supports, half refines, ring contradicts), expanding to So what / Evidence table / Open questions / open file. |
| `Ladder.svelte` | mycelium's confidence ladder as one glyph (●○○ · ●●○ · ●●● · ✕). |
| `model.ts` | Pure derivations: `searchKnowledge`, `whereThingsStand` (contradicted first, then strongest, then newest), `sectionNav`, the ledger summary/caption, the tone tables (colour is never alone), `recentStatusMove` (the Timeline cross-link). Vitest: `model.test.ts`. |

## Invariants / gotchas

- **Agents write, Chimaera reads.** Nothing here writes a knowledge file;
  "open file" is the user's correction path. The confidence ladder is
  mycelium's, derived from the evidence ledger — never computed here.
- **Everything shown is agent/file text.** One-liners go through
  `shared/inlineMarkdown.ts` (escape-then-format), bodies through
  `chat/Markdown.svelte` (sanitized). Never raw `{@html}`.
- **Empty never shows chrome.** Sections and nav rows exist only with
  content; the no-provider state is one card with the single action that
  fills it (the attach sheet, `plugins/store.ts`'s `openAttachSheet`).
- **Paths on the wire are workspace-relative** (`.living/…`, `todo/…`) or
  absolute (claude's memory dir); join with `wsRoot` before opening. There is
  no open-at-line — the file just opens.
