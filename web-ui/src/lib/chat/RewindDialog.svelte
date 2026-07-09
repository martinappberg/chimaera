<script lang="ts">
  /**
   * Checkpoint-rewind confirmation (claude). A dry-run report drives the body:
   * checking → no-checkpoint → the file list + confirm actions. The host owns
   * the intent flag that keeps replayed RewindResult events from reopening this.
   */
  import type { RewindReport } from "./store.svelte";

  interface Props {
    /** The in-flight rewind intent; `stage` gates checking vs applying. */
    intent: { id: string; preceding: string | null; fork: boolean; stage: "dry" | "applying" };
    /** The dry-run/apply report for this intent, once it lands. */
    report: RewindReport | null;
    onCancel: () => void;
    onConfirm: (fork: boolean) => void;
    onOpenFile?: (path: string) => void;
  }

  let { intent, report, onCancel, onConfirm, onOpenFile }: Props = $props();
</script>

<div class="dialog-veil">
  <div class="dialog" role="alertdialog" aria-label="rewind to checkpoint">
    {#if intent.stage === "applying"}
      <div class="dialog-title">rewinding…</div>
    {:else if report === null}
      <div class="dialog-title">checking checkpoint…</div>
      <div class="dialog-actions">
        <button class="opt quiet" onclick={onCancel}>cancel</button>
      </div>
    {:else if !report.canRewind}
      <div class="dialog-title">no checkpoint available for this message</div>
      {#if report.error !== null}
        <div class="dialog-note">{report.error}</div>
      {/if}
      <div class="dialog-actions">
        <button class="opt quiet" onclick={onCancel}>close</button>
      </div>
    {:else}
      <div class="dialog-title">
        rewind files to before this message
        {#if report.filesChanged.length > 0}
          — {report.filesChanged.length} file{report.filesChanged.length > 1 ? "s" : ""} will change
        {/if}
      </div>
      {#if report.filesChanged.length > 0}
        <ul class="dialog-files">
          {#each report.filesChanged as f (f)}
            <li>
              <button class="file-link" title="open in a pane" onclick={() => onOpenFile?.(f)}>
                {f}
              </button>
            </li>
          {/each}
        </ul>
      {/if}
      <div class="dialog-actions">
        <button class="opt primary" onclick={() => onConfirm(false)}>restore files</button>
        {#if intent.preceding !== null}
          <button
            class="opt primary"
            title="also truncate the conversation here (forks to a new native session)"
            onclick={() => onConfirm(true)}
          >
            restore + rewind conversation
          </button>
        {/if}
        <button class="opt quiet" onclick={onCancel}>cancel</button>
      </div>
    {/if}
  </div>
</div>

<style>
  .dialog-veil {
    position: absolute;
    inset: 0;
    background: color-mix(in srgb, var(--bg) 55%, transparent);
    display: grid;
    place-items: center;
    z-index: 30;
    animation: fade 0.12s ease;
  }
  .dialog {
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 8px;
    box-shadow: 0 10px 32px rgba(0, 0, 0, 0.28);
    padding: 12px 14px;
    max-width: min(440px, 90%);
    font-size: var(--text-sm);
    animation: rise 0.14s ease; /* @keyframes rise lives in app.css */
  }
  .dialog-title {
    color: var(--fg);
  }
  .dialog-note {
    color: var(--muted);
    margin-top: 4px;
  }
  .dialog-files {
    margin: 8px 0 0;
    padding-left: 18px;
    max-height: 140px;
    overflow-y: auto;
    scrollbar-width: thin;
    color: var(--muted);
    font-family: var(--mono, monospace);
  }
  .dialog-actions {
    display: flex;
    gap: 6px;
    margin-top: 10px;
    flex-wrap: wrap;
  }
  /* Action buttons are the shared .opt (app.css). */
  .file-link {
    background: none;
    border: none;
    padding: 0;
    color: var(--accent);
    font: inherit;
    font-family: var(--mono, monospace);
    cursor: pointer;
    text-align: left;
    word-break: break-all;
  }
  .file-link:hover {
    text-decoration: underline;
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .dialog-veil,
    .dialog {
      animation: none;
    }
  }
</style>
