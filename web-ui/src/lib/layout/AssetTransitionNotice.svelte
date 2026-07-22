<script lang="ts">
  import type { AssetTransition } from "./assetTransition";

  interface Props {
    transition: AssetTransition;
    blockedFiles: number;
    blockedDrafts: number;
    onReload: (force: boolean) => void;
    onDismiss: () => void;
  }

  let { transition, blockedFiles, blockedDrafts, onReload, onDismiss }: Props = $props();

  const blocked = $derived(blockedFiles > 0 || blockedDrafts > 0);
  const title = $derived(
    transition.reason === "build"
      ? "this window needs the current interface"
      : transition.reason === "connection"
        ? "the remote connection moved"
        : "part of the interface did not load",
  );
  const blockedMessage = $derived.by(() => {
    const drafts = blockedDrafts === 1 ? "chat draft" : "chat drafts";
    if (blockedFiles > 0 && blockedDrafts > 0) {
      return `Save or copy unsaved file edits and finish the unstored ${drafts} before reloading.`;
    }
    if (blockedFiles > 0) {
      return "Save or copy unsaved file edits before reloading.";
    }
    return `Send, remove, or copy the unstored ${drafts} before reloading.`;
  });
  const body = $derived.by(() => {
    if (blocked) return blockedMessage;
    if (transition.reason === "chunk") {
      return "If the affected view offers Retry, try it first. If it still fails, reload this window to obtain the current interface.";
    }
    return "The window will reload with matching interface assets.";
  });
</script>

<div class="asset-notice" role="status" aria-live="polite">
  <div class="head">
    <span class="dot" aria-hidden="true"></span>
    <span class="title">{title}</span>
  </div>
  <p>{body}</p>
  <div class="actions">
    {#if blocked}
      <span class="waiting">waiting for safe reload</span>
      <button class="quiet danger" onclick={() => onReload(true)}>reload anyway</button>
    {:else if transition.reason === "chunk" || !transition.requested}
      <button class="primary" onclick={() => onReload(false)}>reload window</button>
    {:else}
      <span class="waiting">reloading…</span>
    {/if}
    {#if transition.reason === "chunk" && !transition.requested}
      <button class="quiet" onclick={onDismiss}>dismiss</button>
    {/if}
  </div>
</div>

<style>
  .asset-notice {
    position: fixed;
    left: 50%;
    top: 14px;
    z-index: 185;
    width: min(390px, calc(100vw - 28px));
    padding: 11px 13px;
    transform: translateX(-50%);
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 10px;
    box-shadow: 0 8px 28px color-mix(in srgb, var(--fg) 12%, transparent);
    animation: noticein 0.16s ease-out;
  }

  @keyframes noticein {
    from {
      opacity: 0;
      transform: translate(-50%, -6px);
    }
    to {
      opacity: 1;
      transform: translate(-50%, 0);
    }
  }

  .head,
  .actions {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--accent);
    flex: none;
  }

  .title {
    color: var(--fg);
    font-size: var(--text-sm);
    font-weight: 600;
  }

  p {
    margin: 6px 0 0;
    color: var(--muted);
    font-size: var(--text-xs);
    line-height: 1.45;
  }

  .actions {
    margin-top: 9px;
  }

  button,
  .waiting {
    font-size: var(--text-xs);
  }

  button {
    cursor: pointer;
  }

  .primary {
    padding: 4px 11px;
    border: 1px solid var(--accent);
    border-radius: 6px;
    color: var(--fg);
    background: color-mix(in srgb, var(--accent) 14%, transparent);
  }

  .primary:hover {
    background: color-mix(in srgb, var(--accent) 24%, transparent);
  }

  .quiet {
    padding: 4px 6px;
    border: none;
    color: var(--muted);
    background: none;
  }

  .quiet:hover {
    color: var(--fg);
  }

  .danger {
    color: var(--err);
  }

  .waiting {
    color: var(--muted);
  }
</style>
