<script lang="ts">
  /**
   * Both versions a Chimaera Pro return kept, side by side, to choose which
   * stays. When the incoming copy and this computer both changed a file while apart,
   * the file holds the incoming version and this computer's sits beside it as
   * `<name>.mine-<stamp>`; this view lists those files, compares the two
   * versions of the selected one (`KeptDiff`), and settles it: this
   * computer's replaces the file, the incoming version stays, or both stay as they are.
   * Branches the incoming copy kept beside this computer's are listed, without
   * actions (merging them is ordinary Git work).
   *
   * One tab per workspace (`layout.ts` KeptTab). Reads on mount and whenever
   * the tab becomes visible again; each choice's answer replaces the shared
   * summary (`keptReviews`), so the chat's line and Settings follow at once.
   */
  import { untrack } from "svelte";
  import FileIcon from "../shared/FileIcon.svelte";
  import ConfirmDialog from "../shared/ConfirmDialog.svelte";
  import Spinner from "../previews/Spinner.svelte";
  import KeptDiff from "./KeptDiff.svelte";
  import { formatFullTimestamp, formatMessageTimestamp } from "../shared/time";
  import {
    fetchKeptFile,
    canUseMine,
    hereName,
    hereTitle,
    KeptError,
    keptErrorLine,
    deletedNote,
    resolveAllKept,
    resolveKept,
    sizeLabel,
    useCloudForAllBody,
    useCloudHint,
    type KeptChoice,
    type KeptFile,
    type KeptPair,
    type KeptReview,
  } from "./kept";
  import { keptReviews } from "./keptReviews.svelte";

  interface Props {
    wsId: string | null;
    wsRoot: string | null;
    visible: boolean;
    onOpenFile(path: string): void;
  }

  let { wsId, wsRoot, visible, onOpenFile }: Props = $props();

  const here = hereName();
  const Here = hereTitle();

  const review = $derived<KeptReview | null | undefined>(
    wsId === null ? null : keptReviews.byWorkspace[wsId],
  );
  const pairs = $derived(review?.pairs ?? []);
  const canChoose = $derived(review?.here === true);
  /** A discarded copy goes to the Trash here (else it is deleted). */
  const toTrash = $derived(review?.trash === true);

  let selected = $state<string | null>(null);
  let busy = $state<string | null>(null);
  let actionError = $state<string | null>(null);
  /** Copies a choice deleted after all, though the review said Trash. */
  let actionNote = $state<string | null>(null);
  let confirmAll = $state<KeptChoice | null>(null);
  let loadError = $state<string | null>(null);

  // Read again each time the tab comes into view: files may have been
  // renamed or settled elsewhere (the tree, a terminal, another window).
  $effect(() => {
    if (!visible || wsId === null) return;
    const id = wsId;
    untrack(() => {
      loadError = null;
      void keptReviews.refresh(id).then(() => {
        if (keptReviews.byWorkspace[id] === null) loadError = "This project's kept versions couldn't be read.";
      });
    });
  });

  // Keep a selection while there is something to select.
  $effect(() => {
    const list = pairs;
    untrack(() => {
      if (list.length === 0) selected = null;
      else if (selected === null || !list.some((p) => p.mine_path === selected)) selected = list[0].mine_path;
    });
  });

  const current = $derived<KeptPair | null>(pairs.find((p) => p.mine_path === selected) ?? null);

  // Both versions of the selected pair, read once per selection (a refreshed
  // listing hands back new pair objects; the texts only change with the pair
  // or with its files).
  const currentKey = $derived(
    current === null ? null : `${current.mine_path}\u0000${current.mine_changed_at}\u0000${current.changed_at}`,
  );
  let file = $state<KeptFile | null>(null);
  let fileError = $state<string | null>(null);
  $effect(() => {
    const key = currentKey;
    const id = wsId;
    const minePath = untrack(() => current?.mine_path ?? null);
    file = null;
    fileError = null;
    if (key === null || minePath === null || id === null) return;
    const abort = new AbortController();
    fetchKeptFile(id, minePath, abort.signal).then(
      (loaded) => {
        if (!abort.signal.aborted) file = loaded;
      },
      (error: unknown) => {
        if (abort.signal.aborted) return;
        fileError = error instanceof KeptError ? error.message : "Both versions couldn't be read.";
      },
    );
    return () => abort.abort();
  });

  function name(path: string): string {
    const at = path.lastIndexOf("/");
    return at === -1 ? path : path.slice(at + 1);
  }
  function folder(path: string): string {
    const at = path.lastIndexOf("/");
    return at === -1 ? "" : path.slice(0, at);
  }
  function absolute(path: string): string | null {
    return wsRoot === null ? null : `${wsRoot.replace(/\/$/, "")}/${path}`;
  }
  function when(ms: number | null): string {
    return ms === null ? "" : formatMessageTimestamp(ms);
  }
  function open(path: string): void {
    const full = absolute(path);
    if (full !== null) onOpenFile(full);
  }

  /** The pair after this one settles: the next in the list, else the one before. */
  function nextAfter(minePath: string): string | null {
    const at = pairs.findIndex((p) => p.mine_path === minePath);
    const rest = pairs.filter((p) => p.mine_path !== minePath);
    if (rest.length === 0) return null;
    return rest[Math.min(Math.max(at, 0), rest.length - 1)].mine_path;
  }

  /** Say so when copies were deleted where the review promised the Trash
   *  (its drive's Trash refused them). */
  function noteDeleted(answer: KeptReview, promised: boolean): void {
    const deleted = answer.discarded?.deleted ?? 0;
    actionNote = promised && deleted > 0 ? deletedNote(deleted, here) : null;
  }

  async function choose(pair: KeptPair, choice: KeptChoice): Promise<void> {
    if (wsId === null || busy !== null) return;
    if (choice === "use_mine" && !canUseMine(pair)) return;
    busy = pair.mine_path;
    actionError = null;
    actionNote = null;
    const promised = toTrash;
    const next = nextAfter(pair.mine_path);
    try {
      const answer = await resolveKept(wsId, pair.mine_path, choice);
      selected = next;
      keptReviews.set(wsId, answer);
      noteDeleted(answer, promised);
    } catch (error) {
      actionError = error instanceof KeptError ? error.message : keptErrorLine("failed");
      void keptReviews.refresh(wsId);
    } finally {
      busy = null;
    }
  }

  async function chooseAll(choice: KeptChoice): Promise<void> {
    if (wsId === null || busy !== null) return;
    if (choice === "use_mine" && !pairs.every(canUseMine)) return;
    confirmAll = null;
    busy = "all";
    actionError = null;
    actionNote = null;
    const promised = toTrash;
    try {
      const answer = await resolveAllKept(wsId, choice);
      keptReviews.set(wsId, answer);
      noteDeleted(answer, promised);
      const failed = answer.failed?.length ?? 0;
      if (failed > 0) {
        const code = answer.failed?.[0]?.error_code ?? "failed";
        actionError =
          failed === 1
            ? `One file kept both versions: ${keptErrorLine(code)}`
            : `${failed} files kept both versions: ${keptErrorLine(code)}`;
      }
    } catch (error) {
      actionError = error instanceof KeptError ? error.message : keptErrorLine("failed");
      void keptReviews.refresh(wsId);
    } finally {
      busy = null;
    }
  }

  function onListKey(e: KeyboardEvent): void {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    const at = pairs.findIndex((p) => p.mine_path === selected);
    const to = Math.min(pairs.length - 1, Math.max(0, at + (e.key === "ArrowDown" ? 1 : -1)));
    selected = pairs[to]?.mine_path ?? selected;
    const row = (e.currentTarget as HTMLElement).querySelectorAll<HTMLButtonElement>("button.file")[to];
    row?.focus();
  }

  const confirmCopy = $derived.by(() => {
    if (confirmAll === null) return null;
    const n = pairs.length;
    const files = n === 1 ? "1 file" : `${n} files`;
    return confirmAll === "use_cloud"
      ? {
          title: "Use the incoming version for all?",
          body: useCloudForAllBody(n, toTrash, here),
          label: "Use incoming",
          danger: !toTrash,
        }
      : {
          title: `Use ${here}'s version for all?`,
          body: `${Here}'s versions replace the incoming versions in ${files}.`,
          label: `Use ${here}'s`,
          danger: false,
        };
  });
</script>

<div class="kept">
  {#if review === undefined}
    <div class="center"><Spinner /></div>
  {:else if review === null}
    <div class="center">
      <div class="empty">
        <p class="empty-title">Nothing to review</p>
        <p class="empty-body">{loadError ?? "Nothing is waiting for a choice in this project."}</p>
      </div>
    </div>
  {:else if pairs.length === 0 && review.unlisted === 0}
    <div class="center">
      <div class="empty">
        <svg class="done" viewBox="0 0 24 24" width="28" height="28" aria-hidden="true">
          <circle cx="12" cy="12" r="10" fill="none" stroke="currentColor" stroke-width="1.5" />
          <path d="M7.5 12.4l3 3 6-6.6" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" />
        </svg>
        <p class="empty-title">Nothing left to review</p>
        <p class="empty-body">You chose for every file both sides changed.</p>
        {#if actionNote !== null}
          <p class="empty-body" role="status">{actionNote}</p>
        {/if}
        {#if review.branches.length > 0}
          {@render branchList(review.branches)}
        {/if}
      </div>
    </div>
  {:else}
    <header class="head">
      <div class="titles">
        <h1>Both versions kept</h1>
        <p class="lede">
          Both copies changed {review.total === 1 ? "this file" : "these files"} while apart. Each file now
          has the incoming version, and {here}'s version is saved beside it. Choose which one stays.
        </p>
      </div>
      {#if pairs.length > 1}
        <div class="all">
          <button class="btn" disabled={busy !== null || !canChoose} onclick={() => (confirmAll = "use_cloud")}
            >Use incoming for all</button
          >
          <button class="btn" disabled={busy !== null || !canChoose || !pairs.every(canUseMine)} onclick={() => (confirmAll = "use_mine")}
            >Use {here}'s for all</button
          >
        </div>
      {/if}
    </header>
    {#if !canChoose}
      <p class="banner" role="status">
        This project is running somewhere else right now. You can compare both versions, and choose once it's back on {here}.
      </p>
    {/if}
    {#if actionError !== null}
      <p class="banner error" role="alert">{actionError}</p>
    {/if}
    {#if actionNote !== null}
      <p class="banner" role="status">{actionNote}</p>
    {/if}
    <div class="body">
      <nav class="side" aria-label="Files changed on both sides">
        {#if pairs.length > 0}
          <p class="section">{pairs.length === 1 ? "1 file" : `${pairs.length} files`}</p>
          <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
          <ul class="files" onkeydown={onListKey}>
            {#each pairs as pair (pair.mine_path)}
              <li>
                <button
                  class="file"
                  class:on={pair.mine_path === selected}
                  aria-current={pair.mine_path === selected ? "true" : undefined}
                  title={pair.path}
                  onclick={() => (selected = pair.mine_path)}
                >
                  <FileIcon path={pair.path} size={14} />
                  <span class="file-text">
                    <span class="file-name">{name(pair.path)}</span>
                    {#if folder(pair.path) !== "" || pair.size === null}
                      <span class="file-sub"
                        >{#if pair.size === null}deleted in the incoming copy{#if folder(pair.path) !== ""}{" · "}{/if}{/if}{folder(
                          pair.path,
                        )}</span
                      >
                    {/if}
                  </span>
                  {#if busy === pair.mine_path}<span class="mini-spinner" aria-hidden="true"></span>{/if}
                </button>
              </li>
            {/each}
          </ul>
        {/if}
        {#if review.unlisted > 0}
          <div class="note">
            <p>
              Some kept copies aren't listed; they keep their <code>.mine-</code> names in the folder.
            </p>
            {#if pairs.length === 0}
              <button class="btn" disabled={busy !== null || !canChoose} onclick={() => void chooseAll("keep_both")}
                >Done</button
              >
            {/if}
          </div>
        {/if}
        {#if review.branches.length > 0}
          {@render branchList(review.branches)}
        {/if}
      </nav>
      <section class="detail" aria-label={current === null ? "Both versions" : `Both versions of ${current.path}`}>
        {#if current !== null}
          <div class="bar">
            <div class="where">
              <span class="path" title={current.path}>{current.path}</span>
            </div>
            <div class="choices" role="group" aria-label="Choose a version">
              <button
                class="btn primary"
                disabled={busy !== null || !canChoose || !canUseMine(current)}
                title={canUseMine(current) ? `${Here}'s version replaces the file; the copy beside it goes away` : "The original filename can't be recovered safely. Keep both versions or use incoming."}
                onclick={() => void choose(current, "use_mine")}>Use {here}'s</button
              >
              <button
                class="btn primary"
                disabled={busy !== null || !canChoose}
                title={useCloudHint(current.size === null, toTrash, here)}
                onclick={() => void choose(current, "use_cloud")}>Use incoming</button
              >
              <button
                class="btn"
                disabled={busy !== null || !canChoose}
                title="Keep both files as they are and stop asking"
                onclick={() => void choose(current, "keep_both")}>Keep both</button
              >
            </div>
          </div>
          {#if current.size === null}
            <p class="deleted" role="note">
              The incoming copy deleted this file. Use {here}'s to bring it back, or use incoming to leave it deleted.
            </p>
          {/if}
          <div class="labels">
            <div class="label">
              <span class="who">{Here}'s version</span>
              <span class="meta" title={current.mine_changed_at === null ? "" : formatFullTimestamp(current.mine_changed_at)}
                >{sizeLabel(current.mine_size)}{#if current.mine_changed_at !== null}{" · "}{when(current.mine_changed_at)}{/if}</span
              >
              <button class="link" onclick={() => open(current.mine_path)}>Open</button>
            </div>
            <div class="label">
              <span class="who">The incoming version</span>
              {#if current.size === null}
                <span class="meta">deleted in the incoming copy</span>
              {:else}
                <span class="meta" title={current.changed_at === null ? "" : formatFullTimestamp(current.changed_at)}
                  >{sizeLabel(current.size)}{#if current.changed_at !== null}{" · "}{when(current.changed_at)}{/if}</span
                >
                <button class="link" onclick={() => open(current.path)}>Open</button>
              {/if}
            </div>
          </div>
          <div class="compare">
            {#if fileError !== null}
              <div class="center"><p class="quiet">{fileError}</p></div>
            {:else if file === null || file.mine_path !== current.mine_path}
              <div class="center"><Spinner /></div>
            {:else if file.mine.text === null || (file.cloud !== null && file.cloud.text === null)}
              {@const large = file.mine.too_large === true || file.cloud?.too_large === true}
              <div class="center">
                <div class="sizes">
                  <div class="cards">
                    <div class="card">
                      <span class="card-who">{Here}'s version</span>
                      <span class="card-size">{sizeLabel(file.mine.size)}</span>
                      {#if file.mine.changed_at !== null}<span class="card-when">changed {when(file.mine.changed_at)}</span>{/if}
                    </div>
                    <div class="card">
                      <span class="card-who">The incoming version</span>
                      {#if file.cloud === null}
                        <span class="card-size">Deleted</span>
                      {:else}
                        <span class="card-size">{sizeLabel(file.cloud.size)}</span>
                        {#if file.cloud.changed_at !== null}<span class="card-when">changed {when(file.cloud.changed_at)}</span>{/if}
                      {/if}
                    </div>
                  </div>
                  <p class="quiet">
                    {large
                      ? "Too large to compare here. Open either version to look at it."
                      : "Not text, so there's nothing to compare line by line. Open either version to look at it."}
                  </p>
                </div>
              </div>
            {:else}
              {#key file.mine_path}
                <KeptDiff path={file.path} mine={file.mine.text} cloud={file.cloud?.text ?? ""} />
              {/key}
            {/if}
          </div>
        {:else}
          <div class="center">
            <p class="quiet">
              {review.unlisted > 0
                ? `${review.unlisted === 1 ? "1 kept copy isn't" : `${review.unlisted} kept copies aren't`} listed here. Look for names ending in .mine- and a date.`
                : "Nothing to compare."}
            </p>
          </div>
        {/if}
      </section>
    </div>
  {/if}
</div>

{#snippet branchList(branches: string[])}
  <div class="branches">
    <p class="section">Incoming branches</p>
    <p class="hint">The incoming work on these branches is kept under these names; your own branches are unchanged.</p>
    <ul>
      {#each branches as branch (branch)}
        <li title={branch}>
          <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
            <path
              d="M5 3v7.5M5 12.5v.5M11 3v3a2.5 2.5 0 0 1-2.5 2.5H5"
              fill="none"
              stroke="currentColor"
              stroke-width="1.4"
              stroke-linecap="round"
            />
            <circle cx="5" cy="12.6" r="1.5" fill="none" stroke="currentColor" stroke-width="1.4" />
            <circle cx="11" cy="2.4" r="1.5" fill="none" stroke="currentColor" stroke-width="1.4" />
          </svg>
          <span class="branch">{branch}</span>
        </li>
      {/each}
    </ul>
  </div>
{/snippet}

{#if confirmCopy !== null && confirmAll !== null}
  {@const choice = confirmAll}
  <ConfirmDialog
    title={confirmCopy.title}
    body={confirmCopy.body}
    confirmLabel={confirmCopy.label}
    danger={confirmCopy.danger}
    onConfirm={() => void chooseAll(choice)}
    onCancel={() => (confirmAll = null)}
  />
{/if}

<style>
  .kept {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    background: var(--bg);
    color: var(--fg);
    container-type: inline-size;
  }
  .center {
    flex: 1;
    min-height: 0;
    display: grid;
    place-items: center;
    padding: 24px;
  }
  .empty {
    max-width: 420px;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
    text-align: center;
  }
  .done {
    color: var(--git-added);
    margin-bottom: 4px;
  }
  .empty-title {
    margin: 0;
    font-size: var(--text-md);
    font-weight: 600;
  }
  .empty-body,
  .quiet {
    margin: 0;
    color: var(--muted);
    font-size: var(--text-sm);
    line-height: 1.5;
    text-align: center;
    max-width: 440px;
  }
  .empty .branches {
    margin-top: 14px;
    text-align: left;
    align-self: stretch;
  }

  .head {
    flex: none;
    display: flex;
    align-items: flex-end;
    gap: 16px 24px;
    flex-wrap: wrap;
    padding: 20px 24px 14px;
    border-bottom: 1px solid var(--edge);
  }
  .titles {
    flex: 1 1 360px;
    min-width: 0;
  }
  h1 {
    margin: 0 0 6px;
    font-size: 19px;
    font-weight: 600;
    letter-spacing: -0.01em;
  }
  .lede {
    margin: 0;
    max-width: 62ch;
    color: var(--muted);
    font-size: var(--text-sm);
    line-height: 1.5;
  }
  .all {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }

  .banner {
    flex: none;
    margin: 10px 24px 0;
    padding: 8px 12px;
    border: 1px solid var(--edge);
    border-radius: 8px;
    background: color-mix(in srgb, var(--fg) 4%, transparent);
    color: var(--muted);
    font-size: var(--text-sm);
    line-height: 1.45;
  }
  .banner.error {
    color: var(--warn);
    border-color: color-mix(in srgb, var(--warn) 40%, var(--edge));
    background: color-mix(in srgb, var(--warn) 8%, transparent);
  }

  .body {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: minmax(200px, 260px) minmax(0, 1fr);
  }
  .side {
    min-height: 0;
    overflow-y: auto;
    padding: 12px 10px 16px;
    border-right: 1px solid var(--edge);
    background: var(--rail-bg);
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .section {
    margin: 0 6px 6px;
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
    font-weight: 600;
  }
  .files {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .file {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 8px;
    border: none;
    border-radius: 6px;
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    text-align: left;
    cursor: pointer;
  }
  .file:hover {
    background: var(--row-hover);
  }
  .file.on {
    background: var(--row-active);
  }
  .file:focus-visible,
  .btn:focus-visible,
  .link:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 1px;
  }
  .file-text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .file-name,
  .file-sub {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .file-sub {
    color: var(--muted);
    font-size: var(--text-xs);
  }
  .mini-spinner {
    flex: none;
    width: 10px;
    height: 10px;
    border: 1.5px solid var(--edge);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  :global(html.app-hidden) .mini-spinner {
    animation-play-state: paused;
  }

  .note {
    margin: 0 6px;
    padding: 8px 10px;
    border: 1px dashed var(--edge);
    border-radius: 8px;
    color: var(--muted);
    font-size: var(--text-xs);
    line-height: 1.5;
    display: flex;
    flex-direction: column;
    gap: 8px;
    align-items: flex-start;
  }
  .note p {
    margin: 0;
  }
  code {
    font-family: var(--mono);
    font-size: 0.95em;
  }

  .branches {
    display: flex;
    flex-direction: column;
  }
  .hint {
    margin: 0 6px 6px;
    color: var(--muted);
    font-size: var(--text-xs);
    line-height: 1.5;
  }
  .branches ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .branches li {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 3px 8px;
    color: var(--muted);
  }
  .branch {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--fg);
  }

  .detail {
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 10px 16px;
    flex-wrap: wrap;
    padding: 10px 16px;
    border-bottom: 1px solid var(--edge);
  }
  .where {
    flex: 1 1 200px;
    min-width: 0;
  }
  .path {
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono);
    font-size: var(--text-sm);
  }
  .choices {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .labels {
    flex: none;
    display: grid;
    grid-template-columns: 1fr 1fr;
    border-bottom: 1px solid var(--edge);
    background: var(--term-bg);
  }
  .label {
    min-width: 0;
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 6px 12px;
    font-size: var(--text-xs);
  }
  .label + .label {
    border-left: 1px solid var(--edge);
  }
  .who {
    font-weight: 600;
    white-space: nowrap;
  }
  .meta {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  .link {
    margin-left: auto;
    flex: none;
    border: none;
    background: none;
    padding: 0 2px;
    color: var(--accent);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
  }
  .link:hover {
    text-decoration: underline;
  }
  .compare {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    background: var(--term-bg);
  }
  .compare > :global(*) {
    flex: 1;
    min-height: 0;
  }
  .sizes {
    display: flex;
    flex-direction: column;
    gap: 14px;
    align-items: center;
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(2, minmax(150px, 210px));
    gap: 12px;
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: 12px 14px;
    border: 1px solid var(--edge);
    border-radius: 10px;
    background: var(--bg);
  }
  .card-who {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .card-size {
    font-size: var(--text-lg);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .card-when {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .deleted {
    flex: none;
    margin: 0;
    padding: 7px 16px;
    border-bottom: 1px solid var(--edge);
    background: color-mix(in srgb, var(--git-deleted) 7%, transparent);
    color: var(--fg);
    font-size: var(--text-xs);
    line-height: 1.45;
  }

  .btn {
    appearance: none;
    border: 1px solid var(--edge);
    border-radius: 6px;
    background: var(--bg);
    color: var(--fg);
    padding: 5px 11px;
    font: inherit;
    font-size: var(--text-sm);
    white-space: nowrap;
    cursor: pointer;
  }
  .btn:hover:not(:disabled) {
    background: var(--row-hover);
  }
  .btn.primary {
    border-color: color-mix(in srgb, var(--accent) 45%, var(--edge));
  }
  .btn.primary:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
  }
  .btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  /* A narrow pane stacks the list over the comparison. */
  @container (max-width: 640px) {
    .body {
      display: flex;
      flex-direction: column;
    }
    .side {
      flex: 0 1 auto;
      max-height: 36%;
      border-right: none;
      border-bottom: 1px solid var(--edge);
    }
    .detail {
      flex: 1;
    }
    .head {
      padding: 16px 16px 12px;
    }
    .bar {
      padding: 10px 12px;
    }
  }
  @media (pointer: coarse) {
    .btn,
    .file {
      min-height: 40px;
    }
  }
</style>
