<script lang="ts">
  /**
   * The composer's `@` mentions as quiet pills, drawn UNDER the textarea: a
   * mirror of the draft laid out exactly like the field (its own padding,
   * font and wrapping, copied from the textarea's computed style), with
   * transparent text and a tinted background on each mention. The textarea
   * still owns every glyph, the caret, selection, IME and undo — this layer
   * only paints and never changes what is sent.
   *
   * A dropped file's short form (`uploadTokens.ts`) also answers a hover:
   * its name whole and the path the send will put back, in a small tip
   * above the pill — the long location on demand, never in the sentence.
   */
  import FileIcon from "../shared/FileIcon.svelte";
  import { extractFileRefs } from "../shared/fileRef";
  import { tokenSpans, type UploadTokens } from "./uploadTokens";

  interface Props {
    text: string;
    /** Short form → whole mention; read when `text` changes (the composer
     *  records a short form before it writes one into the draft). */
    tokens: UploadTokens;
    field: HTMLTextAreaElement | null;
    /** A completion popover owns the space above the field: no tip. */
    quiet?: boolean;
  }

  let { text, tokens, field, quiet = false }: Props = $props();

  /** Past this the draft is a paste (a log, a table), not a message with
   *  mentions: no pills, and no per-keystroke scan of it. */
  const MAX_SCANNED = 8000;

  interface Run {
    text: string;
    /** For a mention: the whole mention an upload's short form stands for,
     *  or "" for any other mention. Null for plain text. */
    upload: string | null;
  }

  /** The draft cut at its mentions; empty when it has none, so a draft
   *  without pills costs no second layout of its text. */
  const runs = $derived.by((): Run[] => {
    const out: Run[] = [];
    let last = 0;
    if (text.length > MAX_SCANNED || !text.includes("@")) return out;
    // Dropped files' short forms by the composer's own rule (one may sit
    // against a word), then the typed mentions around them.
    const uploads = tokenSpans(text, tokens);
    const typed = extractFileRefs(text)
      .filter((f) => text.startsWith("@", f.start))
      .filter((f) => !uploads.some((u) => u.start < f.end && f.start < u.end))
      .map((f) => ({ start: f.start, end: f.end, upload: "" }));
    const marks = [...uploads.map((u) => ({ start: u.start, end: u.end, upload: tokens.get(u.token) ?? "" })), ...typed];
    marks.sort((a, b) => a.start - b.start);
    for (const mark of marks) {
      if (mark.start > last) out.push({ text: text.slice(last, mark.start), upload: null });
      out.push({ text: text.slice(mark.start, mark.end), upload: mark.upload });
      last = mark.end;
    }
    if (out.length === 0) return out;
    // A trailing space gives a final newline its own line, as the textarea
    // shows it.
    out.push({ text: `${text.slice(last)} `, upload: null });
    return out;
  });

  let mirror = $state<HTMLElement | null>(null);

  /** The textarea's box and type, copied so both lay the text out alike;
   *  its scrollbar (when one takes room) widens the mirror's end padding. */
  function copyLayout(t: HTMLTextAreaElement, m: HTMLElement): void {
    const cs = getComputedStyle(t);
    for (const p of [
      "fontFamily",
      "fontSize",
      "fontWeight",
      "fontStyle",
      "letterSpacing",
      "wordSpacing",
      "lineHeight",
      "tabSize",
      "textIndent",
      "paddingTop",
      "paddingBottom",
      "paddingLeft",
      "borderTopWidth",
      "borderRightWidth",
      "borderBottomWidth",
      "borderLeftWidth",
    ] as const) {
      m.style[p] = cs[p];
    }
    const borders = parseFloat(cs.borderLeftWidth) + parseFloat(cs.borderRightWidth);
    const scrollbar = Math.max(0, t.offsetWidth - t.clientWidth - borders);
    m.style.paddingRight = `${parseFloat(cs.paddingRight) + scrollbar}px`;
  }

  $effect(() => {
    const t = field;
    const m = mirror;
    if (t === null || m === null) return;
    copyLayout(t, m);
    const observer = new ResizeObserver(() => copyLayout(t, m));
    observer.observe(t);
    const follow = () => (m.scrollTop = t.scrollTop);
    t.addEventListener("scroll", follow, { passive: true });
    return () => {
      observer.disconnect();
      t.removeEventListener("scroll", follow);
    };
  });

  // Every re-render re-copies the layout (a font or padding change that
  // leaves the field's box alone reaches no ResizeObserver) and re-aligns
  // the scroll (a paste scrolls the field before its scroll event lands).
  $effect(() => {
    if (runs.length === 0 || field === null || mirror === null) return;
    copyLayout(field, mirror);
    mirror.scrollTop = field.scrollTop;
  });

  interface Hover {
    index: number;
    name: string;
    path: string;
    /** Anchored under the pill's left edge, or — for a pill in the field's
     *  right half — its right edge, so the tip never runs off the pane. */
    side: "left" | "right";
    x: number;
    bottom: number;
  }
  let hovered = $state<Hover | null>(null);

  /** The whole mention's path, without its `@` and quotes. */
  function pathOf(mention: string): string {
    const body = mention.slice(1);
    return body.startsWith('"') && body.endsWith('"') ? body.slice(1, -1) : body;
  }

  // Hover is read off the textarea (the mirror takes no pointer events):
  // which upload pill, if any, holds the pointer.
  $effect(() => {
    const t = field;
    const m = mirror;
    if (t === null || m === null) return;
    const move = (e: PointerEvent) => {
      if (tokens.size === 0) {
        if (hovered !== null) hovered = null;
        return;
      }
      const row = m.parentElement?.getBoundingClientRect();
      if (row === undefined) return;
      for (const mark of m.querySelectorAll<HTMLElement>("mark[data-upload]")) {
        for (const r of mark.getClientRects()) {
          if (e.clientX < r.left - 2 || e.clientX > r.right + 2 || e.clientY < r.top || e.clientY > r.bottom) {
            continue;
          }
          const index = Number(mark.dataset.index);
          if (hovered?.index === index) return;
          const run = runs[index];
          if (run?.upload == null || run.upload === "") return;
          const right = r.left - row.left > row.width / 2;
          hovered = {
            index,
            name: pathOf(run.text),
            path: pathOf(run.upload),
            side: right ? "right" : "left",
            x: right ? Math.max(0, row.right - r.right) : Math.max(0, r.left - row.left),
            bottom: row.bottom - r.top + 6,
          };
          return;
        }
      }
      if (hovered !== null) hovered = null;
    };
    const leave = () => (hovered = null);
    t.addEventListener("pointermove", move);
    t.addEventListener("pointerleave", leave);
    t.addEventListener("scroll", leave, { passive: true });
    return () => {
      t.removeEventListener("pointermove", move);
      t.removeEventListener("pointerleave", leave);
      t.removeEventListener("scroll", leave);
    };
  });

  // A tip for a pill that is gone (the draft changed under it) goes too.
  $effect(() => {
    void runs;
    hovered = null;
  });
</script>

<!-- Whitespace-tight: the mirror is pre-wrap, so template whitespace would
     become layout the textarea doesn't have. -->
<!-- prettier-ignore -->
<div class="mirror" aria-hidden="true" bind:this={mirror}
  >{#each runs as run, i (i)}{#if run.upload === null}{run.text}{:else}<mark
        class:upload={run.upload !== ""}
        data-upload={run.upload !== "" ? "" : undefined}
        data-index={i}>{run.text}</mark>{/if}{/each}</div>
{#if hovered !== null && !quiet}
  {#key hovered.index}
    <div
      class="tip"
      aria-hidden="true"
      style:left={hovered.side === "left" ? `${hovered.x}px` : undefined}
      style:right={hovered.side === "right" ? `${hovered.x}px` : undefined}
      style:bottom="{hovered.bottom}px"
    >
      <span class="tip-name"><FileIcon path={hovered.name} size={12} />{hovered.name}</span>
      <span class="tip-path">{hovered.path}</span>
    </div>
  {/key}
{/if}

<style>
  /* Geometry and type come from the textarea (copyLayout); what stays here
     is what must match regardless: the box, the wrapping, no ink. */
  .mirror {
    position: absolute;
    inset: 0;
    z-index: -1;
    box-sizing: border-box;
    border-style: solid;
    border-color: transparent;
    overflow: hidden;
    white-space: pre-wrap;
    overflow-wrap: break-word;
    word-break: normal;
    color: transparent;
    pointer-events: none;
    user-select: none;
  }
  /* The bubble's mention pill (UserText), in the draft: a tint that grows
     past the glyphs by a hair without moving a single one. */
  mark {
    color: transparent;
    background: color-mix(in srgb, var(--accent) 13%, transparent);
    border-radius: 4px;
    box-shadow: 0 0 0 1.5px color-mix(in srgb, var(--accent) 13%, transparent);
    box-decoration-break: clone;
    -webkit-box-decoration-break: clone;
  }
  /* Inverted ink reads as chrome in both themes; it waits a beat so a
     pointer passing over the text shows nothing. */
  .tip {
    position: absolute;
    z-index: 3;
    display: flex;
    flex-direction: column;
    gap: 2px;
    width: max-content;
    max-width: min(420px, calc(100% - 8px));
    padding: 5px 9px;
    border-radius: 7px;
    background: var(--fg);
    color: var(--bg);
    font-size: var(--text-xs);
    line-height: 1.35;
    box-shadow: 0 4px 16px rgba(0, 0, 0, 0.18);
    pointer-events: none;
    animation: tip-in 0.12s ease 0.3s both;
  }
  .tip-name {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-weight: 500;
    overflow-wrap: anywhere;
  }
  .tip-path {
    font-family: var(--mono, monospace);
    font-size: 11px;
    opacity: 0.7;
    overflow-wrap: anywhere;
  }
  @keyframes tip-in {
    from {
      opacity: 0;
    }
    to {
      opacity: 1;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .tip {
      animation-duration: 0s;
    }
  }
</style>
