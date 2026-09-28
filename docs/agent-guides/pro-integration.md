# Reviewing the Pro integration

Use one existing checkout of `codex/pro-continuity-integration` and
[PR #204](https://github.com/martinappberg/chimaera/pull/204) as the review entrypoint.
Earlier PRs are implementation history, not separate checkouts to run. A fork
is not required. Do not deploy, install an app or modify real accounts during review.

Read [AGENTS.md](../../AGENTS.md), the maps below and matching [path-scoped rules](../../.claude/rules).
Verify code against the [Pro feature page](../features/pro.md),
[HANDOFF](../../crates/chimaera-link/HANDOFF.md) and [VIEWING](../../crates/chimaera-link/VIEWING.md).

## Review boundary

Record the checkout and PR state before reviewing:

```sh
git branch --show-current
git rev-parse HEAD
git status --short
git diff --stat origin/main...HEAD
gh pr view 204 --repo martinappberg/chimaera \
  --json url,baseRefName,headRefName,headRefOid
```

Integration merged main at `f879ac34`; later revisions need their own evidence.
Verify the intended base before using the diff. Preserve concurrent work. Some
legacy fixes have adapted counterparts: compare behavior, functions and tests
before proposing cherry-picks. Do not weaken newer authority/disconnect guards
to reproduce an older patch. The optional supervisor-cleanup prototype is
outside this PR's scope and is not validated here.

## Review map

| Area | Start here | Trace and verify |
| --- | --- | --- |
| Account lifecycle | [Native map](../../crates/chimaera-app/AGENTS.md), `shell/pro.rs`, `shell/pro/` | Sign-in → verified client → memory/keychain → generation changes. Transient recovery, stale writes, revocation, installation binding and preview isolation. |
| Plans and connections | [Pro UI](../../web-ui/src/lib/pro/AGENTS.md), [settings](../../web-ui/src/lib/settings/AGENTS.md), [providers](../../crates/chimaera-server/src/cloud/providers/AGENTS.md) | UI action → transport → job → fresh status → pending continuation. Subscriber/sales state, named disconnect and quiet polling. |
| Public contracts | [Link map](../../crates/chimaera-link/AGENTS.md), [PROTOCOL](../../crates/chimaera-link/PROTOCOL.md), HANDOFF, VIEWING | Capability negotiation, exact acknowledgment, scoped delegation, recovery, placement and receipts; older peers fail closed. |
| Execution and return | [Pro daemon map](../../crates/chimaera-server/src/pro/AGENTS.md), `engine.rs`, `execution/`, `canonical.rs`, `handback.rs` | Owner → release/checkpoint recovery → import → continuation → canonical return. Strict mode, logical identity versus native fork, completed versus interrupted work. |
| Files and durability | [Daemon map](../../crates/chimaera-server/AGENTS.md), `pro/mirror.rs`, `transport.rs`, `bundle.rs`, `ledger.rs` | Snapshot → acknowledged publication → import. Cancellation, temporary/private files, original paths and local conflict preservation. |
| Logical viewing | VIEWING, `session_proxy.rs`, `workspace_scope/`, native `pro/placements.rs`, UI `net/placement.ts` | Passive owner lookup → scope acknowledgment → registered resource → response/watch. Viewing never moves execution; one failed project does not break siblings. |
| Mutation boundary | `workspace_scope.rs`, `pro/execution/mutation.rs`, `api/`, `ws.rs`, [PTY](../../crates/chimaera-pty/AGENTS.md), [agent](../../crates/chimaera-agent/AGENTS.md) | Retain admitted account generation/epoch through body reads and queues to the actual file commit or PTY/chat write. |
| Agent context | `mcp/cloud_context.rs`, [chat protocol](../../crates/chimaera-agent/PROTOCOL.md) | Fresh MCP/profile/arrival context, honest capabilities, internal resource inspection, qualitative capacity answers and no invented allowances. |

Trace actual callers to side effects and returned UI status, including startup,
HTTP, established sockets, queues and restore. Include ordinary local/SSH behavior.

## Reproducible local checks

Use Node 22 ([.nvmrc](../../.nvmrc)) and Rust 1.96.0 ([toolchain](../../rust-toolchain.toml)).
Run at repository root; install missing dependencies without changing lockfiles. Coordinate shared targets.

```sh
npm --prefix web-ui ci
npm --prefix web-ui run check
npm --prefix web-ui run test
npm --prefix web-ui run build
just check
cargo +1.96.0 test -p chimaera-link --all-features
just app-check
node scripts/check-doc-links.mjs
node scripts/check-agent-assets.mjs
node scripts/check-workflow-security.mjs
```

`just check` runs [build-plugins.sh](../../scripts/build-plugins.sh), then
fmt/clippy/tests for **both root and `plugins/` workspaces**. The script builds
`wasm32-wasip2` fixtures and checksum-verifies locked plugin releases; missing
artifacts require download. Offline runs need them already present. Before
running individual server filters, prepare fixtures explicitly:

```sh
bash scripts/build-plugins.sh
cargo +1.96.0 test -p chimaera-server pro::
cargo +1.96.0 test -p chimaera-server workspace_viewer
cargo +1.96.0 test -p chimaera-server session_proxy
cargo +1.96.0 test -p chimaera-server cloud_context
cargo +1.96.0 test -p chimaera-pty
cargo +1.96.0 test -p chimaera-agent --test manager
```

Focused suites do not replace the full gate. Native is a standalone workspace.
On macOS, build without installing or launching:

```sh
npm --prefix crates/chimaera-app ci
npm --prefix crates/chimaera-app run tauri -- build --debug --bundles app \
  --config '{"bundle":{"createUpdaterArtifacts":false}}'
```

For isolated runtime work follow [develop](../../.claude/skills/develop/SKILL.md)
and [verify-app](../../.claude/skills/verify-app/SKILL.md). Check actual build stamp,
generated command permissions, executable, signature and isolated state. A
shared cache can reuse another checkout's code generation; stamp alone is not
proof. CI covers Linux/musl and other native targets that macOS cannot verify.

The [chat-mode workflow](../../.claude/skills/chat-mode/SKILL.md) requires
`just chat-smoke` for driver/CLI protocol changes. It uses authenticated pinned
CLIs and bills small turns: use existing task authorization and fresh isolated
sessions, record versions/results, and never reuse a user's conversation.

## Acceptance matrix

Keep **source checks**, **isolated runtime checks**, and **full user-flow checks**
separate, with exact build and observations. At `f879ac34`, Svelte had zero
errors/warnings, 1,413 UI tests passed with three skipped, and production UI build
passed. Rust/native checks for that merged head were pending when written;
pre-merge totals are not evidence for it.

| Behavior | Source / isolated evidence | Full-flow gate |
| --- | --- | --- |
| Free mode | Endpoint-unset behavior; shared local/SSH routes and UI tests | Local/SSH projects, reconnect, Home/settings and dirty editors work without Pro setup, account traffic or obscured controls. |
| Account and connections | Recovery races, billing return, fresh provider jobs, light/dark/narrow UI | Signup leaves purchase optional; browser completion returns to native; named connection resumes blocked work automatically; disconnect stays explicit. |
| View anywhere | Scope/path/session/ticket, delayed body/socket and sibling-route tests; disposable browser files/terminal walkthrough | Another viewer follows the online home owner without moving execution; files, history and tabs survive owner change/reconnect. |
| Cloud continuation | Real Git import, provider readiness, transcript and completed-idle checks | Verify actual files and substantive conversation events; no duplicate task or new turn merely because completed structured work moved. |
| Automatic return | Hand-back/canonical-content tests and original-path/conflict checks | **NOT PASSED as a complete automatic user flow.** Normal return must preserve cloud work/history without manual repair. Isolated import success is insufficient. |
| Sudden loss | Policy/grace, receipt, stale-owner and fork-context tests | Separate from clean release: automatic checkpoint continuation and canonical return. Do not claim unsaved-byte recovery or exactly-once external effects. |
| Idle preparation | Activity and operation accounting | **Not accepted:** background/cache writers must drain before suspension; sampled activity alone is not proof of quiescence. |
| Resource guidance | Context tests and corrected synthetic response sample | The sample omitted raw allocations and referred allowance questions to Usage details; it does not prove every model complies or real resource inspection occurred. |

Structured and interactive terminal agents need separate acceptance: the feature
page still documents incomplete reliable active/idle detection for every TUI
provider. Do not generalize structured-idle results to all terminal transfers.

## Reporting and safe review

Choose a bounded subsystem in the supplied checkout. Check missing wiring and free-mode
regressions through cancellation, restart, account replacement and delayed replies.
Prefer disposable HTTP/WS/PTY or Git fixtures; avoid tests that repeat implementation text.
Report severity, file/line, failure path, reproduction, tests run and unproved gates.
Preserve others' edits. Sanitize tokens, account identifiers and unrelated conversations;
keep operational artifacts outside the public repo. Do not deploy, reset accounts,
restart working apps or publish review comments simply to complete an audit.

## Earlier public PRs

Review final behavior in #204 rather than reapplying every historical patch.

| PR | Area |
| --- | --- |
| [#178](https://github.com/martinappberg/chimaera/pull/178) | Codex terminal resume |
| [#179](https://github.com/martinappberg/chimaera/pull/179) | Account link protocol |
| [#180](https://github.com/martinappberg/chimaera/pull/180) | Native account connections |
| [#181](https://github.com/martinappberg/chimaera/pull/181) | Handoff/delegation contract |
| [#182](https://github.com/martinappberg/chimaera/pull/182) | Worker activity |
| [#183](https://github.com/martinappberg/chimaera/pull/183) | Project mirrors/handoff |
| [#186](https://github.com/martinappberg/chimaera/pull/186) | Portable Git state |
| [#187](https://github.com/martinappberg/chimaera/pull/187) | Subscriber branding |
| [#190](https://github.com/martinappberg/chimaera/pull/190) | Onboarding/account polish |
| [#192](https://github.com/martinappberg/chimaera/pull/192) | Home/Pro experience |
| [#193](https://github.com/martinappberg/chimaera/pull/193) | Automatic continuity fixes |
| [#197](https://github.com/martinappberg/chimaera/pull/197) | Named provider connections |
| [#198](https://github.com/martinappberg/chimaera/pull/198) | Workspace-bound authority |
| [#204](https://github.com/martinappberg/chimaera/pull/204) | Combined integration |
