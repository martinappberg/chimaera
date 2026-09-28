<script lang="ts">
  /**
   * The markdown toolbar's "publish" menu: export the document as one
   * self-contained HTML file, print it (save as PDF from the dialog), or
   * download a zip of it and every file it names. The menu is the app's
   * one context menu, hung under the button; the work (`doc/publishRun.ts`)
   * loads on first use. While it runs the button says so; afterwards a
   * short note says what was saved and anything left out, then fades.
   */
  import { contextMenu } from "../shared/contextMenu.svelte";
  import type { LinkContext } from "./docLinks";
  import type { PublishResult, PublishSource } from "./doc/publishRun";

  interface Props {
    path: string;
    /** The document's text as the view shows it; null until it is known
     *  (or for a file too large to draw in the browser). */
    text: string | null;
    links: () => LinkContext;
  }

  let { path, text, links }: Props = $props();

  type Action = "html" | "print" | "bundle";
  let busy = $state<Action | null>(null);
  let note = $state<{ text: string; error: boolean; detail: string } | null>(null);
  let button = $state<HTMLButtonElement | null>(null);

  // A note lasts a few seconds (longer when it has something to say).
  $effect(() => {
    const n = note;
    if (n === null) return;
    const timer = setTimeout(() => (note = null), n.error || n.detail !== "" ? 9000 : 4500);
    return () => clearTimeout(timer);
  });
  // A different document owns nothing of this one's note.
  $effect(() => {
    void path;
    note = null;
  });

  const BUSY: Record<Action, string> = { html: "exporting…", print: "preparing…", bundle: "bundling…" };

  async function run(action: Action): Promise<void> {
    const t = text;
    if (t === null || busy !== null) return;
    busy = action;
    note = null;
    const src: PublishSource = { docPath: path, text: t, links: links() };
    try {
      const mod = await import("./doc/publishRun");
      const result: PublishResult =
        action === "html" ? await mod.exportHtml(src) : action === "print" ? await mod.printDocument(src) : await mod.exportBundle(src);
      if (src.docPath !== path) return;
      // What was saved, then what was left out; a print says only the latter.
      const [first, ...rest] = action === "bundle" ? [`${result.saved} · ${result.notes[0] ?? ""}`, ...result.notes.slice(1)] : [result.saved, ...result.notes];
      const lines = [first, ...rest].filter((l): l is string => l !== null && l !== "");
      note = lines.length === 0 ? null : { text: lines[0], error: false, detail: lines.slice(1).join(" · ") };
    } catch (e) {
      if (src.docPath === path) note = { text: `couldn't ${action === "print" ? "print" : "export"}: ${e instanceof Error ? e.message : String(e)}`, error: true, detail: "" };
    } finally {
      busy = null;
    }
  }

  function open(): void {
    const el = button;
    if (el === null) return;
    const r = el.getBoundingClientRect();
    contextMenu.openAtPoint(r.left, r.bottom + 2, [
      { label: "Export HTML", onSelect: () => void run("html") },
      { label: "Print / save as PDF", onSelect: () => void run("print") },
      "separator",
      { label: "Export bundle (.zip)", onSelect: () => void run("bundle") },
    ]);
  }
</script>

{#if note !== null}
  <span class="pub-note" class:err={note.error} role="status" title={note.detail || note.text}>
    {note.text}{#if note.detail !== ""}<span class="pub-detail">{` · ${note.detail}`}</span>{/if}
  </span>
{/if}
<button
  class="pub"
  class:busy={busy !== null}
  bind:this={button}
  aria-haspopup="menu"
  aria-busy={busy !== null}
  disabled={text === null || busy !== null}
  title={text === null ? "available once the document has loaded (files under 1 MB)" : "export as HTML, print to PDF, or bundle with its files"}
  onclick={open}>{busy !== null ? BUSY[busy] : "publish"}</button
>

<style>
  .pub {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    letter-spacing: 0.04em;
    color: var(--muted);
    cursor: pointer;
    padding: 2px 8px;
    border-radius: 4px;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .pub:hover:not(:disabled) {
    color: var(--fg);
  }

  .pub:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .pub.busy:disabled {
    opacity: 1;
    color: var(--fg);
    background: var(--row-active);
  }

  .pub:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 1px;
  }

  .pub-note {
    min-width: 0;
    max-width: 38ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-xs);
    color: var(--muted);
    animation: pub-in 0.14s ease-out;
  }

  .pub-note.err {
    color: var(--err);
  }

  .pub-detail {
    color: color-mix(in srgb, var(--muted) 80%, transparent);
  }

  @keyframes pub-in {
    from {
      opacity: 0;
      transform: translateY(-2px);
    }
  }
</style>
