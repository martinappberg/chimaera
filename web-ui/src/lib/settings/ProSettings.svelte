<script lang="ts">
  import AccountDevices from "../pro/AccountDevices.svelte";
  import CloudSetup from "./CloudSetup.svelte";
  import BrandMark from "../shared/BrandMark.svelte";
  import PlanBadge from "../shared/PlanBadge.svelte";
  import ProWalkthrough from "../pro/ProWalkthrough.svelte";
  import AccountUsage from "../pro/AccountUsage.svelte";
  import { billingCopy, billingPending, billingNeedsReview, explicitCheckoutChoice, canReviewUpgrade, latestBilling, planPrices } from "../pro/billing";
  import { onMount, tick, untrack } from "svelte";
  import MirrorSettings from "./MirrorSettings.svelte";
  import ConfirmDialog from "../shared/ConfirmDialog.svelte";
  import { asyncDisposer } from "../shared/asyncDisposer";
  import { pageVisible } from "../shared/visibility";
  import { paid, readIntent, friendlyError, recoverableAccountRestore, alreadySubscribed, connectionWarningCopy, returningLine, signInNoteCopy, type PaidPlan, type BillingInterval, type PurchaseIntent } from "../pro/presentation";
  import { accountErrorBar, accountPanel, completesReview, isConfirmedFree, nearLimit, offersCheck, reviewKey } from "../pro/account";
  import { accountFailure, connectionWarningCode, grantedPlan, paymentDue, planEnded, returningUntil, signInNote } from "../pro/status";
  import {
    onProChanged, proStatus, proSignIn, proCancelSignIn, proSignOut, proSignOutEverywhere,
    proHosts, proSetHostKept, proDevices, proRevokeDevice, proBillingCheckout, proBillingPortal, proCancelBilling, proRefreshAccount, proMirrorStatus,
    type ProStatus, type ProHost, type ProDevice, type ProAuthScreenHint,
  } from "../net/native";

  let { visible = true, requiredProviders = [], contextLabel, workspaceId, onReady }: { visible?: boolean; requiredProviders?: string[]; contextLabel?: string; workspaceId?: string; onReady?: () => void } = $props();
  const intentKey = "chimaera.pro.purchase";
  const planChoices = [
    { plan: "pro" as PaidPlan, name: "Pro", purpose: "For your everyday projects", detail: "Keep your projects in sync, and let agent work continue in the cloud while you're away.", capacity: "The complete Pro workflow." },
    { plan: "max" as PaidPlan, name: "Max", purpose: "For more cloud work", detail: "The same Pro workflow, with more capacity for longer cloud runs and more room for your projects.", capacity: "More capacity. All the same features." },
  ];
  function savedIntent(): PurchaseIntent | null { try { return readIntent(sessionStorage.getItem(intentKey)); } catch { return null; } }
  const initialIntent = savedIntent();
  let intent = $state<PurchaseIntent | null>(initialIntent);
  let selected = $state<PaidPlan>(initialIntent?.plan ?? "pro");
  let interval = $state<BillingInterval>(initialIntent?.interval ?? "month");
  let authScreen = $state<ProAuthScreenHint>(initialIntent?.screenHint ?? "sign-in");
  let shownSelection: number | null = null;
  /** The last confirmed account read. It stays rendered while newer reads run. */
  let status = $state<ProStatus | null>(null);
  /** No account event arrived since `status` was read. Purchases require it;
   * rendering never does, so background reads cannot unmount the page. */
  let accountFresh = $state(false);
  let refreshing = $state(false);
  /** A check the user asked for. Only it shows "checking"; background reads
   * keep whatever the last status showed. */
  let checking = $state(false);
  let hosts = $state<ProHost[]>([]);
  let devices = $state<ProDevice[]>([]);
  let deviceAccount: string | null = null;
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let reviewed = $state<string | null>(null);
  let reviewRequest: number | null = null;
  let upgradeOpen = $state(false);
  let upgradeInterval = $state<BillingInterval>("month");
  let busy = $state<string | null>(null);
  let openingBillingAfter = $state<number | null>(null);
  let revision = $state(0);
  let connectionsOpen = $state(false);
  let securityOpen = $state(false);
  let mirrorsOpen = $state(false);
  let recoveryOpen = $state(false);
  let plansElement = $state<HTMLElement>();
  let generation = 0;
  let alive = true;
  // An ended plan grants nothing even while the account still names it.
  const subscribed = $derived(status?.signed_in === true && paid(grantedPlan(status)));
  const ended = $derived(planEnded(status));
  /** One quiet line while an ended plan's cloud work can still be brought home. */
  const returning = $derived(returningLine(returningUntil(status)));
  const confirmedFree = $derived(isConfirmedFree(status));
  const panel = $derived(accountPanel(status, checking ? "check" : refreshing ? "background" : "none"));
  const failure = $derived(accountFailure(status));
  const paymentNeeded = $derived(paymentDue(status));
  const warning = $derived(status?.signed_in === true ? connectionWarningCode(status) : null);
  const signInPhase = $derived(status?.sign_in?.phase ?? null);
  const billing = $derived(status?.billing ?? null);
  const billingActive = $derived(billingPending(billing));
  const billingMessage = $derived(billing && !(openingBillingAfter !== null && billing.id <= openingBillingAfter) ? billingCopy(billing, status?.plan ?? null) : null);
  const billingRecovery = $derived(billingNeedsReview(billing, subscribed));
  const offerPlans = $derived(panel === "plans");
  // Amounts come only from the account, all four or none; without them every
  // place names the plans and says prices are shown at checkout.
  const prices = $derived(planPrices(status?.plans));
  const price = (plan: PaidPlan, every: BillingInterval): string | null => prices?.[plan][every] ?? null;
  const priced = $derived(prices !== null);
  const canReturnToPlans = $derived(billingRecovery && confirmedFree && reviewed !== null && reviewed === reviewKey(status));
  const errorBar = $derived(accountErrorBar(panel, error, failure, billingRecovery));
  /** A browser sign-in that ended without signing in: one quiet line; the
   * plans and the Sign in button stay. */
  const signInLine = $derived(signInPhase === null ? signInNoteCopy(signInNote(status)) : null);
  /** Sign-out being confirmed: every "Sign out everywhere", and a plain one
   * while a project is running in the cloud (named, since it stays there). */
  let signOutAsk = $state<{ everywhere: boolean; cloud: string[] } | null>(null);
  /** Sign-out finished here while the saved sign-in is removed later by the
   * app; shown quietly on the signed-out page, not as a failure. */
  let signOutLine = $state<string | null>(null);

  function showPlans(): void {
    plansElement?.scrollIntoView({ block: "start", behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
    plansElement?.focus({ preventScroll: true });
  }
  function remember(next: PurchaseIntent | null): void {
    intent = next;
    try { if (next) sessionStorage.setItem(intentKey, JSON.stringify(next)); else sessionStorage.removeItem(intentKey); } catch { /* Selection still survives while this view is open. */ }
  }
  async function load(refresh = false): Promise<void> {
    const request = ++generation;
    refreshing = true;
    try {
      let next = await proStatus();
      if (refresh && next.signed_in && !next.initializing) { await proRefreshAccount(); next = await proStatus(); }
      if (!alive || request !== generation) return;
      if (next.signed_in && status?.signed_in && next.email === status.email) next.billing = latestBilling(status.billing, next.billing);
      status = next;
      accountFresh = true;
      if (completesReview(next, reviewRequest)) { reviewed = reviewKey(next); reviewRequest = null; }
      if (!next.signed_in) notice = null;
      if (!next.initializing && next.signed_in && paid(grantedPlan(next)) && intent !== null) {
        remember(null);
        notice = "Your plan is active.";
      }
      error = null;
      // Account updates restore the draft only. Purchasing always needs a new click.
      const selection = intent;
      if (selection && next.signed_in && offerPlans && shownSelection !== selection.created) {
        notice = "You're signed in. Review your plan, then continue to checkout.";
        await tick();
        if (alive && request === generation && visible && document.visibilityState === "visible" && plansElement) {
          shownSelection = selection.created;
          showPlans();
        }
      }
    } catch (reason) {
      if (alive && request === generation) error = friendlyError(reason, "Your account couldn't refresh. Please try again in a moment.");
    } finally {
      if (alive && request === generation) refreshing = false;
    }
  }
  async function check(): Promise<void> {
    if (checking) return;
    checking = true;
    try { await load(true); } finally { checking = false; }
  }
  /** A purchase acts on a current read; a pending account event is read first. */
  async function ensureFresh(): Promise<boolean> {
    for (let attempt = 0; attempt < 3 && alive && !accountFresh; attempt += 1) await load();
    return alive && accountFresh;
  }
  $effect(() => {
    revision;
    if (visible && $pageVisible) untrack(() => void load());
    else untrack(() => { accountFresh = false; refreshing = false; generation += 1; });
  });
  $effect(() => {
    if (!visible || !$pageVisible || !status?.signed_in || status.initializing || !subscribed || !connectionsOpen) return;
    let stopped = false;
    void proHosts().then(value => { if (!stopped) hosts = value.filter(host => host.kind !== "worker"); }).catch(() => { if (!stopped) error = "Connected machines couldn't refresh. Please try again."; });
    return () => { stopped = true; };
  });
  $effect(() => {
    revision;
    const account = status?.email;
    if (deviceAccount !== (account ?? null)) { deviceAccount = account ?? null; devices = []; }
    if (!visible || !$pageVisible || !status?.signed_in || status.initializing || !securityOpen || !account) return;
    let stopped = false;
    void proDevices().then(value => { if (!stopped) devices = value; }).catch(() => { if (!stopped) error = "Your devices couldn't refresh. Please try again."; });
    return () => { stopped = true; };
  });
  onMount(() => {
    // An account event marks the rendered status stale without clearing it.
    const changed = () => { if (alive) { generation += 1; accountFresh = false; revision += 1; } };
    const dispose = asyncDisposer(onProChanged(changed).then((unlisten) => { changed(); return unlisten; }));
    const focus = () => { if (visible && document.visibilityState === "visible") void load(true); };
    window.addEventListener("focus", focus);
    return () => { alive = false; generation += 1; dispose(); window.removeEventListener("focus", focus); };
  });
  async function act(name: string, operation: () => Promise<void>, failure: string): Promise<void> {
    if (busy !== null || status?.initializing) return;
    busy = name; error = null;
    try { await operation(); await load(); }
    catch (reason) { error = friendlyError(reason, failure); }
    finally { busy = null; }
  }
  async function checkout(): Promise<void> {
    if (busy !== null || status?.initializing) return;
    // A returning subscriber uses the same sign-in action, never a new checkout.
    if (subscribed) { remember(null); return; }
    busy = "checkout";
    const fresh = await ensureFresh();
    busy = null;
    if (!fresh) return;
    if (subscribed) { remember(null); return; }
    if (!confirmedFree || billingActive || billingRecovery) return;
    const choice = explicitCheckoutChoice(status, accountFresh, { plan: selected, interval });
    if (!choice) return;
    // The shell owns the attempt from here. A view remount must not reopen it.
    remember(null);
    notice = null;
    let planExists = false;
    await act("checkout", async () => {
      try { await proBillingCheckout(choice.plan, choice.interval); }
      catch (reason) { planExists = alreadySubscribed(reason); throw reason; }
      if (status?.billing === undefined) notice = "Checkout opened in your browser. This page updates when you're done.";
    }, "Checkout couldn't open. Please try again.");
    // The account already has a plan: show it rather than asking for a refresh.
    if (planExists && alive) await load(true);
  }
  function selectPlan(plan: PaidPlan, nextInterval = interval): void {
    selected = plan;
    interval = nextInterval;
    if (intent) remember({ ...intent, plan, interval: nextInterval });
  }
  async function authenticate(screenHint: ProAuthScreenHint): Promise<void> {
    if (busy !== null || !status?.available || status.initializing || status.signed_in || billingActive || billingRecovery) return;
    authScreen = screenHint;
    signOutLine = null;
    remember({ plan: selected, interval, stage: "sign_in", screenHint, created: Date.now() });
    await act("sign-in", () => proSignIn(screenHint), "Your browser couldn't open. Please try again.");
  }
  async function openBilling(target?: { plan: PaidPlan; interval: BillingInterval }): Promise<void> {
    // An overdue payment is settled in the billing portal even without an active plan.
    if (busy !== null || billingActive || !status?.signed_in || !(subscribed || paymentNeeded) || status.initializing) return;
    if (target && !subscribed) return;
    if (target) {
      busy = "upgrade";
      const fresh = await ensureFresh();
      busy = null;
      if (!fresh || !canReviewUpgrade(status, accountFresh)) return;
    }
    notice = null;
    openingBillingAfter = billing?.id ?? 0;
    await act(target ? "upgrade" : "billing", async () => {
      await proBillingPortal(target);
      upgradeOpen = false;
      if (status?.billing === undefined) notice = "Billing opened in your browser. This page updates when you're done.";
    }, "Billing couldn't open. Please try again in a moment.");
    openingBillingAfter = null;
  }
  async function checkBillingAccount(): Promise<void> {
    const id = billing?.id;
    if (id === undefined || busy !== null) return;
    // Reads that began before this check cannot complete the review.
    generation += 1;
    reviewRequest = id;
    let refreshed = false;
    await act("billing-check", async () => {
      await proRefreshAccount();
      refreshed = true;
    }, "Your account couldn't be checked. Please try again before starting another checkout.");
    if (!refreshed) reviewRequest = null;
  }
  async function stopBilling(): Promise<void> {
    const id = billing?.id;
    if (id === undefined) return;
    await act("cancel-billing", () => proCancelBilling(id), "This request couldn't be stopped. Please check its latest status.");
  }
  async function dismissBilling(): Promise<void> {
    const id = billing?.id;
    if (id === undefined || billingActive || billingRecovery) return;
    await act("dismiss-billing", () => proCancelBilling(id), "This message couldn't be dismissed. Please try again.");
  }
  async function returnToPlans(): Promise<void> {
    const id = billing?.id;
    if (!canReturnToPlans || id === undefined) return;
    remember(null);
    await act("dismiss-billing", () => proCancelBilling(id), "This request couldn't be closed. Please check your account again.");
    reviewed = null;
  }
  async function cancelSignIn(): Promise<void> {
    remember(null);
    await act("cancel", proCancelSignIn, "Sign-in couldn't be canceled. Please try again.");
  }
  function setKept(host: ProHost, checkbox: HTMLInputElement): void {
    const kept = checkbox.checked; checkbox.checked = host.kept;
    void act(`host:${host.alias}`, async () => { await proSetHostKept(host.alias, kept); hosts = (await proHosts()).filter(host => host.kind !== "worker"); }, "This connection couldn't be updated. Please try again.");
  }
  function projectList(names: string[]): string {
    return names.length < 2 ? names.join("") : `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
  }
  const signOutBody = $derived.by(() => {
    if (signOutAsk === null) return "";
    const { everywhere, cloud } = signOutAsk;
    const lines: string[] = [];
    if (everywhere) lines.push("This signs you out everywhere you use Chimaera Pro, including this computer, and closes the cluster logins Pro keeps connected for you.");
    if (cloud.length === 1) lines.push(`${cloud[0]} is running in the cloud. It stays there until you sign in again.`);
    else if (cloud.length > 1) lines.push(`${projectList(cloud)} are running in the cloud. They stay there until you sign in again.`);
    lines.push("Nothing on this computer stops.");
    return lines.join(" ");
  });
  /** Projects running in the cloud are named before signing out: they stay
   * there until the next sign-in. Unknown (the read failed) names none. */
  async function requestSignOut(everywhere: boolean): Promise<void> {
    if (busy !== null || status?.initializing) return;
    busy = "sign-out-check";
    let cloud: string[] = [];
    try { cloud = (await proMirrorStatus()).workspaces.filter(workspace => workspace.ownership?.state === "remote" && !workspace.never_mirror).map(workspace => workspace.name); }
    catch { /* Nothing to name; signing out still stops nothing here. */ }
    finally { busy = null; }
    if (!alive) return;
    if (everywhere || cloud.length > 0) signOutAsk = { everywhere, cloud };
    else await signOut(false);
  }
  async function signOut(everywhere: boolean): Promise<void> {
    signOutAsk = null;
    remember(null);
    signOutLine = null;
    await act(everywhere ? "sign-out-all" : "sign-out", async () => {
      try { await (everywhere ? proSignOutEverywhere() : proSignOut()); }
      catch (reason) {
        // Signed out here already; the app clears the saved sign-in later.
        if ((reason instanceof Error ? reason.message : String(reason)) !== "sign_out_pending") throw reason;
        signOutLine = friendlyError(reason, "");
      }
    }, "Sign-out couldn't finish. Please try again.");
  }
  async function removeSignIn(device: ProDevice): Promise<void> {
    await act(`device:${device.id}`, async () => { await proRevokeDevice(device.id); devices = await proDevices(); }, "This sign-in couldn't be removed. Please try again.");
  }
</script>

{#snippet paymentNotice(action: boolean)}
  <div class="panel notice payment" role="status">
    <h2>Payment needs attention</h2>
    <p>Update your payment details in billing to keep your plan. Work on this computer continues as usual.</p>
    {#if action}<button disabled={busy !== null || billingActive} onclick={() => void openBilling()}>{busy === "billing" ? "Opening billing…" : "Manage billing"}</button>{/if}
  </div>
{/snippet}

{#snippet billingNotice()}
    {#if billingMessage}
      <div class="panel notice" role="status">
        <h2>{billingMessage.title}</h2><p>{billingMessage.detail}</p>
        {#if billingMessage.pending}<button class="secondary" disabled={busy !== null} onclick={() => void stopBilling()}>Stop waiting</button>
        {:else if billingMessage.check}<div class="actions"><button class="secondary" disabled={busy !== null} onclick={() => void checkBillingAccount()}>{busy === "billing-check" ? "Checking…" : "Check account"}</button>{#if canReturnToPlans}<button onclick={() => void returnToPlans()} disabled={busy !== null}>Return to plans</button>{/if}</div>{/if}
        {#if !billingMessage.pending && !billingRecovery}<button class="text-button" disabled={busy !== null} onclick={() => void dismissBilling()}>Dismiss</button>{/if}
        {#if canReturnToPlans}<p class="small muted">Your account currently has no active plan. You can return to plans when you're ready.</p>{/if}
        {#if billingRecovery && error}<p class="small" role="alert">{error}</p>{/if}
      </div>
    {/if}
{/snippet}

<section class="pro" class:subscriber={subscribed} aria-label="Chimaera Pro">
  {#if status !== null && !status.available}
    <!-- A build without an account endpoint: one line, nothing to sell or set up. -->
    <p class="unavailable" role="status">Chimaera Pro isn't available in this build.</p>
  {:else}
  <header class="heading">
    <div class="brand"><BrandMark size={44} /><span>chimaera</span><span class="product">{subscribed && status?.plan === "max" ? "Max" : "Pro"}</span></div>
    {#if subscribed}
      <h1>Your Chimaera {status?.plan === "max" ? "Max" : "Pro"}</h1>
    {:else if billingActive || billingRecovery || panel === "payment"}
      <h1>Your Chimaera account</h1>
    {:else if offerPlans}
      <h1>Your work, wherever you are.</h1>
      <p class="lede">Let your agents keep working while you're away. Return to the same project, conversation, and files on another device.</p>
      <div class="intro-actions">
        <button class="secondary" onclick={showPlans}>See plans</button>
        {#if status?.signed_in === false}<button class="secondary" disabled={busy !== null || signInPhase !== null || billingActive} onclick={() => void authenticate("sign-in")}>Sign in</button>{/if}
      </div>
      {#if signInLine}<p class="muted small sign-in-note" role="status">{signInLine}</p>{/if}
    {/if}
    {#if signOutLine && status?.signed_in === false}<p class="muted small sign-in-note" role="status">{signOutLine}</p>{/if}
  </header>

  {#if status?.initializing}
    <div class="panel notice" role="status">
      <h2>{status.initialization_phase === "keychain" ? "Opening your saved sign-in" : "Connecting your account"}</h2>
      <p>{status.initialization_phase === "keychain"
        ? "Allow Chimaera in the keychain prompt to finish signing in. Your projects stay available."
        : "We're checking your saved account. Your projects stay available."}</p>
    </div>
  {:else if offerPlans}<ProWalkthrough />{/if}

  {#if status === null}
    <p class="muted" role="status">Loading your account…</p>
  {:else if status.initializing}
    <!-- Account mutations wait for the single startup operation and its keychain fence. -->
  {:else}
    {#if status.signed_in}
      <div class="identity">
        <div><span class="email">{status.email}</span><span class="muted small">{subscribed ? "Your account" : paymentNeeded ? "Signed in · Payment needs attention" : confirmedFree ? "Signed in · No active plan" : "Signed in · Checking your plan"}</span></div>
        <PlanBadge plan={subscribed && paid(status.plan) ? status.plan : null} {ended} />
      </div>
      {#if returning !== null}<p class="muted small returning" role="status">{returning}</p>{/if}
      {#if warning !== null}<p class="muted small connection-warning" role="status">{connectionWarningCopy(warning)}</p>{/if}
    {/if}

    {#if signInPhase === "waiting"}
      <div class="panel notice" role="status"><h2>Continue in your browser</h2><p>Finish signing in there. This page updates when you're done, and the sign-in stays open for 15 minutes.{authScreen === "sign-up" ? " Your plan choice is kept here." : ""}</p><div class="actions"><button disabled={busy !== null} onclick={() => void authenticate(authScreen)}>Start again</button><button class="secondary" disabled={busy !== null} onclick={() => void cancelSignIn()}>Cancel sign-in</button></div></div>
    {:else if signInPhase === "finishing"}
      <div class="panel notice" role="status"><h2>Finishing sign-in…</h2><p>Saving your account securely on this computer.</p></div>
    {/if}

    {#if notice}<p class="notice message" role="status">{notice}</p>{/if}
    {#if !subscribed}{@render billingNotice()}{/if}

    {#if panel === "plans"}
      <section class="plans" aria-labelledby="plans-title" tabindex="-1" bind:this={plansElement}>
        <div class="section-heading plan-heading">
          <div><h2 id="plans-title">Choose your plan</h2><p class="muted small">The same features in both. More cloud capacity with Max.{#if !priced} Prices are shown at checkout.{/if}</p></div>
          <div class="interval" role="group" aria-label="Billing interval"><button class:chosen={interval === "month"} aria-pressed={interval === "month"} onclick={() => selectPlan(selected, "month")}>Monthly</button><button class:chosen={interval === "year"} aria-pressed={interval === "year"} onclick={() => selectPlan(selected, "year")}>Yearly</button></div>
        </div>
        <div class="plan-options" role="group" aria-label="Choose Pro or Max">
          {#each planChoices as choice (choice.plan)}
            <button class="plan-card" class:selected={selected === choice.plan} aria-pressed={selected === choice.plan} onclick={() => selectPlan(choice.plan)}>
              <span class="plan-top"><span class="plan-name">{choice.name}</span><span class="selection-mark" aria-hidden="true"></span></span>
              <span class="plan-purpose">{choice.purpose}</span>
              <span class="plan-detail">{choice.detail}</span>
              {#if priced}<span class="plan-price"><span class="price">{price(choice.plan, interval)}</span><span class="price-period">/ {interval === "year" ? "year" : "month"}</span></span>{/if}
              <span class="plan-note">{choice.capacity}</span>
            </button>
          {/each}
        </div>
        <div class="included"><span class="section-label">Included with both</span><ul><li>Projects stay in sync between your computer and the cloud</li><li>Cluster logins that stay connected</li><li>Browser access to your work</li><li>Project-by-project privacy controls</li></ul></div>
        <div class="purchase"><button disabled={busy !== null || signInPhase !== null || billingActive} onclick={() => status?.signed_in ? void checkout() : void authenticate("sign-up")}>{busy === "checkout" ? "Opening checkout…" : status.signed_in ? "Continue to checkout" : "Sign up"}</button><p class="small muted">{#if status.signed_in}{#if priced}Billed {price(selected, interval)} {interval === "year" ? "yearly" : "monthly"}. {:else}Prices are shown at checkout. {/if}Cloud time and project storage have monthly limits. Review billing details in secure checkout before subscribing.{:else}Create your account first. You can review your plan before checkout.{/if}</p></div>
        {#if !status.signed_in}<p class="signin-alternative small muted">Already have an account? <button class="text-button" disabled={busy !== null || signInPhase !== null || billingActive} onclick={() => void authenticate("sign-in")}>Sign in</button></p>{/if}
        <p class="free-note"><strong>Your local workbench stays free.</strong> Local projects, agents and ordinary SSH work without a Pro account.</p>
      </section>
    {:else if panel === "subscriber"}
      {#key status.email}<CloudSetup {visible} {requiredProviders} {contextLabel} {workspaceId} {onReady} />{/key}
      <section class="panel plan-current" aria-label="Current plan">
        <div class="section-heading"><div><span class="section-label">Your plan</span><h2>Chimaera {status.plan === "max" ? "Max" : "Pro"}</h2></div><button class="secondary" disabled={busy !== null || billingActive} onclick={() => void openBilling()}>{busy === "billing" ? "Opening billing…" : "Manage billing"}</button></div>
        {#if paymentNeeded}{@render paymentNotice(false)}{/if}
        {@render billingNotice()}
        {#if status.plan === "pro" && !paymentNeeded && nearLimit(status)}
          <div class="upgrade-entry">
            <div><h3>More room for your work</h3><p class="small muted">Max includes more cloud time and project storage, with the same workflow.</p></div>
            <button class="secondary" aria-expanded={upgradeOpen} disabled={busy !== null || billingActive || !canReviewUpgrade(status, true)} onclick={() => (upgradeOpen = !upgradeOpen)}>{billing?.kind === "plan_change" && billing.phase === "unconfirmed" ? "Review upgrade again" : "Upgrade to Max"}</button>
          </div>
          {#if upgradeOpen}
            <div class="upgrade-review" aria-label="Review Max upgrade">
              <h3>Review Chimaera Max</h3>
              <div class="interval" role="group" aria-label="Upgrade billing interval"><button class:chosen={upgradeInterval === "month"} aria-pressed={upgradeInterval === "month"} onclick={() => (upgradeInterval = "month")}>Monthly</button><button class:chosen={upgradeInterval === "year"} aria-pressed={upgradeInterval === "year"} onclick={() => (upgradeInterval = "year")}>Yearly</button></div>
              {#if price("max", upgradeInterval)}<p class="upgrade-price">{price("max", upgradeInterval)} <span class="small muted">/ {upgradeInterval === "year" ? "year" : "month"}</span></p>{/if}
              <p class="small muted">Your current plan stays active. Review the final amount, any prorated charge, and when the change takes effect before confirming in secure billing. Any remaining trial time is kept.</p>
              <div class="actions"><button disabled={busy !== null || !canReviewUpgrade(status, true)} onclick={() => void openBilling({ plan: "max", interval: upgradeInterval })}>{busy === "upgrade" ? "Opening review…" : "Review upgrade in browser"}</button><button class="text-button" disabled={busy !== null} onclick={() => (upgradeOpen = false)}>Keep current plan</button></div>
            </div>
          {/if}
        {/if}
        <AccountUsage usage={status.usage} limits={status.limits} />
      </section>
      <details class="section" ontoggle={(event) => (connectionsOpen = event.currentTarget.open)}><summary>Connected machines</summary>{#if connectionsOpen}<div class="section-body"><p class="muted small">Add remote machines on Home. Choose which cluster logins stay connected here.</p>{#if hosts.length === 0}<p class="muted">No machines to show yet.</p>{/if}{#each hosts as host (host.alias)}<div class="row"><div><span>{host.alias}</span><span class="muted small">{host.status === "prompting" ? "Waiting for authentication" : host.status === "connecting" ? "Connecting…" : host.status === "connected" ? "Connected" : "Offline"}</span></div>{#if host.kind === "ssh"}<label class="keep"><input type="checkbox" checked={host.kept} disabled={busy !== null} onchange={(event) => setKept(host, event.currentTarget)} />Keep connected</label>{/if}</div>{/each}</div>{/if}</details>
      <details class="section" ontoggle={(event) => (mirrorsOpen = event.currentTarget.open)}><summary>Projects and privacy</summary>{#if mirrorsOpen}<MirrorSettings visible={visible && mirrorsOpen} />{/if}</details>
    {:else if panel === "payment"}
      {@render paymentNotice(true)}
    {:else if panel === "billing"}
      <!-- The native attempt continues while this surface is hidden or closed. -->
    {:else if panel === "checking"}
      <p class="muted" role="status">Checking your account…</p>
    {:else}
      <div class="panel" role="status"><h2>Your account needs attention</h2><p class="muted">{friendlyError(error ?? failure, "We couldn't confirm your account details. Your local work remains available.")}</p>{#if !status.signed_in && signInPhase === null && !recoverableAccountRestore(failure)}<button disabled={busy !== null} onclick={() => void authenticate(authScreen)}>{authScreen === "sign-up" ? "Try signing up again" : "Sign in again"}</button>{:else if signInPhase === null && offersCheck(error, failure)}<button class="secondary" disabled={busy !== null || checking} onclick={() => void check()}>Check again</button>{/if}</div>
    {/if}

    {#if status.signed_in}
      {#if !subscribed}<details class="section" ontoggle={(event) => (recoveryOpen = event.currentTarget.open)}><summary>Project privacy</summary>{#if recoveryOpen}<MirrorSettings visible={visible && recoveryOpen} recoveryOnly />{/if}</details>{/if}
      <details class="section" ontoggle={(event) => (securityOpen = event.currentTarget.open)}><summary>Account and devices</summary>{#if securityOpen}<div class="section-body"><AccountDevices {devices} busy={busy !== null} onrevoke={removeSignIn} /><div class="actions"><button class="secondary" disabled={busy !== null} onclick={() => void requestSignOut(false)}>Sign out</button><button class="text-button" disabled={busy !== null} onclick={() => void requestSignOut(true)}>Sign out everywhere</button></div><p class="muted small">Signing out doesn't stop anything on this computer.</p></div>{/if}</details>
    {/if}
  {/if}
  {#if signOutAsk}
    <ConfirmDialog title={signOutAsk.everywhere ? "Sign out everywhere?" : "Sign out?"} body={signOutBody} confirmLabel={signOutAsk.everywhere ? "Sign out everywhere" : "Sign out"}
      danger={signOutAsk.everywhere} onConfirm={() => void signOut(signOutAsk?.everywhere ?? false)} onCancel={() => (signOutAsk = null)} />
  {/if}
  {#if errorBar}<div class="error" role="alert"><span>{error ?? friendlyError(failure, "Part of your account couldn't refresh. Your local work remains available.")}</span>{#if errorBar.check}<button class="secondary" disabled={busy !== null || checking} onclick={() => void check()}>{checking ? "Checking…" : "Try again"}</button>{/if}</div>{/if}
  {/if}
</section>

<style>
  .pro { container-type: inline-size; box-sizing: border-box; max-width: 940px; margin: 0 auto; padding: 44px clamp(18px, 4.5%, 42px) 64px; color: var(--fg); font-size: var(--text-md); }
  .unavailable { margin: 0; color: var(--muted); }
  .heading { container-type: inline-size; max-width: 660px; margin-bottom: 36px; }
  .subscriber .heading { margin-bottom: 20px; }
  .subscriber .brand { margin-bottom: 24px; }
  .subscriber h1 { margin-bottom: 0; font-size: clamp(25px, 4.6cqi, 30px); }
  .brand { display: flex; align-items: center; gap: 10px; margin-bottom: 30px; font-size: 23px; font-weight: 600; letter-spacing: -.65px; }
  .product { margin-left: 4px; padding-left: 14px; border-left: 1px solid var(--edge); color: var(--muted); font-size: var(--text-md); font-weight: 450; letter-spacing: 0; }
  h1 { margin: 0 0 15px; font-size: clamp(26px, 5.4cqi, 36px); font-weight: 560; letter-spacing: -1px; line-height: 1.18; }
  h2 { margin: 0; font-size: calc(var(--text-lg) + 2px); font-weight: 560; letter-spacing: -.3px; }
  p { margin: 10px 0; line-height: 1.65; }
  .lede { max-width: 56ch; margin: 0; font-size: var(--text-lg); color: var(--muted); line-height: 1.7; }
  .intro-actions { display: flex; flex-wrap: wrap; gap: 10px; margin-top: 20px; }
  .sign-in-note { margin: 14px 0 0; }
  .plans { scroll-margin-top: 20px; }
  .plans:focus { outline: none; }
  .muted { color: var(--muted); }
  .small { font-size: var(--text-sm); }
  .section-label { display: block; color: var(--muted); font-size: var(--text-xs); font-weight: 550; letter-spacing: .065em; text-transform: uppercase; }
  button { display: inline-flex; justify-content: center; align-items: center; gap: 7px; padding: 10px 16px; border: 1px solid transparent; border-radius: 7px; background: var(--fg); color: var(--bg); font: inherit; font-size: var(--text-sm); font-weight: 500; cursor: pointer; }
  button:hover:not(:disabled) { opacity: .85; }
  button:disabled { opacity: .5; cursor: default; }
  button:focus-visible, summary:focus-visible, input:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 3px; }
  .secondary { background: transparent; border-color: var(--edge); color: var(--fg); }
  .text-button { padding: 3px 0; border: 0; background: transparent; color: var(--fg); font-size: var(--text-sm); font-weight: 450; text-decoration: underline; text-decoration-color: var(--edge); text-underline-offset: 4px; }
  .identity { display: flex; flex-wrap: wrap; align-items: center; gap: 12px; margin-bottom: 25px; padding: 17px 0; border-top: 1px solid var(--edge); border-bottom: 1px solid var(--edge); }
  .identity > div { flex: 1 1 220px; min-width: 0; }
  .email { overflow-wrap: anywhere; }
  .identity .small, .row .small { display: block; margin-top: 4px; }
  .connection-warning, .returning { margin: -13px 0 25px; }
  .panel { margin: 20px 0; padding: 24px; border: 1px solid var(--edge); border-radius: 10px; }
  .section-heading { display: flex; align-items: center; justify-content: space-between; flex-wrap: wrap; gap: 18px; }
  .plan-heading p { margin-bottom: 0; }
  .interval { display: flex; flex: none; gap: 3px; padding: 3px; border: 1px solid var(--edge); border-radius: 8px; }
  .interval button { padding: 7px 10px; background: transparent; color: var(--muted); font-size: var(--text-sm); }
  .interval .chosen { background: var(--row-hover); color: var(--fg); }
  .plan-options { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 14px; margin-top: 24px; }
  .plan-card { display: flex; align-items: flex-start; flex-direction: column; gap: 0; padding: 24px; border-color: var(--edge); border-radius: 10px; background: transparent; color: var(--fg); text-align: left; }
  .plan-card:hover:not(:disabled) { opacity: 1; background: var(--row-hover); }
  .plan-card.selected { border-color: color-mix(in srgb, var(--fg) 48%, var(--edge)); background: color-mix(in srgb, var(--fg) 2.5%, var(--bg)); }
  .plan-top { display: flex; align-items: center; justify-content: space-between; width: 100%; gap: 12px; }
  .plan-name { font-size: calc(var(--text-lg) + 2px); font-weight: 600; }
  .selection-mark { box-sizing: border-box; width: 15px; height: 15px; flex: none; border: 1px solid var(--edge); border-radius: 50%; }
  .selected .selection-mark { border: 4px solid var(--fg); }
  .plan-purpose { margin-top: 18px; font-size: var(--text-md); font-weight: 550; }
  .plan-detail { flex: 1; margin-top: 8px; color: var(--muted); font-size: var(--text-sm); font-weight: 400; line-height: 1.7; }
  .plan-price { display: flex; align-items: baseline; gap: 7px; margin-top: 24px; }
  .price { font-size: 32px; font-weight: 550; letter-spacing: -.8px; }
  .price-period { color: var(--muted); font-size: var(--text-sm); font-weight: 400; }
  .plan-note { margin-top: 7px; color: var(--muted); font-size: var(--text-xs); font-weight: 400; }
  .included { padding: 24px 0; border-bottom: 1px solid var(--edge); }
  .included ul { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 10px 30px; padding-left: 16px; margin: 14px 0 0; font-size: var(--text-sm); line-height: 1.6; }
  .included li::marker { color: var(--muted); font-size: .7em; }
  .purchase { display: flex; align-items: center; gap: 22px; margin-top: 23px; }
  .purchase > button { flex: none; }
  .signin-alternative { margin: 14px 0 0; }
  .purchase p { max-width: 44ch; margin: 0; font-size: var(--text-xs); }
  .free-note { margin-top: 30px; padding-top: 22px; border-top: 1px solid var(--edge); color: var(--muted); font-size: var(--text-sm); }
  .free-note strong { display: block; color: var(--fg); font-weight: 500; margin-bottom: 3px; }
  .notice { background: color-mix(in srgb, var(--accent) 5%, transparent); }
  .message { border-radius: 7px; padding: 12px 15px; }
  .payment { border-color: color-mix(in srgb, var(--warn) 35%, var(--edge)); background: color-mix(in srgb, var(--warn) 5%, transparent); }
  .payment h2 { color: var(--warn); }
  .plan-current .section-label { margin-bottom: 8px; }
  .plan-current :global(.panel.notice) { padding: 16px 0; margin: 18px 0 0; border: 0; border-top: 1px solid var(--edge); border-radius: 0; background: transparent; }
  .plan-current :global(.panel.notice h2) { font-size: var(--text-md); }
  .upgrade-entry { display: flex; align-items: center; justify-content: space-between; gap: 20px; flex-wrap: wrap; margin-top: 24px; padding-top: 20px; border-top: 1px solid var(--edge); }
  .upgrade-entry > div { flex: 1 1 220px; }
  .upgrade-entry h3, .upgrade-review h3 { margin: 0; font-size: var(--text-md); font-weight: 550; }
  .upgrade-entry p { margin-bottom: 0; }
  .upgrade-entry > button { flex: none; }
  .upgrade-review { margin-top: 20px; padding: 20px; border: 1px solid var(--edge); border-radius: 8px; }
  .upgrade-review .interval { margin-top: 16px; width: fit-content; }
  .upgrade-price { margin: 16px 0 0; font-size: 24px; font-weight: 550; }
  .upgrade-review .actions { margin-bottom: 0; }
  .section { margin-top: 14px; border-top: 1px solid var(--edge); }
  summary { padding: 18px 0; color: var(--muted); font-size: var(--text-md); cursor: pointer; }
  summary:hover { color: var(--fg); }
  .section-body { padding-bottom: 14px; }
  .row { display: flex; align-items: center; justify-content: space-between; flex-wrap: wrap; gap: 10px; padding: 10px 0; }
  .row > div { min-width: 0; overflow-wrap: anywhere; }
  .keep { display: flex; align-items: center; gap: 7px; font-size: var(--text-sm); }
  input { accent-color: var(--accent); }
  .actions { display: flex; align-items: center; flex-wrap: wrap; gap: 10px 16px; margin: 14px 0; }
  .error { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; margin-top: 18px; padding: 15px; border-radius: 8px; background: color-mix(in srgb, var(--warn) 8%, transparent); color: var(--warn); }
  .error span { flex: 1; min-width: 160px; }
  @container (max-width: 660px) { .purchase { align-items: flex-start; flex-direction: column; gap: 12px; } }
  @media (pointer: coarse) { button, summary { min-height: 40px; } .text-button { padding: 8px 0; } }
  @container (max-width: 460px) { .plan-options, .included ul { grid-template-columns: 1fr; } .plan-card, .panel { padding: 21px; } .interval { width: 100%; } .interval button { flex: 1; } }
  @media (max-width: 760px) { .pro { padding: 30px 25px 44px; } .purchase { align-items: flex-start; flex-direction: column; gap: 12px; } }
  @media (max-width: 520px) { .pro { padding: 26px 20px 36px; } .heading { margin-bottom: 27px; } .brand { margin-bottom: 24px; font-size: 21px; } .plan-options, .included ul { grid-template-columns: 1fr; } .plan-card, .panel { padding: 21px; } .interval { width: 100%; } .interval button { flex: 1; } .plan-price { margin-top: 20px; } }
</style>
