<script lang="ts">
  import ProSettings from "../settings/ProSettings.svelte";
  import BrandMark from "../shared/BrandMark.svelte";
  import { isNativeShell } from "../net/native";
  import { isBrowserGateway } from "../net/base";
  let { visible = true }: { visible?: boolean } = $props();
</script>

<div class="pro-view">
  {#if isNativeShell()}
    <ProSettings {visible} />
  {:else}
    <div class="browser-account">
      <div class="brand"><BrandMark size={44} /><span>chimaera <span class="product">Pro</span></span></div>
      <h1>Your work, here and away.</h1>
      {#if isBrowserGateway()}
        <p>Your plan, billing and connected devices live in your account.</p>
        <a href="/account">Open your account</a>
      {:else}
        <p>Open Chimaera Pro from the desktop app to manage your account.</p>
      {/if}
    </div>
  {/if}
</div>
<style>
  .pro-view { height: 100%; overflow: auto; background: var(--bg); color: var(--fg); }
  .browser-account { box-sizing: border-box; max-width: 620px; margin: auto; padding: 48px 30px; }
  .brand { display: flex; align-items: center; gap: 10px; margin-bottom: 30px; font-size: 23px; font-weight: 600; letter-spacing: -.6px; }
  .product { margin-left: 10px; padding-left: 14px; border-left: 1px solid var(--edge); color: var(--muted); font-size: var(--text-md); font-weight: 450; letter-spacing: 0; }
  h1 { font-size: clamp(27px, 3vw, 34px); font-weight: 560; letter-spacing: -.8px; line-height: 1.2; }
  p { color: var(--muted); font-size: var(--text-md); line-height: 1.7; }
  a { display: inline-flex; margin-top: 16px; padding: 10px 15px; border-radius: 7px; background: var(--fg); color: var(--bg); font-size: var(--text-sm); text-decoration: none; }
  a:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 3px; }
</style>
