<script lang="ts">
  /**
   * Choose a folder on a cluster to add as a workspace — the only file
   * browsing outside a workspace window (docs/hpc-portal-plan.md §4.4).
   * Folders only; each listing is one short, read-only `chimaera browse` on
   * the login node (nothing keeps running there). You step into the folder
   * you want and add it, like a native "Choose" dialog.
   */
  import { onMount } from "svelte";
  import {
    clusterAddWorkspace,
    clusterListDir,
    type ClusterDirListing,
    type ClusterWorkspace,
  } from "../net/native";
  import { modalFocus } from "../shared/modalFocus";
  import { childPath } from "./cluster";

  interface Props {
    alias: string;
    /** Folders that hold existing workspaces — the quick links. */
    places: string[];
    /** A job is running: adding can open the workspace straight away. */
    canOpen: boolean;
    onAdded: (ws: ClusterWorkspace, openAfter: boolean) => void;
    onClose: () => void;
  }

  let { alias, places, canOpen, onAdded, onClose }: Props = $props();

  let listing = $state<ClusterDirListing | null>(null);
  let loading = $state(true);
  /** A listing taking long is almost always the first-ever copy of chimaera. */
  let slow = $state(false);
  let error = $state<string | null>(null);
  let goTo = $state("");
  let name = $state("");
  let busy = $state(false);
  let addError = $state<string | null>(null);
  let seq = 0;

  const here = $derived(listing?.path ?? "");
  const hereName = $derived(here.split("/").filter((s) => s !== "").pop() ?? "/");
  /** The folder we're in is already a workspace. */
  const hereIsWorkspace = $derived(listing?.workspace !== undefined && listing?.workspace !== null);
  /** The breadcrumb: "/", "home", "u", "proj" with each one's full path. */
  const crumbs = $derived.by(() => {
    const parts = here.split("/").filter((s) => s !== "");
    const out: { label: string; path: string }[] = [{ label: "/", path: "/" }];
    let acc = "";
    for (const p of parts) {
      acc += `/${p}`;
      out.push({ label: p, path: acc });
    }
    return out;
  });
  const quick = $derived.by(() => {
    const seen = new Set<string>();
    const out: { label: string; path: string }[] = [{ label: "Home", path: "~" }];
    for (const p of places) {
      if (seen.has(p) || seen.size >= 4) continue;
      seen.add(p);
      out.push({ label: p.split("/").filter((s) => s !== "").pop() ?? p, path: p });
    }
    return out;
  });

  async function go(path: string): Promise<void> {
    const target = path.trim();
    if (target === "") return;
    const mine = ++seq;
    loading = true;
    slow = false;
    error = null;
    addError = null;
    const slowTimer = setTimeout(() => {
      if (mine === seq) slow = true;
    }, 3000);
    try {
      const next = await clusterListDir(alias, target);
      if (mine !== seq) return;
      listing = next;
      goTo = "";
      name = "";
    } catch (e) {
      if (mine !== seq) return;
      error = e instanceof Error ? e.message : String(e);
    } finally {
      clearTimeout(slowTimer);
      if (mine === seq) {
        loading = false;
        slow = false;
      }
    }
  }

  onMount(() => {
    void go(places[0] ?? "~");
  });

  function enter(folder: { name: string }): void {
    void go(childPath(here, folder.name));
  }

  async function add(openAfter: boolean): Promise<void> {
    if (busy || listing === null) return;
    busy = true;
    addError = null;
    try {
      const ws = await clusterAddWorkspace(alias, listing.path, name.trim());
      onAdded(ws, openAfter);
    } catch (e) {
      addError = e instanceof Error ? e.message : String(e);
      busy = false;
    }
  }

  function onKeydown(e: KeyboardEvent): void {
    if (e.key === "Escape" && !busy) {
      e.preventDefault();
      onClose();
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

<div class="overlay">
  <button class="scrim" aria-label="Close" tabindex="-1" onclick={() => !busy && onClose()}></button>
  <div
    class="panel"
    role="dialog"
    aria-modal="true"
    aria-label={`Choose a folder on ${alias}`}
    tabindex="-1"
    use:modalFocus
  >
    <div class="head">
      <div class="title">Choose a folder on {alias}</div>
      <div class="sub">It becomes a workspace: open it in a job, and its chats stay with it.</div>
      <div class="quick">
        {#each quick as q (q.path)}
          <button type="button" class="chip" title={q.path} onclick={() => void go(q.path)}>{q.label}</button>
        {/each}
      </div>
    </div>

    <div class="crumbs" aria-label="Where you are">
      {#if listing !== null}
        {#each crumbs as c, i (c.path)}
          {#if i > 1}<span class="crumb-sep" aria-hidden="true">/</span>{/if}
          <button
            type="button"
            class="crumb"
            class:here={i === crumbs.length - 1}
            onclick={() => void go(c.path)}>{c.label}</button
          >
        {/each}
      {/if}
    </div>

    <div class="list" class:stale={loading && listing !== null}>
      {#if listing === null && loading}
        <div class="state" role="status">
          {slow ? `First time on ${alias}: copying Chimaera there (one time)…` : "Reading…"}
        </div>
      {:else if error !== null && listing === null}
        <div class="state err">{error}</div>
      {:else if listing !== null}
        {#if error !== null}<div class="state err">{error}</div>{/if}
        {#each listing.folders as f, i (`${i}:${f.name}`)}
          <button type="button" class="row" onclick={() => enter(f)} title={childPath(here, f.name)}>
            <svg class="ico" viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
              <path
                d="M2 4.5c0-.6.4-1 1-1h3.2l1.3 1.4H13c.6 0 1 .4 1 1v5.6c0 .6-.4 1-1 1H3c-.6 0-1-.4-1-1z"
                fill="none"
                stroke="currentColor"
                stroke-width="1.2"
                stroke-linejoin="round"
              />
            </svg>
            <span class="fname">{f.name}</span>
            {#if f.workspace}<span class="mark ws">added</span>{/if}
            {#if f.git}<span class="mark">git</span>{/if}
          </button>
        {:else}
          <div class="state">No folders in here.</div>
        {/each}
        {#if listing.truncated}<div class="state">Only the first 2000 folders are shown.</div>{/if}
      {/if}
    </div>

    <form
      class="goto"
      onsubmit={(e) => {
        e.preventDefault();
        void go(goTo);
      }}
    >
      <input
        class="in mono"
        bind:value={goTo}
        placeholder="Go to a path — ~/x or $SCRATCH/x"
        spellcheck="false"
        autocomplete="off"
        aria-label="Go to a path on the cluster"
      />
      <button type="submit" class="quiet" disabled={goTo.trim() === "" || loading}>Go</button>
    </form>

    <div class="foot">
      {#if listing !== null}
        <div class="pick">
          <span class="pick-lab">Workspace</span>
          <span class="pick-path" title={here}>{here}</span>
          <input
            class="in name"
            bind:value={name}
            placeholder={hereName}
            spellcheck="false"
            autocomplete="off"
            aria-label="Name (optional)"
          />
        </div>
      {/if}
      {#if addError !== null}<div class="err-line">{addError}</div>{/if}
      {#if hereIsWorkspace}<div class="note-line">This folder is already a workspace.</div>{/if}
      <div class="acts">
        <button type="button" class="quiet" disabled={busy} onclick={onClose}>Cancel</button>
        {#if canOpen}
          <button
            type="button"
            class="quiet"
            disabled={busy || listing === null || hereIsWorkspace}
            onclick={() => void add(false)}>Add</button
          >
          <button
            type="button"
            class="cta"
            disabled={busy || listing === null || hereIsWorkspace}
            onclick={() => void add(true)}>{busy ? "Adding…" : "Add and open"}</button
          >
        {:else}
          <button
            type="button"
            class="cta"
            disabled={busy || listing === null || hereIsWorkspace}
            onclick={() => void add(false)}>{busy ? "Adding…" : `Add ${hereName}`}</button
          >
        {/if}
      </div>
    </div>
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    z-index: 100;
    animation: fade 0.1s ease-out;
  }

  @keyframes fade {
    from {
      opacity: 0;
    }
  }

  .scrim {
    position: absolute;
    inset: 0;
    appearance: none;
    border: none;
    padding: 0;
    background: var(--scrim);
    cursor: default;
  }

  .panel {
    position: relative;
    width: min(560px, calc(100vw - 2rem));
    height: min(560px, 80vh);
    margin: 9vh auto 0;
    display: flex;
    flex-direction: column;
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 9px;
    box-shadow: 0 12px 36px rgba(0, 0, 0, 0.22);
    overflow: hidden;
  }

  .head {
    flex: none;
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 16px 18px 8px;
  }

  .title {
    font-size: var(--text-lg);
    font-weight: 600;
    color: var(--fg);
  }

  .sub {
    font-size: var(--text-sm);
    color: var(--muted);
  }

  .quick {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 8px;
  }

  .chip {
    appearance: none;
    border: 1px solid var(--edge);
    background: none;
    color: var(--fg);
    font: inherit;
    font-family: var(--mono);
    font-size: var(--text-xs);
    padding: 3px 9px;
    border-radius: 999px;
    cursor: pointer;
  }

  .chip:hover {
    border-color: color-mix(in srgb, var(--accent) 60%, var(--edge));
  }

  .crumbs {
    flex: none;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 2px;
    padding: 4px 16px 8px;
    min-height: 28px;
    border-bottom: 1px solid var(--edge);
  }

  .crumb {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-family: var(--mono);
    font-size: var(--text-sm);
    color: var(--muted);
    padding: 2px 4px;
    border-radius: 4px;
    cursor: pointer;
  }

  .crumb:hover {
    color: var(--fg);
    background: var(--row-hover);
  }

  .crumb.here {
    color: var(--fg);
  }

  .crumb-sep {
    color: var(--muted);
    opacity: 0.6;
    font-family: var(--mono);
    font-size: var(--text-sm);
  }

  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 4px 6px;
    transition: opacity 0.12s ease;
  }

  .list.stale {
    opacity: 0.55;
  }

  .row {
    appearance: none;
    width: 100%;
    display: flex;
    align-items: center;
    gap: 9px;
    border: none;
    background: none;
    color: var(--fg);
    font: inherit;
    text-align: left;
    padding: 6px 10px;
    border-radius: 5px;
    cursor: pointer;
  }

  .row:hover {
    background: var(--row-hover);
  }

  .ico {
    flex: none;
    color: var(--muted);
  }

  .fname {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono);
    font-size: var(--text-sm);
  }

  .mark {
    flex: none;
    font-size: var(--text-xs);
    color: var(--muted);
    border: 1px solid var(--edge);
    border-radius: 999px;
    padding: 0 7px;
  }

  .mark.ws {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 40%, var(--edge));
  }

  .state {
    padding: 10px;
    font-size: var(--text-sm);
    color: var(--muted);
  }

  .state.err {
    color: var(--err);
    white-space: pre-wrap;
  }

  .goto {
    flex: none;
    display: flex;
    gap: 8px;
    padding: 8px 16px;
    border-top: 1px solid var(--edge);
  }

  .in {
    min-width: 0;
    border: 1px solid var(--edge);
    border-radius: 6px;
    background: var(--overlay-bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    padding: 5px 9px;
    outline: none;
  }

  .in.mono {
    flex: 1;
    font-family: var(--mono);
  }

  .in:focus {
    border-color: var(--focus-ring);
  }

  .in::placeholder {
    color: var(--muted);
    opacity: 0.7;
  }

  .foot {
    flex: none;
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px 16px 14px;
    border-top: 1px solid var(--edge);
    background: color-mix(in srgb, var(--fg) 2%, transparent);
  }

  .pick {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
  }

  .pick-lab {
    flex: none;
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .pick-path {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    direction: rtl;
    text-align: left;
    font-family: var(--mono);
    font-size: var(--text-sm);
    color: var(--fg);
  }

  .in.name {
    flex: none;
    width: 150px;
  }

  .err-line {
    font-size: var(--text-xs);
    color: var(--err);
    white-space: pre-wrap;
  }

  .note-line {
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .acts {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }

  .quiet {
    appearance: none;
    border: 1px solid var(--edge);
    background: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-sm);
    padding: 6px 12px;
    border-radius: 6px;
    cursor: pointer;
  }

  .quiet:hover:enabled {
    color: var(--fg);
  }

  .cta {
    appearance: none;
    border: 1px solid color-mix(in srgb, var(--accent) 55%, var(--edge));
    background: color-mix(in srgb, var(--accent) 14%, transparent);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    padding: 6px 14px;
    border-radius: 6px;
    cursor: pointer;
  }

  .cta:hover:enabled {
    background: color-mix(in srgb, var(--accent) 22%, transparent);
  }

  .quiet:disabled,
  .cta:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
