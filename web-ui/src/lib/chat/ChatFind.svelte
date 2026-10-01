<script lang="ts">
  import { tick } from "svelte";
  import { findTarget } from "../shared/find";
  import { pageVisible } from "../shared/visibility";
  import FindBar from "../shared/FindBar.svelte";
  import { domMatches, matchPainter } from "../shared/domFind";
  import { findMessages, MESSAGE_MATCH_LIMIT, type MessageMatch } from "./chatFind";
  import type { ChatBlock } from "./store.svelte";
  let { target, blocks, revision, visible, reveal, trimmed }: {
    target: HTMLElement | null;
    blocks: () => readonly ChatBlock[];
    revision: number;
    visible: boolean;
    trimmed: boolean;
    reveal: (uid: number) => Promise<HTMLElement | null>;
  } = $props();
  let open = $state(false);
  let query = $state("");
  let caseSensitive = $state(false);
  let matches = $state.raw<MessageMatch[]>([]);
  let current = $state(0);
  let bar = $state<FindBar>();
  let painter: ReturnType<typeof matchPainter> | null = null;
  let row: HTMLElement | null = null;
  let generation = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  function clearMark() { row?.classList.remove("find-current-message"); row = null; painter?.paint([], 0); }
  async function show() {
    const gen = ++generation;
    clearMark();
    const match = matches[current];
    if (!match) return;
    const el = await reveal(match.uid);
    if (gen !== generation || !open || !visible || el === null) return;
    row = el;
    row.classList.add("find-current-message");
    painter?.paint(domMatches(row, query, caseSensitive).ranges, -1);
  }
  function search(navigate = true, reset = false) {
    const uid = reset ? undefined : matches[current]?.uid;
    matches = findMessages(blocks(), query, caseSensitive);
    const preserved = matches.findIndex((m) => m.uid === uid);
    current = preserved < 0 ? 0 : preserved;
    if (navigate) void show();
    else if (row && matches[current]?.uid === uid) painter?.paint(domMatches(row, query, caseSensitive).ranges, -1);
    else clearMark();
  }
  function step(direction: 1 | -1) {
    if (!matches.length) return;
    current = (current + direction + matches.length) % matches.length;
    void show();
  }
  function close(restore = true) {
    generation++;
    open = false;
    clearTimeout(timer);
    timer = undefined;
    clearMark(); painter?.destroy(); painter = null;
    if (restore) target?.querySelector<HTMLElement>(".transcript")?.focus();
  }
  $effect(() => {
    const root = target;
    if (!root) return;
    const registration = findTarget(root, (command) => {
      if (command === "open") {
        const selection = document.getSelection();
        if (selection?.anchorNode && root.querySelector(".transcript")?.contains(selection.anchorNode)) {
          const selected = selection.toString();
          if (selected && selected.length <= 512 && !selected.includes("\n")) query = selected;
        }
        open = true;
        painter ??= matchPainter();
        void tick().then(() => { if (open && visible) { bar?.focus(); search(); } });
        return true;
      }
      if (!open) return false;
      if (command === "close") close(); else step(command === "previous" ? -1 : 1);
      return true;
    });
    return () => { registration.destroy(); close(false); };
  });
  $effect(() => {
    if (!open || !visible || !$pageVisible) {
      clearTimeout(timer);
      timer = undefined;
      return;
    }
    void revision;
    // Throttle instead of debouncing: a continuous stream must still update results.
    // close() owns teardown; hiding the document cancels the pending refresh above.
    if (timer === undefined) timer = setTimeout(() => { timer = undefined; search(false); }, 180);
  });
  $effect(() => { if (!visible) close(false); });
</script>

{#if open}
  <FindBar bind:this={bar} {query} {caseSensitive} scope="Find in conversation messages" canStep={matches.length > 0}
    status={query === "" ? "" : matches.length === 0 ? "No matches" : `${current + 1} of ${matches.length}${matches.length === MESSAGE_MATCH_LIMIT ? "+" : ""}`}
    onQuery={(value) => { query = value; search(true, true); }}
    onCase={(value) => { caseSensitive = value; search(true, true); }} onStep={step} onClose={() => close()} />
  <div class="find-detail">
    {#if matches[current]}<div class="excerpt">{matches[current].excerpt}</div>{/if}
    <span>Matches are messages. Tool output is excluded.{trimmed ? " Earlier history has been trimmed." : ""}</span>
  </div>
{/if}
<style>
  .find-detail { flex: none; padding: 4px 12px 6px; border-bottom: 1px solid var(--edge); color: var(--fg-dim); font-size: 10px; }
  .excerpt { color: var(--fg); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-size: 12px; margin-bottom: 3px; }
</style>
