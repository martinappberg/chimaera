# Claude Mods in structured chat

**Status: in preparation.** The native UI protocol has been verified against
Claude Code 2.1.288 without model requests. The complete billed chat smoke gate
is pending account quota reset; this does not advance the driver's tested pin.

## What and how

Claude plugins with Mods can draw interactive panes and an above-composer band
inside Chimaera's structured chat. Claude owns the installed plugin code and
state on the workspace host. Chimaera supplies the visible interface and routes
its interactions back through the running Claude process.

Install or enable a Mod through Claude's plugin controls, then open its command
in a Claude chat. Pane tabs select the visible Mod; its close button requests a
close that the Mod may decline. Native controls include buttons, text inputs,
selects, links, Markdown, code, and isolated SVG. The core transcript remains
available when a render hook is absent or the corresponding native facts were
not retained in older history.
The pane uses the workbench's tab treatment, typography and compact controls;
at chat-pane widths of 960px or more it docks beside the conversation. Narrower
panes sit above the composer, with an expand button for more height. Resizing
changes placement without remounting the Mod. Arrow keys, Home and End navigate
its tabs. Minimize keeps the pane open while pausing its Client work; restoring
it retains local Client state. Close still asks Claude to close the pane.

Code elements share Chimaera extensions' bounded syntax renderer; native unified
diffs show addition/removal colors and line numbers. The separate
[review-planning fixture](../../crates/chimaera-agent/tests/fixtures/claude-mod-review/README.md)
exercises a realistic two-pane workflow without model calls. Its example data
and manual checkmarks make no claim that a review or tests have run.

Mods can also wrap user and assistant messages, expanded tool cards, the working
indicator, and the session mode. Composer integration supports reading a draft,
filling it, offering a suggestion, and decorating text. Model-driven rendering
on these transcript sites still needs the complete live smoke run.

## Where it lives

- [native_ui.rs](../../crates/chimaera-agent/src/native_ui.rs) owns bounded
  Claude UI requests, pending responses, host callback routing, and expiration.
  [driver.rs](../../crates/chimaera-agent/src/driver.rs) and
  [lib.rs](../../crates/chimaera-agent/src/lib.rs) carry its transient channels.
- [ws.rs](../../crates/chimaera-server/src/ws.rs) extends the existing
  authenticated `/ws/chat/{id}` socket with `native_ui` request/response frames
  and a `native_ui_reset` signal when live UI traffic is lost.
- [nativeUi.ts](../../web-ui/src/lib/chat/nativeUi.ts) and
  [mods.svelte.ts](../../web-ui/src/lib/chat/mods.svelte.ts) own browser requests
  and render invalidation. `ModSite`, `ModNode`, `ModControl`, `ModClient`, and
  `ModsWorkbench` render the interface under `ChatView`.
- [native_ui_live.rs](../../crates/chimaera-agent/tests/native_ui_live.rs) uses
  the real installed Mod engine and a local fixture without requesting a model
  turn (`just chat-mods-smoke`). The complete compatibility gate remains
  `just chat-smoke`.

## Constraints

- Render trees, executable modules, callbacks, and button presses never enter
  the durable journal. A reconnect attaches a new window and requests fresh
  renders. It never replays an action that may already have run.
- Each WebSocket receives a server-generated client ID. Host requests for a
  clipboard operation or composer update can only be answered by that window.
- UI traffic has separate bounded queues and request limits; slow clients reset
  their UI state. Rendering follows invalidation events, not polling. Hidden
  chat views release their native attachment.
- `message_identity` and `tool_render_data` are additive durable *facts*, so a
  reopened transcript can identify render sites. Tool inputs are retained only
  when complete within 32 KiB; native output within 64 KiB. Larger or absent
  native values use the core card rather than an invented tool object.
- Chimaera does not load Mod code into the workbench's JavaScript context.
  Client surface modules run in a terminable Worker inside an opaque sandboxed
  iframe with a restrictive content policy. Native controls are validated and
  rendered by Chimaera. At most 16 Client instances run at once, with four module
  bundles cached; cyclic module imports fail visibly. Permission and question
  cards stay core UI, and terminal-only render sites are not emulated.
- This is an unversioned upstream control protocol. Changes require the live
  fixture and billed compatibility gate; upstream UI support is not inferred
  from the normal chat handshake.

## Intent

### Claude Mods — why they exist

_Captured 2026-10-03 from the maintainer in this implementation conversation._

- **Problem and quality bar (verbatim):** “The one that will need most thought
  are the Claude Mods, and we need to PROPERLY handle this in our UI too”.
- **How settled it is (verbatim):** “Use your judgment; the UI can evolve”. This
  is an addition open to improvement, not a frozen layout or implementation.
- **UI integration (verbatim):** “think of UI / UX in mind please. Especially
  rest of the app”. The maintainer supplied this after reviewing the first live
  pane, which had oversized spacing and controls disconnected from the workbench.
- **Further deliberate constraints:** none supplied.
- **Compatibility direction (verbatim):** “we can have this in a format that is
  compatible with Claude Code mods but also have them as sort of Chimaera mods
  (extensions) as well!” and “they work the same way [or at least are compatible]”.
  The [compatibility design](../design/mods-extension-compatibility.md) separates
  shared presentation already implemented from proposed portable authoring.

---

See also [structured chat](chat-mode.md) and [plugins](plugins.md).
