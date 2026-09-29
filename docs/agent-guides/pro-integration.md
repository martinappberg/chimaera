# Reviewing and verifying the Pro integration

[PR #204](https://github.com/martinappberg/chimaera/pull/204) on
`codex/pro-continuity-integration` is the one public branch for Chimaera Pro:
the optional account, the always-on connection, project mirroring, and the
automatic laptop ↔ cloud handoff. Earlier PRs (#178–#198) are history, not
separate checkouts. The private services that complete the flow (account,
keeper, cloud machine supervisor, Git storage) live in the maintainer's private
repository; this guide names what they must provide, never how they are run.

Read [AGENTS.md](../../AGENTS.md), the maps in the review map below, and every
matching [path-scoped rule](../../.claude/rules). The product rules that decide
review verdicts are in the [Pro feature page](../features/pro.md); the wire
contracts are [PROTOCOL](../../crates/chimaera-link/PROTOCOL.md),
[HANDOFF](../../crates/chimaera-link/HANDOFF.md),
[VIEWING](../../crates/chimaera-link/VIEWING.md) and the
[session bundle](../../crates/chimaera-server/BUNDLE.md).

## The rules a reviewer holds the code to

These are the maintainer's decisions (2026-09-26 to 2026-09-29). A change that
violates one is a defect regardless of tests.

- **Laptop first.** The laptop never stops its own agents or shells because the
  account is unreachable, because the user signed out, because the plan lapsed,
  because the privacy switch was used, or because the daemon restarted. Local
  execution is fenced only after a *verified* newer owner exists, and then at a
  safe pause. Publication (mirror writes) is what an unreachable account fences.
  Plain shells are never managed processes. Sign-out and boot never leave a
  returned session deferred: a sign-out that cuts a return's resume short
  resumes it at once (the resume runs as its own task, one resumer per
  session), and a daemon restarted over one resumes it like any session the
  previous daemon left, unless another machine holds the project.
- **Nothing changes for free users.** No endpoint means no Pro chrome anywhere
  (one line on the Pro page); a native window showing another host's daemon has
  no account bridge; the terminal toolbar, chat reconnect rows, watch-by-default
  and forced focus mode exist only for a routed session or an account-gateway
  browser view. Every Codex terminal keeps its plain argument list unless the
  project is Pro-configured; no agent brief is injected into a local session
  unless its project returned from the cloud in this daemon life.
- **No placement controls.** No "reconnect and wake", no keep-running pin, no
  "open a repository in the cloud" from the laptop, no visible cloud-setup
  terminal. Sends and permission answers carry wake intent themselves; opening
  a view never wakes anything.
- **Plain words.** User-facing text never says baton, epoch, hydrate, keeper,
  worker, placement, delegation, canonical, receipt, fence, mirror, publication
  or host. Errors reach the UI as stable codes with sentences, not raw text.
- **Prices come from the account service**, never from the public source.

## What the 2026-09-28/29 review changed

A nine-part read-only review (native shell, link crate, daemon ownership core,
mirror/Git/bundles, viewing/proxy, providers and agent context, web UI and copy,
six end-to-end journeys, private services) found that the branch could not
complete the round trip and regressed free users. The fixes, all with red/green
tests, are the commits after `eb4ae26b`. The converged root causes, so a
reviewer can check each stays fixed:

1. Under the negotiated (v2) protocol the daemon never published the handoff
   policy the account's automatic wake requires → `pro/engine.rs`
   `publish_policy` runs in both versions before any snapshot bytes move.
2. The clean flush and hydration ran inside HTTP handlers whose callers time out
   (keeper 90 s, supervisor 120 s, sleep 25 s), leaving killed Git children,
   `Transferring` state and a permanent 409 → `pro/detached.rs` owned tasks
   with drop guards; the sleep flush takes a `deadline_ms`, preempts the
   periodic pass, publishes projects in parallel and never renews or resumes
   inside the sleep window.
3. The execution fence locked users out of their own laptop → fencing follows
   the rule above; graceful shutdown clears restart evidence; a same-boot
   restart probes recorded process groups instead of fencing forever; a
   state-file read error is retried and only a parse failure counts as damage,
   scoped to enrolled projects.
4. Refresh-token replay: the service answers `400 invalid_grant` and bumps the
   account epoch on replay → the link client treats any refresh 4xx other than
   404/408/429 as final, retries only when the request provably never left, and
   the keeper's `503 account_unavailable` is a quiet wait.
5. Return silently lost local edits → the three-way baseline is the last
   acknowledged publication; a file only this computer changed keeps its edit;
   the snapshot manifest carries a `left_out` inventory so an excluded file is
   never treated as deleted; a file changed on both sides keeps the user's
   version beside it as `<name>.mine-<stamp>` and the row reports `kept_both`.
6. Viewing: a sleeping owner was unreachable through the native proxy; the
   events socket was handed to the owner wholesale (settings overwritten, tabs
   pruned); sends were lost silently; a move said "agent exited" → per-project
   route generations, a local-authoritative events feed, `moved`/`paused`/
   `waking` frames, refusals that name their command, held first input with a
   daemon-wide budget, wake on the first real input.
7. Agents: a live Codex terminal was never "at pause"; every moved terminal got
   a billed prompt; one blocked provider paused a whole project; enrollment
   restarted running agents; a thawed cloud machine re-sent its pickup → all
   fixed (`agent_state.rs::tui_at_pause`, `spawn.rs`, `provider_gate.rs`,
   `execution::adopt_running`, `execution::thawed`).
8. Nothing told the user: a return that kept both versions only showed up as
   `.mine-…` files (its count lived in memory), and a cloud conversation
   waiting on a permission raised no notification on the open laptop →
   `pro::report_return` persists the report and raises one `kept_both`
   notice per return; a window's feed relays the owner's turn-end,
   approval and agent-message notices into the viewer's own feed once each
   (`notices::relay`), and routed approvals join the viewer's attention set.
9. The transfer pickup spoke jargon ("host", "acknowledged checkpoint") →
   plain words (in the cloud / on the user's computer, same conversation or a
   copy) with a stable `UserMessage.origin` for the chat divider: `moved`,
   `home`, `recovered` (`chat::transfer_origin`,
   `chat::transfer_context`).

10. A thawed cloud machine refused the viewer socket that woke it (its lease
    lapsed while frozen) and dropped it without a close frame → it now waits
    for its own renewal before admitting or refusing that viewer
    (`execution::await_renewal`, `ws.rs::SocketScope::admit`).

## The transfer pick-up message

After a move or a return, a structured conversation whose turn or background
work was cut off gets one visible message from the daemon
(`chat::transfer_context`; idle conversations get none). It says, in plain
words, where the agent now runs ("in the cloud" / "on the user's computer"),
whether it is the same conversation or a copy continuing from the last saved
point, that the project files were installed and may differ, and to re-check
tools and paths; a recovery adds how to treat work of uncertain state. The chat
view folds it into a divider keyed on its `UserMessage.origin`, a stable wire
value (`chat::transfer_origin`, `chimaera_agent::model`):

| `origin` | When | The divider says |
| --- | --- | --- |
| `moved` | a clean move; the conversation now runs in the cloud | Continued in the cloud |
| `home` | a clean return; back on the user's computer | Back on your computer |
| `recovered` | either direction, after the other machine stopped responding; continues from the last saved point (usually a forked copy) | the UI's recovery wording |

`restart` (a daemon restart cut the work off) is the other pick-up origin;
each of the four resets the carryover's ten-minute pick-up clock
(`model::is_pickup_origin`). `worker` (a Mastermind worker's message) is
daemon-sent but not a pick-up.

## The acceptance gate: one flow that runs

The private repository carries a loopback end-to-end harness that starts the
account, keeper and a cloud machine as local processes (no vendors, no cloud)
against this branch's daemon and plays the app's configure sequence. It proves,
in one run of about 22 minutes with production timings:

1. Account, keeper and an uncreated cloud machine come up.
2. A laptop daemon with a Git project, a chat agent mid-turn, and a private
   project with a plain shell.
3. Enrollment: policy and checkpoint receipt published; the running chat stays
   on its process with no new prompt.
4. Lid close: `/pro/sleep` releases within its deadline; the cloud machine
   starts, takes the project, and the conversation continues exactly once in
   the same native conversation with the laptop's files; zero laptop turns.
5. A phone resolves the project to the cloud machine, reads without waking it,
   and its message runs in the same agent process.
6. The cloud machine drains, flushes and suspends while keeping ownership; a
   passive read does not wake it; a send with wake intent does, into the same
   conversation, same process, no fork.
7. The laptop wakes on power; after the settle window the work comes home:
   same conversation, no fork, zero new turns, both machines' files present,
   the branch fast-forwarded.
8. Battery loss mid-turn: the cloud takes over from the last checkpoint with a
   forked conversation and recovery context; the restarted laptop does not
   resume its stale turn; the work comes home with `kept_both` for a file
   edited on both sides.
9. The private project never reaches the account; signing out leaves the local
   chat and shell running.

Its last run on this branch is recorded in the PR. Passing it is the bar for
"the flow works"; unit and integration tests alone are not.

## Reproducible local checks

Node 22 ([.nvmrc](../../.nvmrc)), Rust 1.96.0 ([toolchain](../../rust-toolchain.toml)).

```sh
npm --prefix web-ui ci && npm --prefix web-ui run check && npm --prefix web-ui run test && npm --prefix web-ui run build
bash scripts/build-plugins.sh            # server tests need plugins/dist-test
just check                               # fmt, clippy -D warnings, tests (root + plugins)
cargo +1.96.0 test -p chimaera-link --all-features
just app-check                           # the native shell is its own workspace
node scripts/check-doc-links.mjs && node scripts/check-agent-assets.mjs && node scripts/check-workflow-security.mjs
```

Focused suites: `cargo +1.96.0 test -p chimaera-server pro::`, `workspace_viewer`,
`session_proxy`, `tests::ws`, `cloud_context`. A driver change needs
`just chat-smoke` (billed; the last pass is recorded in
[PROTOCOL.md](../../crates/chimaera-agent/PROTOCOL.md)).

Known intermittent tests under a loaded full run (they pass alone):
`previews/mdBlocks.test.ts` (UI), `cloud_context_real_http_…` and
`pro::repository::tests::cancellation_after_ref_commit_finishes_index_adoption`.

## Review map

| Area | Start here | What to trace |
| --- | --- | --- |
| Account lifecycle, billing, power | [native map](../../crates/chimaera-app/AGENTS.md), `shell/pro.rs`, `shell/pro/` | sign-in → keychain → daemon setup in the background; billing return; the sleep deadline; placements retire only on definitive answers |
| Link crate | [link map](../../crates/chimaera-link/AGENTS.md) | refresh semantics, lenient service decoding, exact daemon acks, tunnel and reverse-serve limits |
| Ownership and transfer | [pro map](../../crates/chimaera-server/src/pro/AGENTS.md), `engine.rs`, `execution/`, `detached.rs`, `drain.rs`, `handback.rs` | laptop-first fence, owned flush/hydrate, sleep deadline, return at a pause, drain for the supervisor |
| Files and durability | `pro/mirror.rs`, `repository.rs`, `canonical.rs`, `persist.rs`, [BUNDLE](../../crates/chimaera-server/BUNDLE.md) | baseline, `left_out`, kept-both siblings, durable Pro state |
| Viewing | [VIEWING](../../crates/chimaera-link/VIEWING.md), `session_proxy.rs`, `ws.rs`, `workspace_scope/` | per-project generations, local-authoritative events, `moved`/`paused`/`waking`, wake on input, refusal codes |
| Agents and context | `agent_state.rs`, `spawn.rs`, `pro/provider_gate.rs`, `mcp/cloud_context.rs`, `codex_notify.rs`, [providers map](../../crates/chimaera-server/src/cloud/providers/AGENTS.md) | pause detection, prompts only for interrupted turns, per-session gate, the shortened brief, notify shim only for Pro projects |
| Web UI | [pro map](../../web-ui/src/lib/pro/AGENTS.md), [settings map](../../web-ui/src/lib/settings/AGENTS.md), [net map](../../web-ui/src/lib/net/AGENTS.md), [chat map](../../web-ui/src/lib/chat/AGENTS.md) | free-user gating, the account page state machine, copy, paused rows |

## What is still open

- A failed return cannot yet restore the pre-install state (needs a staged
  install); it retries with a short backoff instead.
- Codex rollout lookups use the daemon's `CODEX_HOME`, not the login shell's.
- A routed project's conversations notify on the viewing computer only while a
  window has that project open (the relay rides a window's events feed); its
  approvals still count on the Dock from the roster poll.
- The `kept_both` notice names kept `@cloud` branches only once
  `engine::hydrate_scoped` passes `repository::receive`'s result to
  `pro::report_return` (it reports files only today).
- A second computer has no native viewer; adoption of a cloud-created project
  gives it no home, so it never returns automatically after its first cloud stint.
- The account browser's `HEAD` plan check and the settings gateway view are not
  exercised by the loopback harness (they need the private browser gateway).
- Live acceptance on staging (real sleep, real vendors, two devices) follows a
  coordinated private deploy: the branch fails closed against a service without
  the negotiated protocol, so nothing here can be tested against an older
  staging.

## Reporting

Report severity, `path:line`, the failure path, what you ran and observed, and
which of the rules above a finding breaks. Do not deploy, reset accounts,
restart the maintainer's running apps or publish comments to complete a review.
