# Lifecycle & persistence — "close the laptop, nothing dies"

The property that defines Chimaera: sessions are **daemon-owned processes**, and windows are just
views onto them. A dropped socket, a closed laptop, or a quit app leaves everything running;
reconnecting re-attaches with full state. A *daemon restart* (update, crash, `chimaera kill`)
necessarily ends the child processes — the **session ledger + restart handoff** make even that
survivable.

**Where it lives:** `crates/chimaera-server/src/{ledger.rs,lifecycle.rs,persist.rs,update.rs,
state.rs,api/shutdown.rs}`; `Manifest`/`Handoff`/token in `chimaera-core`; CLI daemon-stop in
`crates/chimaera/src/kill.rs`. The two reconnect mechanisms this rests on live with their features:
PTY snapshot-on-attach ([terminals.md](terminals.md)) and the chat seq-journal gap-replay
([chat-mode.md](chat-mode.md)).

## Daemon-owned persistence

- **What & when.** Every terminal and chat session is a daemon child, not owned by any window.
  Close a window → the session keeps running; reopen → it re-attaches exactly where it was.
- **Key behaviors.** The **manifest** (`~/.chimaera/manifest.json`, 0600, carries the bearer token) is
  the single source of truth for "is a local daemon running". Terminals rebuild from a server-side
  snapshot; chat rebuilds by replaying the journal gap. Chat sessions survive a *disconnect* — the chat
  WS handler just exits the socket task when the client leaves, never killing the driver — **and** a
  daemon restart, which the ledger now resurrects them across (see below).
- **One daemon per state dir.** `serve` refuses to start when the manifest's daemon is provably
  alive (live pid **and** an HTTP answer on its port — a crash leftover or recycled pid doesn't
  block startup): a second daemon over the same ledger would respawn every session again as
  duplicate agent processes. Parallel daemons are sanctioned only via distinct `CHIMAERA_HOME`s.

## Session ledger + restart handoff

- **What & when.** A daemon restart ends its children — the ledger makes it survivable. A
  continuously-reconciled `sessions.json` records each session's *semantic* identity (workspace, cwd,
  agent kind, **surface** (term/chat), native conversation/thread id + transcript path, chat model,
  pinned name, dims, theme, linked-terminal edges). On boot the daemon **resurrects**: shells respawn at
  their last cwd, claude TUI agents respawn with `--resume`, **chat sessions respawn as chat** — both
  agents, via `chat::resurrect_chat`, resuming the native conversation (claude `--resume`, codex
  `thread/resume`) and replaying the on-disk journal — and the non-resumable retire into Recents (a TUI
  codex, or a chat when `restoreSessions` is off) so a survivor is offered for manual resume.
- **How it's used.** No route — this is boot/shutdown lifecycle, gated by `daemon.restoreSessions`
  (default true). A graceful stop also writes a **handoff** (port + token) so a successor daemon rebinds
  the same port with the same token — ssh forwards stay valid and every client heals with a plain
  reconnect.
- **Where it lives.** `ledger.rs` (`LedgerEntry`/`LedgerAgent` incl. `ui`+`model`, `snapshot` — now
  enumerating `state.chat` alongside the PTY roster — `plan_restore`, `resolve_resume`, `respawn`,
  `retire_to_recents`), `chat.rs::resurrect_chat` (the chat spawn recipe), `lifecycle.rs`
  (`Handoff::consume`/`rebind`), `state.rs` (the `restored` watch gate).
- **Key behaviors.** The ledger stores **no argv** — resurrection rebuilds commands through the normal
  spawn path so hook URLs / shims / themes match the *new* daemon. **Session ids are preserved**, so
  every persisted layout tab, linked-terminal edge, and open window rebinds with no client migration. A
  recorded claude resume id is a *claim*, not a promise — restore verifies the transcript on disk first
  (claude 2.1.204 interactive sessions persist no transcript, so a missing file boots fresh, not "No
  conversation found"). A client connecting mid-restore waits `wait_restored()` (cap 15s) so it never
  prunes still-respawning tabs. A busy handoff port after ~5s falls back to an OS-assigned port (stay up
  on a fresh port rather than die); a crash leaves no handoff; an explicit conflicting `--port` wins.

## Graceful shutdown & close-all

- **What & when.** End every session (daemon stays up), or end everything and stop the daemon.
- **How it's used.** `DELETE /api/v1/sessions` kills all sessions; `POST /api/v1/shutdown` kills all then
  stops the daemon. SIGINT/SIGTERM does the same graceful stop.
- **Where it lives.** `api/shutdown.rs` (`delete_all_sessions`, `shutdown`, `kill_all_chat`),
  `lifecycle.rs::shutdown_signal`.
- **Key behaviors.** Both stop **chat drivers too** (`SessionManager::kill_all` only covers PTYs — chat
  agents would otherwise keep running and billing). `shutdown` SIGHUPs everything, replies at once (the
  caller's tunnel is about to drop with the daemon), then outlasts the kill-escalation grace before
  tripping the graceful-stop future so a session that ignores SIGHUP isn't orphaned. A graceful stop
  flushes the ledger (which now carries chats for resurrection), writes the handoff, and removes the
  manifest — it must **not** retire chats here (that would strip their workspace mapping and the
  reconciler would drop them). The *deliberate* close-all/`shutdown` above still kills chat drivers.

## Update awareness (daemon-side)

- **What & when.** The daemon checks its own GitHub releases a few times a day and reports whether a
  newer chimaera exists — so even a browser-only user learns about updates from the daemon they're
  already talking to. *Applying* updates stays with the clients (the app's signed updater;
  `chimaera connect --update-daemon`).
- **Where it lives.** `update.rs` (`get_update`, `run_checker`). Route `GET /api/v1/update` + an
  `update` frame on `/ws/events`.
- **Key behaviors.** Transport is a bounded `curl` subprocess (10s, 1 MB) — the one HTTP client every
  HPC site ships and proxies. Check interval 6h with a 60s initial delay (staggers login nodes booting
  together). Dev builds (version `0.0.1`) stay silent and off the network unless `CHIMAERA_RELEASES_API`
  overrides; users can disable with `update.autoCheck`. Test knobs: `CHIMAERA_RELEASES_API`,
  `CHIMAERA_UPDATE_CURRENT`.

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Why persistence is the central bet
_Captured 2026-07-09 — drafted from DESIGN.md + code, confirmed live with the maintainer._

- **Problem it solves.** "Close the laptop, nothing dies" — tmux's ownership model, the exact inverse
  of code-server's failure. The daemon owns everything; windows are just views.
- **Core.** This is *the* core bet, and several mechanics are load-bearing, not conveniences: session
  ids are preserved across a restart so layout/links rebind with no migration; graceful shutdown
  **force-kills sessions first, then waits the SIGKILL grace** (a HUP-ignoring agent would otherwise
  reparent to init and survive); the ledger stores no argv (it rebuilds via the normal spawn path so
  the new daemon's hooks/shims/themes match).
- **Improvable.** Chat-session resurrection across a restart (the former `sv-11` follow-up) now
  ships — see the capture below; its resurrect-vs-retire mechanics remain an improvable UX detail.
- **Do not change:** never-silently-kill; daemon-owns-everything / windows-are-views; the
  force-kill-then-grace shutdown mechanism.

### Chat resurrection across a daemon restart — why it exists
_Captured 2026-07-12 (from the maintainer)._

- **Problem it solves:** native feel — nothing more. A restart (update / crash / `kill`) that lost
  your chats would break "close the laptop, nothing dies"; chats must be daemon-owned and come back
  like the TUI because *"it should be the native workspace."*
- **How settled it is:** the *behavior* — a restart brings your chats back — is the aim and a
  promise (*"yes, that is what I am aiming for"*). The specific mechanics are *"more just a UX,"*
  not a contract.
- **Deliberately open / where it may go:** the resurrect-resumable-live / retire-the-rest policy is
  a UX detail, free to improve toward a smoother experience.
- **Do not change (or: open to change):** *"keep a smooth UX for the user"* — don't regress the
  core "never silently lose a chat" property (the daemon-owns-everything bet above). Grade: an
  **addition** — the exact resurrect-vs-retire mechanics can change if improved.
