# Notifications & attention

How Chimaera tells you something happened while you were looking elsewhere: an agent
**finished** its turn, is **blocked on your approval** (a permission, a plan, a question),
stopped on an **error** or a **usage limit**, or **sent you a message** itself through its
`notify` tool. The daemon decides *what happened*; the native app turns it into real OS
notifications (with the app in the background, for every workspace on every open daemon), a browser tab
into Web Notifications; and inside the workbench the same facts show as the approval count
and the unread mark.

**Where it lives (shared):** daemon `crates/chimaera-server/src/notices.rs` (the feed, the
edge watcher, `GET /api/v1/notices`, the `notify` tool's back half) with the words stashed by
`chat.rs::note_for_notices` / `agents.rs::note_for_notices`, and the tool in `mcp.rs`. Native
shell `crates/chimaera-app/src/shell/notices.rs` (per-daemon watchers, suppression, click
routing, Dock badge, tray counts) over `crates/chimaera-app/src/notify.rs` (the platform
APIs). UI `web-ui/src/lib/workspace/notices.ts` (browser delivery), the `notices` frame in
`net/events.ts`, the notification bridge in `net/native.ts`, `settings/NotificationStatus.svelte`,
and in `App.svelte` the view reporting + `focusFromNotification`.

## The notice feed (daemon)

- **What & when.** Discrete "a person may want to know this now" events, one per real state
  edge of an agent session: `done` (running → finished), `input` (the turn ended waiting on
  you), `permission`, `question`, `error`, `rate_limited`, and `agent` (the `notify` tool).
  Each carries the session's display name, its workspace, a state phrase, and a body quoting
  what it was about — how the final reply starts, the permission line ("Bash: rm -rf build"),
  the question, the error.
- **How it's used.** Consumers pull it: the native shell long-polls `GET /api/v1/notices`
  (bearer-authed) once per daemon it has open; browser tabs get `{"type":"notices"}` frames on
  `/ws/events`. Which kinds exist at all is the daemon's `notifications.*` settings (so every
  consumer agrees).
- **Where it lives.** `notices.rs` (`Notices` store, `run` watcher, `get_notices`,
  `frame_since`, `push_agent_notice`); the record fields `AgentRecord.notice_note` /
  `reply_draft` (`agent_state.rs`).
- **Key behaviors.**
  - **One detector, every surface.** Agent state is written by claude hooks, chat protocol
    events, and the transcript watcher (claude chats get two of them), so notices are NOT
    emitted at the write sites: one watcher diffs `AgentRecord.state` on every change wake
    (plus a 1s backstop) and sees each transition once, whichever writer caused it. First
    sight of a session is a baseline, never an edge — a daemon restart does not re-announce
    resurrected sessions.
  - **Settle before speaking.** An edge becomes a notice only after holding ~0.8s: an
    auto-approved permission or a reply the instant a turn ends never alerts, and a turn end
    the agent immediately follows with "waiting on you" arrives as one `input` notice. A
    `done` is held back while hook-tier subagents are still running (the unread marks'
    rule), and the Mastermind is never announced (its dock owns its attention).
  - **Words ride the record.** Writers stash the descriptive half (`note_for_notices`): the
    chat reply draft keeps the opening of the turn's LAST prose segment (a tool call starts a
    new one), permission/question/error events set their line, TUI hooks contribute the
    Notification message and — on newer claude builds — the Stop hook's final message. The
    watcher consumes the note at the edge, so a stale one never labels a later notice.
    Markdown emphasis is stripped; text is capped at 240 chars.
  - **Bounded, never replayed as news.** A 64-notice ring; ids restart per daemon process,
    so every response carries a `boot` nonce. A consumer with no/mismatched `boot` starts at
    the head (opening the app never replays history); a reconnecting one gets only notices
    younger than 10 minutes. `/ws/events` clients always start at the head.
  - **The long-poll** returns at once for new notices, a changed attention set (`attn` hash),
    or a fresh consumer; otherwise it holds up to `wait` seconds (cap 30). It releases on
    shutdown (`AppState.stopping`) so a held poll never stalls a graceful drain.
  - **The attention set** returned alongside is the live sessions blocked on an approval
    (`needs_permission` — permissions, plan approvals, questions), Mastermind excluded: what
    the Dock badge and the tray count.

## Agent-sent notifications (`notify` tool)

- **What & when.** Every session that has the chimaera MCP server (claude TUI + chat, codex
  chat) can call `notify(message, title?)` — "ping me when the job finishes" now works. The
  tool description and the MCP instructions steer agents to use it only for news the user
  asked for or would want while away: turn ends and permission asks are already automatic.
- **Where it lives.** `mcp.rs` (`notify_tool_def`, `notify`, `ALWAYS_ALLOWED_TOOLS`),
  `notices.rs::push_agent_notice`.
- **Key behaviors.** Pre-approved for every session so it never raises a permission prompt
  of its own — claude via `permissions.allow: ["mcp__chimaera__notify"]` in the generated
  settings, codex via `SpawnSpec.mcp_auto_approve` (the driver answers that one tool's
  elicitation). Rate-limited per session (one per 5s, 20 per hour; the refusal text tells the
  agent why). An agent message supersedes the automatic `done`/`input` notice for the same
  session for 2 minutes, so "notify me when done" pings once, in the agent's words. With
  "Messages from Agents" off, the tool answers that the user turned it off.

## Native notifications (the app)

- **What & when.** Real OS notifications from the shell, posted once however many windows
  are open, for every workspace on every daemon the app has open — including workspaces with
  no window. (Closing the last window quits the app, so notifications stop with it.)
- **How it's used.** Automatic. Clicking one raises the window already showing that session
  (else a window on its workspace, else opens one) and focuses its tab. Settings →
  Notifications shows whether the OS allows them, with **Allow notifications** (asks now),
  **Open System Settings** (after a denial), and **Send test**.
- **Where it lives.** `shell/notices.rs` (`start`, `watch`, `apply`, `route_click`,
  `mark_seen`, `window_scoped`/`take_pending_focus`), `notify.rs` (macOS
  `UNUserNotificationCenter` + delegate, Dock tile badge/bounce; `notify-rust` on
  Linux/Windows). Commands: `report_window_view`, `take_pending_focus`,
  `notification_permission`, `request_notification_permission`, `open_notification_settings`,
  `test_notification`; event `focus-session`.
- **Key behaviors.**
  - **One watcher per daemon** (local, each tunnel, each compute-job tunnel), re-reading the
    port + token before every poll (both move on reconnect); a supervisor starts watchers as
    tunnels appear, and a watcher ends — dropping its badge contribution — when its daemon
    goes away.
  - **Never alert about what you're looking at.** Windows report the sessions on screen
    (each pane's active tab; only the zoomed pane while zoomed); a notice about one of them
    in the focused window is dropped. Other tabs and windows DO alert while Chimaera is in
    front — that is the "which tab needs you" case — unless "Notify While Chimaera Is in
    Front" is off. macOS presents these banners in the foreground (the delegate's
    `willPresent`).
  - **One alert per session.** A newer state takes back the session's older alert (agent
    messages are kept); a blocker that stops blocking (answered anywhere) takes its alert
    back; focusing a window clears alerts for the sessions it shows. Alerts group per
    workspace in Notification Center (thread id = host + workspace); remote hosts append the
    host to the subtitle.
  - **Clicks survive a relaunch**: the notification identifier encodes host, workspace, and
    session, so a click on an alert from an earlier run still routes. A window opened by a
    click is owed its focus until the page asks (`take_pending_focus`), so the event can't
    race the page's listener.
  - **Dock**: the badge counts sessions awaiting approval across every daemon; a new
    approval bounces the icon once while Chimaera is in the background. Both talk to
    `NSApp`'s dock tile directly, independent of which windows exist.
  - **macOS needs a real bundle.** `UNUserNotificationCenter` throws outside an `.app` and
    silently refuses a bundle whose signature doesn't bind its `Info.plist` — a bare
    `cargo run` has no notifications (logged once), and the isolated dev app ad-hoc signs its
    wrapper bundle for exactly this reason. The first notification ever raises the OS
    permission prompt.
  - Linux/Windows: no subtitle line (it leads the body); a bounded number of shown
    notifications wait for their click; alerts can't be taken back.

## Browser notifications

- **What & when.** The same notices as Web Notifications, for a Chimaera tab in a browser.
  Needs permission (Settings → Notifications → Allow; browsers require the click).
- **Where it lives.** `workspace/notices.ts` (`deliverBrowserNotices`, `clearBrowserNotices`).
- **Key behaviors.** Every tab receives each notice, so tabs coordinate over a
  `BroadcastChannel`: a focused tab showing the session claims it as seen (nothing posts);
  the others wait 250ms for a claim, then post under a per-notice `tag`, so duplicates
  collapse into one alert. A newer alert about a session closes that tab's older one;
  returning to a tab clears alerts for what it shows. The native app never takes this path.

## Counts and the unread mark (in the workbench)

- **Counts mean approvals.** Every number — the rail's workspace pill, the window title's
  `(N)`, the focus strip's "N awaiting approval", the Home screen's per-workspace badge, the
  Dock badge, the tray's per-window "— N awaiting approval" — counts only live sessions in
  `needs_permission` (`needsApproval` in `workspace/sessions.ts`). Finished and
  waiting-for-input sessions are news, not a number.
- **Unread is the news cue.** A session whose turn finished (or ended waiting for input)
  while it wasn't focused wears a bold full-ink name plus a small accent dot — on the rail
  row (in the close button's slot), the pane tab (the dot becomes × on hover), the focus-strip
  chip, and the dashboard card (see [dashboard.md](dashboard.md) for the unread rules).
- **Window wayfinding.** macOS's Window menu (and the Dock icon's menu) list every window by
  its title, which carries the `(N)` approval prefix; the tray names each workspace window
  with its count. See [native-app.md](native-app.md).

## Settings

Category **Notifications** (daemon-scoped, `settings/schema.ts`): `notifications.turnFinished`,
`notifications.needsYou` (permission / question / waiting / error / limit),
`notifications.agentMessages`, `notifications.sound`, `notifications.whileFocused`,
`notifications.dockBadge` (badge + bounce). All default on. Per daemon, like every setting: a
remote host's alerts follow that host's settings.

## Status: partial

- Linux and Windows notifications are build-checked only; the click route and the Dock/launcher
  badge there have not been driven on real desktops.
- Hook-less agent TUIs (codex/gemini/agy terminals) have no attention state, so they produce no
  notices — honest absence, same as their rail dots. Codex in chat mode is covered.

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Notifications — why they exist
_Captured 2026-09-25 (from the maintainer, answering the intent questionnaire in the building session)._

- **Problem it solves:** "Just nice, so you know when things happen" — while agents run, you find
  out when one finishes or needs you without watching every tab.
- **How settled it is (intended vs provisional):** having notifications is intended ("we want
  notification"); every mechanism behind them "can change". Nothing here — the kinds, the settle
  window, one-alert-per-session, alerting while Chimaera is in front, the pre-approved `notify`
  tool — is a locked contract.
- **What shaped it / left open:** interactivity is the point — "good to have it interactive": a
  notification should take you somewhere (click → the session's window and tab), not just inform.
  Richer interaction (e.g. answering from the banner) is open for later.
- **Do not change (or: open to change):** **open to change**, with one standing aim: keep a
  **basic observability/notification framework that can be extended** — one feed of
  "something happened" events that new sources (other agents, terminals, jobs) and new
  consumers can plug into, rather than one-off alerts wired per surface. Grade: an addition to
  the core, not a core bet.
- **Earlier direction (same day, while building):** a number (Dock badge, rail pill, window-title
  count) should mean something really requires approval — never unread news — and unread needed
  a clearer cue than a bold name alone, without getting loud. Also an addition: improvable.
