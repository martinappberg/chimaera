# First-party application presentation boundary

This optional scaffold is not wired into App/Pane/Settings. It loads no URL, creates no account client/listener, and changes neither default product behavior nor operation authority. The trusted assembly provides one exact runtime service object per window and an already-selected module; installation/signature/entitlement checks are separate gates.

| File | Responsibility |
| --- | --- |
| `application.ts` | Finite account/account-settings/kept-review types; captured original view/intent and relative file actions; shared host modal handles; exact attempt DOM cleanup and bounded unresolved mounts. |
| `ApplicationSurface.svelte` | Host target/loading/error/Retry/Cancel wrapper; visibility updates keep the owner, original identity changes retire it. |
| `application.test.ts` | Actual session lifecycle tests with controlled DOM targets/module promises; not browser or current-Pro acceptance. |

Mutable account, kept-review, onboarding, route/token/socket and modal services must never be copied into a separately bundled UI. Plain runtime subscription callbacks retain the exact original instance; independent presentation runtimes may copy only types/pure formatting. The original scoped adapters authenticate mutations and retain detached HTTP/process cleanup; UI disposal/AbortSignal cannot sign out or cancel retained work.

Each pending mount owns an isolated child target. Retire/detach synchronously before a successor; dispose late owners against that child, even when disposal throws. Pending mounts retain their per-window reservation until settled (maximum eight); timeout closes presentation after ten seconds, but cannot enable unbounded Retry. Original pane callbacks and full onboarding provider/workspace identity remain captured. File actions use bounded POSIX-relative paths; backslashes remain legitimate filename characters.

The caller supplies the existing modalFocus action at ordinary priority; node must belong to the current attempt target. Host retirement destroys all tracked handles before disposing presentation. No second modal stack, raw invoke/fetch/token API or plugin-WIT change. Eleven controlled lifecycle tests, public Svelte check/build, private standalone Svelte check/build and actual paired browser verification pass. The paired fixture covers shared subscriptions, light/dark rendering, original modal priority/focus, hidden/inert keyboard behavior, bounded Retry/Cancel/late mounts and zero final subscriptions. It uses synthetic runtime values, not authenticated account operations. Current Pro rendering, automatic installation and entitlement remain separate gates.

Presentation and identity projections copy only declared fields; TypeScript structural extras never pass to the private entry. Explicit Cancel closes only the still-current captured host surface and always cleans up presentation; retirement cleanup alone never closes a successor. The emitted private bundle guard also rejects copied host implementation modules and external runtime imports.
