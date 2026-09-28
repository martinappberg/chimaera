# Optional mirrors and workspace ownership

This module owns daemon-side background mirrors and handoff. Parent:
[server map](../../AGENTS.md). It is inert until the native app provides a scoped,
revocable delegation over the authenticated local API.

| File | Responsibility |
| --- | --- |
| `mod.rs` | Bounded, credential-free persistent state, ownership/import fences, session pins and deferred-command policy. |
| `authority.rs` / `authority_tests.rs` | Immutable workspace-bound worker acceptance, credential-free persisted latch, renewal/route/root guards and synthetic side-effect regressions. |
| `routes.rs` | Authenticated configure/status/privacy/profile/power/hydration HTTP handlers. |
| `projects.rs` | Passive bounded cloud-project discovery and explicit per-device local adoption; native-picked folder validation, saved directory identity, retry and legacy-import fences. |
| `projects/tests.rs` | Synthetic loopback HTTP plus real Git transfer, passive-read, conflict, retry, restart and two-device destination checks. |
| `execution.rs` / `execution/` | Negotiated execution leases, independent stop watchdog, durable launch/crash evidence, immutable receipts and stopped same-installation recovery. |
| `engine.rs` | Independent lease renewal, mirror coordinator, transactional hydration, profile execution and lazy return. |
| `release.rs` | Bounded clean-release retry for the account publication fence; changed ownership, account or lease never retries. |
| `provider_gate.rs` / `provider_tests.rs` | Per-agent cloud readiness, bounded blocked-provider status, and staged retry/cancellation tests with a synthetic CLI and real PTY. |
| `protocol.rs` | Additive account contract subset and strict worker host-to-holder identity translation; intentionally no link/TLS dependency in the daemon. |
| `transport.rs` | Bounded external curl/git children; cached mirror-only Git compatibility selection; credentials only in memory, never argv or Git config. |
| `policy.rs` | Mirrored-path policy, credential filtering, size budgets and cloud-profile classification. |
| `mirror.rs` | Separate shadow and repository Git directories, incremental transfer and conservative hand-back. |
| `repository.rs` | Portable remote/tracking allowlist; bounded ref import, compare-and-swap adoption and index/ref-lock cancellation cleanup. |
| `config.rs` | Portable agent configuration export/import, scoped environment-omission diagnostics and destination connection identity preservation. |

One recorded holder and epoch controls shared writes. Legacy v1 retains its
existing offline-local behavior and expired remote conversation forks. Negotiated
v2 execution requires an unexpired acquire/renew proof, never a passive GET.
A request-start deadline reserves stop time; clock divergence closes admission.
The independent watchdog fences chat/PTY input and owned process groups, including
stalled startup. Clean release waits for observed termination, durable publication
and an exact immutable keeper receipt. Each managed launch first persists active
execution evidence; same-boot restart cannot treat an empty registry as proof that
old children stopped. A separate enrollment latch rejects lost ordinary state or
protocol downgrade. macOS boot-session UUID and Linux boot ID can distinguish a
cold reboot; neither authorizes takeover by another device.

The capability remains `managed_processes` with `expired_takeover:false`.
Detached descendants and unannounced OS-resume scheduling require a stronger
supervisor boundary before abrupt cloud takeover is allowed. Native installation
proofs never enter the daemon. Recovery accepts only a bounded same-installation
mirror/release grant after execution stops, keeps it in memory, publishes the
stopped source and consumes release; it cannot acquire/renew or resume execution.
Failed or ambiguous recovery retains the local files and execution fence. Clean
handoff stops agents before final export and releases only after the mirror and
bundles are durable. A fresh publication retains its account fence for ten seconds; release waits at most fifteen seconds and retries only `mirror_commit_in_progress` while the same server-confirmed holder, epoch and live lease remain valid. An unstarted structured Claude chat with no native transcript is omitted only when a complete bounded startup-only journal, fresh-spawn recipe, and no submitted input or background work prove it empty; snapshots leave its source live, while clean handoff atomically fences input and durably suspends it for local return. Only exported agents enable automatic worker wake. Missing meaningful or ambiguous history still fails the flush and retains local ownership.

Configuration export keeps portable settings and MCP definitions, including
validated `bearer_token_env_var` names, while excluding credential values and
positively device-bound app helpers, outside-project home paths and loopback
services. Paths inside the copied project remain portable. Environment omission
diagnostics come only from declared environment maps in active config files;
redacted plugin metadata and desktop runtime variables are not requirements.
These diagnostics mean “not copied”, not “confirmed missing on the destination”.
Import preserves cloud-local credentials. A same-name MCP with a different
URL, process, arguments, credential reference or supplied environment/header
configuration preserves the entire destination connection rather than retargeting
its credentials. Other preferences merge for matching connections. Conflicts do
not yet have a user-facing report; imported settings must not be described as
having replaced every existing connection.

No account refresh token or agent credential enters this module. Delegations and
short-lived Git passwords are memory-only. Never log remote response bodies,
credential helpers, or secret-bearing structs. Filesystem work runs off the
reactor. Every directory walk, child output, transfer, queue and state map is
bounded. Shadow commits never touch the user's index or branch. Hand-back never
resets a dirty worktree or rewrites a divergent branch.

Hand-back fetches never overwrite `FETCH_HEAD`. Active-branch fast-forward holds
the real index reservation and a prepared Git ref transaction before touching
the working tree. Its bounded finalizer survives caller cancellation and installs
the matching index after a committed ref; ambiguous failures retain the prepared
index. Prepared transactions serialize so their helper cannot deadlock on the
two-child transport budget. Git selection is probed once asynchronously with
credential-free, output-capped two-second helpers. The bounded 30-second wait for
a helper slot is retryable and never caches a transient capacity failure. On macOS only, an older or
unknown PATH Git falls back to `/usr/bin/git` if that binary reports at least
2.45 (the upstream curl POST-size reuse fix); modern PATH Git and other platforms
keep their existing selection. This affects only mirror helpers, not ordinary
workspace Git settings. Failed HTTP transfers with an older/unknown selected
Git give static upgrade guidance without exposing stderr. There is no enlarged
POST buffer, automatic failed-push replay, or weakened publication check.
Another worktree's branch is retained separately. Unsupported
transaction support preserves a cloud ref instead. Network Git has a finite
16-minute deadline; ordinary helpers retain short deadlines. Repository and
shadow histories are quota-bound and retained, never silently rewritten/pruned.

Cloud discovery is independent of power state and the obsolete global projects
folder. `GET /api/v1/pro/projects` returns `{projects,error}`; each row has
`workspace_id`, `name`, `host_id`, `host_alias`, `local_root`, `available`, and
`error`. Refreshes are serialized, cached for 30 seconds, bounded to ten seconds,
eight workers and 128 rows. They use ordinary cached worker GETs: no wake intent,
mkdir, Git fetch, workspace registration or baton mutation. The worker's explicit
`cloud_internal` setup-workspace marker excludes provider-login scratch projects
from both discovery and automatic mirroring.

`POST /api/v1/pro/projects/open` accepts `{workspace_id,destination_root?,expected_account_id,expected_endpoint}` and
returns `{workspace_id,root,name}`. The native shell supplies the chosen final
folder; webview arguments contain only a workspace ID. A fresh folder must
already exist, be writable and empty, and lie outside another project/repository.
The selection is checked before cloud hand-back and immediately before install.
Recorded directory identity prevents missing/replaced folders from being silently
recreated. The local configure request accepts additive `account_id`; personal
devices supply it and worker callers may omit it. Each new adoption binds its
folder to the account endpoint and account ID, so signing into another account
cannot reuse a colliding project's local path. Discovery/configuration snapshots
pair runtime and generation under the configuration lock. Unstarted failed choices can be replaced explicitly; started imports
remain pinned to their saved folder. Old `import_roots` entries migrate only to
pending-ID fences, never to permission to import. A partially registered legacy
project needs explicit selection of its original folder before recovery.

Normal lazy return only handles registered projects without a pending adoption.
Existing laptop projects retain their original roots. Worker hand-back and explicit
adoption compare the raw account holder against the typed keeper worker identity
(`worker-{holder}`), while requests retain the complete route ID. A real-Git
regression returns a completed synthetic conversation through that route, retaining
its session/native identity and history without starting a new model turn. Hydration checks account
generation at ownership, filesystem and session-install boundaries; signing out
cannot finish an old transfer as a fresh local ownership grant. HTTP transfer is
bounded to nineteen minutes. Cancellation can leave a persisted Hydrating fence
and partial files; an explicit retry resumes at the saved destination. No worker
project is adopted merely because this daemon starts or becomes suitable for work.

Required worker setup runs before any imported agent resumes. Persisted
`SettingUp` ownership fences ordinary writers and ledger restore while its
explicit daemon setup task alone can spawn/execute the setup terminal. Failure
keeps that fence and exposes an attention error; a hydrate retry runs the updated
setup against already installed files. Laptop-only deferred steps stay in the
profile as instructions for the returning agent under its usual permissions;
the daemon never replays those commands automatically.

Cloud resume additionally checks the providers named by actual deferred ledger
agents after setup, using fresh bounded worker-local readiness probes. Every
required provider must be installed and signed in; another provider's login,
unknown provider ids, timeouts and missing evidence cannot satisfy the gate.
The workspace remains `SettingUp` until all checks succeed. The local status
row exposes additive `blocked_providers: [{id,state,reason}]` and its mirror
error is `cloud_provider_not_ready`; the same bounded rows feed cloud-provider
onboarding and session-scoped MCP guidance without probes. Nonsecret blocked
rows survive daemon restart only beside a persisted `SettingUp` fence; cached
readiness never grants permission to resume. An explicit hydrate retry against the recorded epoch reuses staged
files and repeats setup/readiness without fetching another snapshot. It never
starts authentication or transfers provider credentials. Account replacement,
ownership changes and cancellation retain the fence. Personal-device and
ordinary SSH/free workspace behavior is unchanged.

Structured pause checks accept authoritative completed-turn/idle agent state even when a provider emits no textual idle status, but reject queued input, active turns, and background work (explicit permission/action waits remain safe pause points).

The distinct authenticated `POST /api/v1/pro/configure/workspace` accepts only a
worker delegation bound to one workspace/revision, explicit account identity,
no keeper URL and exactly baton/mirror scopes. Its versioned nonsecret ack and
status field must match the supervisor's registered root before enabling work.
Legacy configure never accepts such a binding. The accepted origin/account/root
and directory identity survive disconnect/restart; invalid records fail closed.
Pro operations reject foreign workspace IDs before filesystem/network work and
hydrate only into the accepted root. Renew cannot drop/change the binding or
holder or add keeper scope. This does not replace service-side authorization,
project namespaces, or project-bound routing of generic daemon/MCP APIs. Those
remain prerequisites to enabling selected-project secrets.
