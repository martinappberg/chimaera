<script lang="ts">
  /**
   * Lazy directory tree for the rail's FILES section. Each expanded dir is
   * one /fs/list call; listings are cached per path and refreshed every time
   * the dir is re-expanded. The tree renders flat rows (indent by depth) —
   * no recursive components, trivial scrolling.
   */
  import { untrack } from "svelte";
  import { fsList, type FsEntry } from "./files";

  interface Props {
    /** Workspace root on the daemon's filesystem. */
    root: string;
    /** Open a file surface in the layout. */
    onOpen(path: string): void;
    /** Begin a pointer drag of a file entry (same grammar as rail rows and
     *  pane tabs; a sub-threshold release becomes a plain open). */
    onDragStart(e: PointerEvent, path: string): void;
    /** The focused pane's active file, for the subtle current marker. */
    activePath: string | null;
  }

  let { root, onOpen, onDragStart, activePath }: Props = $props();

  let expanded = $state<Set<string>>(new Set());
  let listings = $state<Map<string, FsEntry[]>>(new Map());
  let loading = $state<Set<string>>(new Set());
  let rootError = $state<string | null>(null);

  interface Row {
    entry: FsEntry;
    depth: number;
  }

  const rows = $derived.by(() => {
    const out: Row[] = [];
    const walk = (dir: string, depth: number): void => {
      const entries = listings.get(dir);
      if (entries === undefined) return;
      for (const e of entries) {
        out.push({ entry: e, depth });
        if (e.kind === "dir" && expanded.has(e.path)) walk(e.path, depth + 1);
      }
    };
    walk(root, 0);
    return out;
  });

  // Load (or reload) the root whenever the workspace changes. The body
  // writes the state it also reads (via load), so it must not track it —
  // only `root` is a dependency.
  $effect(() => {
    const r = root;
    untrack(() => {
      expanded = new Set();
      listings = new Map();
      rootError = null;
      void load(r);
    });
  });

  async function load(dir: string): Promise<void> {
    loading = new Set(loading).add(dir);
    try {
      const listing = await fsList(dir);
      const next = new Map(listings);
      next.set(dir, listing.entries);
      listings = next;
      if (dir === root) rootError = null;
    } catch (e) {
      if (dir === root) {
        rootError = e instanceof Error ? e.message : "failed to list files";
      } else {
        // Collapse a dir that failed to list (deleted, permission denied).
        const n = new Set(expanded);
        n.delete(dir);
        expanded = n;
      }
    } finally {
      const n = new Set(loading);
      n.delete(dir);
      loading = n;
    }
  }

  function toggle(dir: string): void {
    const next = new Set(expanded);
    if (next.has(dir)) {
      next.delete(dir);
      expanded = next;
    } else {
      next.add(dir);
      expanded = next;
      void load(dir); // fresh listing on every expand
    }
  }

  function onRowKey(e: KeyboardEvent, entry: FsEntry): void {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      if (entry.kind === "dir") toggle(entry.path);
      else onOpen(entry.path);
    }
  }
</script>

<div class="tree" role="tree">
  {#if rootError !== null}
    <div class="tree-error">{rootError}</div>
  {:else if listings.get(root)?.length === 0}
    <div class="tree-empty">empty</div>
  {/if}
  {#each rows as { entry, depth } (entry.path)}
    <div
      class="node"
      class:active={entry.path === activePath}
      role="treeitem"
      aria-expanded={entry.kind === "dir" ? expanded.has(entry.path) : undefined}
      aria-selected={entry.path === activePath}
      tabindex="0"
      title={entry.path}
      style:padding-left={`${8 + depth * 13}px`}
      onclick={() => {
        // Files open via the drag's sub-threshold click path (below), so a
        // completed drag never ALSO opens the file in the focused pane.
        if (entry.kind === "dir") toggle(entry.path);
      }}
      onpointerdowncapture={(e) => {
        if (entry.kind === "file") onDragStart(e, entry.path);
      }}
      onkeydown={(e) => onRowKey(e, entry)}
    >
      {#if entry.kind === "dir"}
        <svg
          class="chev"
          class:open={expanded.has(entry.path)}
          class:busy={loading.has(entry.path)}
          viewBox="0 0 16 16"
          width="9"
          height="9"
          aria-hidden="true"
        >
          <path
            d="M6 4l4 4-4 4"
            fill="none"
            stroke="currentColor"
            stroke-width="1.6"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
      {:else}
        <span class="chev-gap"></span>
      {/if}
      <span class="node-name" class:dir={entry.kind === "dir"}>{entry.name}</span>
    </div>
  {/each}
</div>

<style>
  .tree {
    display: flex;
    flex-direction: column;
    padding: 2px 0.45rem 0.5rem;
  }

  .tree-error,
  .tree-empty {
    padding: 0.3rem 0.45rem;
    font-size: 0.72rem;
    color: var(--muted);
    line-height: 1.4;
  }

  .node {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    height: 22px;
    padding-right: 0.45rem;
    border-radius: 4px;
    cursor: pointer;
    user-select: none;
    min-width: 0;
    outline: none;
  }

  .node:hover {
    background: var(--row-hover);
  }

  .node:focus-visible {
    background: var(--row-hover);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 45%, transparent);
  }

  .node.active {
    background: var(--row-active);
  }

  .chev {
    flex: none;
    color: var(--muted);
    opacity: 0.8;
    transition: transform 0.1s ease;
  }

  .chev.open {
    transform: rotate(90deg);
  }

  .chev.busy {
    opacity: 0.4;
  }

  .chev-gap {
    flex: none;
    width: 9px;
  }

  .node-name {
    font-family: var(--mono);
    font-size: 0.74rem;
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }

  .node-name.dir {
    color: var(--fg);
  }

  .node.active .node-name {
    color: var(--fg);
  }
</style>
