<script lang="ts">
  /**
   * Quick-open palette (Cmd/Ctrl+P): the same overlay language as the folder
   * picker, fuzzy-matching files (server index) and the workspace's sessions
   * in one list. Sessions pin to the top when the query matches their display
   * name. Enter opens in the focused pane; Cmd/Ctrl+Enter opens in a new
   * split; Esc closes. Input stays snappy — cached results render instantly
   * while the server call is debounced 120ms.
   */
  import { onMount } from "svelte";
  import { fsQuickOpen, type QuickOpenEntry } from "../previews/files";
  import { dotState, dotTitle, type Session } from "./sessions";
  import { getSetting } from "../settings/store.svelte";
  import { modalFocus } from "../shared/modalFocus";
  import FileIcon from "../shared/FileIcon.svelte";
  import SessionGlyph from "../shared/SessionGlyph.svelte";

  /** Parent directory of a workspace-relative path ("" for a root file). */
  function dirnameOf(rel: string): string {
    const i = rel.lastIndexOf("/");
    return i > 0 ? rel.slice(0, i) : "";
  }

  interface Props {
    workspaceId: string;
    sessions: Session[];
    /** Display names keyed by session id (naming rule zero). */
    sessionNames: Map<string, string>;
    onOpenFile: (path: string, split: boolean) => void;
    onOpenSession: (id: string, split: boolean) => void;
    onClose: () => void;
  }

  let { workspaceId, sessions, sessionNames, onOpenFile, onOpenSession, onClose }: Props = $props();

  let input = $state("");
  let entries = $state<QuickOpenEntry[]>([]);
  let error = $state<string | null>(null);
  let highlight = $state(0);
  let listEl = $state<HTMLDivElement | null>(null);

  let seq = 0;
  let debounce: ReturnType<typeof setTimeout> | null = null;

  /** Case-insensitive subsequence test (fuzzy), for session names client-side. */
  function subseq(query: string, text: string): boolean {
    if (query === "") return true;
    const q = query.toLowerCase();
    const s = text.toLowerCase();
    let i = 0;
    for (let j = 0; j < s.length && i < q.length; j++) {
      if (s[j] === q[i]) i++;
    }
    return i === q.length;
  }

  function sessionLabel(s: Session): string {
    return sessionNames.get(s.id) ?? s.name;
  }

  // Sessions pin to the top when the query matches their display name (an
  // empty query lists them all). Files follow, in the server's ranked order.
  const matchedSessions = $derived(
    sessions.filter((s) => subseq(input.trim(), sessionLabel(s))),
  );

  type Row =
    | { kind: "session"; session: Session }
    | { kind: "file"; entry: QuickOpenEntry };

  const rows = $derived.by((): Row[] => {
    const out: Row[] = [];
    for (const s of matchedSessions) out.push({ kind: "session", session: s });
    for (const e of entries) out.push({ kind: "file", entry: e });
    return out;
  });

  // Keep the highlight in range as the row set changes.
  $effect(() => {
    if (highlight >= rows.length) highlight = Math.max(rows.length - 1, 0);
  });

  // Debounced fetch: schedule after 120ms; cached entries stay on screen so
  // typing never blanks the list. A seq guard drops out-of-order responses.
  $effect(() => {
    const q = input;
    const ws = workspaceId;
    if (debounce !== null) clearTimeout(debounce);
    debounce = setTimeout(() => {
      const mine = ++seq;
      void fsQuickOpen(ws, q.trim(), getSetting("quickOpen.maxResults"))
        .then((res) => {
          if (mine !== seq) return;
          entries = res;
          error = null;
        })
        .catch((e) => {
          if (mine !== seq) return;
          error = e instanceof Error ? e.message : "quick-open failed";
        });
    }, 120);
    return () => {
      if (debounce !== null) clearTimeout(debounce);
    };
  });

  onMount(() => {
    // Prime results immediately (no debounce) so the palette opens populated.
    const mine = ++seq;
    void fsQuickOpen(workspaceId, "", getSetting("quickOpen.maxResults"))
      .then((res) => {
        if (mine === seq) entries = res;
      })
      .catch((e) => {
        if (mine === seq) error = e instanceof Error ? e.message : "quick-open failed";
      });
  });

  function activate(row: Row | undefined, split: boolean): void {
    if (row === undefined) return;
    if (row.kind === "session") onOpenSession(row.session.id, split);
    else onOpenFile(row.entry.path, split);
    onClose();
  }

  function onKeydown(e: KeyboardEvent): void {
    if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      if (rows.length > 0) highlight = Math.min(highlight + 1, rows.length - 1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      if (rows.length > 0) highlight = Math.max(highlight - 1, 0);
    } else if (e.key === "Enter") {
      e.preventDefault();
      activate(rows[highlight], e.metaKey || e.ctrlKey);
    }
  }

  function focusOnMount(node: HTMLElement): void {
    node.focus();
  }

  // Keep the highlighted row in view.
  $effect(() => {
    const el = listEl?.querySelector(`[data-idx="${highlight}"]`);
    el?.scrollIntoView({ block: "nearest" });
  });
</script>

<div class="overlay">
  <button class="scrim" aria-label="close" tabindex="-1" onclick={onClose}></button>
  <div
    class="panel"
    role="dialog"
    aria-modal="true"
    aria-label="quick open"
    tabindex="-1"
    use:modalFocus
  >
    <input
      class="filter"
      bind:value={input}
      placeholder="open a file or session"
      spellcheck="false"
      autocomplete="off"
      use:focusOnMount
      onkeydown={onKeydown}
    />
    <div class="list" bind:this={listEl}>
      {#if error !== null}
        <div class="error">{error}</div>
      {/if}
      {#if rows.length === 0 && error === null}
        <div class="empty">no matches</div>
      {/if}
      {#each rows as row, i (row.kind === "session" ? `s:${row.session.id}` : `f:${row.entry.path}`)}
        <div
          class="rowwrap"
          role="presentation"
          class:hl={highlight === i}
          data-idx={i}
          onmouseenter={() => (highlight = i)}
        >
          <button
            class="row"
            tabindex="-1"
            title={row.kind === "session" ? sessionLabel(row.session) : row.entry.rel}
            onmousedown={(e) => e.preventDefault()}
            onclick={(e) => activate(row, e.metaKey || e.ctrlKey)}
          >
            {#if row.kind === "session"}
              {@const s = row.session}
              <span class="glyph-slot">
                <!-- Session-type glyph (agent_kind-driven), state-colored. -->
                <SessionGlyph
                  kind={s.kind}
                  agentKind={s.agent_kind}
                  state={dotState(s)}
                  size={13}
                  title={dotTitle(s)}
                />
              </span>
              <span class="name">{sessionLabel(s)}</span>
              <span class="meta">session</span>
            {:else}
              {@const e = row.entry}
              <span class="glyph-slot"><FileIcon path={e.path} size={14} /></span>
              <span class="name">{e.name}</span>
              <span class="meta path">{dirnameOf(e.rel)}</span>
            {/if}
          </button>
        </div>
      {/each}
    </div>
    <div class="foot">
      <span><kbd>↵</kbd> open</span>
      <span><kbd>⌘↵</kbd> split</span>
      <span><kbd>esc</kbd> close</span>
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
    max-height: 60vh;
    margin: 13vh auto 0;
    display: flex;
    flex-direction: column;
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 8px;
    box-shadow: 0 12px 36px rgba(0, 0, 0, 0.22);
    overflow: hidden;
  }

  .filter {
    flex: none;
    border: none;
    outline: none;
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-md);
    padding: 12px 16px 10px;
    border-bottom: 1px solid var(--edge);
  }

  .filter::placeholder {
    color: var(--muted);
    opacity: 0.7;
  }

  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 4px 8px 8px;
    scrollbar-width: thin;
    scrollbar-color: color-mix(in srgb, var(--fg) 22%, transparent) transparent;
  }

  .error {
    padding: 8px;
    font-size: var(--text-sm);
    color: var(--err);
  }

  .empty {
    padding: 10px 8px;
    font-size: var(--text-sm);
    color: var(--muted);
  }

  .rowwrap {
    display: flex;
    align-items: center;
    border-radius: 5px;
    transition: background-color 0.12s ease;
  }

  /* Single highlight: hover MOVES the keyboard highlight (onmouseenter). */
  .rowwrap.hl {
    background: var(--row-active);
  }

  .row {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 9px;
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-md);
    color: var(--fg);
    text-align: left;
    padding: 6px 8px;
    cursor: pointer;
  }

  .glyph-slot {
    flex: none;
    display: flex;
    align-items: center;
    width: 15px;
    justify-content: center;
  }

  /* Session glyphs (and their state palette) live in SessionGlyph. */

  .name {
    flex: none;
    max-width: 55%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono);
    font-size: var(--text-sm);
  }

  .meta {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    text-align: right;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    opacity: 0.85;
  }

  .foot {
    flex: none;
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 6px 14px;
    border-top: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .foot kbd {
    font-family: var(--mono);
    font-size: var(--text-xs);
    padding: 0 0.25rem;
    border: 1px solid var(--edge);
    border-radius: 4px;
  }
</style>
