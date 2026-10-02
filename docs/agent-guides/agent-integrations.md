# Agent integrations and the shared chat core

Status: four built-in agents; third-party harness registration remains a proposed extension.

## Product boundary

Choose an agent, then choose one of the models its signed-in account offers. Users do not
choose protocols. Antigravity is Google's harness for Gemini models; its official ACP
server is how Chimaera connects to that harness, not a new Chimaera-owned Gemini agent.
Grok Build exposes ACP from its own executable. Claude Code and Codex keep their native
protocols because their controls and lifecycle features are richer than this common subset.

The new-agent menu keeps ready chats first, with installation/setup below. A conversation
remembers its own view independently of the default for new conversations. Unknown legacy
CLI histories reopen in terminal. A failed resumed chat stays a visible failed chat.

## Implemented seams

- `AgentKind::chat_adapter` explicitly registers the four built-in adapters. There is no
  default-to-Codex branch for an unknown identity.
- `driver.rs` owns process lifetime, framing, startup deadlines, delivery ordering and
  teardown. `ChatManager` owns command admission, the retained-send budget, journals and
  gap replay. These are shared by every driver.
- `capabilities` and `catalog` events describe the current session's controls. The UI
  uses these instead of treating every agent other than Claude as Codex. Unknown agents
  start with no advertised advanced controls; old Claude/Codex journals have a compatibility
  fallback. Model options come from the provider's account-specific session, not guesses.
- `acp.rs` translates ACP v1 into the same events, including approvals, tool output,
  text/reasoning, plans, queueing, cancellation and configuration readback. Providers still
  execute their own tools; Chimaera does not advertise client filesystem/terminal callbacks.
- Forks share a bounded conversation-copy path. Claude/Codex use native forks only at a
  verified native boundary. ACP holds the copy until the first real user prompt, so opening
  a fork remains idle. After the provider accepts the prompt, its session owns the copy; restart/reopen
  does not append it again. A rejected first prompt retains the copy for retry. Copied history does not imply transferred images, hidden model
  state, tasks or restored workspace files.
- Antigravity's installer supplies both its terminal executable and Google's complete chat
  package, including the companion harness. Grok uses xAI's native release. Executable
  discovery and arguments belong to the daemon, never to transcript components.

## Proposed extension contract

Pi and other optional harnesses should join the same menu when installed through Extensions.
The maintainer chose four curated built-ins, with other agents through Extensions; the
exact built-in set can evolve. Do not fill the picker with every available integration.
The existing Wasm plugin API has tools, screens and bounded jobs. It does **not** yet declare
persistent agent registrations. A plugin job is not a safe substitute for a chat driver.

A future manifest contribution should supply a namespaced identity, display name, provider
label/icon, documentation/sign-in instructions, verified platform runtime artifacts,
terminal command (optional), and a structured adapter declaration. ACP agents can reuse
`acp.rs`; Pi's native RPC needs its own translator or a separately verified ACP adapter.
Capabilities must be negotiated at runtime, not accepted as optimistic manifest promises.

The daemon must own the long-lived process and journal exactly as it does for built-ins.
Adding registration must first replace the closed `AgentKind` persistence boundary with a
validated string identity while continuing to read all existing identifiers. Extension
removal must not delete history or retarget an old conversation to another harness. Missing
extensions should offer reinstall; an unavailable native fork should still allow a conversation
copy. Executable launch requires the existing explicit extension trust grant and verified
artifacts; a Wasm render/tool call must never own an unbounded protocol loop.

### Shared Settings and Extensions behavior

Agent registrations should join the same `/agents` catalog used by the launcher and Settings.
The registration owns its runtime installation and update source; the UI receives readiness,
provenance, supported actions and an installer result. A successful download alone is not a
ready agent: executable detection, startup and authentication must all be represented honestly.
Settings now consumes the same catalog as Updates and never coerces an unknown identity into
a built-in path setting. Installer outcomes survive terminal removal, so both entry points can
show the same completion state.

The Extensions card should say “Adds Pi” (or the contributed agent's name), with one Install
or Set up action. After installation it should offer “Start chat”; the agent appears once in
the picker and once in Settings, attributed to its extension. Updating an extension must say
whether it also changes the agent runtime, preserve running conversations, and support the
existing rollback flow. Disabling or removing it preserves history and offers Reinstall for
unavailable conversations; never substitute another agent silently.

Do not conflate an extension that adds a new agent with an add-on installed *inside* Claude,
Codex or another agent. The existing Extensions Plugins/Connections/Skills reports use each
agent's own plugin, authentication and skill APIs. Those probes cover all four built-ins through separate native adapters;
ACP chat support alone does not imply plugin, connection or skill discovery.
Each registration needs separate capabilities for those surfaces. Unknown capability means
unavailable, not an empty list of plugins or a fallback to Codex's commands or `$skill` syntax.

Acceptance for a future registration includes an unknown namespaced agent identity, duplicate
registration rejection, missing runtime, signed-out state, failed install/update, permission
changes on update, rollback, disabled/removed extension, fork-copy fallback, and close/reopen
across a daemon upgrade. Its real upstream protocol must pass the shared live gate before its
card advertises Chat. This proposal does not claim that installing a current plugin can already
add Pi to the picker.

## Chimaera MCP and messages

All four Chat adapters receive the same per-session Chimaera MCP endpoint. Tool visibility,
workspace isolation, plugin tools, inbox storage and wake policies belong to the daemon.
Claude has a hook carrier and Codex has native mid-turn steering. Antigravity and Grok keep
a busy chat's messages in its inbox; unread direct messages meet the wake policy when that
turn ends. Broadcasts never wake a chat. ACP still shows the provider's approval requests;
Chimaera does not infer standing consent from an arbitrary tool title.

Automatic MCP injection into Terminal currently supports Claude and Codex. The installed
Antigravity/Grok terminal CLIs lack a verified session-only configuration override (Grok
1.0.46 rejects the documented `--plugin-dir`). Chimaera does not rewrite a user's global
config or move their login/history to manufacture parity. Use Chat for Chimaera MCP and
communication with those agents. Their ordinary terminal launch remains available.

## Verification contract

Every adapter needs the same tests for startup failure, ordered streaming, permission allow/
deny, stop while awaiting approval, queue delivery/cancellation, replay without duplicates,
reopen with the same native id, idle forks, copied-context recall, and teardown. Provider-only
features need their own tests rather than pretend parity. Hermetic mapper and lifecycle tests
run without billing; explicit live tests verify installed upstream versions. The real UI must
be exercised in both themes, including a daemon restart and both terminal/chat histories.

Official references: [Google's integration](https://antigravity.google/docs/ide/extensions),
[Grok's headless interfaces](https://docs.x.ai/build/cli/headless-scripting),
[Pi RPC](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/rpc.md).
