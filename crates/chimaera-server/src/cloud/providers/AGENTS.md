# Cloud provider connections

Worker-only readiness and explicitly requested provider authentication. Parent:
[server map](../../../AGENTS.md). Shared IDs, labels and browser origins:
[core catalog](../../../../chimaera-core/src/cloud-providers.json).

| File | Responsibility |
|---|---|
| `mod.rs` | Catalog adapters, allowlisted status parsing, bounded single-flight cache, authenticated HTTP handlers and the handoff `readiness` helper. |
| `process.rs` | Capped CLI output and Codex auth-only JSON-RPC; owned process-group cleanup. No raw output enters HTTP errors or logs. |
| `connect.rs` | Short-lived connection jobs, curated runtime installation, exact login terminals, Codex device codes, cancellation and cleanup acknowledgement. |
| `tests.rs`, `connect_tests.rs` | Status isolation, cache/freshness, real child/PTY cleanup, cancellation/retry races and device completion verification. |

## Contract

All routes are behind the daemon bearer middleware, under `/api/v1/pro/cloud`:

- `GET /providers` returns `{available,providers,handoffs}`. Provider rows contain
  `id,label,category,installed,state,reason,checked_at,methods`. State is `missing`,
  `needs_sign_in`, `signed_in`, `unknown` or `unavailable`; `installed` is nullable.
  `handoffs` comes from `pro::cloud_provider_blocks`, never inferred from UI state.
- `POST /providers/{id}/connect`, `GET /connections/{id}`, and
  `POST /connections/{id}/cancel` return `{available,connection}`. An attempt has
  `id,provider_id,phase,expires_at,action,error_code`. Phase is `preparing`,
  `waiting`, `verifying`, `connected`, `failed`, `canceled` or `expired`.
- An action is `{type:"device_code",verification_url,user_code}` or
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
  `auth login --claudeai`; GitHub is optional and uses `gh auth status` JSON plus
  its official login/setup-git terminal. Unknown protocols fail closed.
- `readiness` is shared by onboarding and handoff. Only `signed_in` with a
  confirmed installed runtime may satisfy a required session provider. An
  unsupported future provider remains blocked until it has an adapter.
- One status probe at a time, 30-second cache, 12-second individual and 25-second
  whole-batch budgets including queue time. Concurrent fresh requests join the
  same completed probe; a later fresh request bypasses cached auth state.
  Each subprocess stream/JSON-RPC frame is capped at 64 KiB; request RPCs have an
  eight-second deadline and bounded notification skips. Cache keys are catalog IDs.
- Connection lifetime is at most 15 minutes plus bounded cleanup. The daemon
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
- Install/login terminals live only in `~/projects/.chimaera-setup`, the existing
  excluded setup workspace. Cancel kills only the job's own session. Install uses
  the runtime subsystem's curated official downloads and existing reservation;
  it never attaches to or cancels someone else's install.
