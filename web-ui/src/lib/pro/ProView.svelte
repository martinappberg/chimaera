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
      <BrandMark size={42} />
      <h1>Chimaera Pro</h1>
      {#if isBrowserGateway()}
        <p>Your plan, billing and connected devices live in your account.</p>
        <a href="/account">Open your account →</a>
      {:else}
        <p>Open Chimaera Pro from the desktop app to manage your account.</p>
      {/if}
    </div>
  {/if}
</div>
<style>
  .pro-view { height: 100%; overflow: auto; background: var(--bg); color: var(--fg); }
  .browser-account { max-width: 520px; padding: 40px 28px; margin: auto; }
  h1 { font-size: 24px; font-weight: 600; }
  p { color: var(--muted); line-height: 1.6; }
  a { color: var(--accent); }
</style>
