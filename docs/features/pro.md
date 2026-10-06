# Chimaera Pro

Chimaera Pro is an optional, paid extension to the free workbench. It continues a computer's agent work in the cloud when the computer goes away, brings it back when the computer returns, and lets the user see and drive the same conversations from a browser or another computer. The extension is pre-installed in the official app and activates only with an active plan. A build of this repository without the extension has no Pro entry and behaves exactly as the free product: nothing in this page runs, dials, writes or renders for a user without Pro.

This page describes what the public code does. The private extension supplies the account, billing, the cloud machine and the policy that decides when to copy; the public daemon owns the mechanism, the routes and the wire contracts described here and in [`crates/chimaera-link`](../../crates/chimaera-link/).

**Status: partial.** The mechanism below is implemented and proven on the local loopback harness; acceptance on deployed staging with a real agent is pending.

## The model

Whoever holds a project's lease runs it, and a computer holds it while its daemon can reach the account. Everything else follows from that:

- **Quitting the app changes nothing.** The daemon keeps running the work and keeps the computer reachable from the user's other devices by itself, so the app does not have to be open for a browser to show and drive the conversations.
- **Closing the lid hands over cleanly on a Mac.** The daemon hears system sleep itself and, within the time macOS allows, hands its working projects to the cloud: the same conversation continues there on current files.
- **Any other way of going away is noticed by the lease.** Shutdown, a lost network or a crashed daemon stops the lease renewals; the account lets the cloud continue about 75 seconds after the last renewal (a 60-second lease plus a 15-second grace), from the latest synced state.
- **Exactly once.** A computer whose lease ran out stops its own agents before the account lets anyone else take the project, so a turn never runs in two places. Its plain terminals keep working. Only a successful renewal counts: if the account cannot be reached for about a minute, agents on this computer pause at the deadline until it is reachable again; shells keep running. That includes the account answering with its own server errors, since its takeover does not depend on the requests that failed. When nobody took the work in the meantime, the computer simply carries on with its own conversations.
- **Coming back is automatic.** When the computer can reach the account again without a gap for a few seconds, the work returns at the conversation's next pause. Those seconds are the only guard against bouncing; power and the app play no part.
- **Sync is continuous.** The project's files and agent conversations are copied through Git when a turn starts (no sooner than 20 seconds after the previous copy), when it ends, every minute while an agent works, and every two minutes otherwise. After a sudden loss the cloud continues from the last copy and the agent is told to check work whose outcome is uncertain.
- **Idle projects cost nothing.** The cloud starts for a project only if a conversation was working or waiting on the user when it was last copied, or when someone opens the project from a browser after its computer is gone.
- **Conflicting edits keep both versions.** When work comes home and both sides changed a file, the cloud's version takes the file and this computer's sits beside it as `<name>.mine-<yyyymmdd-hhmm>`.

## What the user sees and does

- **Where the work runs** is shown where the host is shown: this computer, the cloud, or another computer by name. It normally only informs.
- **Run in the cloud** moves one project to the cloud now and keeps it there. Its working and waiting conversations travel together or not at all; if one cannot travel, the project stays and keeps running here and the reason is shown.
- **Run here** brings one project back to this computer at its conversation's next pause.
- **Keep on this computer only** (per project) means the project is never copied or moved. Once the account has acknowledged the switch the project holds no lease, so its agents never stop for one.
- When the cloud cannot run a project that needs it (an agent not signed in there, cloud time or storage used up, the cloud unavailable), the reason is shown in one plain sentence, and a project the user sent there comes back by itself.
- On another computer, a synced project's conversations and terminals are driven through the relay on the computer that runs them.

Words shown to the user name places (this computer, the cloud, another computer), never the machinery; a vocabulary test in the web UI enforces this.

## What the daemon does

- **Reachability** (`crates/chimaera-server/src/pro/reach.rs`). For a personal computer configured with an active account and the extension composed in, the daemon opens the keeper's reverse-serve socket with its own daemon delegation (an account-wide credential limited to sync, leases and keeper transport, held in memory only, at most 24 hours, renewed by the daemon) and bridges each stream the keeper opens to its own loopback port. Remote viewers still need this daemon's own token. Sign-out, a new configuration, device revocation and sign-out everywhere end the socket; reconnects back off from half a second to ten seconds, and a credential the keeper refuses, or one that expired, ends the link until the daemon is configured again. At most 8 MiB read from the daemon wait to be written to the keeper across all streams. The same module records whether the lease loop's calls reach the account (a success or a 409; a refused credential or any server error does not count), which drives the return guard. The account's `X-Chimaera-Account` header on its answers only tells its own server errors apart from a proxy's in the log.
- **Fencing** (`pro/execution.rs`, `pro/execution/watchdog.rs`, `pro/execution/lease.rs`). Every managed host runs a watchdog thread (a 100 ms tick on a cloud machine, 1 s on a computer). A computer's local deadline ends 15 seconds before the account's expiry; at the deadline its managed agents are signalled and their sessions kept for resuming, and new agent launches and agent input are refused until it holds the project again. A computer that was frozen past its deadline renews first only while nobody could have taken the project yet; otherwise it is fenced at once and resumes only after re-acquiring its own untouched lease. After a restart or a wake, no agent starts or resumes in a synced project until the lease loop has confirmed the computer still holds it (its terminals keep working); interrupted sessions then resume. Only a sign-out, which the daemon records on disk, lets them resume without the account, and only for a project the account acknowledged standing down. Signing out first tells the account there is nothing to continue for each project this computer holds (`pro/sign_out.rs`), so neither the lapse takeover nor a browser's open resumes it in the cloud; a project whose stand-down was not acknowledged keeps its lease's deadline, its agents pause there, and the daemon keeps asking in the background (a refusal, for example because the lease already lapsed, leaves it paused until the next sign-in). The account never takes over or wakes for a revoked computer's lapsed lease. Sessions a lapse stopped are dropped once another machine held the project, so a finished turn never runs again. The lease loop renews up to 16 projects at once and waits at most 20 seconds per pass; a slower project keeps going on its own and is skipped meanwhile.
- **Sleep** (`pro/sleep_watch.rs`, macOS only). The daemon registers for system power notices itself, so it works with the app closed. Before sleep it runs the hand-over with a 23-second budget and always acknowledges; on wake (or a sleep that was announced and then cancelled) nothing resumes on sight and the lease loop verifies who holds each project. Signed out, or without the extension, a power notice is only acknowledged. Linux and Windows rely on the lease (the daemon has no D-Bus dependency, and on Windows it runs inside WSL).
- **Choices and where work runs** (`pro/place.rs`). The two choices and the status row fields below; the reason the account gives on each ownership read; and, on the cloud machine, the report after a project arrives of whether it runs the work.
- **Where agents run** (`mcp/cloud_context.rs`). Every Claude and Codex conversation in a synced project gets one short note, invisible to the user, once per change of machine: where it runs, what differs from the other machine, what did not travel (ignored files, rebuilt folders such as `node_modules`, files left out for size, environment variable names), which sign-ins exist there, and files kept in both versions. It is told to install or rebuild what is missing and to leave for later what only the user's own computer can do. The read-only `where_am_i` tool answers the same at any time. File names in the note are cleaned and shown as data, and the note stays within 4 KiB.
- **Kept both versions** (`pro/kept.rs`, `pro/canonical.rs`, `pro/trash.rs`). The review lists the kept pairs and the project's `@cloud` branches; each pair can use this computer's version, use the cloud's (the copy goes to the Trash where the drive has one) or keep both.

## Routes and wire shapes

All routes are authenticated with the daemon's bearer like every other route. On a daemon without the extension they change nothing: the choices answer 409 or 404 and the status rows carry null and false.

| Route | What it does |
| --- | --- |
| `GET /api/v1/pro/status` | Configuration and one row per project. |
| `POST /api/v1/pro/projects/{id}/cloud` | Run in the cloud: 202 `{"moving":true}`; 409 `{"error":"cloud_time_used_up"}` or `{"error":"not_here"}`; 404 for an unknown project. |
| `POST /api/v1/pro/projects/{id}/here` | Run here: 202 `{"returning":true}`; 409 `{"error":"not_elsewhere"}`; 404. |
| `POST /api/v1/pro/sleep {deadline_ms?}` | The hand-over the sleep watcher runs, for tests and hosts that learn of sleep another way. |
| `GET /api/v1/pro/projects/{id}/kept`, `…/kept/file`, `POST …/kept/resolve`, `…/kept/resolve_all` | The review of files kept in both versions. |

Each `/pro/status` project row carries, besides its ownership and copy fields, four additive fields:

```json
{
  "place": {"where": "here"},
  "reason": null,
  "run_here": false,
  "run_in_cloud": true
}
```

`place` is `{"where":"here"}`, `{"where":"cloud"}`, `{"where":"computer","computer":"<name>"}` (`computer` absent when the account has no name for it), or null for a project that is not synced. `reason` is null or one closed code: `agent_not_connected_in_cloud`, `cloud_time_used_up`, `cloud_storage_full`, `cloud_unavailable` (from the account), or `conversation_too_large`, `conversation_not_saved`, `not_synced_yet` (a Run in the cloud that could not start). `run_here` and `run_in_cloud` say whether each choice applies now.

The account's ownership read carries the additive `reason`, `holder_kind` (`computer`, `cloud` or null) and `holder_name`; the lease, grace and reverse-serve contracts are in [HANDOFF](../../crates/chimaera-link/HANDOFF.md#one-handoff-mechanism) and [PROTOCOL](../../crates/chimaera-link/PROTOCOL.md#reverse-serve).

## Where it lives

| Area | Entry points |
| --- | --- |
| The mechanism | `crates/chimaera-server/src/pro/` (`reach.rs`, `place.rs`, `sleep_watch.rs`, `execution*`, `engine*`, `routes.rs`); map: [pro/AGENTS.md](../../crates/chimaera-server/src/pro/AGENTS.md) |
| The seam the extension plugs into | `crates/chimaera-server/src/daemon_extension.rs` (daemon), `crates/chimaera-app/src/account/` (native app) |
| Viewing a project another machine runs | `crates/chimaera-server/src/session_proxy.rs`, [VIEWING](../../crates/chimaera-link/VIEWING.md) |
| Agents' note | `crates/chimaera-server/src/mcp/cloud_context.rs` |
| Web UI | `web-ui/src/lib/pro/`, `web-ui/src/lib/extensions/` |
| Contracts | [`crates/chimaera-link`](../../crates/chimaera-link/) |

## Constraints and edge cases

- Exactly-once covers the managed agents' turns. External side effects an agent made just before a sudden loss can repeat in the cloud; the recovery note tells the agent to check them.
- A computer frozen or offline for more than about 45 seconds stops its own agents even when nobody takes over (for example on a plane); its terminals keep working, and its conversations resume when it can reach the account again and still holds the project. The same holds after a restart without a network: agents in synced projects wait for the account.
- A prompt sent in the last seconds before a sudden loss may not be in the copy yet; the cloud then continues from the copy before it.
- The clean hand-over before sleep needs macOS; elsewhere a closed lid is noticed by the lease, so the cloud continues from the last copy rather than the moment of sleep.
- After a reboot the daemon runs only once the app has started it again; meanwhile the cloud continues the work.
- With the app closed the computer stays reachable until the daemon's credential reaches the device's sign-in expiry or the daemon stops.
- Only Claude and Codex conversations move between machines; other agents stay where they run.

## Intent — human-authored ground truth

### Viewing anywhere, execution at home while available
_Captured 2026-09-28 from the maintainer's instructions in this conversation._

- **Placement:** The maintainer clarified that cloud is the sync and handoff
  fallback. Viewing from a phone or browser should still execute on the home
  computer while it is available; the viewing device must not choose the host.
- **Continuity:** If the home computer becomes unavailable, including sudden
  battery loss, work should continue from the same project, session history and
  files in the cloud, then return locally when the computer is available again.
- **Experience:** The user should not need cloud-machine chooser controls or
  backend startup decisions. The system should handle placement automatically.

This records the intended experience. Recovery depends on durable checkpoints;
it cannot promise recovery of unsaved state or exact replay of external actions.
Existing ownership and publication fences must remain intact. Routing/failover
coverage and selected-project secret isolation are separate implementation gates.

### Acting brings the work to you
_Captured 2026-09-30 from the maintainer's decision, relayed with the task that implemented it._

- **The rule:** "acting brings the work to you; looking doesn't". Sending a
  message or typing on another of your computers moves the work there;
  opening, reading and watching never do.
- **Phone:** "if you are on your phone, and the laptop is connected, it should
  still run on the computer and not the cloud (to minimize cloud usage)".
  Superseded on 2026-10-05 by the Pro contract: a phone's action no longer
  moves the work to a computer.

### Sync a local copy; take over separately
_Captured 2026-10-02 from the maintainer's direct answers in this conversation._

- **Open on another Mac:** “Yes—sync a local copy on open; take over separately”.
  This supersedes automatic takeover on ordinary input between computers in the
  earlier “Acting brings the work to you” decision. Opening copies files; the
  separate takeover action moves execution.
- **Wake:** “Explicit opens may wake the cloud; polls and refreshes never do
  (latest recorded decision)”. The phone-to-available-computer policy was removed on 2026-10-05.
- **Custom secrets:** “Include selected-project custom secrets in this completion”.
  For changes while work is active: “Queue until idle; offer Apply now”.
  Named provider connections and custom project permissions remain distinct.

### Project-specific secret delivery; one trusted user
_Captured 2026-10-04 from the maintainer's direct decision in this conversation._

- **Model:** “Yes—project-specific delivery, one trusted user”. One ordinary
  Chimaera daemon handles many workspaces on the customer's VM. Custom values
  belong in newly started agents and terminals of the selected workspace.
- **Boundary:** This supersedes the stronger hostile cross-project isolation
  interpretation in earlier implementation notes. It does not promise separate
  per-project daemons, users or process/file/network sandboxes. Per-account VM
  separation and honest selected-workspace delivery remain required.
- **Controls:** The existing “Queue until idle; offer Apply now” decision stays.
  Apply/removal settles the selected workspace's work; siblings continue. This
  intent does not certify the new shared-daemon integration or enable capability.

### Cloud connections — scope and experience
_Captured 2026-09-28 from the maintainer's instructions in this conversation._

- **Problem it solves:** The maintainer asked why credentials stay on a device
  when the intended experience is to continue work automatically in the cloud.
  They explicitly requested implementation of the credential proposal with a
  subagent.
- **Current direction, open to improvement:** Asked to choose between selected
  projects and the whole personal cloud for custom secrets, the maintainer chose
  selected projects: “1, I think? but GitHub for instance feels like something you
  would allow for the full cloud box ?” This records the scope decision; it does
  not claim custom-secret sharing is implemented by the connection controls above.
- **Experience:** “I dont want this to meddle with the UI / UX and become unclear
  so the user doesn't know what to do”. Named service connections and project
  secret permissions must be understandable in the flow where they are needed.
- **Extension:** The maintainer explicitly wants other providers to fit later,
  giving Grok as an example. This addition is intended to evolve; no fixed
  provider list or immutable interface was requested.

### Keeper SSH and Slurm continuity
_Captured 2026-10-03 from the maintainer's direct answers in this conversation._

- **Continuity:** “Yes, keeper should keep interactive jobs connected too!”
  The maintainer also explicitly requires the app-created compute-node forward
  to remain held for the length of the Slurm job.
- **HPC policy:** “infinite ssh we can have to a remote but NOT a HPC login
  node, unless the user enables this”, using the existing opt-in behavior.
- **Experience and compatibility:** “you dont want to have to set up the keeper
  to have ssh keys etc.” Asked about jump hosts, the maintainer answered:
  “I have no idea but I feel like we should support most things”. The requested
  experience is broad compatibility with the Mac's existing SSH setup; the exact
  implementation remains open to improvement.
- **Explicit fallback:** The maintainer requested a direct connection choice in
  Advanced settings for workspaces or sites where keeper access is not allowed.
  This is a deliberate user choice, not a silent fallback.

_Broader Pro intent capture remains pending. The behavior above the divider is
derived from code; the statements here record the maintainer's instructions._
