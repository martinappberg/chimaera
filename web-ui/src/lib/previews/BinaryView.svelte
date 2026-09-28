<script lang="ts">
  /**
   * Quiet info card for files with no preview (binary). Size comes from the
   * probe FileView already ran when available; size+mtime are otherwise
   * looked up from the parent directory's listing. Offers "open as text"
   * (a per-tab override the host applies) and, on a remote host, a download
   * so an app on this computer can open it.
   */
  import {
    basename,
    formatMtime,
    fsDownload,
    fsList,
    humanSize,
    type FsEntry,
  } from "./files";
  import { isRemoteHost } from "../net/api";

  interface Props {
    path: string;
    /** Size already known from a fetched chunk's X-File-Size, if any. */
    knownSize?: number | null;
    /** Show the file as text anyway (the host swaps the view). */
    onText?: () => void;
  }

  let { path, knownSize = null, onText }: Props = $props();

  let entry = $state<FsEntry | null>(null);

  $effect(() => {
    const p = path;
    entry = null;
    const dir = p.slice(0, Math.max(p.lastIndexOf("/"), 1));
    let stale = false;
    fsList(dir, true)
      .then((listing) => {
        if (stale) return;
        entry = listing.entries.find((e) => e.path === p) ?? null;
      })
      .catch(() => {
        // metadata is best-effort; the card renders without it
      });
    return () => {
      stale = true;
    };
  });

  const size = $derived(entry?.size ?? knownSize);

  // Same rule as the media player's card: on the laptop the file is already
  // here (a desktop app can open it from its folder); on a remote host the
  // download is the way to get it to one.
  const remote = isRemoteHost();
  let downloadError = $state<string | null>(null);
  async function download(): Promise<void> {
    downloadError = null;
    try {
      await fsDownload(path);
    } catch (e) {
      downloadError = e instanceof Error ? e.message : "download failed";
    }
  }
</script>

<div class="binary-view">
  <div class="card">
    <svg viewBox="0 0 24 24" width="26" height="26" aria-hidden="true">
      <path
        d="M6 2.75h7.5L19 8.25V20a1.25 1.25 0 0 1-1.25 1.25H6A1.25 1.25 0 0 1 4.75 20V4A1.25 1.25 0 0 1 6 2.75Z"
        fill="none"
        stroke="currentColor"
        stroke-width="1.4"
        stroke-linejoin="round"
      />
      <path d="M13.5 2.75v5.5H19" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" />
    </svg>
    <span class="name">{basename(path)}</span>
    <dl>
      <dt>size</dt>
      <dd>{size !== null && size !== undefined ? humanSize(size) : "—"}</dd>
      <dt>modified</dt>
      <dd>{entry !== null ? formatMtime(entry.mtime) : "—"}</dd>
    </dl>
    <span class="note">binary file — no preview</span>
    {#if onText !== undefined || remote}
      <div class="actions">
        {#if onText !== undefined}
          <button class="opt" onclick={() => onText?.()} title="show the bytes as text">open as text</button>
        {/if}
        {#if remote}
          <button class="opt primary" onclick={() => void download()} title="download to this computer">download</button>
        {/if}
      </div>
    {/if}
    {#if downloadError !== null}<span class="err" role="alert">{downloadError}</span>{/if}
  </div>
</div>

<style>
  .binary-view {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .card {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.55rem;
    color: var(--muted);
    max-width: 80%;
  }

  svg {
    opacity: 0.55;
  }

  .name {
    font-family: var(--mono);
    font-size: var(--text-md);
    color: var(--fg);
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  dl {
    display: grid;
    grid-template-columns: auto auto;
    gap: 0.15rem 0.7rem;
    margin: 0;
    font-size: var(--text-sm);
  }

  dt {
    text-align: right;
    opacity: 0.7;
  }

  dd {
    margin: 0;
    font-family: var(--mono);
    font-variant-numeric: tabular-nums;
  }

  .note {
    margin-top: 0.35rem;
    font-size: var(--text-xs);
    opacity: 0.75;
  }

  .actions {
    display: flex;
    gap: 0.5rem;
    margin-top: 0.35rem;
  }

  .opt {
    appearance: none;
    border: 1px solid var(--edge);
    border-radius: 6px;
    background: var(--term-bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    padding: 0.25rem 0.8rem;
    cursor: pointer;
    transition:
      background-color 0.12s ease,
      border-color 0.12s ease;
  }

  .opt:hover {
    background: var(--row-hover);
  }

  .opt.primary {
    border-color: color-mix(in srgb, var(--accent) 45%, var(--edge));
    color: var(--accent);
  }

  .err {
    font-size: var(--text-xs);
    color: var(--err);
  }

  @media (prefers-reduced-motion: reduce) {
    .opt {
      transition: none;
    }
  }
</style>
