<script lang="ts">
  import { untrack } from "svelte";
  import { pastedImageName, uploadAndInsert } from "../net/uploads";
  import { focusTerminal, release, show } from "./termPool";

  interface Props {
    /** Session whose pooled terminal this pane shows. */
    sessionId: string;
    /** True when this pane is the focused pane and this tab is active. */
    focused: boolean;
    /** The pane's terminal font-size override (px); undefined = default. */
    fontSize?: number;
  }

  let { sessionId, focused, fontSize = undefined }: Props = $props();

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

<div class="term-view" bind:this={host} onpastecapture={onPasteCapture}></div>

<style>
  .term-view {
    position: absolute;
    inset: 0;
  }
</style>
