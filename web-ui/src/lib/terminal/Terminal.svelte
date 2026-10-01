<script lang="ts">
  import { tick, untrack } from "svelte";
  import { pastedImageName, uploadAndInsert } from "../net/uploads";
  import { isBrowserGateway } from "../net/base";
  import { isWatching, setWatching } from "./viewerMode.svelte";
  import { refusalFor } from "./refusals.svelte";
  import { beginFind, focusTerminal, release, show, refreshAccess, retryTerminal } from "./termPool";
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
    /** Where a routed session runs ("In the cloud", "On another computer",
     *  with "· reconnecting" while unreachable or "· asleep" once its socket
     *  heard so); null for a session here. */
    placement?: string | null;
    /** The row facts that decide whether its owner can be reached (where it
     *  runs, and whether the daemon reaches it). A change retries the socket
     *  at once — a socket waiting on a sleeping owner dials instead of
     *  waiting for a keystroke. Never derived from the socket's own state,
     *  which would loop. */
    reach?: string;
  }

  let { sessionId, focused, fontSize = undefined, placement = null, reach = "" }: Props = $props();

  const watching = $derived(isWatching(sessionId));
  /** Watch/control only means something for a project viewed from another
   *  device: an ordinary local terminal never grows this strip. */
  const showAccess = $derived(placement !== null || isBrowserGateway());
  /** Typing the daemon refused, said here instead of in the console. A
   *  routed terminal's refusal comes from the machine its row points at, so
   *  "running elsewhere" there is a route about to change: the text names no
   *  machine rather than a wrong one. */
  const refusal = $derived(refusalFor(sessionId, { where: placement !== null ? null : undefined, watching }));
  let lastReach: { id: string; reach: string } | null = null;
  $effect(() => {
    const id = sessionId;
    const now = reach;
    if (lastReach?.id === id && lastReach.reach !== now) retryTerminal(id);
    lastReach = { id, reach: now };
  });
  function toggleAccess(): void { setWatching(sessionId, !watching); refreshAccess(sessionId); }

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
    if (focused && !findOpen) focusTerminal(sessionId);
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
    {#if placement !== null}<span class="where">{placement}</span>{/if}
    {#if watching}<span>Just watching</span>{/if}
    <button type="button" aria-pressed={!watching} onclick={toggleAccess}>{watching ? "Take control" : "Just watch"}</button>
  </div>
{/if}
<div class="term-view" class:watching class:with-access={showAccess} use:findTarget={find}>
  {#if findOpen}
    <FindBar bind:this={bar} {query} {caseSensitive} scope="Find in terminal scrollback"
      status={query === "" ? "" : resultCount === 0 ? "No matches" : resultIndex < 0 ? `${resultCount}+ matches` : `${resultIndex + 1} of ${resultCount}${resultCount >= 1000 ? "+" : ""}`}
      canStep={query !== "" && resultCount > 0}
      onQuery={(value) => { query = value; run(); }} onCase={(value) => { caseSensitive = value; run(); }}
      onStep={run} onClose={() => closeFind()} />
  {/if}
  <div class="term-host" bind:this={host} onpastecapture={onPasteCapture}></div>
</div>
{#if refusal !== null}
  <div class="terminal-refusal" role="status">{refusal}</div>
{/if}

<style>
  .term-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }
  .term-host { position: relative; flex: 1; min-height: 0; }
  /* Only a viewed terminal reserves the strip and may clip a wider owner grid.
     The grid scrolls inside its host, so the find bar above it stays put. */
  .term-view.with-access { inset: 34px 0 0; }
  .term-view.with-access .term-host { overflow: auto; }
  .terminal-access { position: absolute; inset: 0 0 auto; height: 34px; display: flex; align-items: center; justify-content: flex-end; gap: 10px; padding: 0 10px; color: var(--muted); background: var(--rail-bg); font-size: var(--text-xs); border-bottom: 1px solid var(--edge); }
  .where { margin-right: auto; color: var(--accent); }
  .terminal-access button { min-height: 28px; padding: 3px 9px; border: 1px solid var(--edge); border-radius: 5px; color: var(--fg); background: var(--bg); cursor: pointer; }
  .terminal-access button:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 1px; }
  /* A refusal floats over the grid's bottom edge; it never takes grid rows. */
  .terminal-refusal { position: absolute; left: 50%; bottom: 12px; transform: translateX(-50%); max-width: calc(100% - 32px); padding: 6px 12px; border: 1px solid var(--edge); border-radius: 6px; background: var(--rail-bg); color: var(--fg); font-size: var(--text-xs); pointer-events: none; }
  @media (max-width: 700px) { .terminal-access { height: 44px; } .terminal-access button { min-height: 36px; } .term-view.with-access { top: 44px; } }
</style>
