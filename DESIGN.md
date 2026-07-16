# Chimaera — Design Document

*Founding design, 2026-07-05. Synthesized from a multi-agent research pass over the July 2026
ecosystem (agent session managers, remote-dev prior art, Claude Code integration surfaces, HPC
constraints) and three competing architecture proposals, each adversarially critiqued.*

## One-liner

**Chimaera is an agent workbench.** A single static Rust binary runs as a
tmux-grade session daemon on whatever host owns the work — a remote server, an HPC login node,
or your laptop — and serves a workspace-oriented UI where the primary objects are *agent
sessions* living inside *folders*, surrounded by the file previews, terminals, and git state
that show what those agents actually produced.

**Chimaera is a general-purpose coding tool** — any language, any repo, any host. The author's
bioinformatics-on-Slurm workflow is the founding use case and the harshest deployment test
(no root, old glibc, SSH-only), not the product boundary: a design that survives an HPC login
node runs anywhere, and the scientific-format previews are one instance of a general
rich-preview layer.

## The problem

The current state of the art for "Claude Code on a remote machine" is a two-tool split:

- **code-server in a browser tab**: great file/image/Markdown/git inspection, but terminals are
  structurally coupled to the browser tab — reload loses them, reconnection tokens break
  permanently ([code-server#2276](https://github.com/coder/code-server/issues/2276),
  [#4773](https://github.com/coder/code-server/issues/4773)) — so you end up nesting tmux inside
  a browser terminal, which is misery.
- **Ghostty + SSH + tmux/zellij**: real persistence, but switching between many agent chats is
  awkward and there is zero rich file viewing. The deliverable of a bioinformatics agent session
  is usually *files* — plots, MultiQC reports, tables — not the conversation.

Anthropic's own surfaces don't close the gap either: the Claude Code desktop app is
**chat-first, workspace-weak** — the folder is an attribute of the chat, so it's hard to see
outputs and what exists on disk. Chimaera inverts that: **workspace-first, chat-many**.

## What exists (July 2026) and the gap

The multi-session manager space is crowded but consolidating (Terragon shut down 2026-02,
Omnara archived 2026-02 noting CLI-wrapping "became unfeasible to maintain", CUI archived,
Crystal deprecated for Nimbalyst). Verified state of the nearest neighbors:

| Tool | Has | Missing |
|---|---|---|
| Claude Desktop (SSH sessions, Apr 2026) | parallel sessions, diff/terminal panes | Electron, no survive-disconnect story, weak file/workspace surface, closed |
| Claude Code Remote Control / claude.ai/code | phone steering, push notifications | no multi-session HPC dashboard, no previews, process must stay alive |
| opencode (`opencode serve`) | headless server + attachable TUI/web clients, SSE bus | its own agent (not Claude Code), zero file previews, unbounded storage growth |
| Zed remote dev | musl server auto-deployed over SSH, ACP agents | no `RemoteCommand` support ([zed#25896](https://github.com/zed-industries/zed/issues/25896)), idle disconnects, no session dashboard |
| Wave Terminal | persistent blocks + previews | Electron, remote previews need extra deployment, not agent-native |
| claude-squad | tmux + worktrees over SSH | terminal-only, no rendering of anything |
| Happy | best mobile/notification story | chat-only, no files |
| Operon (closest in spirit — bioinformatics AI IDE) | Claude-in-tmux over SSH, Slurm-aware | fat desktop client (no headless server), glibc ≥ 2.35, previews limited |
| Vibe Kanban | multi-agent orchestration, runs headless | task/PR-centric, no data-file previews, no HPC awareness |

**The four-way intersection nobody ships:** (1) a no-root single-static-binary server *on* the
cluster, (2) tmux-grade persistence with instant lossless reattach, (3) an attention-aware
multi-agent dashboard, (4) scientific file previews (MultiQC HTML, ipynb, Parquet, PDF).
Every competitor covers at most two. That intersection is Chimaera.

Honest sizing: this is a *niche workbench with a real moat*, not a mass-market platform. The
moat is exactly the part Anthropic won't build — open source, no Electron on the cluster,
RHEL 8/no-root deployment, and previews for the files scientists actually make.

## Product model: workspace-first

The VS Code mental model, applied to agents:

- **Window = workspace = folder.** Opening a folder — local, or `host:path` over SSH — gives
  one window rooted there: file tree, previews, git state, and N agent sessions all scoped to
  that folder.
- **Sessions belong to the workspace, not the client.** This aligns perfectly with Claude Code
  itself: sessions persist as JSONL under `~/.claude/projects/<encoded-cwd>/` and `--resume` is
  cwd-scoped, so workspace root ≡ session cwd is the natural join key.
- **Plain shells are first-class sessions.** A workspace hosts agent sessions *and* ordinary
  persistent terminals (zsh/bash rooted in the workspace) for running things manually, fixing
  up state, poking at pipelines. Under the hood they are the same thing — daemon-owned PTYs
  with server-side terminal state, instant reattach, multi-attach — differing only in chrome
  (shells get no attention badges). This is where tmux/zellij gets replaced, not just the agent
  chats.
- **The daemon owns everything; windows are views.** Close the laptop mid-run: nothing happens
  to the sessions. Reopen: the window reattaches to identical state. This is tmux's ownership
  model (daemon owns PTYs/state, clients are dumb renderers) — the exact inverse of
  code-server's failure mode.
- **A remote window is spawned like code-server**: `claude` + `chimaerad` installed user-side on
  the HPC, logged in once there; the client machine needs nothing but Chimaera (or just a
  browser).

## Architecture

The deep architecture + rationale — one-binary/two-roles, the SSH transport and
lossless reconnect, HPC-realistic state storage, the three-tier agent event model,
the web-first client, in-window layout, naming/orientation, file previews, linked
terminals, and Git + Slurm — lives in
**[docs/agent-guides/architecture.md](docs/agent-guides/architecture.md)** (moved out
to keep this document a readable spine). Nested `AGENTS.md` maps link straight to the
section they need.

## Scope philosophy and non-goals

**v1 targets the optimal build, not a compromised first cut.** Nothing on the roadmap is
scaffolding to be thrown away: the daemon, the wire protocol shape, the web UI, the Tauri
shell, and every agent tier are pieces of the final architecture, added in a buildable order.
The milestone sequence exists so each stage is independently dogfoodable — not so the good
parts can be deferred indefinitely.

True non-goals (product boundaries, not deferrals):

- **No IDE-grade editor** (amended 2026-07-06: author wants markdown/text *editable*).
  Lightweight single-file editing IS in scope: the CodeMirror viewer flips editable, Cmd+S
  saves through the daemon, dirty-dot on the tab, markdown gets an edit/preview toggle. The
  non-goal that remains: no LSP, no completions, no multi-file refactoring, no debugger —
  serious editing still lives in real editors; agents write most code anyway.
- No own mobile app, no E2E relay service (free-ride `--remote-control` / ntfy), no Electron,
  nothing heavy on the cluster. (In-window split panes ARE in scope — author decision
  2026-07-06, superseding the earlier "no tiling WM" line; see In-window layout below. The
  non-goal was solo-scope caution, not product conviction, and panes are the mechanism that
  puts an agent and its outputs side by side.)
- No bespoke GPU-native client (see Client section — a different project, not a deferral of
  this one).

After-1.0 (sequencing, not compromise — these need a working product to be designed well):

- Published/versioned public protocol. tmux and LSP earned protocol status *after* adoption;
  the internal protocol is designed cleanly so publication is a docs-and-freeze exercise.
- Multi-host hub federation (manifest + per-host windows cover multi-cluster use until then).
- sbatch-offloaded agent sessions — **Mode 2**: own the full workbench on a compute node via the
  negotiated tunnel ladder (specced in Architecture → Environment prelude & compute-node sessions).

## Risks (ranked)

1. **The Anthropic billing overhang.** Verified: Anthropic announced (June 15, 2026), then
   paused indefinitely, moving Agent SDK / `claude -p` / third-party-app usage onto small
   monthly credit pools billed at API rates, while interactive TUI keeps subscription limits
   ([support article](https://support.claude.com/en/articles/15036540),
   [Zed's response](https://zed.dev/blog/anthropic-subscription-changes)). Tier B is exactly
   the usage class targeted — and since 2026-07-07 it IS the default view (author decision,
   accepting the exposure). Mitigation is structural: Tier A stays fully supported, every
   chat session is one toggle from the TUI, and one settings default
   (`agents.defaultView`) flips new sessions back to terminals if the change lands.
2. **Protocol churn.** The stream-json control protocol is semi-documented and has already
   changed shape once this year. Mitigation: version pinning, handshake-keyed compat shim,
   startup smoke-test, thin adapter.
3. **First-party convergence.** Remote Control (Feb 2026) already commoditized phone
   steering; a first-party session dashboard within 18 months is probable. The durable moat is
   previews + no-root HPC deployment + workspace-first UX — which argues for building the
   preview layer *early*, not last.
4. **Per-site HPC policy roulette.** systemd `KillUserProcesses`, process reapers that
   whitelist tmux by name, tmp-scrubbing, login-node reboots, outbound-blocked login nodes.
   `chimaera doctor` diagnoses; some sites simply won't allow it. Also expect one "please
   explain this binary making HTTPS POSTs" ticket from HPC security.
5. **Solo scope.** Every reviewed plan was judged 2–3x optimistic. The mitigations are
   structural: each milestone independently retires a piece of the current workflow, so
   partial completion still pays; and the graveyard (Terragon, Omnara, CUI, Crystal — all dead
   within a year) says keep the core small and the Claude coupling behind one seam.

## Roadmap

Estimates are the adversarially corrected ones (~2x the optimistic drafts), for a strong solo
dev working with AI agents, part-time.

| Milestone | Scope | Retires | Est. |
|---|---|---|---|
| **M0 — Walking skeleton** | musl CI, `serve` + token + hello-world UI, `connect` (ssh push, attach-or-spawn, tunnel), manifest. **Prove the deployment story on the real cluster first** — incl. round-robin login nodes and daemon-survival policy. | — | 2–3 wks |
| **M1 — Persistent terminals** | PTY sessions with server-side terminal state (resize/multi-attach-safe): named plain shells per workspace as first-class sessions, xterm.js+WebGL, event-bus seq replay, workspace + session list UI. | tmux/zellij-in-code-server | 4–6 wks |
| **M2 — Agent sessions + attention** | Tier A: workspace-scoped `claude` TUI sessions in PTYs, hook injection, JSONL transcript viewer, attention state machine, naming pipeline, session strip + triage dashboard + digest (see Interaction model), ntfy/webhook. | scattered agent chats | 5–6 wks |
| **M3 — Previews wave 1** | file tree, images+thumbnails, Markdown, sandboxed HTML reports (MultiQC), CSV/TSV with gzip tier. | most code-server use | 3–4 wks |
| **M4 — Previews wave 2 + git** | Parquet paging, ipynb, PDF, JSON tree, large-file guards; git status/log/diff panel. | code-server, entirely | 3–4 wks |
| **M4.5 — Linked terminals** | OSC 133 shell integration + per-session command journal, daemon HTTP MCP server (`run_in_terminal`/`read_terminal`/`list_terminals`), link UX (agent top-bar chips, reference-band drag, auto-link on `@term:` mention), sentinel fallback for remote shells. | agent↔shell copy-paste | 3–4 wks |
| **M5 — HPC layer** | Environment prelude (host/workspace/session scopes, one spawn seam) + Slurm detection/strip + job↔session links + Mode 1 (login-node agent + Slurm skill); `doctor`, transcript pruning/quotas, docs, demo, v0.1 release. *(scope grew with the 2026-07-14 design pass — estimate predates it.)* | — | 2–3 wks |
| **M6 — Optimal-build completion → v1.0** | Tauri native shell (window per workspace, native notifications, menubar badge), Tier C ACP agents (Gemini native). ~~Tier B structured chat mode~~ (+ native codex app-server) delivered early, 2026-07-07. | Claude desktop app | 6–8 wks |
| After 1.0 | Hub federation, sbatch-offloaded sessions, protocol publication, single-file editing. | | ongoing |

Realistic wall-clock: "code-server uninstalled" at M4 (**~5–7 months part-time**); the full
optimal build (v1.0 at M6) **~8–10 months part-time**. M1 alone already improves daily life,
which is the survival property that matters.

## Decisions log

- **2026-07-08 — Full protocol coverage for the chat surface.** Everything the official
  VS Code extensions drive is now mapped (or explicitly recorded as out of scope) in
  `crates/chimaera-agent/PROTOCOL.md`: claude checkpoints/rewind (client-minted user-frame
  uuids anchor `rewind_files`; the conversation forks via `--fork-session
  --resume-session-at` through `POST /sessions/{id}/rewind`; needs
  `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING=true`), subagent task rows, the /mcp panel,
  `generate_session_title` feeding the workbench naming chain (same ai-title slot the TUI's
  transcript records land in), rate-limit telemetry (header chip + RateLimited rail state),
  permission-destination cycler, native mid-turn queueing (NO client queue — the CLI queues
  stdin writes; the extension's own model); codex `turn/steer` type-through with the
  error-parse retry, the approval-mode table (read-only/auto/full-access/plan via
  `thread/settings/update` + per-turn fallback; requires `capabilities:{experimentalApi}`
  at initialize), per-model reasoning efforts from `model/list`, execpolicy/network-policy
  amendment decisions, live `outputDelta` command streaming (capped at TOOL_OUTPUT_HEAD),
  data-URL image input, and `thread/name/updated` → naming. State rule of thumb: hooks stay
  authoritative for Tier A (TUI) sessions and keep working across CLI versions; chat mode is
  protocol-authoritative because `UserPromptSubmit`/`Stop` don't fire under `-p` stream-json.
- **2026-07-07 — Structured chat is the default agent view.** Supersedes the *default* half
  of the 2026-07-06 "PTY/TUI primary" decision (the tier itself is not deprecated: Tier A
  is the toggle target and the automatic handshake-failure fallback). New claude/codex
  sessions open in the chat surface; `agents.defaultView` is the one-key flip-back lever
  against the paused billing split. Toggle semantics: same chimaera session id, stop the
  current process → resume in the other mode (`--resume <native-id>` / `thread/resume` /
  `codex resume <uuid>`); a mid-task switch requires an explicit confirm. Chat journals:
  seq-numbered append-only JSONL, 4→2 MiB turn-boundary compaction, 100 MiB/200-file dir
  budget, gap-replay reconnects. Wire-format facts + extension-mined protocol inventory
  live in `crates/chimaera-agent/PROTOCOL.md`.
- **2026-07-06 — Frontend: Svelte 5** + TypeScript + Vite (author's call).
- **2026-07-06 — Primary agent mode: PTY/TUI + hooks.** Claude Code runs inside Chimaera the
  way it runs in VS Code's integrated terminal — the real interactive TUI in a pane, normal
  subscription billing. The stream-json structured chat UI (Tier B) lands at M6 as an
  enhancement; TUI mode remains the default.
- **2026-07-06 — v1 is the optimal build.** No "maybe in v3" deferrals: Tauri native shell,
  structured chat mode, and ACP multi-agent are all on the v1 path (M6). Plain persistent
  shells are first-class workspace sessions from M1. The only exclusions are true non-goals,
  not compromises.
- **2026-07-06 — exited shells vanish (tmux semantics).** The daemon reaps a session when its
  child exits; no gray corpse rows. Persistent lifecycle rows ("finished", awaiting review)
  are *agent*-session semantics and arrive with M2's attention states.
- **2026-07-06 — in-window split panes, focus mode, bundled mono.** Author request: tiling
  panes (drag-and-drop + keyboard), pane zoom, a rail-collapsing focus mode that leaves the
  session strip as the always-on orientation layer, and JetBrains Mono bundled for terminal +
  UI monospace accents. Supersedes the "no tiling WM" non-goal. Builds after M2a.
- **2026-07-06 — window host label.** The daemon indicator shows what the user calls the
  machine: "local" for an untunneled daemon, else the ssh alias passed by `chimaera connect`
  via the `#host=` hash param (the VS Code Remote convention); the raw hostname is hover
  detail. Raw machine hostnames (the opaque `abc12de3.example` kind) confused the author on
  day one — labels must be human.
- **2026-07-06 — native shell pulled forward from M6 (windows + home screen + remotes).**
  `crates/chimaera-app` is a Tauri 2 shell in a deliberately standalone cargo workspace (the
  webview stack never touches the musl/HPC builds). Its binary doubles as the daemon
  (`--daemon`), spawned detached so quitting the app kills nothing. Windows load the
  daemon-served UI over `127.0.0.1` with the token in the URL fragment — the exact browser
  mechanism, one UI codebase; a Tauri capability grants those origins the IPC bridge. A
  workspace-less window is a **home screen**: workspaces by recency with live session/
  needs-you badges, plus saved remote hosts (`~/.chimaera/hosts.json`) with one-click
  connect — probe, auto-install from `~/.chimaera/dist` (built by `just dist`, target
  detected via `uname -sm`), start, tunnel — surfaced as progress states. The connect flow
  lives in the shared `chimaera-remote` crate; the CLI drives the same code. Menu bar owns
  the browser-reserved chords: Cmd+W = close view (emitted to the focused window only),
  Cmd+T/Cmd+Shift+T = new terminal/agent, Cmd+Shift+N = new home window. M6's remaining
  scope (Tier B structured chat, ACP, notifications/menubar badge) is unchanged.

- **2026-07-06 — context bridge refinements (author feedback).** Three fixes after first
  use: (1) *explicit targeting* — a selection in a LINKED terminal references its linked
  agent (the leash is the bridge), the target's name is always shown on the reference
  affordance, and the target agent is revealed (split beside the source) before typing —
  a reference never lands out of sight. (2) *Copy provenance* — copying from any tracked
  view snapshots the selection's source; pasting that snippet into an agent composer
  appends a visible ` [from @path#L3-L9] ` / ` [from <name> output] ` tag (additive only,
  never for shells, never mutates the pasted bytes, 5-min TTL, min 24 chars — plain
  Cmd+C/V stays plain). (3) *Partitioned drop previews* — while a bottom band (reference
  or link) is armed, the adopt-as-tab preview stops above it and both bands share one
  quiet recipe (10% tint, dashed hairline, pill label; the link band tints in the
  agent's hue) — no more full-pane flashes mid-gesture.
- **2026-07-06 — linked terminals wave 1 SHIPPED (M4.5).** Daemon: OSC 133 marks scanner +
  per-session command journal, bash/zsh/fish integration injection (verified on bash 3.2 +
  zsh 5.9), exec engine (queue-with-timeout, sentinel fallback, ssh-allowlist policy),
  links model, stateless HTTP MCP server (list/run/read scoped to links), `@term:`
  mention auto-link via UserPromptSubmit with additionalContext confirmation,
  `chimaera shell-integration` CLI. UI: chips both sides in the agent hue, exec border
  pulse, drag-to-link band, link menu, auto-reveal split. 33 new tests (98 total) +
  live browser verification against a sandboxed daemon with a real claude TUI session.
  Deferred to wave 2: shell cloning (env-duplicate / journal setup-replay), file
  drag-to-reference band, journal-powered UI surfaces.
- **2026-07-06 — linked terminals (agent ↔ shell).** One primitive: a user-granted link
  between an agent session and a terminal session. Linked-only access (the agent's tools see
  exactly its links), one agent per terminal (re-link moves the leash), busy shells queue
  execs with a timeout. OSC 133 shell integration + server-side command journal; sentinel
  fallback for SSH'd remotes with a `chimaera shell-integration` one-liner for full fidelity.
  `@term:` mention by the *user* auto-links; agents cannot self-link. Link ≠ layout — the
  sidecar split is default placement, not a container. Approvals remain Claude Code's own.
- **2026-07-07 — git integration scoped (M4), read-only-first + worktree-aware.** Read-only
  inspection ships first (status decoration, side-by-side diff via `@codemirror/merge`, a simple
  full-pane changes panel, strip/session branch orientation) — stage/commit stay in a terminal
  for now. Refresh is event-driven (file save + agent PostToolUse write + terminal command-done
  + slow backstop), never a status poll; wire is an `/ws/events` invalidate-nudge + per-workspace
  pull; tree status is a client overlay, not baked into `fs::list`. A **git worktree is a
  dimension of one workspace** (not a peer workspace): agent↔branch is derived from cwd +
  `git worktree list`; P2 adds read-only worktree orientation (Branches view, branch per agent),
  P3 adds the one mutation (spawn agent/terminal into a new branch via `git worktree add`). Full
  treatment in Architecture → Git + Slurm.
- **2026-07-08 — disconnect ≠ shutdown; explicit teardown is first-class (SHIPPED).**
  Disconnect stays non-destructive: it drops the tunnel and leaves the remote daemon and its
  agents running (the survive-disconnect promise — close the laptop, the HPC run keeps going).
  The missing half was an *explicit* off switch, so a connected host row gains two confirmed
  actions distinct from disconnect: **end sessions** (force-kill every session; the daemon and
  tunnel stay up, so you start fresh without reconnecting) and **shut down** (end every session
  AND stop the daemon, then drop the tunnel; reconnecting starts a fresh daemon). This is the
  deliberate complement to the "never *silently* kill sessions" rule — accidental loss is
  forbidden, user-confirmed teardown is a feature. Correctness note that shaped the mechanism:
  killing the daemon does NOT reliably kill its work — on SIGTERM the daemon just stops serving
  and removes its manifest, so its PTY children only get a kernel SIGHUP, and a HUP-ignoring
  agent survives and reparents to init. So shutdown force-kills the sessions *first* and waits
  out the SIGKILL-escalation grace before the process exits. Built in-band (uniform local/
  remote, no ssh): `POST /api/v1/shutdown` (end all → trip graceful exit after the grace) and
  `DELETE /api/v1/sessions` (end all, daemon stays), driven through the tunnel with the remote
  token; `SessionManager::kill_all` + a public `KILL_ESCALATION_GRACE`. Local-daemon shutdown
  is intentionally not surfaced — quitting the app is the local equivalent, and the endpoints
  already work on loopback if that changes.

- **2026-07-14 — HPC environment + compute-node placement (design pass; tunnel reachability
  verified live on Sherlock).** Two deliberately separated axes. **Environment prelude:** opaque
  shell text (`ml …`, `micromamba activate`, `export …`) run before a shell/agent, *never parsed*
  by Chimaera (so conda/lmod/spack/venv/nix need zero tool-specific code), concatenated across
  host→workspace→session scopes (env last-wins), injected at the one spawn seam both shells and
  agents already share (`CHIMAERA_PRELUDE` sourced by the shell-integration rc / the `-lc` agent
  wrapper; env via `SpawnOpts.env`). Federated by the daemon-per-host model — each host's daemon
  owns its own defaults, no config sync. **Compute placement, two modes (not either/or):** Mode 1
  (login node, agent + Slurm skill — the safe default, works wherever Slurm exists; agent keeps
  API internet and dispatches to compute via `sbatch`/`srun`); Mode 2 (own the full session on the
  compute node — `sbatch --job-name=chimaera-<ws>` runs the prelude then `chimaera serve` inside
  the job cgroup; isolated, walltime-bounded, cleaned up on `scancel`). Mode 2 mechanics: `squeue`
  job-name is the reconnect registry (`%N` node, `%L` walltime), the shared FS carries a per-jobid
  `{node,port,token}` manifest (no sync — same Lustre path, same binary already visible), dynamic
  `:0` ports (multi-user/multi-workspace on one node never collide). Reaching a compute-node daemon
  is a **negotiated, bounded probe ladder** — B (loopback + ssh-adopt forward through the login
  node; *preferred*, port unexposed; verified on Sherlock incl. `pam_slurm_adopt`) → A (routable
  bind + direct login→node forward + token; verified reachable on Sherlock) → **unsupported → fall
  back to Mode 1, stated plainly** (reverse-tunnel rung dropped as scope, ladder left open).
  **Two-tier persistence** made explicit: login-node daemon = forever; compute-node daemon = until
  walltime. **Outbound gate closed on Sherlock (2026-07-14):** direct compute-node egress to
  `api.anthropic.com` verified (HTTP 405, no proxy) — Mode 2 fully viable there. Elsewhere a
  per-cluster fact: probed at job start, recorded in the manifest; where blocked, Mode 2 degrades
  by capability (terminals/previews on the node, agents via Mode 1). Deep spec: Architecture →
  Environment prelude & compute-node sessions.
- **2026-07-15 — Mode 2 core SHIPPED (daemon routes + tunnel ladder + CLI + app/home-screen
  surfaces), verified end-to-end on Sherlock.** Implemented shape: launch/discover/cancel are
  LOGIN-daemon routes (it owns sbatch, the preludes, its own shared-FS binary) — the client
  side only tunnels and opens windows; the registry stays stateless (`squeue` ⋈ per-job
  manifests under `data_dir()/compute/<jobid>`). The live pass forced one ladder amendment:
  hostbased-only node sshd (Sherlock) defeats a laptop-originated node leg, so rung B gained a
  **chained** mechanic — a login-resident `ssh -N -L` relay to the node's loopback running as
  the remote command of the same laptop ssh that forwards to it (lifetimes coupled, nothing
  orphaned, daemon stays loopback). Rung A's `--bind-routable` (opt-in 0.0.0.0, token-gated)
  amends the loopback-only security note deliberately. The app shell keeps tokens/tunnels in
  Rust under composite `"{alias}#job{id}"` keys; window restore skips compute windows — the
  squeue-rebuilt home-screen card is the reconnect path. Verified live: launch → RUNNING →
  ready → chained-B tunnel → health/self-walltime/sessions on the node → cancel → queue clean.
  Native-app visual pass outstanding (needs the maintainer's screen).
- **2026-07-16 (later) — SUPERSEDED below: Mode 2 launches are srun-only (maintainer
  decision).** The sbatch-retention rationale (next entry) over-weighted login-daemon
  restarts ("once in a blue moon" in steady state — the maintainer, correctly: "most
  people have tmux sessions going there") and the srun-as-child objection dissolves once
  the client is DETACHED: `setsid nohup srun … &` orphans it onto init, tmux-grade — it
  survives daemon restarts and ends only at walltime, scancel, or a login-node reboot.
  What srun-only buys: ONE launch mechanism that works on every partition (including
  interactive-only ones like Sherlock's `dev`, whose job_submit policy refuses batch —
  found live), no batch/interactive mode switch, no learned per-partition preferences.
  Costs, accepted openly: sessions die with a login-node reboot, and clusters that reap
  login-node user processes (where tmux dies too) are honest "not supported" territory.
  The job id comes from queue adoption (srun can't print it detached); launch refusals
  surface from the srun log's tail — Slurm's own words, preserved.
- **2026-07-16 — Mode 2 stays sbatch-based; srun-as-child rejected for session ownership
  (maintainer question, answered; SUPERSEDED same day by the srun-only decision above).** The question: "surely srun would be most compatible —
  the login-node daemon runs srun as a background process, owns it, can kill it
  automatically." The reason it loses: an srun/salloc allocation's lifetime is chained to
  its CLIENT PROCESS — if the login daemon restarts (self-update on every merge, dev
  cycles, crashes), every compute session dies with it. sbatch hands ownership to Slurm
  itself: sessions survive laptop closes AND login-daemon restarts, `squeue` is the
  stateless reconnect registry, walltime ends things deterministically. The ownership the
  maintainer wants exists already at the right level — the login daemon kills via scancel
  (cards' cancel, the banner's end-job), which beats a process handle as owner-of-record.
  srun's one genuine advantage — interactive-only partitions (Sherlock's `dev` refuses
  batch; found live) — is noted as a possible future "Mode 2b" interactive-allocation
  flavor that would be explicitly connection-tied; not in scope now. Programmatic/web
  access is unaffected either way (launch is a daemon HTTP route).
- **2026-07-16 — Loopback stays the compute-daemon default; routable bind remains per-launch
  opt-in (maintainer decision).** Raised because the connection is token-gated anyway and the
  node is "our own"; decided against flipping: compute nodes are routinely SHARED (co-tenant
  jobs on the same node reach a 0.0.0.0 bind; only the token gates them), the chained-B rung
  gives every ssh-reachable cluster a loopback path anyway (verified on hostbased-only
  Sherlock), and one leaked/logged token on a routable bind is a cluster-internal exposure
  loopback never has. Rung A via the explicit `routable` launch flag (dialog checkbox with
  exposure warning, CLI flag) is the escape hatch for clusters whose ladder finds no ssh
  path; "not supported on this cluster" stays an acceptable honest end state. No per-host
  auto-routable memory for now — premature until a real cluster defeats rung B.

Still open:

1. **License** — DECIDED (author, 2026-07-07): **AGPL-3.0-only + dual licensing**
   (commercial license by contacting the author; CLA from the first outside PR so
   the relicensing right survives contributions). Free for everyone to use,
   self-host, and modify; closed-source products/services on top pay. A later
   carve-out of protocol/SDK crates under Apache-2.0 stays possible while the
   author holds copyright.
2. **`--remote-control` free-riding** as the blessed mobile story vs. building ntfy
   approve/deny round-trips (doc recommends: both are cheap; ntfy is vendor-neutral).
## Field notes & verified-component log

Dated live-verification findings and first-deployment notes live in
**[docs/history/field-notes.md](docs/history/field-notes.md)** — a running log kept
out of the spine.

## Sources

Key verified references: [Claude Desktop](https://code.claude.com/docs/en/desktop) ·
[Remote Control](https://code.claude.com/docs/en/remote-control) ·
[Agent SDK sessions](https://code.claude.com/docs/en/agent-sdk/sessions) ·
[headless CLI](https://code.claude.com/docs/en/headless) ·
[hooks](https://code.claude.com/docs/en/hooks) ·
[ACP](https://agentclientprotocol.com/) ·
[claude-agent-acp adapter](https://github.com/zed-industries/claude-code-acp) ·
[Zed remote dev](https://zed.dev/docs/remote-development) ·
[zed#25896](https://github.com/zed-industries/zed/issues/25896) ·
[opencode server](https://opencode.ai/docs/server/) ·
[Eternal Terminal](https://eternalterminal.dev/howitworks/) ·
[SQLite locking](https://sqlite.org/lockingv3.html) ·
[Arbiter2](https://github.com/CHPC-UofU/arbiter2) ·
[Agent SDK plan usage / paused billing change](https://support.claude.com/en/articles/15036540) ·
[Operon](https://github.com/swaruplab/operon) ·
[claude-squad](https://github.com/smtg-ai/claude-squad) ·
[Happy](https://github.com/slopus/happy) ·
[Vibe Kanban](https://github.com/BloopAI/vibe-kanban)
