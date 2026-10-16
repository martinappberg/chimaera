# web-ui/src/lib/shared — cross-cutting primitives

Orientation for coding agents. This area owns leaf modules that many areas import: icons and glyphs, small UI controls, dialogs and menus, keys and keybindings, file references and locators, the reference/provenance bridge, link and path policies, the math and Mermaid policies, embed cards, find, clipboard, and visibility. A module here must not depend on a surface area's internals. It does not own any pane surface. Parent map: repo-root [AGENTS.md](../../../../AGENTS.md). Rules: [.claude/rules/web-ui.md](../../../../.claude/rules/web-ui.md). Contracts these primitives implement (focus, dialogs, states, visibility, a11y): [ui-contracts.md](../../../../docs/agent-guides/ui-contracts.md).

## File map

| Concern | Files |
|---|---|
| Glyphs and marks | `icons.ts` (generated, see below), `FileIcon.svelte`, `FolderIcon.svelte`, `TerminalIcon.svelte`, `SessionGlyph.svelte`, `BrandMark.svelte`, `Chevron.svelte`, `BranchChip.svelte`, `agentNames.ts` (fallback agent names) |
| Controls | `Switch.svelte`, `Segmented.svelte`, `SlowLoad.svelte`, `UsageBars.svelte` + `usage.ts`, `WorkTray.svelte` + `WorkTrayRow.svelte`, `ExtensionCode.svelte` + `extensionCode.ts`; `ui/` (`Badge`, `Callout`, `FileCard`, `KeyValue`, `Row`, `tone.ts`: the `ui/1` props shared with Knowledge and plugins) |
| Dialogs, menus, focus | `ConfirmDialog.svelte`, `contextMenu.svelte.ts` + `ContextMenuHost.svelte` (the one context menu), `dismiss.ts` (outside-click/Escape action), `modalFocus.ts`, `focusOnMount.ts`, `toolbarPopover.ts`, `tabNavigation.ts`, `scrollRegion.ts` |
| Keys | `keys.ts` (the chord engine: every action and default chord), `keybindings.ts` (`keyHint`, `matchAction` over live settings), `chordHints.svelte.ts` (held-modifier hints) |
| References and links | `fileRef.ts` (the one file-mention parser), `locator.ts` (the `#L12` / `#page=` grammar), `reference.ts` (selection → agent input text), `references.ts` (ids as links), `provenance.ts` (copy provenance), `ReferenceChip.svelte`, `ReferenceButton.svelte`, `askAgent.ts` (draft into a composer, never send), `openPath.ts` (open a daemon path; App registers the opener), `reveal.ts` (one-shot scroll-to-line request), `urlOpen.ts` (link policy: proxyable → browser pane, else system browser) |
| Rendering policies | `math.ts` (KaTeX → sanitized MathML), `mermaid.ts`, `inlineMarkdown.ts` (escape-first inline subset), `copyDecor.ts`, `pdfCanvas.ts` (PDF raster limits) |
| Embeds | `embed/` — `EmbedCard.svelte` (one card for any embedded file), `embed.ts` (`POST /fs/resolve_targets` client), `fragment.ts`, `mount.svelte.ts` (`mountEmbed` for raw-DOM hosts), `pdfEmbed.ts`, and one body per file kind: `CodeBody`, `HtmlBody`, `ImageBody`, `MarkdownBody`, `MediaBody`, `NotebookBody`, `PdfBody`, `SlideBody`, `TableBody` (`.svelte`) |
| Find | `find.ts` (`findTarget` registry), `FindBar.svelte`, `DocumentFind.svelte`, `domFind.ts`, `findText.ts` |
| Editing and IO | `editing.ts` (`dirtyFiles`, `daemonLinkUp`, close-with-unsaved helpers), `clipboard.ts` (native-first copy), `fsNames.ts` (name validation) |
| Time and lifecycle | `visibility.ts` (`pageVisible`), `liveClock.svelte.ts` (one shared 1 Hz clock), `time.ts`, `format.ts` (token counts), `asyncDisposer.ts` |

Tests: `*.test.ts` beside the modules; `fileRefs.fixture.json` feeds `fileRef.test.ts`; `nativeDialogs.test.ts` scans all sources.

## State owners

| State | Owner | Who mutates |
|---|---|---|
| `dirtyFiles` | `editing.ts` | Mirrored from `previews/buffers.svelte.ts`; nothing else writes it. |
| `pageVisible` (readable) | `visibility.ts` | The document `visibilitychange` event. |
| Context menu | `contextMenu.svelte.ts` | `contextMenu.openAt(e, items)` from any surface. |
| `activeSelection`, `referenceTarget` | `reference.ts` | Its exported functions. |
| `revealRequest` | `reveal.ts` | `requestReveal`. |
| `findTargetsChanged` | `find.ts` | `findTarget` registration. |
| Path opener, URL pane opener | `openPath.ts`, `urlOpen.ts` | App registers the handler once. |

## Invariants / gotchas

- **`icons.ts` is generated** by `web-ui/scripts/gen-icons.mjs`. Do not edit it by hand.
- **Never call `window.confirm`, `alert`, or `prompt`.** Use `ConfirmDialog.svelte`. `nativeDialogs.test.ts` fails on a new call (rule bullet).
- **One math policy.** Every equation goes through `math.ts`: KaTeX with trust off, then DOMPurify with `<style>` and `style=` forbidden. Keep `math.ts` a leaf: `vite.config.ts` pins it to its own chunk so previews load it on demand.
- **`inlineMarkdown.ts` escapes first.** Agent text is untrusted; the only tags in its output are the ones it adds (module header). For block markdown use `chat/Markdown.svelte` (rule bullet).
- **One link policy.** A proxyable URL (loopback, or a host with an explicit port) opens in a browser pane; any other URL opens in the system browser (`urlOpen.ts` header).
- **One file-reference grammar.** Chat, terminals, and markdown parse mentions with `fileRef.ts` and locators with `locator.ts`, so the surfaces cannot drift.
- **`askAgent` drafts and never sends** (`askAgent.ts` header).
- **A reveal is a one-shot gesture.** It is not tab or layout state and is never persisted (`reveal.ts` header).
- **Gate recurring work on `$pageVisible`** in components. See the rule bullet on recurring work.

## Checks

```sh
npm --prefix web-ui run check
npm --prefix web-ui run test -- src/lib/shared
```

A change here reaches many surfaces. Drive at least one consumer of the changed module live (**verify-app**).

## Neighbours

[net](../net/AGENTS.md) · [layout](../layout/AGENTS.md) · [previews](../previews/AGENTS.md) · [terminal](../terminal/AGENTS.md) · [browser](../browser/AGENTS.md) · [workspace](../workspace/AGENTS.md) · [chat](../chat/AGENTS.md) · [knowledge](../knowledge/AGENTS.md) · [plugins](../plugins/AGENTS.md).
