# web-ui/src/lib/pro — Pro navigation and provider connections

Parent map: [settings](../settings/AGENTS.md). These components share the native
account bridge and host-pinned browser daemon routes. Provider credentials never
enter UI settings or local storage.

| File | Responsibility |
| --- | --- |
| `ProView.svelte` | Desktop account surface and browser worker detection; shared project/provider context. |
| `ProNavigation.svelte` | Quiet workbench Pro entry. |
| `ProWalkthrough.svelte` | Static, decorative continuity sketches for confirmed free accounts only; repeated project/thread motif, theme tokens, responsive captions. |
| `CloudProjects.svelte` | Passive cloud-project discovery and explicit per-project local opening. |
| `ProviderConnections.svelte` | First-agent onboarding, guided connection lifecycle, optional repository providers and exact pending-handoff continuation. |
| `cloudTransport.ts` | Native/browser request parity; passive GETs, explicit wake intent and focused terminal routing. |
| `providers.ts` | Readiness and safe provider-link presentation; imports the core provider catalog. |
| `onboarding.svelte.ts` | Validated shared intent so a paused project opens the same onboarding flow. |
| `AccountUsage.svelte` / `usage.ts` | Percentage-first account usage; real limits, bounded accessible bars, neutral unknown/zero allowance. |
| `billing.ts` | Native billing copy, stale-attempt fencing and explicit upgrade-review eligibility; browser return never grants entitlement and raw errors never render. |
| `presentation.ts` | Account, billing-intent and cloud-state copy; truthful project status and the finite preparation polling cadence. |

## Boundaries

- Cloud status distinguishes availability, verified agent connection and
  recorded project copies without numbered tasks. Infrastructure `phase` stays off
  the user surface; ready and idle compute share the same calm availability state.
  Progress describes the requested task, such as loading agent connections or
  preparing a named provider's sign-in, never machine startup or shutdown.
  A reachable daemon does not imply an agent is signed in or files are copied.
  Project transfer labels come only from recorded ownership; saved-copy counts
  require `last_mirrored_at`, and never-mirror projects are excluded. There is no
  initial-sync or percentage estimate.
- Preparation checks are sequential and visible-only: 5 seconds for the first
  five minutes, then 30 seconds. Other states use 30 seconds. A provider readiness
  component keeps its catalog poll and sign-in state when management is collapsed;
  first/required connections and active sign-in remain inline.
- Provider rows come from the daemon catalog. Adding a future provider requires
  its backend adapter and trusted auth origins, not another bespoke UI card.
- `signed_in` is CLI-confirmed configured authentication. Installed, unknown and
  unavailable states never satisfy readiness; one agent suffices initially, but
  a handoff needs every provider it names.
- Connection completion comes from the daemon. A browser opening, terminal exit,
  copied code or local marker cannot confirm it. Codes are transient and never
  stored. Native browser/terminal actions send only a connection ID.
- Claude browser authorization stays in the connection panel. The one-time reply
  is cleared from the input on submit, cancellation, hide or teardown and sent
  only to its current attempt; it is never saved in browser storage. Installation
  remains automatic and does not navigate to its internal workspace.
- Provider and connection polls are single-flight and visibility-gated. The
  connection effect depends on primitive ID/deadline/active-state values so
  replacing a response object cannot restart it into an immediate request loop.
  Polls use a finite deadline; only explicit Connect scrolls/focuses the guide.
- Mutations invalidate older connection responses. An old waiting response cannot
  overwrite a confirmed cancellation. Queued catalog refresh confirms completion
  even when an earlier passive read was in flight.
- An already staged provider-blocked handoff continues automatically once a fresh
  catalog confirms every named provider. Attempts are sequential and once per
  workspace/epoch; visibility or connection mutations invalidate that catalog.
  A failed attempt offers explicit retry. Completion returns to the originating
  project only if that context is still current. The daemon rechecks ownership,
  authentication and setup; no connected agent authorizes a new move.
- All external auth links require an exact HTTPS origin from
  `crates/chimaera-core/src/cloud-providers.json`, without credentials or a fragment.
  No status or discovery operation carries wake intent.

Pure readiness/transport tests cover these boundaries; real rendered components
still need light/dark, narrow, keyboard and lifecycle verification per verify-app.

Native account status includes optional `initializing` / `initialization_phase`.
Show the keychain/account recovery explanation immediately while startup waits;
keep sign-in, billing and sign-out mutations behind that startup fence. Older
native builds omit these fields and retain their existing account behavior.

Billing verification belongs to the native shell, including while Pro is hidden or
closed. The page reads optional `ProStatus.billing` and listens for `pro-changed`;
it never polls checkout or trusts a browser query. Storage retains only a selected
plan selection across sign-in; it never authorizes automatic checkout. Old
checkout markers are ignored. `pro-return` is targeted
to one native window and `pro_take_return` consumes that window's pending marker
after listener registration. App defers opening Pro until its layout is ready.
