<script lang="ts">
  /**
   * Small confirmation modal for destructive actions (file-manager delete).
   * Scrim + dialog per the AskpassModal/FolderPicker pattern; focus lands on
   * Cancel (the safe default), Escape cancels, Enter activates the focused
   * button. A failure keeps the dialog open with an inline error, so the
   * caller passes `error` instead of closing.
  */
  import { focusOnMount } from "./focusOnMount";
  import { modalFocus } from "./modalFocus";

  interface Props {
    title: string;
    body: string;
    confirmLabel: string;
    /** Err-tinted confirm button (delete). */
    danger?: boolean;
    /** Inline failure line; the dialog stays open while set. */
    error?: string | null;
    onConfirm(): void;
    onCancel(): void;
  }

  let {
    title,
    body,
    confirmLabel,
    danger = false,
    error = null,
    onConfirm,
    onCancel,
  }: Props = $props();
</script>

<div
  class="backdrop"
  role="presentation"
  onclick={onCancel}
  onkeydown={(e) => {
    if (e.key === "Escape") {
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
    aria-label={title}
    tabindex="-1"
    use:modalFocus
    onclick={(e) => e.stopPropagation()}
  >
    <div class="title">{title}</div>
    <div class="body">{body}</div>
    {#if error !== null}
      <div class="error">{error}</div>
    {/if}
    <div class="actions">
      <button class="opt quiet" use:focusOnMount onclick={onCancel}>cancel</button>
      <button class="opt confirm" class:danger onclick={onConfirm}>{confirmLabel}</button>
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 110; /* above the context menu (90) and pickers (100) */
    display: grid;
    place-items: center;
    padding: 24px;
    background: var(--scrim);
    backdrop-filter: blur(2px);
  }

  .dialog {
    width: min(400px, 100%);
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

  .body {
    font-size: var(--text-sm);
    line-height: 1.5;
    color: var(--muted);
    word-break: break-word;
  }

  .error {
    font-size: var(--text-sm);
    color: var(--err);
    word-break: break-word;
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 4px;
  }

  /* Err-tinted confirm: the .opt.primary accent formula, on --err. */
  .confirm.danger {
    background: color-mix(in srgb, var(--err) 15%, transparent);
    border-color: color-mix(in srgb, var(--err) 55%, var(--edge));
  }

  .confirm.danger:hover {
    background: color-mix(in srgb, var(--err) 24%, transparent);
  }
</style>
