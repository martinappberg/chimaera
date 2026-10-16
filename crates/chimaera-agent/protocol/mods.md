# Claude Mods: the native UI controls

The current contract of the Claude Mods surface (`src/native_ui.rs`, driven through the Claude Code driver). It shares the Claude pin `TESTED_CLAUDE_VERSION`. Its live gate is `just chat-mods-smoke` (`tests/native_ui_live.rs` with the local `tests/fixtures/claude-mod` plugin; no model turns, no billing); it complements, and does not replace, `just chat-smoke`. The feature page is [docs/features/claude-mods.md](../../../docs/features/claude-mods.md). The dated record is [passes.md](passes.md).

## Lane and ownership

- Native UI is a separate transient lane: `ChatManager::native_ui` → driver → Claude controls → a bounded broadcast. It never takes a journal seq (2026-10-03, claude 2.1.288).
- The authenticated WebSocket owns the client id. UI responses and addressed notifications go only to that window, over a separate bounded transient lane (2026-10-03).
- A Mod's render tree, JS module and callback handle are never conversation data. Only a stable `message_identity` and bounded, complete `tool_render_data` facts are journaled, so render-site identities survive normal history replay (2026-10-03).

## Attach, panes and render

Recorded at claude 2.1.288 (2026-10-03).

- `ui_attach {surface:"desktop", client_id, answers}` acknowledges before asynchronous `session.attach` hooks finish. A pane can appear in a later `system/ui_panes`.
- `ui_render {component, instance_id, props}` → `{tree, props, hooked, rewritten, client_modules?}`. The native cache may return its old tree before a coalesced `system/ui_invalidate` (100 ms); render again on invalidation.
- A no-hook site returns an engine reference and `hooked:false`. `engine.ref=0` means the original props. Returned rewritten props belong to the first engine node only; later references whose process-local props are unavailable use the original core view.
- Verified without model turns: initialize, desktop `ui_attach`, the pane roster, `ui_render` for Pane and AbovePrompt, `ui_press`, `ui_input`, `ui_select`, `ui_client_module`, close, detach, and reattach with plugin state retained.

## Controls and callbacks

Recorded at claude 2.1.288 (2026-10-03).

- Buttons carry a plugin and a process-local numeric handle. Input and select requests also carry their key, component and instance, so Claude can re-resolve a field after a redraw.
- `handled:false` means refresh the control; never replay the side effect.
- `ui_client_module` returns the hash, entry/export names, keyed JS files, and `claude:surface-runtime`; the runtime exports `h` / `Fragment` / `install`. Client closures stay in the isolated surface runtime.
- `ui_client_press` runs the native hook chain before its `reached` event reaches the local closure.
- Host requests `ui_copy` and `ui_prompt_read` / `ui_prompt_fill` / `ui_prompt_suggest` have five-second native timeouts and typed success answers; another window's answer cannot resolve them.
- `ui_focus`'s native element table contains Button / Input / Select, not Client controls. Report only `is_held` for Client focus; supplying its key resolves to null and would move the browser focus back to the site container.
- Client module workers use a classic bootstrap with dynamic imports, which works inside the opaque sandbox; a module Worker failed before startup in the tested web view.
- Browser verification covered Client state/press/post, pointer capture and native key names (`space`, `right`), clipboard forwarding, draft fill/append decorations, Tab acceptance of suggestions, reload, and light/dark rendering.

## Message and tool render hooks

Recorded at claude 2.1.289 (2026-10-04).

- A real Haiku Bash turn provided user and assistant identities and native tool input/output. The `UserMessage`, `AssistantMessage` and `ToolUse` render hooks each returned the Mod wrapper and the delegated engine node, both directly and through the daemon's normalized events and native UI WebSocket.
- The browser showed all three wrappers and the original Bash output; a reload reattached and rendered them again with clean browser diagnostics. The no-model control/module/reattach suite also passed.

## Later builds

- 2.1.300 adds the Mods control `ui_prompt_autocomplete` (`prompt.autocomplete` hooks answer a surface's composer), and the hooks `prompt.change` (12 sites) and `$.ui.view` (7). None is adopted. `chat-mods-smoke` (control, module, reattach) passed (Pass 43, claude 2.1.300).
- `system/init` advertises `ui_surface_v1` at 2.1.289 (2026-10-05).
