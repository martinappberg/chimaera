<script lang="ts">
  /**
   * What a session changed, one list with or without git: the files, each
   * with a small edit count. A click shows the file's diff — the git diff
   * when the host says there is one (`onOpenDiff`), otherwise the agent's
   * own edits for that file, in order, opened in place (from
   * `GET /sessions/{id}/edits`: the chat journal, or claude's transcript for
   * a claude terminal). Edit text is agent-written, so it renders as plain
   * text, never HTML. A change a shell command made has no before/after in
   * the agent's records: the muted note at the bottom says so, but only when
   * the session ran commands and no repository shows those changes.
   */
  import { untrack } from "svelte";
  import FileIcon from "../shared/FileIcon.svelte";
  import { workspaceRelative } from "../shared/reference";
  import { fetchSessionEdits, type FileEdit, type SessionEdits } from "./history";

  interface GitMark {
    letter: string;
    color: string;
    label: string;
  }

  interface Props {
    sessionId: string;
    wsId: string;
    wsRoot: string | null;
    /** Files known to be written besides those with edits (the live
     *  `files_touched`, newest first). */
    paths?: string[];
    /** A repository shows this session's changes (hides the shell note). */
    repo?: boolean;
    /** The git mark for a path, when the host has git status. */
    gitMark?: (path: string) => GitMark | null;
    /** Open the git diff for a path; false = there is none, show the edits. */
    onOpenDiff?: (path: string, e: MouseEvent) => boolean;
    onOpenFile?: (path: string, e: MouseEvent) => void;
    /** Re-fetch when this changes (the live session's touched-files count). */
    refreshKey?: number;
    /** False while the hosting tab is hidden: no fetch until it shows. */
    visible?: boolean;
  }

  let {
    sessionId,
    wsId,
    wsRoot,
    paths = [],
    repo = false,
    gitMark,
    onOpenDiff,
    onOpenFile,
    refreshKey = 0,
    visible = true,
  }: Props = $props();

  let edits = $state<SessionEdits | null>(null);
  let failed = $state(false);
  let open = $state(new Set<string>());
  /** The key the current answer was fetched for — a hidden tab fetches on return. */
  let fetchedFor = "";

  $effect(() => {
    const key = `${wsId}:${sessionId}:${refreshKey}`;
    if (!visible || key === fetchedFor) return;
    fetchedFor = key;
    let cancelled = false;
    untrack(() => {
      void fetchSessionEdits(sessionId, wsId).then(
        (e) => {
          if (cancelled) return;
          edits = e;
          failed = false;
        },
        () => {
          if (!cancelled) failed = true;
        },
      );
    });
    return () => {
      cancelled = true;
    };
  });

  /** One row per file: the known paths first (newest first), then any file
   *  only the edit record names. */
  const rows = $derived.by(() => {
    const byPath = new Map<string, FileEdit[]>();
    for (const f of edits?.files ?? []) byPath.set(f.path, f.edits);
    const out: { path: string; edits: FileEdit[] }[] = [];
    const seen = new Set<string>();
    for (const p of paths) {
      if (seen.has(p)) continue;
      seen.add(p);
      out.push({ path: p, edits: byPath.get(p) ?? [] });
    }
    for (const [p, list] of byPath) {
      if (seen.has(p)) continue;
      seen.add(p);
      out.push({ path: p, edits: list });
    }
    return out;
  });

  function click(path: string, list: FileEdit[], e: MouseEvent): void {
    if (onOpenDiff?.(path, e) === true) return;
    if (list.length === 0) {
      onOpenFile?.(path, e);
      return;
    }
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
</script>

<div class="edits">
  {#if rows.length === 0}
    <p class="note">{edits === null && !failed ? "" : "No changes recorded."}</p>
  {:else}
    <div class="files">
      {#each rows as f (f.path)}
        {@const isOpen = open.has(f.path)}
        {@const mark = gitMark?.(f.path) ?? null}
        <div class="file">
          <button class="fhead" aria-expanded={f.edits.length > 0 ? isOpen : undefined} onclick={(e) => click(f.path, f.edits, e)} title={f.path}>
            <FileIcon path={f.path} size={14} />
            <span class="name">{rel(f.path)}</span>
            {#if f.edits.length > 0}
              <span class="n">{f.edits.length} edit{f.edits.length === 1 ? "" : "s"}</span>
            {/if}
            {#if mark !== null}
              <span class="badge" style:color={mark.color} title={mark.label}>{mark.letter}</span>
            {/if}
          </button>
          {#if isOpen}
            <ol class="list">
              {#each f.edits as ed, i (i)}
                <li class="edit">
                  <span class="when">{i + 1}{ed.ts !== undefined ? ` · ${time(ed.ts)}` : ""}{ed.old_text === undefined ? " · whole file" : ""}</span>
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
              {#if onOpenFile !== undefined}
                <li><button class="openf" onclick={(e) => onOpenFile(f.path, e)}>open {rel(f.path)}</button></li>
              {/if}
            </ol>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
  {#if edits !== null && edits.ran_commands && !repo}
    <p class="limit">Changes made by shell commands aren't captured here.</p>
  {/if}
</div>

<style>
  .edits {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .note,
  .limit {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .note:empty {
    display: none;
  }
  .limit {
    font-size: var(--text-xs);
    padding: 0 8px;
  }
  .files {
    display: flex;
    flex-direction: column;
  }
  .fhead {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 8px;
    border: none;
    border-radius: 5px;
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    text-align: left;
    cursor: pointer;
    transition: background-color 0.12s ease;
  }
  .fhead:hover {
    background: var(--row-hover);
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
  .badge {
    flex: none;
    width: 1.2em;
    text-align: center;
    font-family: var(--mono, monospace);
    font-weight: 600;
  }
  .list {
    list-style: none;
    margin: 2px 0 8px 30px;
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
  .openf {
    border: none;
    background: none;
    padding: 0;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
  }
  .openf:hover {
    color: var(--fg);
  }
</style>
