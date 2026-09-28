# web-ui/src/lib/pro — Pro navigation and provider connections

Parent map: [settings](../settings/AGENTS.md). These components share the native
account bridge and host-pinned browser daemon routes. Provider credentials never
enter UI settings or local storage.

| File | Responsibility |
| --- | --- |
| `ProView.svelte` | Desktop account surface and browser worker detection; shared project/provider context. |
| `ProNavigation.svelte` | Quiet workbench Pro entry. |
| `CloudProjects.svelte` | Passive cloud-project discovery and explicit per-project local opening. |
| `ProviderConnections.svelte` | First-agent onboarding, guided connection lifecycle, optional repository providers and exact pending-handoff continuation. |
| `cloudTransport.ts` | Native/browser request parity; passive GETs, explicit wake intent and focused terminal routing. |
| `providers.ts` | Readiness and safe provider-link presentation; imports the core provider catalog. |
| `onboarding.svelte.ts` | Validated shared intent so a paused project opens the same onboarding flow. |
| `presentation.ts` | Account, billing-intent and cloud-state copy helpers. |

## Boundaries

- Provider rows come from the daemon catalog. Adding a future provider requires
  its backend adapter and trusted auth origins, not another bespoke UI card.
- `signed_in` is CLI-confirmed configured authentication. Installed, unknown and
  unavailable states never satisfy readiness; one agent suffices initially, but
  a handoff needs every provider it names.
- Connection completion comes from the daemon. A browser opening, terminal exit,
  copied code or local marker cannot confirm it. Codes are transient and never
  stored. Native browser/terminal actions send only a connection ID.
- Provider and connection polls are single-flight and visibility-gated. The
  connection effect depends on primitive ID/deadline/active-state values so
  replacing a response object cannot restart it into an immediate request loop.
  Polls use a finite deadline; only explicit Connect scrolls/focuses the guide.
- Mutations invalidate older connection responses. An old waiting response cannot
  overwrite a confirmed cancellation. Queued catalog refresh confirms completion
  even when an earlier passive read was in flight.
- Continuing a handoff sends the current daemon-reported workspace and epoch.
  The daemon retains authority to check ownership, authentication and setup.
- All external auth links require an exact HTTPS origin from
  `crates/chimaera-core/src/cloud-providers.json`, without credentials or a fragment.
  No status or discovery operation carries wake intent.

Pure readiness/transport tests cover these boundaries; real rendered components
still need light/dark, narrow, keyboard and lifecycle verification per verify-app.
