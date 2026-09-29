<script lang="ts">
  import { untrack } from "svelte";
  import { pastedImageName, uploadAndInsert } from "../net/uploads";
  import { isBrowserGateway } from "../net/base";
  import { isWatching, setWatching } from "./viewerMode.svelte";
  import { focusTerminal, release, show, refreshAccess } from "./termPool";

  interface Props {
    /** Session whose pooled terminal this pane shows. */
    sessionId: string;
    /** True when this pane is the focused pane and this tab is active. */
    focused: boolean;
    /** The pane's terminal font-size override (px); undefined = default. */
    fontSize?: number;
    remote?: boolean;
    available?: boolean;
  }

  let { sessionId, focused, fontSize = undefined, remote = false, available = true }: Props = $props();

  const watching = $derived(isWatching(sessionId));
  /** Watch/control only means something for a project viewed from another
   *  device: an ordinary local terminal never grows this strip. */
  const showAccess = $derived(remote || isBrowserGateway());
  function toggleAccess(): void { setWatching(sessionId, !watching); refreshAccess(sessionId); }

  let host = $state<HTMLDivElement | null>(null);

  // Attach the pooled terminal into this pane's container; the cleanup
  // (tab switch, pane close, unmount) parks it back in the warm stash.
  // Font size is deliberately untracked here — the second effect handles
  // live size changes without a park/re-attach round trip.
  $effect(() => {
    const el = host;
    const id = sessionId;
    if (el === null) return;
    show(id, el, untrack(() => fontSize));
    return () => release(id, el);
  });

  // Live per-pane font-size changes: show() on an attached terminal just
  // re-measures and refits in place.
  $effect(() => {
    const size = fontSize;
    if (host !== null) show(sessionId, host, size);
  });

  $effect(() => {
    if (focused) focusTerminal(sessionId);
  });

  /**
   * Screenshot paste into a terminal: a PTY can't take pixels, so the image
   * uploads to the session's host and its shell-quoted path types at the
   * cursor instead. Capture-phase (fires before xterm's own paste handler),
   * and ONLY when the clipboard holds an image and no text — a normal text
   * paste must keep flowing to the PTY untouched.
   */
  function onPasteCapture(e: ClipboardEvent): void {
    if (watching) { e.preventDefault(); return; }
    const dt = e.clipboardData;
    if (dt == null || dt.types.includes("text/plain")) return;
    const items = [...dt.items].filter((i) => i.type.startsWith("image/"));
    if (items.length === 0) return;
    e.preventDefault();
    e.stopPropagation();
    for (const item of items) {
      const file = item.getAsFile();
      if (file !== null) void uploadAndInsert(sessionId, file, pastedImageName(file.type));
    }
  }
</script>

{#if showAccess}
  <div class="terminal-access" role="toolbar" aria-label="Terminal access">
    {#if remote}<span class="cloud-chip">Cloud{available ? "" : " · unavailable"}</span>{/if}
    <span>{watching ? "Just watching" : "Terminal control"}</span>
    {#if remote && !available && !watching}<button type="button" onclick={() => refreshAccess(sessionId)}>Reconnect and wake</button>{/if}
    <button type="button" aria-pressed={!watching} onclick={toggleAccess}>{watching ? "Take control" : "Just watch"}</button>
  </div>
{/if}
<div class="term-view" class:watching class:with-access={showAccess} bind:this={host} onpastecapture={onPasteCapture}></div>

<style>
  .term-view {
    position: absolute;
    inset: 0;
  }
  /* Only a viewed terminal reserves the strip and may clip a wider owner grid. */
  .term-view.with-access {
    inset: 34px 0 0;
    overflow: auto;
  }
  .terminal-access { position: absolute; inset: 0 0 auto; height: 34px; display: flex; align-items: center; justify-content: flex-end; gap: 10px; padding: 0 10px; color: var(--muted); background: var(--rail-bg); font-size: 11px; border-bottom: 1px solid var(--edge); }
  .cloud-chip { margin-right: auto; color: var(--accent); }
  .terminal-access button { min-height: 28px; padding: 3px 9px; border: 1px solid var(--edge); border-radius: 5px; color: var(--fg); background: var(--bg); cursor: pointer; }
  @media (max-width: 700px) { .terminal-access { height: 44px; } .terminal-access button { min-height: 36px; } .term-view.with-access { top: 44px; } }
</style>
