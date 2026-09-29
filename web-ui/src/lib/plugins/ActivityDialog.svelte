<script lang="ts">
  /**
   * A plugin's activity log (the card's "…" → Activity): installs, updates,
   * trust given and withdrawn, blocks, the programs it ran and the tools it
   * downloaded — newest first, in words
   * (`activityWords`), fetched when opened. Kept by the daemon after a
   * Remove, so a removed plugin's history is still there.
   */
  import { focusOnMount } from "../shared/focusOnMount";
  import { modalFocus } from "../shared/modalFocus";
  import { activityTime, activityWords } from "./installCopy";
  import { fetchPluginActivity, isMissingRoute, type ActivityEntry } from "./store";

  interface Props {
    pluginId: string;
    name: string;
    onClose(): void;
  }

  let { pluginId, name, onClose }: Props = $props();

  let entries = $state<ActivityEntry[] | null>(null);
  let error = $state<string | null>(null);

  $effect(() => {
    let live = true;
    fetchPluginActivity(pluginId)
      .then((e) => {
        if (live) entries = e;
      })
      .catch((e: unknown) => {
        if (live) error = isMissingRoute(e) ? "this daemon keeps no activity log yet — update chimaera" : e instanceof Error ? e.message : String(e);
      });
    return () => {
      live = false;
    };
  });
</script>

<div
  class="backdrop"
  role="presentation"
  onclick={onClose}
  onkeydown={(e) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      onClose();
    }
  }}
>
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div
    class="dialog"
    role="dialog"
    aria-modal="true"
    aria-label="{name} activity"
    tabindex="-1"
    use:modalFocus
    onclick={(e) => e.stopPropagation()}
  >
    <div class="title">{name}: what it did on this host</div>
    {#if error !== null}
      <p class="line err" role="alert">{error}</p>
    {:else if entries === null}
      <p class="line" role="status">loading…</p>
    {:else if entries.length === 0}
      <p class="line">Nothing recorded yet.</p>
    {:else}
      <ol class="log">
        {#each entries as e, i (i)}
          <li>
            <span class="when">{activityTime(e.ts)}</span>
            <span class="what">{activityWords(e)}</span>
          </li>
        {/each}
      </ol>
    {/if}
    <div class="actions">
      <button class="opt" use:focusOnMount onclick={onClose}>close</button>
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 110;
    display: grid;
    place-items: center;
    padding: 24px;
    background: var(--scrim);
    backdrop-filter: blur(2px);
  }
  .dialog {
    width: min(520px, 100%);
    max-height: calc(100vh - 48px);
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 18px 20px;
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 10px;
    box-shadow: 0 16px 48px rgba(0, 0, 0, 0.35);
  }
  .title {
    font-size: var(--text-md);
    font-weight: 600;
    color: var(--fg);
  }
  .line {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .line.err {
    color: var(--err);
    overflow-wrap: anywhere;
  }
  .log {
    list-style: none;
    margin: 0;
    padding: 0;
    min-height: 0;
    overflow: auto;
    display: flex;
    flex-direction: column;
  }
  .log li {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    gap: 14px;
    padding: 6px 0;
    font-size: var(--text-sm);
    line-height: 1.45;
  }
  .log li + li {
    border-top: 1px solid var(--edge);
  }
  .when {
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  .what {
    color: var(--fg);
    overflow-wrap: anywhere;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
  }
</style>
