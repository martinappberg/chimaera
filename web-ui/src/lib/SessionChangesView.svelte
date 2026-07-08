<script lang="ts">
  import { gitStatus, type DiffMode, type GitEntry } from "./git";
  import { decoFor } from "./gitDeco";
  import { workspaceRelative } from "./reference";
  import { displayName, type Session } from "./sessions";
  import type { LayoutCtrl } from "./dnd";
  import FileIcon from "./FileIcon.svelte";

  /**
   * Session-scoped changes review: the files THIS agent touched (its
   * hook-derived files_touched list), cross-referenced with the workspace's
   * live git status. Each row opens the same side-by-side diff the Source
   * Control panel uses (ctrl.openDiffFrom), so this is purely a session-scoped
   * entry point onto main's git service — no duplicated diff plumbing, and it
   * inherits the resolved git.path. Files with no current git change still
   * open in a viewer.
   */
  interface Props {
    session: Session;
    wsRoot: string | null;
    paneId: string;
    ctrl: LayoutCtrl;
  }

  let { session, wsRoot, paneId, ctrl }: Props = $props();

  const status = $derived($gitStatus);
  /** Absolute path -> its git entry, for the touched files. */
  const byPath = $derived.by(() => {
    const m = new Map<string, GitEntry>();
    for (const e of status?.entries ?? []) m.set(e.path, e);
    return m;
  });
  /** Touched files, newest first (files_touched is oldest-first on the wire). */
  const files = $derived([...(session.files_touched ?? [])].reverse());
  const base = $derived(wsRoot ?? session.cwd_current ?? session.cwd);

  /** The comparison to open for an entry — mirrors the Source Control panel:
   *  a purely-staged change diffs staged, everything else diffs the worktree. */
  function modeFor(e: GitEntry): DiffMode {
    return e.staged && !e.unstaged && !e.untracked && !e.conflicted ? "staged" : "unstaged";
  }
  function rel(path: string): string {
    return base !== null ? workspaceRelative(path, base) : path;
  }
  function open(e: MouseEvent, path: string): void {
    const entry = byPath.get(path);
    if (entry !== undefined) ctrl.openDiffFrom(paneId, path, modeFor(entry), e.metaKey || e.ctrlKey);
    else ctrl.openFileFrom(paneId, path, e.metaKey || e.ctrlKey);
  }
</script>

<div class="changes">
  <header class="head">
    <span class="title">Changes · {displayName(session)}</span>
    <span class="count">{files.length} file{files.length === 1 ? "" : "s"}</span>
  </header>

  {#if status !== null && status.git_ok === false}
    <div class="note">
      Source control needs <b>git ≥ {status.git?.min ?? "2.15"}</b>. Set a newer
      <code>git.path</code> in Settings to see diffs here.
    </div>
  {/if}

  {#if files.length === 0}
    <div class="empty">this agent hasn't changed any files yet</div>
  {:else}
    <div class="list">
      {#each files as path (path)}
        {@const entry = byPath.get(path)}
        {@const deco = entry !== undefined ? decoFor(entry) : null}
        <button class="row" title={path} onclick={(e) => open(e, path)}>
          <span class="glyph"><FileIcon {path} size={14} /></span>
          <span class="name">{rel(path)}</span>
          {#if deco !== null}
            <span class="badge" style:color={deco.color} title={deco.label}>{deco.letter}</span>
          {:else}
            <span class="badge quiet" title="no current git change">·</span>
          {/if}
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .changes {
    height: 100%;
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: var(--bg);
    color: var(--fg);
  }
  .head {
    flex: none;
    display: flex;
    align-items: baseline;
    gap: 10px;
    padding: 8px 14px;
    border-bottom: 1px solid var(--edge);
  }
  .title {
    font-weight: 600;
    font-size: var(--text-md);
  }
  .count {
    color: var(--muted);
    font-size: var(--text-sm);
  }
  .note {
    margin: 10px 12px 0;
    padding: 8px 10px;
    border: 1px solid color-mix(in srgb, var(--warn) 45%, var(--edge));
    border-radius: 6px;
    background: color-mix(in srgb, var(--warn) 8%, transparent);
    color: var(--fg);
    font-size: var(--text-sm);
  }
  .note code {
    font-family: var(--mono, monospace);
    font-size: 0.92em;
  }
  .empty {
    padding: 20px;
    text-align: center;
    color: var(--muted);
    font-size: var(--text-sm);
  }
  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    scrollbar-width: thin;
    padding: 6px 8px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
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
  .row:hover {
    background: var(--row-hover);
  }
  .glyph {
    flex: none;
    display: inline-flex;
    align-items: center;
  }
  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono, monospace);
  }
  .badge {
    flex: none;
    width: 1.2em;
    text-align: center;
    font-family: var(--mono, monospace);
    font-weight: 600;
  }
  .badge.quiet {
    color: var(--muted);
    font-weight: 400;
  }
</style>
