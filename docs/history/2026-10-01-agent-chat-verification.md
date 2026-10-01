# Four-agent chat verification — 2026-10-01

Local verification on macOS arm64 in an isolated Chimaera state directory and a
disposable workspace. Normal Chimaera settings were not edited. Native plugin
tests temporarily registered named disposable fixtures and removed them afterward.
Existing provider logins were reused; prompts used a synthetic recall word and
disposable files.

## Versions exercised

| Agent | Verified runtime |
|---|---|
| Claude Code | 2.1.287; all 26 native smoke tests passed together with Codex |
| Codex | 0.159.3; the installed executable changed during this work, so the full native gate was repeated |
| Antigravity | CLI 1.2.14 with Google's complete ACP package 1.2.1; personal Google login |
| Grok Build | 1.0.46, standalone managed runtime |

These are tested versions, not a promise that every newer build has the same
protocol. Claude/Codex retain their dedicated adapters and provider-specific
controls; Antigravity/Grok share the ACP adapter.

## Checks completed

- `just check`: format, Clippy with warnings denied, complete workspace and plugin
  tests passed. Regression coverage includes failed chat startup without a silent
  terminal fallback, retained native resume handles, unknown token usage, failed
  release checks and successful/failed installer results after PTY removal.
- UI type check: zero errors and warnings. Production build passed (the existing
  large-chunk advisory remains). Full Vitest run: 1,591 passed, 3 skipped, 109 files;
  subsequent focused runs passed the added terminal-status and quiet-update-copy
  regressions (7 and 3 tests respectively).
- Native app `cargo check` passed during this change.
- `just chat-smoke`: 26/26 passed on the native versions above.
- `just chat-smoke-acp`: both live suites passed, including initial handshake,
  streaming/replay, recall, native resume, model readback, allow/deny approvals,
  queued delivery/cancellation, interrupt while awaiting approval, and idle copied
  forks. Denied and interrupted file writes did not create files.
- Real daemon/UI: forked across all four agents; all recalled the synthetic word.
  Closed and reopened all four through Recents. Google/Grok forks closed before
  their first prompt still retained the copied conversation on reopening.
- Restarted the isolated daemon and verified saved chat/terminal identities. Claude's
  terminal conversation reopened as its real TUI, with its existing conversation.
  Unknown terminal status now reads “terminal open” rather than perpetual startup.
- Both new managed installers completed using vendor artifacts. Settings exercised
  personal versus managed paths, retained unsaved edits across another row's save,
  Google chat setup, Grok reinstall, progress, completion, and page reload. The
  completed install is retained independently of the short-lived terminal row.
- Both new agents called Chimaera's static document guide through MCP. Grok used
  its own discovery/wrapper tools; the test checked the returned guide, rather than
  assuming direct tool names match Google's.
- Picker and Settings inspected in the browser in light and dark themes.
  “Updates not checked” was removed from agent rows after maintainer feedback;
  an unchecked release neither adds placeholder copy nor claims to be current.
- Existing Extensions plugin and connection views inspected. The subsequent native-extension pass below expands their add-on reports; these remain
  distinct from new-agent registration.

## Deliberate limits

No Pi or plugin-provided agent registration is implemented in this change. The
[extension contract](../agent-harness-design.md) describes the required registry,
installation, capability, update/rollback and history behavior. The shared UI
catalog accepts unknown identities without impersonating a built-in, but the
server's persisted agent registry still needs that extension work.

ACP conversation forks copy visible conversation context; they do not claim to
clone hidden provider state, images, background tasks or workspace files. Google
enterprise/API-key authentication and other operating systems were not exercised
live here. Google/Grok do not inherit Claude/Codex-only controls or their separate
plugin, skills and connection APIs merely because chat works.

## Follow-up: native extensions and communication

The maintainer explicitly requested that all four core agents also participate in
Extensions and Chimaera communication. Native inventory adapters, fixed-argument
management actions, provider-specific source installation and dynamic UI identities
now cover this separately from chat transport.

Observed against Antigravity CLI 1.2.14 / ACP 1.2.1 and Grok Build 1.0.46:

- Disposable workspace skills loaded through each real Chat integration.
- Disposable skill-only plugins installed through the native managers. AGY's exact
  `/chimaera-fixture:probe` and Grok's plugin skill returned `silver-maple-fixture`.
  A vague initial AGY prompt selected the similarly named project skill instead;
  the exact command verifies the invocation exposed by the UI.
- Grok inspect includes compatible Claude add-ons; native list commands alone do
  not. Project trust remains upstream-owned. AGY distinguishes imported packages
  from loaded project skills; unknown enablement remains unknown.
- Both called Chimaera `workspace_agents`, `message_agent` and `read_messages`,
  with synthetic messages verified in both directions and native approvals intact.
- For each new agent, a document-guide approval held a real turn open while a
  message arrived. No wake was requested mid-turn. At completion one Ask request
  appeared; accepting it delivered the message once and cleared unread state.
- Cold AGY startup exceeded the initial 20-second window under concurrent work.
  The bounded 60-second ACP startup window passed the repeated real flow.
- Grok's `/plugins` opens its native manager. AGY's does not: it runs a model turn,
  so the UI uses verified install/enable/disable commands and explicit command help.
- The Skills agent filter is a compact selector suitable for additional providers;
  counts describe the filtered list. Different source files keep separate identities.
- All four native enable/disable actions completed through Chimaera's authenticated
  endpoint and retained PTY output. Antigravity connection help also opened through
  the UI and survived a browser reload.
- Extensions was inspected in light/dark themes and at 700 × 850. An incomplete
  skill list now offers retry rather than claiming no skills exist. Explicit retry
  clears inventory caches and rechecks missing versions without changing the selected
  installation; a regression covers recovery from a transient version failure.
- Full UI tests passed: 1,597 tests, 3 skipped, 109 files. The UI type check
  and production build passed. Doc links passed (102 Markdown files).
- Removed the disposable user-level Antigravity/Grok plugins and disabled localhost
  MCP fixture after verification; closed the test chats and management terminals.

The adapters do not translate hook formats, permission grants or login flows between
providers. AGY's connection CLI omits project/plugin servers; the UI describes that
limit. Terminal MCP injection for the two new CLIs is not verified/supported;
Chimaera communication uses Chat, without changing global native configuration.

## Final review and merge gate

Rebased onto `5ab16d83` (including pane Find and file browsing changes), preserving
the new Chat Find integration alongside capability-based controls.

- `just check` passed with Rust 1.96: format, workspace/plugin Clippy, workspace
  tests (including all 806 server tests) and plugin tests. Test concurrency was
  limited to two on this busy development machine. Earlier runs exposed two stale
  plugin response expectations and an overly broad assertion in the new retry
  regression; all were corrected. Timing-sensitive existing tests also passed
  with bounded concurrency.
- UI type checking reported zero errors/warnings; production build passed. The
  complete rebased UI suite passed: 1,612 tests, three skipped, 112 files, using
  two workers and a 20-second timeout. Documentation and agent-asset checks passed.
- Restarted the isolated daemon into the final build. Native skill, plugin and
  connection refresh endpoints all succeeded; 89 skills were reported with both
  new agents' project/plugin fixtures present. The running versions were Claude
  Code 2.1.287, Codex 0.159.3, Antigravity 1.2.14 and Grok Build 1.0.46.
- Browser verification showed all four agents ready, the existing Claude terminal
  conversation still in Terminal, and the existing Grok conversation still in
  Chat. The final Settings screenshot was saved with the task's artifacts.
- Self-review covered transport/capability handling, saved surfaces, copied forks,
  installer provenance, native extension actions/trust and communication delivery.
  The transient-version retry bug found during this review has a passing route
  regression. No remaining blocking findings were identified before opening the PR.
