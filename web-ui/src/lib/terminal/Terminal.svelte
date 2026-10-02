<script lang="ts">
  import { tick, untrack } from "svelte";
  import { paneTabHasKeyboardFocus } from "../shared/tabNavigation";
  import { pastedImageName, uploadAndInsert } from "../net/uploads";
  import { beginFind, focusTerminal, release, show } from "./termPool";
  import { findTarget, type FindCommand } from "../shared/find";
  import { pageVisible } from "../shared/visibility";
  import FindBar from "../shared/FindBar.svelte";

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
  let findOpen = $state(false);
  let query = $state("");
  let caseSensitive = $state(false);
  let resultIndex = $state(-1);
  let resultCount = $state(0);
  let bar = $state<FindBar>();
  let search: ReturnType<typeof beginFind> = null;

  function run(direction: -1 | 0 | 1 = 0): void { search?.search(query, caseSensitive, direction); }
  function closeFind(restore = true): void {
    findOpen = false;
    search?.dispose();
    search = null;
    if (restore) focusTerminal(sessionId);
  }
  function find(command: FindCommand): boolean {
    if (command === "open") {
      if (search === null) {
        search = beginFind(sessionId, (index, count) => { resultIndex = index; resultCount = count; });
        if (search === null) return false;
        if (search.selection && !search.selection.includes("\n") && search.selection.length <= 512) query = search.selection;
      }
      findOpen = true;
      void tick().then(() => { if (findOpen) { bar?.focus(); run(); } });
      return true;
    }
    if (!findOpen) return false;
    if (command === "close") closeFind();
    else run(command === "previous" ? -1 : 1);
    return true;
  }
  $effect(() => () => closeFind(false));
  $effect(() => {
    if (!findOpen) return;
    if (!$pageVisible) {
      // xterm's addon schedules work on every write; suspend it in hidden windows.
      search?.dispose();
      search = null;
    } else if (search === null) {
      search = beginFind(sessionId, (index, count) => { resultIndex = index; resultCount = count; });
      untrack(() => run());
    }
  });

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
    if (focused && !findOpen && !paneTabHasKeyboardFocus()) focusTerminal(sessionId);
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

<div class="term-view" use:findTarget={find}>
  {#if findOpen}
    <FindBar bind:this={bar} {query} {caseSensitive} scope="Find in terminal scrollback"
      status={query === "" ? "" : resultCount === 0 ? "No matches" : resultIndex < 0 ? `${resultCount}+ matches` : `${resultIndex + 1} of ${resultCount}${resultCount >= 1000 ? "+" : ""}`}
      canStep={query !== "" && resultCount > 0}
      onQuery={(value) => { query = value; run(); }} onCase={(value) => { caseSensitive = value; run(); }}
      onStep={run} onClose={() => closeFind()} />
  {/if}
  <div class="term-host" bind:this={host} onpastecapture={onPasteCapture}></div>
</div>

<style>
  .term-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }
  .term-host { position: relative; flex: 1; min-height: 0; }
</style>
