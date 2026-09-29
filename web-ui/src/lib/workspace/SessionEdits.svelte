<script lang="ts">
  /**
   * What a session changed without git: the agent's own edits, per file, in
   * the order it made them (`GET /sessions/{id}/edits` — from the chat
   * journal, or claude's transcript for a claude terminal). Each edit opens
   * to its before/after text, rendered as plain text like the chat's tool
   * cards (agent-written, so never HTML). A change a shell command made has
   * no before/after in the agent's records; the footer says so, quietly.
   */
  import { untrack } from "svelte";
  import { ApiError } from "../net/api";
  import FileIcon from "../shared/FileIcon.svelte";
  import { workspaceRelative } from "../shared/reference";
  import { fetchSessionEdits, type SessionEdits } from "./history";

  interface Props {
    sessionId: string;
    wsId: string;
    wsRoot: string | null;
    /** Re-fetch when this changes (the live session's touched-files count). */
    refreshKey?: number;
    /** False while the hosting tab is hidden: no fetch until it shows. */
    visible?: boolean;
    onOpenFile?: (path: string, e: MouseEvent) => void;
  }

  let { sessionId, wsId, wsRoot, refreshKey = 0, visible = true, onOpenFile }: Props = $props();

  let edits = $state<SessionEdits | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(false);
  /** Files the reader opened, by path. */
  let open = $state(new Set<string>());
  /** The key the current answer was fetched for — a hidden tab fetches on return. */
  let fetchedFor = "";

  $effect(() => {
    const key = `${wsId}:${sessionId}:${refreshKey}`;
    if (!visible || key === fetchedFor) return;
    fetchedFor = key;
    let cancelled = false;
    untrack(() => (loading = true));
    void fetchSessionEdits(sessionId, wsId).then(
      (e) => {
        if (cancelled) return;
        edits = e;
        error = null;
        loading = false;
      },
      (e: unknown) => {
        if (cancelled) return;
        loading = false;
        error =
          e instanceof ApiError && e.status === 404
            ? "no record of this session's edits"
            : e instanceof Error
              ? e.message
              : String(e);
      },
    );
    return () => {
      cancelled = true;
    };
  });

  function toggle(path: string): void {
    const next = new Set(open);
    if (next.has(path)) next.delete(path);
    else next.add(path);
    open = next;
  }

  function rel(path: string): string {
    return wsRoot !== null ? workspaceRelative(path, wsRoot) : path;
  }

  function time(ts: number | undefined): string {
    if (ts === undefined) return "";
    return new Date(ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  }

  /** Why nothing is listed, in plain words, per agent. */
  const noSource = $derived.by(() => {
    if (edits === null || edits.source !== null) return null;
    if (edits.agent === "codex") {
      return "A codex terminal keeps its changes in codex's own store, which chimaera doesn't read.";
    }
    if (edits.agent === "claude") return "Claude's transcript for this session is gone, so its edits are too.";
    return `chimaera has no record of ${edits.agent}'s edits.`;
  });
</script>

<section class="edits" aria-label="edits by this agent">
  <div class="shead">
    <span class="lbl">Edits by this agent</span>
    {#if edits !== null && edits.edits > 0}
      <span class="count">{edits.edits} in {edits.files.length} file{edits.files.length === 1 ? "" : "s"}</span>
    {/if}
  </div>
  {#if error !== null}
    <p class="note err">{error}</p>
  {:else if edits === null}
    <p class="note">{loading ? "loading…" : ""}</p>
  {:else if noSource !== null}
    <p class="note">{noSource}</p>
  {:else if edits.files.length === 0}
    <p class="note">No edits recorded yet.</p>
  {:else}
    <div class="files">
      {#each edits.files as f (f.path)}
        {@const isOpen = open.has(f.path)}
        <div class="file">
          <div class="frow">
            <button class="fhead" aria-expanded={isOpen} onclick={() => toggle(f.path)} title={f.path}>
              <svg class="chev" class:open={isOpen} viewBox="0 0 16 16" width="9" height="9" aria-hidden="true">
                <path d="M6 4l4 4-4 4" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
              </svg>
              <FileIcon path={f.path} size={13} />
              <span class="name">{rel(f.path)}</span>
              <span class="n">{f.edits.length} edit{f.edits.length === 1 ? "" : "s"}</span>
            </button>
            {#if onOpenFile !== undefined}
              <button class="openf" title="open {rel(f.path)}" onclick={(e) => onOpenFile(f.path, e)}>open</button>
            {/if}
          </div>
          {#if isOpen}
            <ol class="list">
              {#each f.edits as ed, i (i)}
                <li class="edit">
                  <span class="when">{i + 1}{ed.ts !== undefined ? ` · ${time(ed.ts)}` : ""}{ed.old_text === undefined ? " · written whole" : ""}</span>
                  {#if ed.old_text !== undefined && ed.old_text !== ""}
                    <pre class="old">{ed.old_text}</pre>
                  {/if}
                  {#if ed.new_text !== ""}
                    <pre class="new">{ed.new_text}</pre>
                  {/if}
                  {#if ed.truncated}
                    <span class="trunc">cut short — open the file for the whole change</span>
                  {/if}
                </li>
              {/each}
            </ol>
          {/if}
        </div>
      {/each}
    </div>
    {#if edits.truncated}
      <p class="note">Only the first {edits.edits} edits are shown.</p>
    {/if}
  {/if}
  <p class="limit">Changes made by shell commands — a script, <code>sed</code>, a pipeline's outputs — have no before and after here.</p>
</section>

<style>
  .edits {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .shead {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .lbl {
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
    font-weight: 600;
  }
  .count {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .note,
  .limit {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--muted);
    line-height: 1.5;
  }
  .limit {
    font-size: var(--text-xs);
    opacity: 0.85;
  }
  .limit code {
    font-family: var(--mono, monospace);
  }
  .err {
    color: var(--err);
  }
  .files {
    display: flex;
    flex-direction: column;
  }
  .frow {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .fhead {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 4px 6px;
    border: none;
    border-radius: 5px;
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    text-align: left;
    cursor: pointer;
  }
  .fhead:hover {
    background: var(--row-hover);
  }
  .chev {
    flex: none;
    color: var(--muted);
    transition: transform 0.12s ease;
  }
  .chev.open {
    transform: rotate(90deg);
  }
  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono, monospace);
  }
  .n {
    flex: none;
    color: var(--muted);
    font-size: var(--text-xs);
  }
  .openf {
    flex: none;
    border: none;
    background: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    padding: 2px 6px;
    border-radius: 4px;
    cursor: pointer;
  }
  .openf:hover {
    color: var(--accent);
  }
  .list {
    list-style: none;
    margin: 2px 0 8px 22px;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .edit {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .when {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  pre {
    margin: 0;
    max-height: 280px;
    overflow: auto;
    font-size: var(--text-sm);
    font-family: var(--mono, monospace);
    white-space: pre-wrap;
    word-break: break-word;
    padding: 3px 6px;
    border-radius: 4px;
  }
  .old {
    background: color-mix(in srgb, var(--err) 12%, transparent);
  }
  .new {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
  }
  .trunc {
    font-size: var(--text-xs);
    color: var(--muted);
  }
</style>
