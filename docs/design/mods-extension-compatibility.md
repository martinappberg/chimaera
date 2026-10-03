# Claude Mods and Chimaera extensions

_Design direction, 2026-10-03. UI reuse is implemented where noted; portable
execution and a common authoring format remain proposals._

The maintainer proposed that extensions could also be compatible with Claude
Code Mods: “they work the same way [or at least are compatible]”. The intended
experience is one workbench with consistent interfaces, while authors can reuse
as much of an extension as each host supports.

## Current boundaries

Chimaera workbench plugins are capability-gated WASM components. They return
semantic `ui/1` trees, receive named actions, and run in declared workspace slots
(`tab`, `panel`, `file`, `status`, `card`). Claude Mods run inside a live Claude
process; Chimaera adapts its render trees, transient callback handles and host
requests. Claude also owns hooks into its model requests, tool execution and
session lifecycle. Rendering the same button does not supply those engine hooks.

[Anthropic's introduction](https://github.com/anthropics/claude-code/issues/91870)
demonstrates both added controls and changes to existing transcript rows. Its
[interface guide](https://code.claude.com/docs/en/plugins/mods/interface)
describes a pane beside a wide transcript, moving above the prompt when narrow.
Those are useful interaction patterns for Chimaera; an all-controls fixture is
only a protocol exercise, not the intended appearance of every Mod.

## Share presentation; adapt each runtime

| Surface | Reuse | Runtime boundary |
|---|---|---|
| Markdown | Existing sanitized chat renderer | Each adapter validates its input |
| Source code | Shared `ExtensionCode`, including syntax colors and bounds | `ui/1` supplies `text`/`language`; Claude supplies `source`/`language`/`path` |
| Unified diffs | `ExtensionCode` draws Claude's supplied hunks with line gutters | `ui/1`'s existing before/after and file-backed diff contract stays intact |
| Controls | Shared theme and action styles; accessible pending/error states | Named plugin actions and Claude closure handles remain distinct |
| Panes | Responsive placement, sizing, tab navigation and focus conventions | Workspace views and live-session panes keep their own lifecycle |
| Host actions | Consistent user feedback | Capabilities and authorization remain owned by the appropriate host |
| Executable Client surfaces | Claude's bounded isolated worker host | No new JavaScript execution capability is granted to WASM plugins |

`ExtensionCode` is the first extracted leaf used by both renderers. Markdown
was already shared. This changes no plugin manifest, WIT world, daemon/UI
contract or installation format. Syntax inference from a path never reads that
file. Mod callback handles never become Chimaera built-in actions such as
file saving or tool installation.

## A portable authoring layer

A useful next step is an explicitly versioned, small presentation vocabulary
with adapters to both existing formats. A package could contain a shared view
model and separate Claude and Chimaera entry points. Authors should be able to
write a checklist, inspector or chart once while declaring which host actions
it needs. The adapter would report missing capabilities, with an explicit
fallback, instead of pretending an engine-specific hook ran.

Before exposing Chimaera extensions in chat, define a session-scoped slot and
its capability rules: which session a view sees, which events it receives,
whether it can read or fill a draft, and how it detaches on reconnect. Keep
permission prompts and agent account ownership with their existing hosts.
Changing the WIT contract requires a versioned API alongside the frozen worlds.

Compatibility should be demonstrated by one portable example rendered through
both adapters: equal content/actions, keyboard behavior, light/dark themes,
narrow/wide layout, reconnect and host-capability refusal. Unmodified Claude
Mods that depend on Claude's engine still require Claude; supporting their UI
does not promise they execute under every agent.

## Implementation pointers

- [Claude Mods](../features/claude-mods.md): current supported sites and limits.
- [Extension authoring](../agent-guides/plugins.md): current `ui/1`, slots and
  capability contract.
- [ExtensionCode.svelte](../../web-ui/src/lib/shared/ExtensionCode.svelte):
  shared bounded code presentation.
- [ModNode.svelte](../../web-ui/src/lib/chat/ModNode.svelte) and
  [UiNode.svelte](../../web-ui/src/lib/plugins/ui/UiNode.svelte): the two adapters.
