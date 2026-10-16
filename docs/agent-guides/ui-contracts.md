# UI contracts

The contracts that the shared web-UI primitives already implement. Read it
before you add a control, dialog, loading state or background task. Paths are
relative to `web-ui/src/`. **Contract** is what a surface must do; **Observed**
is what the code does now; proposals are only in [Gaps](#gaps). The hard rules
stay in [.claude/rules/web-ui.md](../../.claude/rules/web-ui.md); the detail
stays in the [shared map](../../web-ui/src/lib/shared/AGENTS.md).

## 1. Theme tokens, type roles, density and layout

**Primitive.** CSS custom properties on `:root` in `app.css`, with the full
color set per theme in `lib/settings/themes.ts` (`TOKEN_NAMES`, `THEMES`).
The settings store applies the selected theme inline on `<html>`. The `app.css`
color values are only the no-JS fallback.

**Contract.**

- Use a token for every color (`TOKEN_NAMES`: `--bg`, `--fg`, `--muted`,
  `--accent`, `--err`, `--warn`, `--edge`, `--row-hover`, …). Add a new
  themable color to `TOKEN_NAMES` and to every theme, not to `app.css`.
- Use `--accent-text` for accent-colored text. It keeps 75% `--accent` and
  mixes in 25% `--fg`, so that text holds 4.5:1 (`app.css:35-39`).
- Use `--focus-ring` for every keyboard focus outline (`app.css:33-34`).
- Use the four type roles: `--text-xs` (tabs, titles, badges), `--text-sm`
  (monospace names, hints), `--text-md` (rows, buttons, inputs) and `--text-lg`
  (body text). `lib/settings/store.svelte.ts:351-355` derives them from
  `appearance.interfaceFontSize` (11–18 px, default 13.5). Do not hard-code a
  chrome font size. Chat, terminal and editor have their own size settings.
- Use `--mono`, `--ui-font` and `--editor-font` for type faces;
  `--radius-card`, `--overlay-radius`, `--overlay-shadow` and `--hover-tint`
  for surfaces; `--pane-tab-height`, `--pane-toolbar-height` and
  `--pane-control-size` for pane chrome (`app.css:757-764`).

**Contrast.** `lib/settings/themes.contrast.test.ts` holds every theme to
4.5:1 against `--term-bg` for `--fg`, `--muted`, `--err`, `--warn`, the
`--syn-*` text tokens, `--accent-text` and normal ANSI colors. Lower floors:
3.8:1 `--syn-comment`, 3.5:1 bright ANSI, 3:1 `--rate`. `--fg` and `--muted`
are also checked on `--bg`, `--rail-bg` and `--row-active` (`--muted` on
`--row-active`: 4.0:1).

**Observed.** There are no spacing tokens. Density is one setting,
`dashboard.cardDensity`, read only by `lib/dashboard/DashboardView.svelte:107`.
Narrow layouts use container queries in Settings (for example
`lib/settings/SettingsView.svelte:708`) and per-component media queries.

**Example** (`lib/shared/ui/Callout.svelte:27`):

```css
font-size: var(--text-sm);
```

## 2. Buttons, dialogs, popovers, menus, tabs, selection, destructive actions

| Need | Primitive | Call |
|---|---|---|
| Confirm a destructive action | `lib/shared/ConfirmDialog.svelte` | `<ConfirmDialog title body confirmLabel danger error onConfirm onCancel />` |
| Right-click menu | `lib/shared/contextMenu.svelte.ts`, rendered once by `ContextMenuHost.svelte` | `contextMenu.openAt(e, items)`; `contextMenu.openAtPoint(x, y, items, { alignRight })` for a button |
| Toolbar picker | `lib/shared/toolbarPopover.ts` | `use:toolbarPopover={{ onClose }}` on the popup surface (`role="menu"` or `role="dialog"`) |
| In-pane find | `lib/shared/FindBar.svelte` | `<FindBar />` inside a `findTarget` host; its `role="search"` handles Escape for every control inside it |
| Close on outside press or Escape | `lib/shared/dismiss.ts` | `use:dismiss={{ enabled, onDismiss, keepOpenWithin }}` |
| One of N | `lib/shared/Segmented.svelte` | `<Segmented options value label onChange />` (`role="radiogroup"`) |
| On/off | `lib/shared/Switch.svelte` | `<Switch on label onToggle />` (`role="switch"`) |
| Tab strip | `lib/shared/tabNavigation.ts` | `use:tabNavigation` on the `role="tablist"` element |
| Toned note | `lib/shared/ui/Callout.svelte`, `ui/tone.ts` | `<Callout tone title text />`; a tone always carries its word |

**Contract.**

- Never call `window.confirm`, `alert` or `prompt`. The native app does not
  show them. `lib/shared/nativeDialogs.test.ts` fails on a new call.
- Mark a destructive action with `danger`: the `ConfirmDialog` prop or the
  `danger: true` field of a context-menu item
  (`lib/workspace/FileTree.svelte:885`).
- Keep a dialog open on failure. Pass the reason in `error`.
- Give a toolbar popup an opener with `aria-haspopup`. `toolbarPopover` places
  the popup, moves focus into it, handles arrow keys, Home and End, and gives
  focus back to the opener on close.
- Give `Segmented` and `Switch` an accessible name in `label`.

**Observed.** `ConfirmDialog` has 9 uses in 6 files. Its focus starts on Cancel
unless `enterConfirms` is set. `toolbarPopover` does not handle Escape. Its
hosts pair it with `dismiss` (`lib/chat/ChatView.svelte:2895`,
`lib/previews/DocIssues.svelte:171`).

**Example** (`App.svelte:6751-6758`):

```svelte
<ConfirmDialog
  title={$pendingDelete.kind === "dir" ? "Delete folder" : "Delete file"}
  body={`Permanently delete “${basename($pendingDelete.path)}”${
    $pendingDelete.kind === "dir" ? " and everything inside it" : ""
  }? This cannot be undone.${unsavedDeleteNote($pendingDelete.path, $dirtyFiles)}`}
  confirmLabel="delete"
  danger
```

## 3. Focus, Escape and keyboard navigation

**Primitives.**

- `modalFocus` (`lib/shared/modalFocus.ts`): Tab containment, restoration to
  the opener on close, stacked modals by `priority`, and modals parked in an
  `inert` keep-alive layer. `modalOpen()` tells App that a modal owns the
  keyboard, and App's chords stand down (`App.svelte:2834`).
- `focusOnMount` (`lib/shared/focusOnMount.ts`): initial focus, one microtask
  after mount, so `modalFocus` records the real opener.
- `keys.ts` is the chord registry (`ACTIONS`). `keybindings.ts` joins it with
  the live settings: `matchAction(e)` to match, `keyHint(id)` and
  `keyHintSuffix(id)` to show a chord.
- `chordHints.svelte.ts`: `hintsActive()` while the user holds the modifier.
- `scrollRegion.ts`: `markScrollRegions(root, targets)` makes a horizontal
  scroller a Tab stop only while it overflows.

**Contract.**

1. Choose the initial focus with `focusOnMount`. For a destructive dialog, put
   it on the safe button.
2. Add `use:modalFocus` to every `aria-modal="true"` element.
3. Handle Escape in the component. `modalFocus` does not handle it. Call
   `e.stopPropagation()` so that an outer surface does not also close.
4. If a button disables itself while it works, hold focus on the dialog, so
   that Escape still reaches it (`lib/layout/CloseDirtyDialog.svelte:36-40`).
5. Read chords through `matchAction`. Do not compare `e.key` with a modifier by
   hand for a rebindable action.

**Example** (`lib/layout/CloseDirtyDialog.svelte:68` and `:90`):

```svelte
use:modalFocus
<button class="opt primary" disabled={saving} use:focusOnMount onclick={onSave}>
```

## 4. Loading, empty, partial, stale, error, retry, cancelled

**Primitives.**

- `SlowLoad.svelte`: a `role="status"` line that shows only after 800 ms. Put
  it in an `{#await}` pending branch.
- `lib/layout/Pane.svelte`: every pane surface renders inside a
  `<svelte:boundary>` (`:683-688`). A render error shows `viewCrash` with
  "try again" (the boundary `reset`) and "reload window". A failed lazy chunk
  shows `loadFailure` with "retry" (`:353-359`).
- Settings writes: `lib/settings/settingsWriter.ts` `SavePhase`
  (`idle | pending | inflight | failed | rejected`). Settings shows an
  "unsaved" button only for `failed` or `rejected`; a press calls
  `flushSettings()` (`lib/settings/SettingsView.svelte:255-260`).
- Editor saves: `lib/previews/buffers.svelte.ts` `SaveState`
  (`idle | saving | retrying | offline | failed`) and `Conflict`
  (`deleted` or `changed`, for a disk change under unsaved edits).
- Connection: `lib/net/reconnect.ts` (`Reconnector`, `retryDelayMs`) and
  `lib/net/poll.ts` (`startVisibilityPoll`). App shows a `role="status"` reconnect
  strip, or a `role="alertdialog"` with retry when reconnect fails
  (`App.svelte:6905-6950`).

**Contract.** A surface that loads shows each state in words. Do not flash a
loading line for a fast load. Show an error with the reason and one recovery
action (retry, reload or dismiss). Show a stale or conflicting state; do not
replace the user's text without asking.

**Observed.** Previews write their own loading and error text (for example
`lib/previews/PdfView.svelte:1686`). `SlowLoad` has two uses, both in
`App.svelte`.

**Example** (`lib/layout/Pane.svelte:683-688`):

```svelte
<svelte:boundary onerror={(e) => console.error("pane view failed", e)}>
  {@render surface(tab, active)}
  {#snippet failed(error, reset)}
    {@render viewCrash(error, reset)}
  {/snippet}
</svelte:boundary>
```

## 5. Unsaved edits and recovery

**Primitives.**

- `lib/shared/editing.ts`: `dirtyFiles` (the window's unsaved paths, mirrored
  from the buffer store), `daemonLinkUp`, `saveDirtyFile`, `discardDirtyFile`,
  `unsavedDeleteNote`, `unsavedCloseError` and `CLOSE_SAVE_DEADLINE_MS`
  (15 s).
- `lib/layout/CloseDirtyDialog.svelte`: Save, Don't save, Cancel. Every close
  gesture routes here, and so do the native window close and quit
  (`lib/layout/windowClose.svelte.ts`).
- `lib/previews/buffers.svelte.ts`: a dirty `Buffer` outlives its view,
  retries a save once across a dead link and journals its text
  (`lib/previews/drafts.ts`: IndexedDB and the daemon's `/fs/drafts`).
- `lib/chat/drafts.ts`: composer drafts are keyed by session. Text up to 64 KB
  is mirrored to `sessionStorage`. `volatileChatDrafts` lists drafts that a
  reload would lose (images, larger text, storage failures).

**Contract.**

- Do not close, evict or delete a dirty file without the user's choice. Pane
  keep-alive never evicts it.
- Name the files that a delete would discard with `unsavedDeleteNote`.
- Let Cancel work while a save is in flight. A save across a dead link must
  not trap the user.
- App defers its automatic asset reload, and the native Reload Window, while
  `dirtyFiles` or `volatileChatDrafts` is not empty, unless the transition is
  forced (`App.svelte:1597`). App installs a `beforeunload` guard while
  `dirtyFiles` is not empty (`:3208-3216`).

**Example** (`App.svelte:6782-6790`):

```svelte
<CloseDirtyDialog
  paths={windowClose.prompt.paths}
  saving={windowClose.saving}
  error={windowClose.error}
  action={windowClose.prompt.reason}
  onSave={() => void windowClose.save()}
  onDiscard={() => windowClose.discard()}
  onCancel={() => windowClose.cancel()}
/>
```

## 6. Visibility and disposal of pooled views and background work

**Primitives.**

- `lib/shared/visibility.ts`: `pageVisible`, a store that is `true` while the
  document is visible.
- The `html.app-hidden` root class (`App.svelte:1568-1571`). Each infinite
  animation pauses itself with a `:global(html.app-hidden)` rule beside it.
- `lib/shared/asyncDisposer.ts`: `asyncDisposer(promise)` turns an async
  unlisten (Tauri `listen()`) into a synchronous teardown.
- `lib/terminal/termPool.ts` and `termPoolRuntime.ts`: one warm xterm per
  session, at most `POOL_CAP` (12). A parked terminal buffers raw bytes and
  does not parse them.
- Pane keep-alive (`lib/layout/Pane.svelte:680`): one `.layer` per tab, hidden
  with `opacity: 0` and `inert={!active}`.

**Contract.** Follow the recurring-work and keep-alive bullets in
[.claude/rules/web-ui.md](../../.claude/rules/web-ui.md):

1. Gate a component ticker on the pane's `visible` prop and `$pageVisible`.
   Refresh in the effect's first statement; the re-run is the catch-up.
2. Give every timer and listener an `$effect` teardown.
3. Hide a keep-alive view with `opacity: 0` and `inert`. Do not use
   `display: none`, `visibility: hidden` or a `z-index` on the shown layer.
4. Do not steal focus from a hidden view. Check the `visible` prop first.

**Example** (`lib/settings/UpdatesStatus.svelte:65-70`):

```ts
$effect(() => {
  if (!visible || !$pageVisible) return;
  now = Date.now();
  const timer = setInterval(() => (now = Date.now()), 30_000);
  return () => clearInterval(timer);
});
```

## 7. Accessibility names, announcements, contrast, motion and zoom

**Observed.**

- Names: `Segmented` and `Switch` take `label`. A one-time scan on 2026-10-16
  (by hand over every `<button>` in `web-ui/src`; no script re-checks it)
  found no `<button>` whose only content is an icon or glyph and that has
  neither `aria-label` nor `title` (92 such buttons, all named).
- Announcements: there is no shared announcer. Surfaces use `role="status"`
  (75 lines), `role="alert"` (33) and `aria-live` (22) directly. The chat
  transcript is `role="log"` (`lib/chat/ChatView.svelte:2953-2957`).
- Contrast: see [section 1](#1-theme-tokens-type-roles-density-and-layout).
- Reduced motion: 85 `prefers-reduced-motion` blocks. `app.css` defines the
  shared `rise`, `pulse` and `label-pulse` keyframes; each component that uses
  one adds its own `prefers-reduced-motion` override on the animated element.
- Zoom: `appearance.interfaceFontSize` scales the chrome. `fontChord`
  (`lib/shared/keys.ts:556`) steps the text size of a focused terminal or
  markdown pane. Everywhere else, browser zoom keeps working
  (`App.svelte:2835-2837`).

**Contract.** Give every control an accessible name. Announce a transient
outcome with `role="status"` and `aria-live="polite"`. Add a
`prefers-reduced-motion` override for every new animation, and an
`html.app-hidden` pause for an infinite one.

**Example** (`App.svelte:6824`):

```svelte
<div class="flash-chip" role="status" aria-live="polite">{flash}</div>
```

## 8. Product terms and provider-native vocabulary

**Contract.**

- Use the stable product terms: workspace, session, window, pane, host, job
  and provider.
- Show a provider's own words verbatim: model ids, effort levels (`xhigh`
  stays `xhigh`), permission modes and tool names. Do not relabel them
  ([.claude/rules/web-ui.md](../../.claude/rules/web-ui.md)).
- Show an agent's product name from the daemon catalog
  (`GET /api/v1/agents` → `name`). Use `agentProductName(id)`
  (`lib/shared/agentNames.ts`) only before the catalog arrives or for an agent
  that the catalog no longer lists. An unknown id stays verbatim.

**Example** (`lib/chat/EffortPopover.svelte:22`, each level as the agent
reports it):

```svelte
<span>{level}</span>
```

## Gaps

These are proposals, not current behavior. None of them is built.

1. **Spacing and breakpoints.** There are no spacing tokens and no shared
   breakpoints. Components use literal pixel values and their own queries.
2. **Radio groups.** `Segmented` has `role="radiogroup"` but no arrow-key
   navigation; each option is its own Tab stop
   (`lib/shared/Segmented.svelte:31-45`). Four other `role="radiogroup"`
   elements do the same.
3. **Tab strips.** `tabNavigation` has three consumers (`lib/layout/PaneTabs.svelte`,
   `lib/chat/ModsWorkbench.svelte`, `lib/previews/MarkdownView.svelte`). Ten other tab
   lists in eight files handle keys ad hoc or not at all:
   `lib/settings/SettingsView.svelte:268`, `lib/plugins/ui/PluginFileGate.svelte:148`,
   `lib/plugins/ui/UiNode.svelte:367`, `lib/previews/XlsxView.svelte:130`,
   `lib/previews/BoardView.svelte:450`, `lib/previews/HtmlView.svelte:125`,
   `lib/previews/ParquetView.svelte:142`, `lib/previews/FileView.svelte:275`, `:286`
   and `:297`.
4. **Modal focus.** Two `aria-modal="true"` overlays focus a panel on open but
   do not use `modalFocus`: `lib/chat/ImagePreview.svelte:35` and
   `lib/chat/PlanApprovalCard.svelte:136`. Their Tab containment and focus
   restoration are not verified.
5. **Escape.** Each dialog handles Escape itself on its backdrop (for example
   `lib/shared/ConfirmDialog.svelte:52-56`). A shared helper could own it.
6. **Preview states.** No shared loading, empty or error component exists for
   previews (section 4).
7. **Announcements.** No shared live-region announcer exists (section 7).
8. **Keyboard resize.** The split edit/preview divider is pointer-only
   (`lib/previews/SplitEditPreview.svelte:57`, `role="separator"`, no
   `tabindex`). The Finder divider has arrow keys
   (`lib/previews/FinderView.svelte:988-1007`). A fix needs a live check.
9. **Recents.** "Archive all" is available only from the Recents context menu
   (`App.svelte:6044-6056`).
10. **Native zoom.** This review did not check whole-window zoom in the native
    app.

## Accessibility suppressions (reviewed 2026-10-16)

Method: every `svelte-ignore a11y_*` comment was disabled at the same time, and
`svelte-check` was run. A suppression that produced no warning is stale. The
review removed the stale ones and dropped the unused second code from five
dialog suppressions. Line numbers are after those edits. For a removed
suppression, the line is the element it covered.

Recurring reasons:

- **delegation**: the container catches clicks from rendered links or chips.
  Each anchor is focusable and fires `click` on Enter, so the keyboard path
  exists.
- **scroll region**: the element must be a Tab stop so that a keyboard user can
  scroll it. WebKit does not make a scroller focusable on its own.
- **dialog panel**: the panel stops click propagation to the backdrop. Escape
  and buttons are the keyboard path.
- **card keys**: a focusable card takes number-key shortcuts. Its buttons stay
  the accessible path.

| file:line | code(s) | decision | reason |
|---|---|---|---|
| `App.svelte:5790` | autofocus | keep | Rename input appears on a user command (F2 or double-click). |
| `App.svelte:5809` | no_static_element_interactions | keep | Double-click is a mouse shortcut; F2 on the row is the keyboard path. |
| `App.svelte:6044` | no_static_element_interactions | keep | Context-menu shortcut; see Gaps 9. |
| `lib/chat/ChatView.svelte:2953` | no_noninteractive_tabindex, no_noninteractive_element_interactions | keep | `role="log"` transcript is a scroll region; listeners only observe scrolling. |
| `lib/chat/ImagePreview.svelte:35` | no_noninteractive_element_interactions | removed (stale) | `role="dialog"` with a key handler does not warn. |
| `lib/chat/ImagePreview.svelte:37` | no_noninteractive_tabindex | removed (stale) | `tabindex="-1"` does not warn. |
| `lib/chat/Markdown.svelte:1055` | no_static_element_interactions | keep | Delegation for links, copy and context menu in rendered markdown. |
| `lib/chat/ModClient.svelte:170` | no_noninteractive_tabindex | keep | Focusable only when the Mod registers key or pointer listeners (`role="application"`). |
| `lib/chat/ModNode.svelte:33` | no_static_element_interactions | removed (stale) | A capture-phase click listener does not warn. |
| `lib/chat/ModSite.svelte:155` | no_noninteractive_tabindex | keep | Dynamic `tabindex` is `-1` or absent; the checker cannot see that. |
| `lib/chat/PermissionCard.svelte:116` | no_noninteractive_tabindex | keep | Card keys. |
| `lib/chat/PermissionCard.svelte:117` | no_noninteractive_element_interactions | keep | Card keys. |
| `lib/chat/PlanApprovalCard.svelte:84` | no_noninteractive_tabindex | keep | Card keys. |
| `lib/chat/PlanApprovalCard.svelte:85` | no_noninteractive_element_interactions | keep | Card keys. |
| `lib/chat/PlanApprovalCard.svelte:136` | no_noninteractive_element_interactions | removed (stale) | `role="dialog"` with a key handler does not warn. |
| `lib/chat/PlanApprovalCard.svelte:150` | no_noninteractive_tabindex | removed (stale) | `tabindex="-1"` does not warn. |
| `lib/dashboard/AgentCard.svelte:337` | no_static_element_interactions, click_events_have_key_events | keep | Wrapper only stops propagation to the card's open handler. |
| `lib/knowledge/EntryBody.svelte:126` | click_events_have_key_events, no_static_element_interactions | keep | Delegation. |
| `lib/knowledge/KnowledgeView.svelte:334` | no_static_element_interactions | keep | Delegation for chips and view keys. |
| `lib/layout/CloseDirtyDialog.svelte:60` | click_events_have_key_events | keep (trimmed) | Dialog panel; unused `no_static_element_interactions` removed. |
| `lib/layout/PaneTabs.svelte:716` | no_static_element_interactions | keep | Context-menu shortcut; the "Pane actions" button is the keyboard path. |
| `lib/layout/PaneTabs.svelte:730` | click_events_have_key_events | keep | `tabNavigation` on the strip handles Enter and Space. |
| `lib/layout/PaneTabs.svelte:921` | autofocus | removed (stale) | Input focuses through `use:renameFocus`, not `autofocus`. |
| `lib/layout/PaneTabs.svelte:1097` | no_noninteractive_element_interactions | removed (stale) | `onpointerdown` is not a checked handler. |
| `lib/plugins/ActivityDialog.svelte:51` | click_events_have_key_events | keep (trimmed) | Dialog panel. |
| `lib/plugins/AttachSheet.svelte:313` | click_events_have_key_events | keep (trimmed) | Dialog panel. |
| `lib/plugins/TrustDialog.svelte:53` | click_events_have_key_events | keep (trimmed) | Dialog panel. |
| `lib/plugins/ui/UiNode.svelte:416` | no_noninteractive_tabindex | keep | Scroll region (capped code block). |
| `lib/previews/BoardView.svelte:474` | no_noninteractive_tabindex | keep | `role="application"` pan and zoom surface. |
| `lib/previews/BoardView.svelte:475` | no_noninteractive_element_interactions | keep | Same surface. |
| `lib/previews/boards/CanvasBoard.svelte:265` | click_events_have_key_events | keep | Delegation. |
| `lib/previews/boards/CanvasBoard.svelte:266` | no_static_element_interactions | keep | Delegation. |
| `lib/previews/CompareView.svelte:83` | no_noninteractive_element_interactions | removed (stale) | `role="dialog"` with a key handler does not warn. |
| `lib/previews/DocxView.svelte:374` | no_noninteractive_tabindex | keep | Scroll region (`role="document"`). |
| `lib/previews/DocxView.svelte:375` | no_noninteractive_element_interactions | keep | Its key handler pages the document. |
| `lib/previews/FinderView.svelte:870` | autofocus | removed (stale) | Input focuses through `use:editFocus`. |
| `lib/previews/FinderView.svelte:900` | autofocus | removed (stale) | Input focuses through `use:editFocus`. |
| `lib/previews/FinderView.svelte:988` | no_noninteractive_tabindex, no_noninteractive_element_interactions | keep | Focusable `role="separator"` with value and arrow keys. |
| `lib/previews/ImageView.svelte:508` | no_noninteractive_element_interactions | keep | `role="application"` zoom and pan viewport. |
| `lib/previews/ImageView.svelte:509` | no_noninteractive_tabindex | removed (stale) | `tabindex="-1"` does not warn. |
| `lib/previews/LogView.svelte:516` | no_noninteractive_tabindex | keep | Scroll region (`role="log"`). |
| `lib/previews/MarkdownView.svelte:1592` | no_static_element_interactions | keep | Delegation. |
| `lib/previews/MarkdownView.svelte:1593` | click_events_have_key_events | keep | Delegation. |
| `lib/previews/MarkdownView.svelte:1612` | no_noninteractive_tabindex, no_noninteractive_element_interactions | keep | Scroll region; Enter starts editing in live mode. |
| `lib/previews/MarkdownView.svelte:1662` | no_static_element_interactions | keep | Edit layer forwards editor keys. |
| `lib/previews/MediaView.svelte:366` | media_has_caption | keep | A workspace video has no caption track to offer. |
| `lib/previews/NotebookView.svelte:441` | click_events_have_key_events | keep | Delegation. |
| `lib/previews/NotebookView.svelte:442` | no_noninteractive_element_interactions | keep | Delegation. |
| `lib/previews/NotebookView.svelte:443` | no_noninteractive_tabindex | keep | Scroll region. |
| `lib/previews/PdfView.svelte:1507` | no_static_element_interactions | keep | View-level keys (paging, find). |
| `lib/previews/PdfView.svelte:1669` | no_noninteractive_tabindex | keep | Scroll region (`role="document"`). |
| `lib/previews/PptxView.svelte:452` | no_noninteractive_tabindex | keep | Slide stage takes paging keys. |
| `lib/previews/PptxView.svelte:453` | no_noninteractive_element_interactions | keep | Same stage. |
| `lib/previews/PptxView.svelte:454` | click_events_have_key_events | removed (stale) | The stage already has `onkeydown`. |
| `lib/previews/SlidesView.svelte:353` | no_noninteractive_tabindex | keep | Slide stage takes paging keys. |
| `lib/previews/SlidesView.svelte:354` | no_noninteractive_element_interactions | keep | Same stage. |
| `lib/previews/SplitEditPreview.svelte:57` | no_static_element_interactions | removed (stale) | `role="separator"` does not warn; keyboard resize is Gaps 8. |
| `lib/shared/ConfirmDialog.svelte:63` | click_events_have_key_events | keep (trimmed) | Dialog panel. |
| `lib/shared/embed/MarkdownBody.svelte:144` | click_events_have_key_events, no_static_element_interactions | keep | Delegation. |
| `lib/shared/embed/MediaBody.svelte:47` | media_has_caption | keep | A workspace video has no caption track to offer. |
| `lib/shared/embed/NotebookBody.svelte:155` | click_events_have_key_events, no_static_element_interactions | keep | Delegation. |
| `lib/shared/FindBar.svelte:31` | no_noninteractive_element_interactions | keep | `role="search"` handles Escape for every control inside it. |
| `lib/workspace/AgentSetupDialog.svelte:165` | no_noninteractive_tabindex | keep | Scroll region (installer output). |
| `lib/workspace/FileTree.svelte:1117` | autofocus | removed (stale) | Input focuses through an action. |
| `lib/workspace/FileTree.svelte:1229` | autofocus | removed (stale) | Input focuses through an action. |
| `lib/workspace/FolderPicker.svelte:612` | autofocus | removed (stale) | Input focuses through `use:focusOnMount`. |
| `lib/workspace/HomeScreen.svelte:955` | autofocus | keep | Add-host input appears on a user command. |

Count: 67 occurrences. Kept 51 (5 of them trimmed to one code). Fixed 0.
Removed 16 (stale; removed with no attribute or behavior change). Needs live
check 0; one related keyboard defect is Gaps 8.

## Checks

```sh
npm --prefix web-ui run check
npm --prefix web-ui run test -- src/lib/shared
npm --prefix web-ui run test -- src/lib/settings/themes.contrast.test.ts
```

These checks prove types and pure helpers only. Drive a changed surface in the
isolated preview before you claim it works
([verify-app](../../.claude/skills/verify-app/SKILL.md)).
