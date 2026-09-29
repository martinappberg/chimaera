<script lang="ts">
  /**
   * The trust prompt (docs/plugin-platform-plan.md §2): shown when the
   * daemon refuses an install, update, switch or trust with 409 and what the
   * plugin can do (`TrustAsk`). Who asks and from where; for an update that
   * asks for more, what it would also do (the running version keeps running
   * until the user decides); then everything it can do, in the daemon's
   * words. A plugin that runs programs says so plainly and is confirmed by
   * typing its name. Confirming sends the capability digest shown — if the
   * plugin changed meanwhile, the daemon asks again.
   *
   * ConfirmDialog's scrim, focus and keys: focus starts on Cancel, Escape
   * cancels; a failure keeps the dialog open with its error.
   */
  import { focusOnMount } from "../shared/focusOnMount";
  import { modalFocus } from "../shared/modalFocus";
  import { trustWords } from "./installCopy";
  import type { TrustAsk } from "./store";

  interface Props {
    ask: TrustAsk;
    mode: "install" | "update" | "trust";
    busy?: boolean;
    error?: string | null;
    onConfirm(): void;
    onCancel(): void;
    /** An update that asks for more: Skip this version. */
    onSkip?: (() => void) | null;
  }

  let { ask, mode, busy = false, error = null, onConfirm, onCancel, onSkip = null }: Props = $props();

  const words = $derived(trustWords(ask, mode));
  const grown = $derived(ask.grown !== null && ask.grown.length > 0 ? ask.grown : null);
  const privileged = $derived(ask.tier === "privileged");
  let typed = $state("");
  const confirmed = $derived(ask.confirm === null || typed.trim() === ask.confirm);
</script>

<div
  class="backdrop"
  role="presentation"
  onclick={() => {
    if (!busy) onCancel();
  }}
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
    aria-labelledby="trust-title"
    tabindex="-1"
    use:modalFocus
    onclick={(e) => e.stopPropagation()}
  >
    <div class="title" id="trust-title">{words.title}</div>
    <p class="lead">{words.lead}</p>
    {#if privileged}
      <p class="warn" role="note">
        It runs programs on this host. A program can do anything its arguments allow, so trust it only if you trust
        its author.
      </p>
    {/if}
    <div class="lists">
      {#if grown !== null}
        <div class="group">
          <div class="head">It would also</div>
          <ul>
            {#each grown as line, i (i)}
              <li class:priv={line.privileged}>{line.text}</li>
            {/each}
          </ul>
        </div>
      {/if}
      <div class="group">
        <div class="head">{grown !== null ? "Everything it can do" : "It can"}</div>
        {#if ask.can.length === 0}
          <p class="none">Nothing beyond running in its sandbox.</p>
        {:else}
          <ul>
            {#each ask.can as line, i (i)}
              <li class:priv={line.privileged}>{line.text}</li>
            {/each}
          </ul>
        {/if}
      </div>
    </div>
    {#if ask.confirm !== null}
      <label class="confirm-field">
        <span>Type <b>{ask.confirm}</b> to trust it</span>
        <input type="text" spellcheck="false" autocomplete="off" bind:value={typed} disabled={busy} />
      </label>
    {/if}
    {#if error !== null}
      <div class="error" role="alert">{error}</div>
    {/if}
    <div class="actions">
      {#if onSkip !== null && grown !== null}
        {@const skip = onSkip}
        <button class="opt quiet skip" disabled={busy} title="Keep the version you have; this one isn't offered again" onclick={skip}
          >Skip this version</button
        >
      {/if}
      <button class="opt quiet" use:focusOnMount disabled={busy} onclick={onCancel}>cancel</button>
      <button class="opt primary" disabled={busy || !confirmed} onclick={onConfirm}>
        {busy ? "Working…" : words.confirm}
      </button>
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 110; /* ConfirmDialog's layer: above the context menu and pickers */
    display: grid;
    place-items: center;
    padding: 24px;
    background: var(--scrim);
    backdrop-filter: blur(2px);
  }
  .dialog {
    width: min(480px, 100%);
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
  .lead,
  .warn,
  .none {
    margin: 0;
    font-size: var(--text-sm);
    line-height: 1.5;
    color: var(--muted);
    overflow-wrap: anywhere;
  }
  .warn {
    color: var(--fg);
    padding: 8px 10px;
    border-radius: 8px;
    background: color-mix(in srgb, var(--warn) 9%, transparent);
    border: 1px solid color-mix(in srgb, var(--warn) 35%, var(--edge));
  }
  .lists {
    min-height: 0;
    overflow: auto;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .head {
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
    font-weight: 600;
    margin-bottom: 4px;
  }
  ul {
    margin: 0;
    padding: 0 0 0 18px;
    display: flex;
    flex-direction: column;
    gap: 3px;
    font-size: var(--text-sm);
    line-height: 1.45;
    color: var(--fg);
  }
  li {
    overflow-wrap: anywhere;
  }
  li.priv {
    color: var(--warn);
  }
  .confirm-field {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .confirm-field input {
    font: inherit;
    font-family: var(--mono);
    color: var(--fg);
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 7px;
    padding: 5px 10px;
  }
  .confirm-field input:focus {
    outline: 2px solid var(--focus-ring);
    outline-offset: 1px;
  }
  .error {
    font-size: var(--text-sm);
    color: var(--err);
    overflow-wrap: anywhere;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    flex-wrap: wrap;
    gap: 8px;
    margin-top: 4px;
  }
  .skip {
    margin-right: auto;
  }
</style>
