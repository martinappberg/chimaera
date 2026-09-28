# Cloud provider connections

Worker-only readiness and explicitly requested provider authentication. Parent:
[server map](../../../AGENTS.md). Shared IDs, labels and browser origins:
[core catalog](../../../../chimaera-core/src/cloud-providers.json).

| File | Responsibility |
|---|---|
| `mod.rs` | Catalog adapters, allowlisted status parsing, bounded single-flight cache, authenticated HTTP handlers and the handoff `readiness` helper. |
| `process.rs` | Capped CLI output and Codex auth-only JSON-RPC; owned process-group cleanup. No raw output enters HTTP errors or logs. |
| `claude.rs` | Official Claude CLI headless browser/code adapter; bounded URL extraction and one-time stdin reply, without a workspace or PTY. |
| `connect.rs` | Short-lived connection/disconnection jobs, one writer per provider, curated runtime installation, exact login terminals, Codex device codes, cancellation and cleanup acknowledgement. |
| `disconnect.rs` | Official CLI logout adapters and fresh negative verification; personal-cloud scope, no credential-file reads or claims of vendor-wide revocation. |
| `tests.rs`, `connect_tests.rs` | Status isolation, cache/freshness, real child/PTY cleanup, cancellation/retry races and device completion verification. |

## Contract

All routes are behind the daemon bearer middleware, under `/api/v1/pro/cloud`:

- `GET /providers` returns `{available,providers,handoffs}`. Provider rows contain
  `id,label,category,installed,state,reason,checked_at,methods,disconnect_supported`. Older servers omit the additive capability, so clients hide disconnect unless it is true. State is `missing`,
  `needs_sign_in`, `signed_in`, `unknown` or `unavailable`; `installed` is nullable.
  `handoffs` comes from `pro::cloud_provider_blocks`, never inferred from UI state.
  An optional `connection` recovers the earliest unfinished disconnect after a
  lost mutation response or reopened UI; reading it starts no operation.
- `POST /providers/{id}/connect`, `GET /connections/{id}`, and
  `POST /connections/{id}/cancel`, and `POST /connections/{id}/input` return `{available,connection}`. An attempt has
  `id,provider_id,operation,phase,expires_at,action,error_code`. Additive `operation` is `connect` or `disconnect`; an absent operation means `connect` for older servers. Phase is `preparing`,
  `waiting`, `verifying`, `connected`, `disconnected`, `failed`, `canceled` or `expired`.
- `POST /providers/{id}/disconnect {acknowledge_cloud_work:true}` starts or joins
  a bounded 60-second disconnect job. The UI first names the provider and warns
  that cloud tasks may lose access. A concurrent login returns `409 provider_busy`;
  login likewise cannot race a disconnect. This operation is personal-cloud-wide,
  never project-only, and does not stop existing agent tasks. Once accepted it
  completes independently of UI visibility; cancel does not undo logout. Success
  requires a fresh official signed-out result, not a timeout or invalid-token row.
- `POST /connections/{id}/input {code}` accepts a single-use, nonempty authorization
  code (maximum 4096 bytes, no whitespace/control characters) only while that
  exact attempt is waiting for `authorization_code`. Code bodies are never logged,
  persisted, returned, or added to a command line. Native submission remains bound
  to its account generation through the request.
- An action is `{type:"browser",url,input:"authorization_code"}`,
  `{type:"device_code",verification_url,user_code}` or
  `{type:"terminal",workspace_id,session_id}`. Openers validate the shared
  provider origin policy and resolve the action afresh; client-supplied URLs or
  commands are never executed. Timestamps are Unix seconds.
- Nonworkers return `available:false` with empty lists and `connection:null`.
  Unsupported provider IDs and expired connection IDs return static error codes.

## Invariants

- Passive status never installs, logs in, starts a model turn, or wakes compute.
  Readiness is auth configuration reported by the official CLI, not a promise
  about its billing, quota or model entitlement. Never read credential files.
- Codex uses only auth app-server requests: initialize, account/read with
  `refreshToken:false`, and explicit account/login/start with
  `type:"chatgptDeviceCode"`. Claude uses `auth status --json` and explicit
  `auth login --claudeai` with piped I/O, its own browser URL and code prompt; GitHub is optional and uses `gh auth status` JSON plus
  its official login/setup-git terminal. Unknown protocols fail closed.
- `readiness` is shared by onboarding and handoff. Only `signed_in` with a
  confirmed installed runtime may satisfy a required session provider. An
  unsupported future provider remains blocked until it has an adapter.
- One status probe at a time, 30-second cache, 12-second individual and 25-second
  whole-batch budgets including queue time. Concurrent fresh requests join the
  same completed probe; a later fresh request bypasses cached auth state.
  Each subprocess stream/JSON-RPC frame is capped at 64 KiB; request RPCs have an
  eight-second deadline and bounded notification skips. Cache keys are catalog IDs.
- Login lifetime is at most 15 minutes; disconnect at most 60 seconds, plus bounded cleanup. The daemon
  health active-operation count includes pending auth/install jobs. Jobs continue
  across UI disconnect but cancel on daemon stop; they are not restart-restored.
- A provider has at most one unfinished credential writer. Cancel clears the
  action immediately but retains that reservation until owned process-group/PTY
  exit is observed. The cancel endpoint waits up to four seconds; uncertain
  cleanup fails closed with `cleanup_failed`. Retained finished attempts are
  capped at 24 and expire on the next explicit connection request.
- Cancellation stops the CLI process and device polling. It does not revoke a
  vendor code already issued (that code expires at the provider), or sign out an
  account whose login completed concurrently. No user credentials are copied from
  another host. Raw CLI errors, account email and token fields never leave a probe.
- Disconnect uses Claude `auth logout`, Codex `account/logout`, and GitHub
  `auth logout --hostname github.com --user <CLI-reported account>`. GitHub removes
  at most eight stored accounts for that host, preserving enterprise hosts and
  refusing environment-owned tokens. Names stay internal; no token values are read.
  GitHub logout removes local stored auth, not vendor authorization. Neither a
  successful logout nor Chimaera sign-out promises to erase tokens already cached
  by running agents. Cloud-wide provider connections are reused until disconnected
  or their official auth state requires sign-in again.
- Auth changes invalidate cached readiness and fence in-flight results by an
  auth generation. While disconnect is unfinished, readiness remains unknown;
  a stale signed-in cache cannot authorize a handoff during removal.
- Claude sign-in creates no workspace or PTY. It validates the current official
  `https://claude.com/cai/oauth/authorize` URL and prompt before exposing the
  browser action. Unrecognized CLI output fails closed. A code is passed once
  to the owned CLI; success still requires a fresh status probe.
- Install/login terminals live only in `~/projects/.chimaera-setup`, the existing
  excluded setup workspace. Its purpose is recorded as `cloud_internal`, and normal
  workspace lists omit it; old worker-reserved paths migrate to that marker. Cancel kills only the job's own session. Install uses
  the runtime subsystem's curated official downloads and existing reservation;
  it never attaches to or cancels someone else's install.

## Adding a provider

Cloud connections currently support Claude Code and Codex, with GitHub as an
optional repository connection. Grok is an example of a future provider, not a
shipped adapter. Adding a JSON entry alone neither enables authentication nor
makes an agent's sessions portable. Complete this checklist before advertising
support:

1. Add the stable ID, label, `agent` or `repository` category, and exact verified
   HTTPS authentication origins to the [shared catalog](../../../../chimaera-core/src/cloud-providers.json).
   Keep terminal-only providers' origins empty. Add the server adapter to
   `PROVIDERS` in `mod.rs`; the HTTP catalog lists implemented adapters, not every
   JSON entry. Keep catalog/adapter consistency tests passing.
2. For an agent, extend [AgentKind](../../agent_state.rs),
   [launcher detection](../../launcher.rs) and the
   [curated runtime installer](../../runtimes.rs) as needed. Verify the official
   binary/download and supported platforms. Install only after an explicit
   connection request; never interpret a provider ID as an executable or reuse
   another provider's install/login fallback. Repository adapters must define
   their own binary discovery and honest missing-runtime behavior.
3. Implement and live-verify the official auth-status protocol in `mod.rs` and
   explicit login adapter in `connect.rs`/`process.rs`. Distinguish missing,
   signed out, signed in, and unverifiable status using allowlisted fields;
   never inspect credential files or expose raw output. Preserve probe deadlines,
   output caps, one writer per provider, expiry, activity accounting, cancellation
   and observed child cleanup. Login completion must trigger a fresh auth probe.
4. Validate every published browser/device URL against that provider's catalog
   origins in the daemon, and preserve the matching
   [native opener](../../../../chimaera-app/src/shell/cloud.rs) and
   [browser policy](../../../../../web-ui/src/lib/pro/providers.ts) checks.
   Keep client-supplied URLs/commands out of opener requests. A new action shape
   needs coordinated daemon, native and UI support; existing terminal/device-code
   actions can reuse their current presentation.
5. Treat handoff support as a separate gate. Implement the provider's native
   conversation identity, history discovery, archive validation/export/import,
   resume and native fork semantics in [bundle.rs](../../bundle.rs),
   [ledger.rs](../../ledger.rs), [spawn.rs](../../spawn.rs) and the corresponding
   agent integration. Preserve conversation bytes and public IDs, and verify
   move, return, fork, restart and destination-root behavior against the real CLI.
   `AgentKind::as_str()` must match the adapter ID because
   [provider_gate.rs](../../pro/provider_gate.rs) derives required providers from
   deferred sessions. Authentication alone must never grant unsupported resume.
6. Keep [ProviderConnections](../../../../../web-ui/src/lib/pro/ProviderConnections.svelte)
   driven by returned catalog rows, labels, categories and methods rather than a
   new hard-coded provider card. General onboarding needs one connected agent;
   handoff requires every provider used by that project's deferred agents.
   Unsupported required IDs must remain visible and blocked, never silently
   substituted with a connected provider.
7. Cover unknown IDs, malformed status, missing runtime, denied/expired login,
   cancellation/retry cleanup and fresh handoff checks with focused tests. Record
   real CLI versions and bounded auth-flow observations in the
   [protocol log](../../../../chimaera-agent/PROTOCOL.md); run the applicable live
   agent verification before claiming session compatibility. Keep unknown
   protocols and incomplete adapters fail-closed throughout rollout.
