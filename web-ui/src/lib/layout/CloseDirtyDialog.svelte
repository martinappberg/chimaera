<script lang="ts">
  /**
   * "Save changes before closing?" for tabs whose files hold unsaved edits —
   * every close gesture routes here (the ×, middle-click, the context menu's
   * Close / Close Others / Close All, Cmd/Ctrl+W and the native close-view).
   * Save closes only what saved; a failure keeps its tab open with the reason
   * inline. Don't save drops the edits (and their journaled draft). Focus
   * lands on Save — the choice that loses nothing — and Escape cancels.
   * Scrim + dialog per ConfirmDialog.
   */
  import { focusOnMount } from "../shared/focusOnMount";
  import { modalFocus } from "../shared/modalFocus";
  import { basename } from "../previews/files";

  interface Props {
    /** Absolute paths of the dirty files being closed. */
    paths: string[];
    saving: boolean;
    error: string | null;
    onSave(): void;
    onDiscard(): void;
    onCancel(): void;
  }

  let { paths, saving, error, onSave, onDiscard, onCancel }: Props = $props();

  const title = $derived(
    paths.length === 1
      ? `Save changes to “${basename(paths[0])}”?`
      : `Save changes to ${paths.length} files?`,
  );
</script>

<div
  class="backdrop"
  role="presentation"
  onclick={() => {
    if (!saving) onCancel();
  }}
  onkeydown={(e) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      if (!saving) onCancel();
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
    <div class="body">
      {#if paths.length === 1}
        Your edits will be lost if you close without saving.
      {:else}
        <ul class="files">
          {#each paths as p (p)}
            <li title={p}>{basename(p)}</li>
          {/each}
        </ul>
      {/if}
    </div>
    {#if error !== null}
      <div class="error">{error}</div>
    {/if}
    <div class="actions">
      <button class="opt quiet discard" disabled={saving} onclick={onDiscard}>don't save</button>
      <span class="spacer"></span>
      <button class="opt quiet" disabled={saving} onclick={onCancel}>cancel</button>
      <button class="opt primary" disabled={saving} use:focusOnMount onclick={onSave}>
        {saving ? "saving…" : paths.length === 1 ? "save" : "save all"}
      </button>
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
    width: min(420px, 100%);
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
    word-break: break-word;
  }

  .body {
    font-size: var(--text-sm);
    line-height: 1.5;
    color: var(--muted);
  }

  .files {
    margin: 0;
    padding-left: 1.1rem;
    max-height: 9rem;
    overflow: auto;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--fg);
  }

  .error {
    font-size: var(--text-sm);
    color: var(--err);
    word-break: break-word;
  }

  .actions {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 4px;
  }

  .spacer {
    flex: 1;
  }

  .discard:hover:not(:disabled) {
    color: var(--err);
  }
</style>
