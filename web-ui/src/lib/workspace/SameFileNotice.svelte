<script lang="ts">
  /**
   * "qc.py also edited by 'fix normalization' · 3 min ago" — this live
   * session and another live one in the workspace wrote the same file, the
   * other one recently or in its running turn (`sameFile.svelte.ts`; the
   * note ages out on its own). Small, and warn-toned only while it is news
   * (the writer's running turn, or the first minutes after the write): then
   * it fades to the quiet tone until it ages out. A click opens the other
   * session. Renders nothing without an overlap, so a host can mount it
   * unconditionally. Nothing locks.
   */
  import { pageVisible } from "../shared/visibility";
  import { sameFileLoud } from "./history";
  import { sameFile } from "./sameFile.svelte";

  interface Props {
    sessionId: string;
    /** Pause the minute ticker while the host is hidden. */
    visible?: boolean;
  }

  let { sessionId, visible = true }: Props = $props();

  const notes = $derived(sameFile.notesFor(sessionId));
  const first = $derived(notes[0] ?? null);

  // "3 min ago" moves while someone is looking.
  let now = $state(Date.now());
  $effect(() => {
    if (first === null || !visible || !$pageVisible) return;
    now = Date.now();
    const t = setInterval(() => (now = Date.now()), 60_000);
    return () => clearInterval(t);
  });

  const loud = $derived(first !== null && sameFileLoud(first, now));

  function base(path: string): string {
    return path.split("/").pop() || path;
  }

  function ago(at: number | null): string {
    if (at === null) return "";
    const mins = Math.max(0, Math.floor((now - at) / 60_000));
    if (mins < 1) return " · just now";
    if (mins < 60) return ` · ${mins} min ago`;
    return ` · ${Math.floor(mins / 60)} h ago`;
  }

  const title = $derived(
    notes
      .map((n) => `${n.path} also edited by “${sameFile.nameOf(n.other)}” — open that session`)
      .join("\n"),
  );
</script>

{#if first !== null}
  <button class="same-file" class:aged={!loud} {title} onclick={() => sameFile.open(first.other)}>
    {base(first.path)} also edited by ‘{sameFile.nameOf(first.other)}’{ago(first.at)}{#if notes.length > 1}<span
        class="more">+{notes.length - 1}</span
      >{/if}
  </button>
{/if}

<style>
  .same-file {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    min-width: 0;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--warn);
    text-align: left;
    cursor: pointer;
    transition: color 1.2s ease;
  }
  /* No longer news: still there to open, no longer asking to be read. */
  .same-file.aged {
    color: var(--muted);
  }
  @media (prefers-reduced-motion: reduce) {
    .same-file {
      transition: none;
    }
  }
  .same-file:hover {
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .more {
    margin-left: 5px;
    opacity: 0.8;
  }
</style>
