<script lang="ts">
  /**
   * Quiet info card for files with no preview (binary). Size comes from the
   * probe FileView already ran when available; size+mtime are otherwise
   * looked up from the parent directory's listing.
   */
  import {
    basename,
    formatMtime,
    fsList,
    humanSize,
    type FsEntry,
  } from "./files";

  interface Props {
    path: string;
    /** Size already known from a fetched chunk's X-File-Size, if any. */
    knownSize?: number | null;
  }

  let { path, knownSize = null }: Props = $props();

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
</style>
