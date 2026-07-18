<script lang="ts">
  /** First-enable explanation for the compact rail/tray Caffeinate controls. */
  import { focusOnMount } from "../shared/focusOnMount";

  interface Props {
    busy?: boolean;
    error?: string | null;
    canOpenSettings?: boolean;
    closedLidReady?: boolean;
    onEnable(): void;
    onCancel(): void;
    onOpenSettings(): void;
  }

  let {
    busy = false,
    error = null,
    canOpenSettings = false,
    closedLidReady = false,
    onEnable,
    onCancel,
    onOpenSettings,
  }: Props = $props();
</script>

<div
  class="backdrop"
  role="presentation"
  onclick={() => !busy && onCancel()}
  onkeydown={(e) => {
    if (e.key === "Escape" && !busy) {
      e.stopPropagation();
      onCancel();
    }
  }}
>
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div
    class="dialog"
    role="dialog"
    aria-modal="true"
    aria-label="Enable Caffeinate"
    tabindex="-1"
    onclick={(e) => e.stopPropagation()}
  >
    <div class="head">
      <span class="cup" aria-hidden="true">☕︎</span>
      <div>
        <div class="title">Enable Caffeinate?</div>
        <div class="kicker">Keep Chimaera working while this Mac is locked.</div>
      </div>
    </div>
    <p>
      Active local chats, terminals, and commands can continue. If an SSH tunnel drops,
      Caffeinate keeps retrying eligible network failures until connectivity returns.
    </p>
    <ul>
      <li>The display may turn off and the Mac remains locked normally.</li>
      {#if closedLidReady}
        <li>
          This Mac is docked: Caffeinate can keep working in macOS closed-display mode while power
          and the external display remain connected.
        </li>
      {:else}
        <li>Locking is supported. Closing the lid can still sleep an undocked Mac.</li>
      {/if}
      <li>Chimaera must remain open; SSH password or 2FA prompts may still need you.</li>
    </ul>
    {#if error !== null}
      <div class="error">{error}</div>
    {/if}
    <div class="actions">
      <button class="quiet" disabled={busy} use:focusOnMount onclick={onCancel}>cancel</button>
      {#if canOpenSettings}
        <button class="quiet" disabled={busy} onclick={onOpenSettings}>details in Settings</button>
      {/if}
      <button class="primary" disabled={busy} onclick={onEnable}>
        {busy ? "enabling…" : "enable Caffeinate"}
      </button>
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 210;
    display: grid;
    place-items: center;
    padding: 24px;
    background: var(--scrim);
    backdrop-filter: blur(2px);
  }

  .dialog {
    width: min(500px, 100%);
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 20px;
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 11px;
    box-shadow: 0 18px 56px rgba(0, 0, 0, 0.38);
  }

  .head {
    display: flex;
    align-items: center;
    gap: 11px;
  }

  .cup {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    flex: none;
    border: 1px solid color-mix(in srgb, var(--accent) 45%, var(--edge));
    border-radius: 9px;
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 10%, transparent);
    font-size: 17px;
  }

  .title {
    color: var(--fg);
    font-size: var(--text-lg);
    font-weight: 650;
  }

  .kicker,
  p,
  li {
    color: var(--muted);
    font-size: var(--text-sm);
    line-height: 1.5;
  }

  .kicker {
    margin-top: 2px;
  }

  p,
  ul {
    margin: 0;
  }

  ul {
    padding-left: 20px;
  }

  li + li {
    margin-top: 4px;
  }

  .error {
    color: var(--err);
    font-size: var(--text-sm);
    overflow-wrap: anywhere;
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    flex-wrap: wrap;
    gap: 8px;
    margin-top: 2px;
  }

  button {
    appearance: none;
    font: inherit;
    font-size: var(--text-sm);
    padding: 7px 13px;
    border: 1px solid var(--edge);
    border-radius: 7px;
    color: var(--fg);
    cursor: pointer;
  }

  button:disabled {
    cursor: default;
    opacity: 0.55;
  }

  .quiet {
    background: transparent;
  }

  .quiet:hover:enabled {
    background: var(--row-hover);
  }

  .primary {
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
    background: color-mix(in srgb, var(--accent) 16%, transparent);
  }

  .primary:hover:enabled {
    background: color-mix(in srgb, var(--accent) 24%, transparent);
  }
</style>
