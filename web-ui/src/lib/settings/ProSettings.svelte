<script lang="ts">
  import CloudSetup from "./CloudSetup.svelte";
  import BrandMark from "../shared/BrandMark.svelte";
  import PlanBadge from "../shared/PlanBadge.svelte";
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

  let { visible = true }: { visible?: boolean } = $props();
  const intentKey = "chimaera.pro.purchase";
  function savedIntent(): PurchaseIntent | null { try { return readIntent(sessionStorage.getItem(intentKey)); } catch { return null; } }
  const initialIntent = savedIntent();
  let intent = $state<PurchaseIntent | null>(initialIntent);
  let selected = $state<PaidPlan>(initialIntent?.plan ?? "pro");
  let interval = $state<BillingInterval>(initialIntent?.interval ?? "month");
  let status = $state<ProStatus | null>(null);
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
  let generation = 0;
  let alive = true;
  const subscribed = $derived(status?.signed_in === true && paid(status.plan));
  const signInPhase = $derived(status?.sign_in?.phase ?? null);
  const checkoutPending = $derived(intent?.stage === "checkout");
  const cloudHours = $derived(status?.usage?.cloud_hours);
  const cloudLimit = $derived(status?.limits?.cloud_hours);

  function remember(next: PurchaseIntent | null): void {
    intent = next;
    try { if (next) sessionStorage.setItem(intentKey, JSON.stringify(next)); else sessionStorage.removeItem(intentKey); } catch { /* Selection still survives while this view is open. */ }
  }
  async function load(refresh = false): Promise<void> {
    const request = ++generation;
    try {
      let next = await proStatus();
      if (refresh && next.signed_in) { await proRefreshAccount(); next = await proStatus(); }
      if (!alive || request !== generation) return;
      status = next;
      if (next.signed_in && paid(next.plan) && intent !== null) {
        remember(null);
        notice = "Your plan is active. You're ready to continue.";
      }
      error = null;
    } catch (reason) {
      if (alive && request === generation) error = friendlyError(reason, "Your account couldn't refresh. Please try again in a moment.");
    }
  }
  $effect(() => {
    revision;
    if (visible && $pageVisible) untrack(() => void load());
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
    if (!visible || !$pageVisible || !status?.signed_in || !subscribed || !connectionsOpen) return;
    let stopped = false;
    void proHosts().then(value => { if (!stopped) hosts = value; }).catch(() => { if (!stopped) error = "Connected machines couldn't refresh. Please try again."; });
    return () => { stopped = true; };
  });
  $effect(() => {
    if (!visible || !$pageVisible || !status?.signed_in || !securityOpen) return;
    let stopped = false;
    void proDevices().then(value => { if (!stopped) devices = value; }).catch(() => { if (!stopped) error = "Your devices couldn't refresh. Please try again."; });
    return () => { stopped = true; };
  });
  $effect(() => {
    if (visible && status?.signed_in && !subscribed && intent?.stage === "sign_in" && busy === null) {
      untrack(() => void checkout());
    }
  });
  onMount(() => {
    const dispose = asyncDisposer(onProChanged(() => { revision += 1; }));
    const focus = () => { if (visible && document.visibilityState === "visible") void load(true); };
    window.addEventListener("focus", focus);
    return () => { alive = false; generation += 1; dispose(); window.removeEventListener("focus", focus); };
  });
  async function act(name: string, operation: () => Promise<void>, failure: string): Promise<void> {
    if (busy !== null) return;
    busy = name; error = null;
    try { await operation(); await load(); }
    catch (reason) { error = friendlyError(reason, failure); }
    finally { busy = null; }
  }
  async function checkout(): Promise<void> {
    if (busy !== null) return;
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

<section class="pro" aria-label="Chimaera Pro">
  <header class="heading">
    <div class="brand"><BrandMark size={38} /><span>chimaera</span><span class="product">Pro</span></div>
    <p class="eyebrow">Your work, within reach.</p>
    <h1>{subscribed ? "A little more room for your work." : "Keep working beyond your laptop."}</h1>
    <p class="lede">Stay connected to your machines. Let work continue while your computer is away. Your own agents, with the subscriptions you already use.</p>
  </header>

  {#if status === null}
    <p class="muted" role="status">Loading your account…</p>
  {:else if !status.available}
    <div class="panel"><h2>Pro isn't available in this build</h2><p class="muted">Your local workbench and SSH connections are ready to use.</p></div>
  {:else}
    {#if status.signed_in}
      <div class="identity">
        <div><span class="email">{status.email}</span><span class="muted small">{subscribed ? "Your account" : "Signed in · No active plan"}</span></div>
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
    {#if checkoutPending && !subscribed}
      <div class="panel notice"><h2>Finish in your browser</h2><p>Review {intent?.plan === "max" ? "Max" : "Pro"} and complete checkout there. We'll show your plan here once payment is confirmed.</p><div class="actions"><button disabled={busy !== null} onclick={() => void load(true)}>Check plan status</button><button class="text-button" disabled={busy !== null} onclick={() => { remember(null); notice = null; }}>Back to plans</button></div><p class="small">Closed checkout? Choose a plan below to open it again. Closing this view won't affect your local work.</p></div>
    {/if}

    {#if !subscribed}
      <section class="plans" aria-labelledby="plans-title">
        <div class="section-heading"><h2 id="plans-title">Choose your room to work</h2><div class="interval" role="group" aria-label="Billing interval"><button class:chosen={interval === "month"} aria-pressed={interval === "month"} onclick={() => (interval = "month")}>Monthly</button><button class:chosen={interval === "year"} aria-pressed={interval === "year"} onclick={() => (interval = "year")}>Yearly <span>2 months free</span></button></div></div>
        <div class="plan-options">
          {#each ["pro", "max"] as plan}
            <button class="plan-card" class:selected={selected === plan} aria-pressed={selected === plan} onclick={() => (selected = plan as PaidPlan)}>
              <span class="plan-name">{plan === "pro" ? "Pro" : "Max"}</span><span class="price">${plan === "pro" ? (interval === "month" ? "8" : "80") : (interval === "month" ? "30" : "300")}<span> / {interval}</span></span>
              <span class="plan-detail">{plan === "pro" ? "100" : "500"} cloud hours / month</span><span class="plan-detail">{plan === "pro" ? "20" : "100"} GB mirrored storage</span><span class="plan-note">{plan === "pro" ? "Everything in Pro." : "The same features, more capacity."}</span>
            </button>
          {/each}
        </div>
        <ul class="benefits"><li>Keep remote machines connected</li><li>Automatic laptop and cloud handoff</li><li>Reach your work from another device</li></ul>
        <div class="purchase"><button disabled={busy !== null || signInPhase !== null} onclick={() => void checkout()}>{busy === "checkout" ? "Opening checkout…" : status.signed_in ? `Continue with ${selected === "max" ? "Max" : "Pro"}` : "Sign in to continue"}</button><p class="small muted">{interval === "year" ? `Billed $${selected === "pro" ? "80" : "300"} yearly` : `Billed $${selected === "pro" ? "8" : "30"} monthly`}. Review the full details in secure browser checkout before subscribing.</p></div>
        {#if !status.signed_in}<p class="small muted">Already subscribed? <button class="text-button" disabled={busy !== null || signInPhase !== null} onclick={() => { remember(null); void act("sign-in", proSignIn, "Sign-in couldn't start. Please try again."); }}>Sign in to your account</button></p>{/if}
        <p class="free-note">Local work, agents and ordinary SSH remain free. You don't need an account to keep using them.</p>
      </section>
    {:else}
      <section class="panel plan-current" aria-label="Current plan"><div class="section-heading"><h2>Chimaera {status.plan === "max" ? "Max" : "Pro"}</h2><button class="secondary" disabled={busy !== null} onclick={() => void act("billing", proBillingPortal, "Billing couldn't open. Please try again in a moment.")}>{busy === "billing" ? "Opening billing…" : "Manage billing"}</button></div>
        {#if cloudHours !== undefined && cloudLimit !== undefined}<p><strong>{cloudHours.toFixed(1)}</strong> of {cloudLimit} cloud hours used this month</p><progress max={cloudLimit} value={Math.min(cloudHours, cloudLimit)} aria-label="Monthly cloud hours used"></progress>{/if}
        {#if status.usage && status.limits}<p class="small muted">{(status.usage.storage_bytes / 1e9).toFixed(1)} of {Math.round(status.limits.storage_bytes / 1e9)} GB storage used</p>{/if}
      </section>
      <CloudSetup {visible} />
      <details class="section" ontoggle={(event) => (connectionsOpen = event.currentTarget.open)}><summary>Connected machines</summary>{#if connectionsOpen}<div class="section-body"><p class="muted small">Add your remote hosts on Home. Keep a connection available through Pro here.</p>{#if hosts.length === 0}<p class="muted">No machines to show yet.</p>{/if}{#each hosts as host (host.alias)}<div class="row"><div><span>{host.alias}</span><span class="muted small">{host.status === "prompting" ? "Waiting for authentication" : host.status === "connecting" ? "Connecting…" : host.status === "connected" ? "Connected" : "Offline"}</span></div>{#if host.kind === "ssh"}<label class="keep"><input type="checkbox" checked={host.kept} disabled={busy !== null} onchange={(event) => setKept(host, event.currentTarget)} />Keep connected</label>{/if}</div>{/each}</div>{/if}</details>
      <details class="section" ontoggle={(event) => (mirrorsOpen = event.currentTarget.open)}><summary>Project mirrors and privacy</summary>{#if mirrorsOpen}<MirrorSettings visible={visible && mirrorsOpen} />{/if}</details>
    {/if}

    {#if status.signed_in}
      {#if !subscribed}<details class="section" ontoggle={(event) => (recoveryOpen = event.currentTarget.open)}><summary>Existing project privacy</summary>{#if recoveryOpen}<MirrorSettings visible={visible && recoveryOpen} recoveryOnly />{/if}</details>{/if}
      <details class="section" ontoggle={(event) => (securityOpen = event.currentTarget.open)}><summary>Account and devices</summary>{#if securityOpen}<div class="section-body">{#each devices as device (device.id)}<div class="row"><div><span>{device.name}</span><span class="muted small">{lastSeen(device.last_seen)}</span></div>{#if device.this}<span class="small muted">This device</span>{/if}</div>{/each}<div class="actions"><button class="secondary" disabled={busy !== null} onclick={() => { remember(null); void act("sign-out", proSignOut, "Sign-out couldn't finish. Please try again."); }}>Sign out</button><button class="text-button" disabled={busy !== null} onclick={() => { remember(null); void act("sign-out-all", proSignOutEverywhere, "Sign-out couldn't finish. Please try again."); }}>Sign out everywhere</button></div><p class="muted small">Signing out everywhere also closes the SSH logins held by Pro.</p></div>{/if}</details>
    {/if}
  {/if}
  {#if error || status?.error}<div class="error" role="alert"><span>{error ?? friendlyError(status?.error, "Part of your Pro connection couldn't refresh. Your local work remains available.")}</span><button class="secondary" disabled={busy !== null} onclick={() => void load(true)}>Try again</button></div>{/if}
</section>

<style>
  .pro { box-sizing: border-box; max-width: 780px; margin: 0 auto; padding: 36px 32px 48px; color: var(--fg); font-size: var(--text-md); }
  .heading { max-width: 620px; margin-bottom: 26px; }
  .brand { display: flex; align-items: center; gap: 9px; font-size: 22px; font-weight: 600; letter-spacing: -.5px; }
  .product { font-size: var(--text-sm); font-weight: 500; color: var(--muted); margin-left: 3px; padding-left: 12px; border-left: 1px solid var(--edge); letter-spacing: 0; }
  .eyebrow { color: var(--muted); font-size: var(--text-xs); margin: 24px 0 9px; }
  h1 { font-size: clamp(24px, 3vw, 31px); font-weight: 600; letter-spacing: -.7px; line-height: 1.2; margin: 0 0 12px; }
  h2 { font-size: var(--text-lg); font-weight: 600; margin: 0; }
  p { line-height: 1.6; margin: 10px 0; }
  .lede, .muted { color: var(--muted); }
  .small { font-size: var(--text-sm); }
  button { display: inline-flex; justify-content: center; align-items: center; gap: 7px; padding: 9px 14px; border: 1px solid transparent; border-radius: 7px; font: inherit; cursor: pointer; background: var(--fg); color: var(--bg); }
  button:hover:not(:disabled) { opacity: .85; }
  button:disabled { opacity: .5; cursor: default; }
  button:focus-visible, summary:focus-visible, input:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 3px; }
  .secondary { background: transparent; border-color: var(--edge); color: var(--fg); }
  .text-button { border: 0; padding: 3px 0; background: transparent; color: var(--accent); font-size: var(--text-sm); }
  .identity { display: flex; flex-wrap: wrap; align-items: center; gap: 12px; margin-bottom: 22px; padding: 14px 0; border-bottom: 1px solid var(--edge); }
  .identity > div { flex: 1 1 220px; min-width: 0; }
  .email { overflow-wrap: anywhere; }
  .identity .small, .row .small { display: block; margin-top: 4px; }
  .panel { border: 1px solid var(--edge); border-radius: 12px; padding: 20px; margin: 18px 0; }
  .section-heading { display: flex; align-items: center; justify-content: space-between; gap: 14px; flex-wrap: wrap; }
  .interval { display: flex; gap: 3px; padding: 3px; background: var(--row-hover); border-radius: 7px; }
  .interval button { background: transparent; color: var(--muted); padding: 6px 8px; font-size: var(--text-sm); }
  .interval .chosen { background: var(--bg); color: var(--fg); border-color: var(--edge); }
  .interval span { font-size: var(--text-xs); color: var(--accent); }
  .plan-options { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 240px), 1fr)); gap: 12px; margin-top: 18px; }
  .plan-card { display: flex; flex-direction: column; align-items: flex-start; gap: 6px; text-align: left; color: var(--fg); background: transparent; border-color: var(--edge); padding: 20px; }
  .plan-card.selected { border-color: var(--accent); background: color-mix(in srgb, var(--accent) 5%, var(--bg)); }
  .plan-name { font-weight: 600; font-size: var(--text-lg); }
  .price { font-size: 29px; font-weight: 600; letter-spacing: -.6px; margin: 7px 0; }
  .price span { font-size: var(--text-sm); font-weight: 400; color: var(--muted); letter-spacing: 0; }
  .plan-detail { font-size: var(--text-sm); }
  .plan-note { font-size: var(--text-xs); color: var(--muted); margin-top: 7px; }
  .benefits { display: flex; flex-wrap: wrap; gap: 8px 24px; padding: 0 0 0 17px; margin: 20px 0; color: var(--muted); font-size: var(--text-sm); line-height: 1.6; }
  .purchase { margin-top: 20px; }
  .purchase p { max-width: 55ch; }
  .free-note { font-size: var(--text-sm); color: var(--muted); border-top: 1px solid var(--edge); padding-top: 16px; margin-top: 24px; }
  .notice { background: color-mix(in srgb, var(--accent) 7%, transparent); }
  .message { border-radius: 7px; padding: 12px; }
  progress { width: 100%; height: 5px; accent-color: var(--accent); }
  .section { border-top: 1px solid var(--edge); margin-top: 14px; }
  summary { color: var(--muted); padding: 17px 0; cursor: pointer; font-size: var(--text-md); }
  summary:hover { color: var(--fg); }
  .section-body { padding-bottom: 14px; }
  .row { display: flex; align-items: center; justify-content: space-between; flex-wrap: wrap; gap: 10px; padding: 10px 0; }
  .row > div { min-width: 0; overflow-wrap: anywhere; }
  .keep { display: flex; gap: 7px; align-items: center; font-size: var(--text-sm); }
  input { accent-color: var(--accent); }
  .actions { display: flex; flex-wrap: wrap; align-items: center; gap: 10px 16px; margin: 14px 0; }
  .error { display: flex; flex-wrap: wrap; gap: 10px; align-items: center; padding: 14px; margin-top: 18px; background: color-mix(in srgb, var(--warn) 8%, transparent); color: var(--warn); border-radius: 8px; }
  .error span { flex: 1; min-width: 160px; }
  @media (max-width: 520px) { .pro { padding: 24px 18px 36px; } .plan-card { padding: 15px 12px; } .identity { flex-wrap: wrap; } .panel { padding: 16px; } .brand { font-size: 20px; } }
  @media (max-width: 340px) { .plan-options { grid-template-columns: 1fr; } }
</style>
