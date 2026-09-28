<script lang="ts">
  import CloudSetup from "./CloudSetup.svelte";
  import BrandMark from "../shared/BrandMark.svelte";
  import PlanBadge from "../shared/PlanBadge.svelte";
  import ProWalkthrough from "../pro/ProWalkthrough.svelte";
  import { onMount, untrack } from "svelte";
  import MirrorSettings from "./MirrorSettings.svelte";
  import { asyncDisposer } from "../shared/asyncDisposer";
  import { pageVisible } from "../shared/visibility";
  import { paid, readIntent, friendlyError, type PaidPlan, type BillingInterval, type PurchaseIntent } from "../pro/presentation";
  import {
    onProChanged, proStatus, proSignIn, proCancelSignIn, proSignOut, proSignOutEverywhere,
    proHosts, proSetHostKept, proDevices, proBillingCheckout, proBillingPortal, proRefreshAccount,
    type ProStatus, type ProHost, type ProDevice,
  } from "../net/native";

  let { visible = true, requiredProviders = [], contextLabel, workspaceId, onReady }: { visible?: boolean; requiredProviders?: string[]; contextLabel?: string; workspaceId?: string; onReady?: () => void } = $props();
  const intentKey = "chimaera.pro.purchase";
  const prices: Record<PaidPlan, Record<BillingInterval, number>> = {
    pro: { month: 8, year: 80 }, max: { month: 30, year: 300 },
  };
  const planChoices = [
    { plan: "pro" as PaidPlan, name: "Pro", purpose: "For your everyday projects", detail: "Keep your projects in sync, and let agent work continue in the cloud while you're away.", capacity: "The complete Pro workflow." },
    { plan: "max" as PaidPlan, name: "Max", purpose: "For more cloud work", detail: "The same Pro workflow, with more capacity for longer cloud runs and more mirrored projects.", capacity: "More capacity. All the same features." },
  ];
  function savedIntent(): PurchaseIntent | null { try { return readIntent(sessionStorage.getItem(intentKey)); } catch { return null; } }
  const initialIntent = savedIntent();
  let intent = $state<PurchaseIntent | null>(initialIntent);
  let selected = $state<PaidPlan>(initialIntent?.plan ?? "pro");
  let interval = $state<BillingInterval>(initialIntent?.interval ?? "month");
  let status = $state<ProStatus | null>(null);
  let accountFresh = $state(false);
  let accountLoading = $state(false);
  let hosts = $state<ProHost[]>([]);
  let devices = $state<ProDevice[]>([]);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let busy = $state<string | null>(null);
  let revision = $state(0);
  let connectionsOpen = $state(false);
  let securityOpen = $state(false);
  let mirrorsOpen = $state(false);
  let recoveryOpen = $state(false);
  let plansElement = $state<HTMLElement>();
  let generation = 0;
  let alive = true;
  const subscribed = $derived(status?.signed_in === true && paid(status.plan));
  const confirmedFree = $derived(accountFresh && status?.available === true && !status.initializing && !status.error && (status.signed_in ? status.plan === "none" : true));
  const accountNeedsAttention = $derived(status?.available === true && !status.initializing && !subscribed && !confirmedFree && !accountLoading);
  const signInPhase = $derived(status?.sign_in?.phase ?? null);
  const checkoutPending = $derived(intent?.stage === "checkout");
  const cloudHours = $derived(status?.usage?.cloud_hours);
  const cloudLimit = $derived(status?.limits?.cloud_hours);

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
    accountFresh = false;
    accountLoading = true;
    try {
      let next = await proStatus();
      if (refresh && next.signed_in && !next.initializing) { await proRefreshAccount(); next = await proStatus(); }
      if (!alive || request !== generation) return;
      status = next;
      accountFresh = true;
      if (!next.initializing && next.signed_in && paid(next.plan) && intent !== null) {
        remember(null);
        notice = "Your plan is active. Follow your cloud setup below.";
      }
      error = null;
    } catch (reason) {
      if (alive && request === generation) error = friendlyError(reason, "Your account couldn't refresh. Please try again in a moment.");
    } finally {
      if (alive && request === generation) accountLoading = false;
    }
  }
  $effect(() => {
    revision;
    if (visible && $pageVisible) untrack(() => void load());
    else untrack(() => { accountFresh = false; accountLoading = false; generation += 1; });
  });
  // Only an explicitly opened checkout gets short-lived confirmation polling.
  // Normal account changes arrive from the shell; hidden panes do no polling.
  $effect(() => {
    if (!visible || !$pageVisible || !checkoutPending) return;
    const timer = setInterval(() => {
      if (intent && Date.now() - intent.created < 5 * 60_000) void load(true);
      else clearInterval(timer);
    }, 5000);
    return () => clearInterval(timer);
  });
  $effect(() => {
    if (!visible || !$pageVisible || !status?.signed_in || status.initializing || !subscribed || !connectionsOpen) return;
    let stopped = false;
    void proHosts().then(value => { if (!stopped) hosts = value; }).catch(() => { if (!stopped) error = "Connected machines couldn't refresh. Please try again."; });
    return () => { stopped = true; };
  });
  $effect(() => {
    if (!visible || !$pageVisible || !status?.signed_in || status.initializing || !securityOpen) return;
    let stopped = false;
    void proDevices().then(value => { if (!stopped) devices = value; }).catch(() => { if (!stopped) error = "Your devices couldn't refresh. Please try again."; });
    return () => { stopped = true; };
  });
  $effect(() => {
    if (visible && $pageVisible && status?.signed_in && confirmedFree && intent?.stage === "sign_in" && busy === null) {
      untrack(() => void checkout());
    }
  });
  onMount(() => {
    const dispose = asyncDisposer(onProChanged(() => { accountFresh = false; revision += 1; }));
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
    if (!confirmedFree) return;
    if (!status?.signed_in) {
      remember({ plan: selected, interval, stage: "sign_in", created: Date.now() });
      await act("sign-in", proSignIn, "Sign-in couldn't start. Please try again.");
      return;
    }
    // Mark before opening so an unrelated status event cannot launch it twice.
    const choice = intent?.stage === "sign_in" ? intent : { plan: selected, interval };
    remember({ ...choice, stage: "checkout", created: Date.now() });
    await act("checkout", () => proBillingCheckout(choice.plan, choice.interval), "Checkout couldn't open. Your plan hasn't changed. Please try again.");
  }
  async function cancelSignIn(): Promise<void> {
    remember(null);
    await act("cancel", proCancelSignIn, "Sign-in couldn't be cancelled. Please try again.");
  }
  function setKept(host: ProHost, checkbox: HTMLInputElement): void {
    const kept = checkbox.checked; checkbox.checked = host.kept;
    void act(`host:${host.alias}`, async () => { await proSetHostKept(host.alias, kept); hosts = await proHosts(); }, "This connection couldn't be updated. Please try again.");
  }
  function lastSeen(value: string): string {
    const date = new Date(value);
    return Number.isNaN(date.valueOf()) ? "Last seen unavailable" : `Last seen ${date.toLocaleString()}`;
  }
</script>

<section class="pro" class:subscriber={subscribed} aria-label="Chimaera Pro">
  <header class="heading">
    <div class="brand"><BrandMark size={44} /><span>chimaera</span><span class="product">{subscribed && status?.plan === "max" ? "Max" : "Pro"}</span></div>
    {#if subscribed}
      <h1>Your Chimaera {status?.plan === "max" ? "Max" : "Pro"}</h1>
    {:else if confirmedFree}
      <h1>Your work, wherever you are.</h1>
      <p class="lede">Let your agents keep working while you’re away. Return to the same project, conversation, and files on another device.</p>
      <button class="secondary intro-plans" onclick={showPlans}>See plans</button>
    {/if}
  </header>

  {#if status?.initializing}
    <div class="panel notice" role="status">
      <h2>{status.initialization_phase === "keychain" ? "Opening your saved sign-in" : "Connecting your account"}</h2>
      <p>{status.initialization_phase === "keychain"
        ? "Your system keychain is checking access to your saved account. Respond to any keychain prompt for chimaera to continue. Your workspaces remain available while you do."
        : "We're checking your saved account and restoring its connections. Your workspaces remain available."}</p>
      <button class="secondary" onclick={() => void load()}>Check again</button>
    </div>
  {:else if confirmedFree}<ProWalkthrough />{/if}

  {#if status === null}
    <p class="muted" role="status">Loading your account…</p>
  {:else if status.initializing}
    <!-- Account mutations wait for the single startup operation and its keychain fence. -->
  {:else if !status.available}
    <div class="panel"><h2>Pro isn't available in this build</h2><p class="muted">Your local workbench and SSH connections are ready to use.</p></div>
  {:else}
    {#if status.signed_in}
      <div class="identity">
        <div><span class="email">{status.email}</span><span class="muted small">{subscribed ? "Your account" : confirmedFree ? "Signed in · No active plan" : "Signed in · Checking your plan"}</span></div>
        <PlanBadge plan={paid(status.plan) ? status.plan : null} />
        <button class="text-button" disabled={busy !== null} onclick={() => void load(true)}>Refresh</button>
      </div>
    {/if}

    {#if signInPhase === "waiting"}
      <div class="panel notice" role="status"><h2>Continue in your browser</h2><p>Complete sign-in and verification there. Your selection is saved here, and this request stays open for up to 15 minutes.</p><div class="actions"><button disabled={busy !== null} onclick={() => void act("sign-in", proSignIn, "Sign-in couldn't restart. Please try again.")}>Start again</button><button class="secondary" disabled={busy !== null} onclick={() => void cancelSignIn()}>Cancel sign-in</button></div></div>
    {:else if signInPhase === "finishing"}
      <div class="panel notice" role="status"><h2>Finishing sign-in…</h2><p>Saving your account securely on this computer.</p></div>
    {/if}

    {#if notice}<p class="notice message" role="status">{notice}</p>{/if}
    {#if checkoutPending && confirmedFree}
      <div class="panel notice"><h2>Finish in your browser</h2><p>Review {intent?.plan === "max" ? "Max" : "Pro"} and complete checkout there. We'll show your plan here once payment is confirmed.</p><div class="actions"><button disabled={busy !== null} onclick={() => void load(true)}>Check plan status</button><button class="text-button" disabled={busy !== null} onclick={() => { remember(null); notice = null; }}>Back to plans</button></div><p class="small">Closed checkout? Choose a plan below to open it again. Closing this view won't affect your local work.</p></div>
    {/if}

    {#if confirmedFree}
      <section class="plans" aria-labelledby="plans-title" tabindex="-1" bind:this={plansElement}>
        <div class="section-heading plan-heading">
          <div><h2 id="plans-title">Choose your plan</h2><p class="muted small">The same features in both. More cloud capacity with Max.</p></div>
          <div class="interval" role="group" aria-label="Billing interval"><button class:chosen={interval === "month"} aria-pressed={interval === "month"} onclick={() => (interval = "month")}>Monthly</button><button class:chosen={interval === "year"} aria-pressed={interval === "year"} onclick={() => (interval = "year")}>Yearly <span>2 months free</span></button></div>
        </div>
        <div class="plan-options" role="group" aria-label="Choose Pro or Max">
          {#each planChoices as choice (choice.plan)}
            <button class="plan-card" class:selected={selected === choice.plan} aria-pressed={selected === choice.plan} onclick={() => (selected = choice.plan)}>
              <span class="plan-top"><span class="plan-name">{choice.name}</span><span class="selection-mark" aria-hidden="true"></span></span>
              <span class="plan-purpose">{choice.purpose}</span>
              <span class="plan-detail">{choice.detail}</span>
              <span class="plan-price"><span class="price">${prices[choice.plan][interval]}</span><span class="price-period">/ {interval === "year" ? "year" : "month"}</span></span>
              <span class="plan-note">{choice.capacity}</span>
            </button>
          {/each}
        </div>
        <div class="included"><span class="section-label">Included with both</span><ul><li>Project mirrors and agent handoff</li><li>Persistent remote connections</li><li>Browser access to your work</li><li>Project-by-project privacy controls</li></ul></div>
        <div class="purchase"><button disabled={busy !== null || signInPhase !== null} onclick={() => void checkout()}>{busy === "checkout" ? "Opening checkout…" : status.signed_in ? `Continue with ${selected === "max" ? "Max" : "Pro"}` : "Sign in"}</button><p class="small muted">Billed ${prices[selected][interval]} {interval === "year" ? "yearly" : "monthly"}. Cloud work and mirrored storage have plan limits. Review billing details in secure checkout before subscribing.</p></div>
        <p class="free-note"><strong>Your local workbench stays free.</strong> Local projects, agents and ordinary SSH work without a Pro account.</p>
      </section>
    {:else if subscribed}
      {#key status.email}<CloudSetup {visible} {requiredProviders} {contextLabel} {workspaceId} {onReady} />{/key}
      <section class="panel plan-current" aria-label="Current plan">
        <div class="section-heading"><div><span class="section-label">Your plan</span><h2>Chimaera {status.plan === "max" ? "Max" : "Pro"}</h2></div><button class="secondary" disabled={busy !== null} onclick={() => void act("billing", proBillingPortal, "Billing couldn't open. Please try again in a moment.")}>{busy === "billing" ? "Opening billing…" : "Manage billing"}</button></div>
        <details class="usage-details"><summary>Usage and plan details</summary><div class="usage-grid">
          {#if cloudHours !== undefined && cloudLimit !== undefined}<div><span class="usage-label">Cloud work this month</span><p class="usage-value"><strong>{cloudHours.toFixed(1)}</strong><span> / {cloudLimit} hours</span></p>{#if cloudLimit > 0}<progress max={cloudLimit} value={Math.max(0, Math.min(cloudHours, cloudLimit))} aria-label="Monthly cloud hours used"></progress>{/if}</div>{/if}
          {#if status.usage && status.limits}<div><span class="usage-label">Mirrored projects</span><p class="usage-value"><strong>{(status.usage.storage_bytes / 1e9).toFixed(1)}</strong><span> / {(status.limits.storage_bytes / 1e9).toLocaleString(undefined, { maximumFractionDigits: 1 })} GB</span></p>{#if status.limits.storage_bytes > 0}<progress max={status.limits.storage_bytes} value={Math.max(0, Math.min(status.usage.storage_bytes, status.limits.storage_bytes))} aria-label="Mirrored storage used"></progress>{/if}</div>{/if}
        </div>{#if !status.usage || !status.limits}<p class="muted small">Usage isn't available yet. Refresh your account to check again.</p>{:else}<p class="muted small usage-note">These are your account's current allowances. Local work remains available when a cloud limit is reached.</p>{/if}</details>
      </section>
      <details class="section" ontoggle={(event) => (connectionsOpen = event.currentTarget.open)}><summary>Connected machines</summary>{#if connectionsOpen}<div class="section-body"><p class="muted small">Add your remote hosts on Home. Keep a connection available through Pro here.</p>{#if hosts.length === 0}<p class="muted">No machines to show yet.</p>{/if}{#each hosts as host (host.alias)}<div class="row"><div><span>{host.alias}</span><span class="muted small">{host.status === "prompting" ? "Waiting for authentication" : host.status === "connecting" ? "Connecting…" : host.status === "connected" ? "Connected" : "Offline"}</span></div>{#if host.kind === "ssh"}<label class="keep"><input type="checkbox" checked={host.kept} disabled={busy !== null} onchange={(event) => setKept(host, event.currentTarget)} />Keep connected</label>{/if}</div>{/each}</div>{/if}</details>
      <details class="section" ontoggle={(event) => (mirrorsOpen = event.currentTarget.open)}><summary>Project mirrors and privacy</summary>{#if mirrorsOpen}<MirrorSettings visible={visible && mirrorsOpen} />{/if}</details>
    {:else if accountLoading}
      <p class="muted" role="status">Refreshing your account…</p>
    {:else}
      <div class="panel" role="status"><h2>Your account needs attention</h2><p class="muted">{friendlyError(error ?? status.error, "We couldn't confirm your account details. Your local work remains available.")}</p>{#if !status.signed_in && signInPhase === null}<button disabled={busy !== null} onclick={() => void act("sign-in", proSignIn, "Sign-in couldn't restart. Please try again.")}>Sign in</button>{:else if signInPhase === null}<button class="secondary" disabled={busy !== null} onclick={() => void load(true)}>Check again</button>{/if}</div>
    {/if}

    {#if status.signed_in}
      {#if !subscribed}<details class="section" ontoggle={(event) => (recoveryOpen = event.currentTarget.open)}><summary>Existing project privacy</summary>{#if recoveryOpen}<MirrorSettings visible={visible && recoveryOpen} recoveryOnly />{/if}</details>{/if}
      <details class="section" ontoggle={(event) => (securityOpen = event.currentTarget.open)}><summary>Account and devices</summary>{#if securityOpen}<div class="section-body">{#each devices as device (device.id)}<div class="row"><div><span>{device.name}</span><span class="muted small">{lastSeen(device.last_seen)}</span></div>{#if device.this}<span class="small muted">This device</span>{/if}</div>{/each}<div class="actions"><button class="secondary" disabled={busy !== null} onclick={() => { remember(null); void act("sign-out", proSignOut, "Sign-out couldn't finish. Please try again."); }}>Sign out</button><button class="text-button" disabled={busy !== null} onclick={() => { remember(null); void act("sign-out-all", proSignOutEverywhere, "Sign-out couldn't finish. Please try again."); }}>Sign out everywhere</button></div><p class="muted small">Signing out everywhere also closes the SSH logins held by Pro.</p></div>{/if}</details>
    {/if}
  {/if}
  {#if (error || status?.error) && !accountNeedsAttention}<div class="error" role="alert"><span>{error ?? friendlyError(status?.error, "Part of your Pro connection couldn't refresh. Your local work remains available.")}</span><button class="secondary" disabled={busy !== null} onclick={() => void load(true)}>Try again</button></div>{/if}
</section>

<style>
  .pro { container-type: inline-size; box-sizing: border-box; max-width: 940px; margin: 0 auto; padding: 44px clamp(18px, 4.5%, 42px) 64px; color: var(--fg); font-size: var(--text-md); }
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
  .intro-plans { margin-top: 20px; }
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
  .panel { margin: 20px 0; padding: 24px; border: 1px solid var(--edge); border-radius: 10px; }
  .section-heading { display: flex; align-items: center; justify-content: space-between; flex-wrap: wrap; gap: 18px; }
  .plan-heading p { margin-bottom: 0; }
  .interval { display: flex; flex: none; gap: 3px; padding: 3px; border: 1px solid var(--edge); border-radius: 8px; }
  .interval button { padding: 7px 10px; background: transparent; color: var(--muted); font-size: var(--text-sm); }
  .interval .chosen { background: var(--row-hover); color: var(--fg); }
  .interval span { margin-left: 2px; color: var(--muted); font-size: var(--text-xs); font-weight: 400; }
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
  .purchase p { max-width: 44ch; margin: 0; font-size: var(--text-xs); }
  .free-note { margin-top: 30px; padding-top: 22px; border-top: 1px solid var(--edge); color: var(--muted); font-size: var(--text-sm); }
  .free-note strong { display: block; color: var(--fg); font-weight: 500; margin-bottom: 3px; }
  .notice { background: color-mix(in srgb, var(--accent) 5%, transparent); }
  .message { border-radius: 7px; padding: 12px 15px; }
  .plan-current .section-label { margin-bottom: 8px; }
  .usage-details { margin-top: 20px; border-top: 1px solid var(--edge); }
  .usage-details summary { padding: 15px 0 0; font-size: var(--text-sm); }
  .usage-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 28px; margin-top: 24px; }
  .usage-label { color: var(--muted); font-size: var(--text-sm); }
  .usage-value { margin: 8px 0; font-size: var(--text-sm); }
  .usage-value strong { font-size: 23px; font-weight: 500; }
  .usage-value span { color: var(--muted); }
  .usage-note { margin: 16px 0 0; font-size: var(--text-xs); }
  progress { width: 100%; height: 4px; accent-color: var(--fg); }
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
  @container (max-width: 460px) { .plan-options, .included ul, .usage-grid { grid-template-columns: 1fr; } .plan-card, .panel { padding: 21px; } .interval { width: 100%; } .interval button { flex: 1; } }
  @media (max-width: 760px) { .pro { padding: 30px 25px 44px; } .purchase { align-items: flex-start; flex-direction: column; gap: 12px; } }
  @media (max-width: 520px) { .pro { padding: 26px 20px 36px; } .heading { margin-bottom: 27px; } .brand { margin-bottom: 24px; font-size: 21px; } .plan-options, .included ul, .usage-grid { grid-template-columns: 1fr; } .plan-card, .panel { padding: 21px; } .interval { width: 100%; } .interval button { flex: 1; } .plan-price { margin-top: 20px; } }
</style>
