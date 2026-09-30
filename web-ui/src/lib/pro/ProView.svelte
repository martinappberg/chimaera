<script lang="ts">
  import { untrack } from "svelte";
  import ProSettings from "../settings/ProSettings.svelte";
  import CloudSetup from "../settings/CloudSetup.svelte";
  import BrandMark from "../shared/BrandMark.svelte";
  import { pageVisible } from "../shared/visibility";
  import { isNativeShell } from "../net/native";
  import { isBrowserGateway } from "../net/base";
  import { cloudRequest } from "./cloudTransport";
  import { BILLING_PATH } from "./accountHome";
  import { cloudOnboarding } from "./onboarding.svelte";
  let { visible = true, requiredProviders, contextLabel, workspaceId, onReady }: {
    visible?: boolean; requiredProviders?: string[]; contextLabel?: string; workspaceId?: string; onReady?: () => void;
  } = $props();
  const native = isNativeShell();
  const required = $derived(requiredProviders ?? cloudOnboarding.context?.providerIds ?? []);
  const project = $derived(contextLabel ?? cloudOnboarding.context?.workspaceName);
  const workspace = $derived(workspaceId ?? cloudOnboarding.context?.workspaceId);
  let worker = $state<boolean | null>(null);
  let checking = $state(false);
  let error = $state(false);
  let revision = 0;
  async function check(wake = false, signal?: AbortSignal): Promise<void> {
    if (checking) return;
    const current = ++revision;
    checking = true;
    try {
      const info = await cloudRequest({ operation: wake ? "start" : "info" }, signal);
      if (!signal?.aborted && current === revision) { worker = info.available === true; error = false; }
    } catch { if (!signal?.aborted && current === revision) error = true; }
    finally { if (current === revision) checking = false; }
  }
  $effect(() => {
    if (native || !visible || !$pageVisible) return;
    const controller = new AbortController();
    untrack(() => void check(false, controller.signal));
    return () => { revision += 1; checking = false; controller.abort(); };
  });
  function done(): void {
    if (onReady) { onReady(); return; }
    const context = cloudOnboarding.context;
    cloudOnboarding.clear();
    window.dispatchEvent(new CustomEvent("chimaera:providers-ready", { detail: context }));
  }
</script>

<div class="pro-view">
  {#if native}
    <ProSettings {visible} requiredProviders={required} contextLabel={project} workspaceId={workspace} onReady={done} />
  {:else}
    <div class="browser-account" class:worker>
      <div class="brand"><BrandMark size={44} /><span>chimaera <span class="product">Pro</span></span></div>
      {#if worker}
        <CloudSetup {visible} requiredProviders={required} contextLabel={project} workspaceId={workspace} onReady={done} />
        {#if isBrowserGateway()}<a class="account-link" href={BILLING_PATH}>Manage account and billing</a>{/if}
      {:else if worker === null}
        <h1>Connect your cloud agents</h1>
        {#if error}<p role="status">We couldn’t load your agent connections. Try again in a moment.</p><div class="actions"><button disabled={checking} onclick={() => void check(true)}>Try again</button></div>{:else}<p role="status">Loading your agent connections…</p>{/if}
      {:else}
        <h1>Your work, here and away.</h1>
        {#if isBrowserGateway()}<p>Your plan, billing and connected devices live in your account.</p><a class="button" href={BILLING_PATH}>Open your account</a>{:else}<p>Open Chimaera Pro from the desktop app to manage your account.</p>{/if}
      {/if}
    </div>
  {/if}
</div>
<style>
  .pro-view { height: 100%; overflow: auto; background: var(--bg); color: var(--fg); }
  .browser-account { box-sizing: border-box; max-width: 620px; margin: auto; padding: 48px 30px; }
  .browser-account.worker { max-width: 940px; }
  .brand { display: flex; align-items: center; gap: 10px; margin-bottom: 30px; font-size: 23px; font-weight: 600; letter-spacing: -.6px; }
  .product { margin-left: 10px; padding-left: 14px; border-left: 1px solid var(--edge); color: var(--muted); font-size: var(--text-md); font-weight: 450; letter-spacing: 0; }
  h1 { font-size: clamp(27px, 3vw, 34px); font-weight: 560; letter-spacing: -.8px; line-height: 1.2; }
  p { color: var(--muted); font-size: var(--text-md); line-height: 1.7; }
  button, .button { display: inline-flex; margin-top: 16px; padding: 10px 15px; border: 1px solid transparent; border-radius: 7px; background: var(--fg); color: var(--bg); font: inherit; font-size: var(--text-sm); text-decoration: none; cursor: pointer; }
  .account-link { color: var(--muted); font-size: var(--text-sm); text-underline-offset: 3px; }
  button:disabled { opacity: .5; cursor: default; }
  button:focus-visible, a:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 3px; }
  .actions { display: flex; gap: 12px; flex-wrap: wrap; }
  @media (max-width: 520px) { .browser-account { padding: 28px 16px; } }
</style>
