<script lang="ts">
  /**
   * A read-only excerpt of a text file: `#L10-L30` shows those lines, a
   * whole-file embed its first lines, with line numbers and the same
   * highlighting the notebook cells use (`previews/highlight.ts`, language
   * picked from the file name, parsers loaded on first use). One `fs/file`
   * read of the file's head; a range past it reads once more, up to the
   * daemon's 2 MB chunk cap.
   */
  import { fsFile, looksBinary } from "../../previews/files";

  interface Props {
    path: string;
    /** The file version: a new one re-reads. */
    version: string;
    lines?: { start: number; end: number };
    compact: boolean;
    active: boolean;
  }

  let { path, version, lines, compact, active }: Props = $props();

  /** Lines a whole-file excerpt shows; the most a range shows (it scrolls). */
  const HEAD_LINES = 14;
  const TILE_LINES = 10;
  const MAX_LINES = 400;
  const LINE_H = 18;
  const FIRST_READ = 64 * 1024;
  const DEEP_READ = 2 * 1024 * 1024;

  let text = $state<string | null>(null);
  let firstLine = $state(1);
  let note = $state<string | null>(null);
  let error = $state<string | null>(null);
  let codeEl = $state<HTMLElement | null>(null);

  const want = $derived(
    lines !== undefined
      ? Math.min(MAX_LINES, lines.end - lines.start + 1)
      : compact
        ? TILE_LINES
        : HEAD_LINES,
  );
  /** Rows the frame reserves before (and after) the text arrives. */
  const rows = $derived(Math.min(want, compact ? TILE_LINES : 24));

  async function read(p: string, range: { start: number; end: number } | undefined): Promise<void> {
    const start = range?.start ?? 1;
    const end = range?.end ?? start + want - 1;
    let chunk = await fsFile(p, 0, FIRST_READ);
    const decode = (bytes: Uint8Array) => new TextDecoder().decode(bytes);
    if (looksBinary(chunk.bytes)) throw new Error("a binary file — nothing to excerpt");
    let all = decode(chunk.bytes).split("\n");
    // The last line of a cut read is partial: only count whole lines.
    let whole = chunk.truncated ? all.length - 1 : all.length;
    if (whole < end && chunk.truncated) {
      chunk = await fsFile(p, 0, DEEP_READ);
      all = decode(chunk.bytes).split("\n");
      whole = chunk.truncated ? all.length - 1 : all.length;
    }
    if (start > whole) {
      throw new Error(
        chunk.truncated
          ? `line ${start} is past the first ${Math.round(DEEP_READ / 1024 / 1024)} MB of this file`
          : `the file has ${whole} line${whole === 1 ? "" : "s"}`,
      );
    }
    const last = Math.min(end, whole);
    firstLine = start;
    text = all.slice(start - 1, last).join("\n").replace(/\r$/gm, "");
    note =
      range === undefined && (whole > last || chunk.truncated)
        ? `first ${last} lines`
        : range !== undefined && last < end
          ? `the file ends at line ${last}`
          : null;
  }

  let gen = 0;
  $effect(() => {
    const p = path;
    void version;
    const range = lines;
    if (!active) return;
    const mine = ++gen;
    error = null;
    read(p, range).catch((e: unknown) => {
      if (mine === gen) error = e instanceof Error ? e.message : "couldn't read this file";
    });
  });

  // Highlight once the text is in, with the parser the file name picks.
  $effect(() => {
    const el = codeEl;
    const t = text;
    const p = path;
    if (el === null || t === null) return;
    el.textContent = t;
    let stale = false;
    void (async () => {
      const [{ LanguageDescription }, { languages }, { renderCode }] = await Promise.all([
        import("@codemirror/language"),
        import("@codemirror/language-data"),
        import("../../previews/highlight"),
      ]);
      const name = p.slice(p.lastIndexOf("/") + 1);
      const desc = LanguageDescription.matchFilename(languages, name);
      if (desc === null) return;
      const support = await desc.load();
      if (!stale) renderCode(el, t, support.language.parser);
    })().catch(() => {
      // Plain text stays; highlighting is decoration.
    });
    return () => {
      stale = true;
    };
  });

  const numbers = $derived.by(() => {
    if (text === null) return "";
    const count = text.split("\n").length;
    return Array.from({ length: count }, (_, i) => String(firstLine + i)).join("\n");
  });
</script>

<div class="code-body" class:tile={compact} style:--rows={rows} style:--line-h="{LINE_H}px">
  {#if error !== null}
    <div class="note">{error}</div>
  {:else}
    <div class="scroll">
      <div class="gutter" aria-hidden="true">{numbers}</div>
      <div class="code" bind:this={codeEl}></div>
    </div>
    {#if note !== null}<div class="more">{note}</div>{/if}
  {/if}
</div>

<style>
  .code-body {
    position: relative;
    background: color-mix(in srgb, var(--term-bg) 60%, transparent);
    font-family: var(--mono, monospace);
    font-size: 12px;
    line-height: var(--line-h);
  }
  .code-body.tile {
    flex: 1;
    min-height: 0;
  }
  .scroll {
    display: flex;
    height: calc(var(--rows) * var(--line-h) + 16px);
    overflow: auto;
    scrollbar-width: thin;
  }
  .tile .scroll {
    height: 100%;
    min-height: calc(var(--rows) * var(--line-h) + 16px);
  }
  .gutter {
    position: sticky;
    left: 0;
    flex: none;
    padding: 8px 8px 8px 10px;
    min-width: 2.5em;
    text-align: right;
    white-space: pre;
    color: color-mix(in srgb, var(--muted) 75%, transparent);
    background: color-mix(in srgb, var(--term-bg) 92%, var(--fg) 3%);
    user-select: none;
  }
  .code {
    flex: 1;
    padding: 8px 12px 8px 10px;
    white-space: pre;
    color: var(--fg);
    tab-size: 4;
  }
  .more {
    position: absolute;
    right: 10px;
    bottom: 4px;
    padding: 0 6px;
    border-radius: 4px;
    background: color-mix(in srgb, var(--bg) 80%, transparent);
    color: var(--muted);
    font-family: var(--ui-font, inherit);
    font-size: var(--text-xs);
  }
  .note {
    padding: 12px;
    color: var(--muted);
    font-family: var(--ui-font, inherit);
    font-size: var(--text-xs);
  }
  .code :global(.hl-keyword) { color: var(--syn-keyword); }
  .code :global(.hl-string) { color: var(--syn-string); }
  .code :global(.hl-comment) { color: var(--syn-comment); font-style: italic; }
  .code :global(.hl-number) { color: var(--syn-number); }
  .code :global(.hl-type) { color: var(--syn-type); }
  .code :global(.hl-func) { color: var(--syn-func); }
  .code :global(.hl-def) { color: var(--syn-def); }
  .code :global(.hl-prop) { color: var(--syn-prop); }
  .code :global(.hl-invalid) { color: var(--err); }
</style>
