# web-ui/src/lib/pro — Pro navigation and provider connections

Cloud allowance presentation uses the additive `attended_actions` capability
only with `limited/hours_exhausted`. Absence keeps older service behavior;
other restrictions never inherit the exception. Passive metadata probes still
never wake compute, and repeated failures show an outage for attended access too.

Parent map: [settings](../settings/AGENTS.md). These components share the native
account bridge and host-pinned browser daemon routes. Provider credentials never
enter UI settings or local storage (a browser view keeps only catalog rows there).

| File | Responsibility |
| --- | --- |
| `ProNavigation.svelte` | Quiet workbench Pro entry. |
| `ProjectCopyStatus.svelte` / `projectCopy.ts` | Existing workspace placement strip for local native copy roles: passive visible-only status reads, exact ready role + authenticated `owner_epoch` (including a released holder; verified Remote epoch fallback only for older markers) for explicit **Take over**, fixed errors and bounded Git staging recovery presentation. Missing/future evidence stays neutral; uncaptured staging never means empty or synced. Home copied rows refresh via native Open, including Open in new window. |
| Public `projectSecrets.ts` wire types / private `account/control/projectSecrets.ts` validation | Capability-gated account Home controls using existing friendly project names: one-name value updates carry the displayed queued batch and wait until idle; Apply now and Remove access require exact stop warnings. Values never appear in status or storage and clear on send, hide and account events. Closed policy/context/receipt validation never interprets an unknown state as ready. |
| Private `account/control/projectSecretsTransport.ts` / `projectSecretsMemory.ts` | Additive native personal IPC or fixed same-origin account Home browser routes only; no daemon/project or older-shell fallback. One POST, fixed redacted errors, bounded responses and original-operation passive reconciliation. At most 24 value-free unresolved records remain in window memory across Settings closure; new authenticated contexts clear them. Catalog/receipt polling runs only while the disclosure and document are visible. |
| Private `account/control/accountHome.ts` | The web Home's same-origin, cookie-authenticated reads (`GET /home/projects`, `GET /home/account`, `POST /home/sign-out`; `PROTOCOL.md` "Account home in a browser"): `readHomeProjects` keeps well-formed rows sorted by name, `projectHref` follows only `/workspace/{id}/` and `/app/{host}/#ws={id}`, `readBrowserAccount` shapes the account as signed-in status and retains an exact optional `account_lifetime`; new private surfaces require it and guarded sign-out sends it once, never downgrades after refusal. Legacy absence remains compatible only for original callers. Guarded reads suppress late 401 navigation after their owner retires; ordinary reads return to `/` (the sign-in page). `BILLING_PATH` is the account's billing page, the only billing link. |
| Private `account/control/personalProviders.ts` validation / `personalProviderTransport.ts` and tests | Closed redacted personal-control DTO validation and the existing named connection panel's positively selected account transport. Fresh refresh-stable context selects Legacy or Personal; missing negotiation never downgrades Personal or routes project intent through a daemon. Original parent IDs survive ambiguous Connect/Submit/Cancel, children and submission nonces are generated once, and status only polls the parent. Codes never enter panel transport state or storage. Browser/native openers reread the exact parent and validate fixed provider origins. Ordinary free daemon entry remains separate; durable production mode migration and runtime distribution are still disabled gates. |
| Private `account/control/cloudTransport.ts` + public `extensions/accountDaemon.ts` finite daemon bridge | Native/browser request parity; passive GETs and explicit wake intent; no terminal operation (an unknown operation is refused before any request); `isCloudMachine` and `peekCatalog` (passive; the connections section's only catalog read). A cloud asleep or still starting (503 `worker_asleep`/`worker_unavailable`, or a reply marked `X-Chimaera-Worker-State: sleeping`) rejects with the fixed `cloud_asleep`, as the native shell does. `cloudAction` sends a pressed action again while it answers `cloud_asleep`, starting no attempt later than `WAKE_BOUND_MS` after the press. |
| Public `providers.ts` leaf / private `account/providers.ts` policy | Readiness and safe provider-link presentation; imports the core provider catalog (`providerLabel` names an unlisted required provider; `pausedConnect` reads a paused row's additive `blocked_provider` for the pane's and chat's **Connect <agent> to continue**). `rememberedRows` validates remembered rows into catalog rows; `panelRows` decides which rows show and whether they read as settled; `catalogRows` names the shared catalog's providers without a state; `agentsConnected`; `connectingLabel` (a pressed Connect's words); an older cloud's sign-in: `olderCloudSignIn` (a `waiting` connection whose action is a terminal; `preparing` with one is the cloud setting up an agent, never a sign-in), `awaitingCloudUpdate` / `signInGuided` (a catalog row's `methods`), `stillAwaitingUpdate` and `cloudUpdateLine`. |
| Private `account/control/catalogMemory.ts` | A browser view's remembered catalog rows (the fields rendering needs, never credentials), per cloud address (`gatewayPrefix`: `/app/{host}` or `/workspace/{id}`, each one account's), so the connections section shows the last known rows at once as the app does natively; every storage access guarded; `forgetCatalogs` on sign-out; a no-op natively. |
| `vocabulary.test.ts` | Scans the Pro and cloud settings copy: the cloud is never a machine (only the user's own SSH hosts are), and nothing narrates it sleeping or waking. |
| `onboarding.svelte.ts` | Validated shared intent so a paused project opens the same onboarding flow; `canOpenOnboarding` (native app or account browser view) gates the paused-session connect action, since a plain browser tab has no Pro page. |
| Private `account/devices.ts` | Verified installation grouping, separate older sign-ins and named per-sign-in removal confirmation. Names never identify a computer. |
| Private `account/usage.ts` | Percentage-first account usage; real limits, bounded accessible bars, neutral unknown/zero allowance. |
| Private `account/billing.ts` | Native billing copy, stale-attempt fencing and explicit upgrade-review eligibility; browser return never grants entitlement and raw errors never render. `planPrice` formats only service-supplied prices; `planPrices` is all four or none; `planMultiples` reads Max's whole-number multiples of Pro's cloud time and storage from its catalog entries (each a safe integer of at least 2, else null); `maxCapacityNote` is the Max card's capacity line built from them ("5× the cloud time and storage of Pro", each number named when the two differ, only the stated one otherwise, null when the service states none so the card keeps its generic line). The app never learns an absolute allowance: multiples, like prices, are never literals. |
| Private `account/presentation.ts` / public two-constant `presentation.ts` leaf | Account, billing-intent and cloud-state copy; truthful project status (`copyIssue`: `pending`, `checkpoint_pending` and `ownership_unverified` are quiet progress, never attention; a `setting_up` project is progress unless it waits on an agent), `projectPlace` (who runs a project, in plain words), `signInNoteCopy` (the quiet line for an ended browser sign-in), `keeperRestartLine` / `restartWhen` (the quiet line for a planned restart of the cloud connection: "tonight at 02:00" in the locale's short time, "shortly" once due; names the cluster only when exactly one login is kept), `cloudCopy` (only a first setup, `readyOnce` false, reads as setup; ready, idle and a later `preparing` read alike; its remembered agent fact: true claims connected agents, false names the step, unknown claims nothing) and `cloudReadyOnce`, `projectCopiesSetupLine` (`renewal_failed` reads "Reconnecting your account…"), `connectionWarningCopy` (one quiet line per connection state), `cloudAsleep` (the fixed `cloud_asleep` code, never words on a passive path), `WAKE_BOUND_MS` / `CHECKING_AFTER_MS`, `afterFailedRead` / `MISSES_REPORTED` (one failed read in a row keeps the last confirmed state; the second is reported) and the finite preparation polling cadence. |
| Private `account/account.ts` | The Pro page's panel from the last confirmed status (`accountPanel`: background reads never change it; only a check the user asked for shows checking), the error bar and whether it offers a check (`accountErrorBar`, `offersCheck`), the billing-review key, and when Max is offered (`nearLimit`). |
| `kept.ts` / `keptReviews.svelte.ts` | Both versions a return kept: which names are kept copies (`isKeptCopy`, mirroring `canonical::kept_copy_name`; the file tree's "from this Mac" badge and its **Review both versions** menu item), the words (`hereName`: "this Mac", "this computer" off a Mac or on a remote host, "your computer" in a browser view; `backNote`), the four daemon routes, and the shared per-project answer the chat line and the review read (`keptReviews`: on demand, on a `kept_both` notice, and from each choice's answer; never polled). `requestKeptReview` asks App to open the review, switching the window to that project first. |
| `../extensions/KeptApplicationView.svelte` | Captured host for the optional private `packages/pro-client-ui` list/diff presentation; without it, Close/Open project folder keeps ordinary file/Git recovery available. Public `kept.ts` / `keptReviews` and daemon routes remain shared. The review tab (`layout.ts` `KeptTab`, "Both versions"): the files on the left, the selected pair side by side (read-only `@codemirror/merge`, both sides one neutral change tint), **Use this Mac's** / **Use incoming** / **Keep both** per file and **… for all** (confirmed) in the header; "Use incoming" says this Mac's copy moves to the Trash, or is deleted when the listing's `trash` is false (its drive has none), and a choice whose `discarded.deleted` broke that promise says so (private `kept/copy.ts` `useCloudHint` / `useCloudForAllBody` / `deletedNote`); binary or larger than 512 KiB shows both sizes; a file the incoming copy deleted says so; "Incoming branches" lists the kept branches without actions; An additive `pair.can_use_mine: false` disables local replacement (including for-all confirmation) when the original basename cannot be recovered safely; absence keeps older-daemon behavior. Use incoming and Keep both remain available. Origin-neutral incoming wording also covers returns from another computer; wire `cloud` / `use_cloud` keys stay unchanged. Kept copies past the 32 named are mentioned, never scanned for. Opened from the chat line, Settings' project row, the tree, or the `kept_both` notice (`App.svelte` `focusFromNotification` reads the notice's `kept-both-<workspace>` key; the native shell already routes that click to the project's window). |
| Private `account/status.ts` | Reading `ProStatus`: a real `accountFailure` vs an informational `connectionWarningCode` (older shells' two `error` messages map to `connection_preparing`/`connection_retrying`), `signInNote` (an ended browser sign-in: `sign_in_timed_out`, `sign_in_incomplete`, `browser_unavailable`, never an account failure), `paymentDue`, `keeperRestartAt` (a planned restart's time, null unless signed in and a time), and `rechecksItself` (a failure the app rechecks on its own). |

## Boundaries

- Cloud status distinguishes availability, verified agent connection and
  recorded project copies without numbered tasks. Infrastructure `phase` stays off
  the user surface; ready and idle compute share the same calm availability state.
  Progress describes the requested task, such as loading agent connections or
  preparing a named provider's sign-in, never machine startup or shutdown. Only
  an account's very first setup reads as setup; the cloud is never called a
  machine, and the last known connections show at once while it is idle.
  A reachable daemon does not imply an agent is signed in or files are copied.
  Project transfer labels come only from recorded ownership; saved-copy counts
  require `last_mirrored_at` or a well-formed recorded `checkpoint_id`, and
  never-mirror projects are excluded. There is no
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
  stored. The native browser action sends only a connection ID. No client
  opens a terminal action: the daemon's `{type:"terminal"}` connection action
  (its agent install while `preparing`, and an older cloud's GitHub sign-in
  while `waiting`) is never read beyond its type, so it is removable from the
  daemon wire once no older cloud remains.
- Claude browser authorization stays in the connection panel. The one-time reply
  is cleared from the input on submit, cancellation, hide or teardown and sent
  only to its current attempt; it is never saved in browser storage. Half of a
  `code#state` reply is refused with `authorization_code_incomplete` (the
  attempt keeps waiting); the browser transport carries only that code back,
  and the panel asks for the whole code. Installation
  remains automatic and does not navigate to its internal workspace.
- Provider and connection polls are single-flight and visibility-gated; the
  catalog is polled only while the cloud answers (`live`). The
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

- Prices are never built into the UI; they come only from optional
  `ProStatus.plans` (the signed-in account's list, else the native shell's read
  of the service's public catalog, so a signed-out page shows them too), and
  only when all four (Pro/Max, monthly/yearly) are valid. Otherwise no amount shows anywhere: the cards name the plans and the
  heading and purchase line say prices are shown at checkout.
- Plan capacity (`cloud_time_multiple`, `storage_multiple` on the same entries,
  additive) is likewise the service's numbers only, and only ever a whole-number
  multiple relative to Pro: the app shows and builds in no absolute allowance
  (a plan's real hours and storage are private to the service). The Max card's last
  line is `maxCapacityNote(...) ?? <generic line>`, independent of the billing
  interval (both intervals of a plan carry the same pair); Pro's card and an
  older service keep the generic line. The Max card compares usage with Pro's,
  never price with price.
- Nothing asks the user to refresh or retry what the page can do itself:
  polls re-check, a pending privacy change is re-sent (≤1/min while visible),
  a refused checkout for an existing plan re-reads the account. A manual check
  appears only after polling stopped or a real failure, never for
  `service_unsupported` (the app rechecks it every ten minutes).

Pure readiness/transport/account tests cover these boundaries and pin relations
(which states read alike or apart, precedence, no raw errors), never wording;
the exceptions are `vocabulary.test.ts`, which pins what the copy never says,
and the private account presentation policy suite, which pins its extracted panel source: no terminal anywhere,
and the older cloud's row line with its passive Try again.
Real rendered components still need light/dark, narrow, keyboard and lifecycle
verification per verify-app.

Native account status includes optional `initializing` / `initialization_phase`,
`connection_warning` (informational: `connection_preparing`,
`connection_retrying`, `account_unreachable`, one quiet line each with no button;
never a failure or an entitlement signal — older shells sent two such messages
in `error`, which `status.ts` recognizes),
`payment_due` (billing, never checkout), `returning_until` (an ended plan's
window to bring cloud work home: `status.ts` `grantedPlan` reads the plan as none
while it is set, so the paid badge, subscriber view and upgrade review never
follow an ended plan; the page shows one quiet line, `presentation.ts`
`returningLine`, and the code `return_window_ended` reads as a plain sentence
wherever it arrives), `keeper_restart_at` (a planned restart of the always-on
cloud connection: one quiet line under Connected machines while set, which reads
the kept logins to name a single cluster and stays quiet if that read fails) and
`plans` (display prices, each with the plan's optional
whole-number `cloud_time_multiple` and `storage_multiple` relative to Pro).
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

The two unused original public kept presentation files are removed after actual host acceptance; the pane loader points to `../extensions/KeptApplicationView.svelte`. The actual presentation is in the selected private package; a default absent build keeps copies and ordinary file/Git recovery without a paywall or choice. `kept.ts` retains the four existing routes/types and optional host-only guards. `keptReviews.svelte.ts` remains the SAME chat/tree/settings singleton: guarded refresh cannot adopt a different-owner/unguarded in-flight result, publishes only while original owner current and propagates refusal instead of treating it as fresh success. Ordinary refresh preserves prior display behavior while preventing old auth-owner publication. New owner domains establish a successful guarded list before choices. The actual selected host/assembly and public-only absent recovery pass checks/builds and browser verification against a closed synthetic peer, including dark rendering and the corrected shared hide/show focus repeat. Earlier paired evidence and failed receipts remain retained; these results do not certify authenticated account operations, installation, signatures or entitlement.

Account/settings/cloud/provider/secret presentation now lives in the private optional client package behind finite `../extensions/accountPresentation.ts` services. Pane/Home/Settings/main use the installed/default-null `AccountApplicationView`; the twelve unused public presentation copies are removed after actual selected and absent renderer acceptance. Synthetic selected native Personal proves original sign-in draft custody, account replacement retirement and one-send billing uncertainty; separately selected Legacy proves one-time setup continuation. Personal handoff metadata remains an open completion gap. Absent assembly has zero private inputs/account requests/effects/owners. Existing mutable transports/catalog/secret memory remain the sole host owner for this transitional checkpoint; moving those Pro-only controllers is required next. Shared plan/placement/kept/privacy recovery stays public. Presentation source policies moved with the views into private `account-policy.test.mjs`; public fixed transport behavior tests remain here.

The unchanged fixed `requestKeptReview` event leaf now lives in `kept.ts` and remains re-exported from `keptReviews.svelte.ts`. FileTree can open the same immediate free recovery without eagerly loading the mutable review owner. App loads that exact original singleton on notice/review refresh; no second store, route or choice exists. Account entry-budget gate is unchanged.

Paid-only navigation and native status presentation predicates are lazy leaves. The existing plan lookup retains its original ten-second cancellation/deadline/generation; passive plain-browser/local unavailable paths never fetch those leaves. No account owner or action is created by these imports. This entry graph split does not claim removal of the remaining Pro controllers.

The shared Home navigation defers the exact PlanBadge component until a paid plan exists. Brand/workspace/settings navigation stays immediate; this is cosmetic chunk separation, not new entitlement or account authority. Selected/free final entry-size gates remain required after each graph change.

Account-only controller implementations and their original behavioral tests are extracted into the private optional entry after presentation acceptance. Shared free/recovery DTOs, onboarding drafts and actual daemon API ownership remain public; only fixed cloud/profile operations cross the host SDK. The new private controller/tests are source-only pending root gates. No capability or installation is enabled by relocation.

The six unused public account/billing/device/status/usage/personal-validation copies are removed (440 production lines). Their actual private counterparts retain the original behavior; five original public suites (529 lines) now live beside those helpers with only type-only host SDK import changes. Shared provider/placement, kept recovery, fixed account contracts and free missing-extension behavior are unchanged. This dead-policy removal awaits root checks/tests; it does not grant installation, account or service readiness.

Pure paid provider/presentation/secret-validation policy now lives only in the existing private helpers. Public providers retains catalog naming and paused-session parsing; presentation retains only two fixed protocol/recovery constants; projectSecrets retains wire types only. Original policy assertions move with those private helpers, while public shared label/paused-row, kept-name/chat/recovery assertions remain. Only unused kept presentation helper bodies are removed; the same API/route/pair/confirmation/domain owners and missing-extension file/folder recovery are unchanged. This follow-up source freeze awaits root full gates.
