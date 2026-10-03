<script lang="ts">
  import { focusOnMount } from "../shared/focusOnMount";
  import { modalFocus } from "../shared/modalFocus";
  import type { SetupRequest } from "./agentSetup";
  import type { LaunchPick } from "./launcher";

  let { request, onclose, onlaunch }: {
    request: SetupRequest;
    onclose(): void;
    onlaunch(pick: LaunchPick): void;
  } = $props();

  // Agent setup is an explicit, infrequent action. Keep its dialog out of the
  // always-loaded shell, with a closeable modal while the chunk loads.
  const dialog = import("./AgentSetupDialog.svelte");
</script>

{#snippet fallback(failed: boolean)}
  <div class="backdrop" role="presentation" onclick={onclose} onkeydown={e => {
    if (e.key === "Escape") { e.stopPropagation(); onclose(); }
  }}>
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="setup-loading-title"
      tabindex="-1" onclick={e => e.stopPropagation()} onkeydown={e => {
        if (e.key === "Escape") onclose();
        e.stopPropagation();
      }}>
      <h2 id="setup-loading-title">{failed ? "Couldn’t load agent setup" : "Loading agent setup…"}</h2>
      {#if failed}<p>Close this dialog, reload the app, and try again.</p>{/if}
      <button use:focusOnMount onclick={onclose}>Close</button>
    </div>
  </div>
{/snippet}

<!-- The trap survives the loading/result swap so closing always returns to
     the invoking control, even when the focused loading button disappears. -->
<div class="loader" use:modalFocus>
  {#await dialog}
    {@render fallback(false)}
  {:then { default: AgentSetupDialog }}
    <AgentSetupDialog {request} {onclose} {onlaunch} />
  {:catch}
    {@render fallback(true)}
  {/await}
</div>

<style>
  .loader { display: contents; }
  .backdrop { position: fixed; inset: 0; z-index: 1500; background: var(--scrim); display: grid; place-items: center; padding: 24px; }
  .dialog { width: min(480px, 100%); max-height: calc(100vh - 48px); overflow: auto; background: var(--bg); color: var(--fg); border: 1px solid var(--edge); border-radius: 16px; padding: 24px; box-shadow: 0 22px 90px var(--scrim); }
  h2 { font-size: 18px; margin: 0 0 16px; }
  p { color: var(--muted); line-height: 1.5; }
  button { margin-top: 8px; font: inherit; font-size: 12px; padding: 9px 13px; border: 1px solid var(--edge); border-radius: 7px; background: var(--bg); color: var(--fg); cursor: pointer; }
</style>
