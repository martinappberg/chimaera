<script lang="ts">
  import FileIcon from "../shared/FileIcon.svelte";
  import FolderIcon from "../shared/FolderIcon.svelte";
  import { extractFileRefs, revealOf, type FileRef } from "../shared/fileRef";
  import { quoteRuns } from "../shared/reference";
  import { chipLabels } from "./artifacts";
  import MathText from "./MathText.svelte";
  import { splitUserMath } from "./math";
  import {
    mentionChipLabel,
    menuPoint,
    reopenResolution,
    uploadName,
    type OpenPathFn,
    type PathResolver,
    type Resolution,
  } from "./paths";

  /**
   * The user's own message text: plain (never markdown — prompts are not
   * documents), whitespace preserved, with recognized LaTeX spans rendered
   * as math and @-mentions / real paths made clickable through the same
   * resolver as agent prose. An `@` mention reads as a file chip — its
   * icon and name, like a document the prose names, where it was written —
   * the receipt that the tag landed, which a long path would wrap and
   * break; a desktop drop's machine-made landing-pad path never shows at
   * all. The mention's whole text stays in its tooltip, in a copy, and in
   * what the agent got. A path written without `@` stays as written. A
   * quoted passage (`>`-led lines, as the transcript's quote chip writes
   * them) reads muted, markers kept, so the words about it stand apart.
   */
  interface Props {
    text: string;
    onOpenPath?: OpenPathFn;
    resolvePaths?: PathResolver;
  }

  let { text, onOpenPath, resolvePaths }: Props = $props();

  interface Token {
    /** Verbatim text: a plain run, or exactly the reference's span. */
    text: string;
    ref: FileRef | null;
    mention: boolean;
    math: { source: string; display: boolean } | null;
    /** Inside a quoted passage. */
    quote: boolean;
  }

  function appendPlain(out: Token[], run: string, quote: boolean) {
    const plain = (t: string): Token => ({ text: t, ref: null, mention: false, math: null, quote });
    let last = 0;
    for (const f of extractFileRefs(run)) {
      if (f.start > last) out.push(plain(run.slice(last, f.start)));
      const t = run.slice(f.start, f.end);
      out.push({ text: t, ref: f.ref, mention: t.startsWith("@"), math: null, quote });
      last = f.end;
    }
    if (last < run.length) out.push(plain(run.slice(last)));
  }

  const tokens = $derived.by(() => {
    const out: Token[] = [];
    for (const passage of quoteRuns(text)) {
      for (const run of splitUserMath(passage.text)) {
        if (run.kind === "text") {
          appendPlain(out, run.text, passage.quote);
        } else {
          const math = { source: run.source, display: run.display };
          out.push({ text: "", ref: null, mention: false, math, quote: passage.quote });
        }
      }
    }
    return out;
  });

  /** Bumped when the resolver answered something linkable — the template's
   *  cue to re-read its (non-reactive) cache. */
  let answered = $state(0);
  /** Bumped when a turn end expired the resolver's misses: ask again. */
  let expired = $state(0);
  $effect(() => {
    const resolver = resolvePaths;
    if (resolver === undefined) return;
    return resolver.onExpire(() => (expired += 1));
  });
  // Sent messages are immutable, so this asks once per mount — and again
  // after a turn end, for whatever was a miss (a file created since).
  $effect(() => {
    void expired;
    const resolver = resolvePaths;
    const candidates = [...new Set(tokens.flatMap((t) => (t.ref !== null ? [t.ref.path] : [])))];
    if (candidates.length === 0 || resolver === undefined) return;
    let stale = false;
    void resolver.resolve(candidates).then((linkable) => {
      if (!stale && linkable) answered += 1;
    });
    return () => {
      stale = true;
    };
  });

  function resFor(t: Token): Resolution | undefined {
    void answered;
    if (t.ref === null || resolvePaths === undefined) return undefined;
    const res = resolvePaths.peek(t.ref.path);
    return res?.state === "miss" ? undefined : res;
  }

  /** Each mentioned file's name, widened with its folders only where two
   *  mentioned files in this message share one. Uploads read by their own
   *  name (`mentionChipLabel`). */
  const names = $derived(
    chipLabels([
      ...new Set(
        tokens.flatMap((t) =>
          t.mention && t.ref !== null && uploadName(t.ref.path) === null ? [t.ref.path] : [],
        ),
      ),
    ]),
  );

  /** A mention's chip text (no `@` — the icon says "file"), or null for
   *  any other token. */
  function chipLabel(t: Token): string | null {
    if (!t.mention || t.ref === null) return null;
    const segments = t.ref.path.split("/").filter((s) => s !== "");
    return mentionChipLabel(t.text, t.ref.path, names.get(t.ref.path) ?? segments.at(-1) ?? t.ref.path);
  }

  function isDir(t: Token, res: Resolution | undefined): boolean {
    if (res?.state === "hit") return res.hit.kind === "dir";
    return t.text.replace(/"$/, "").endsWith("/");
  }

  /** The resolver answered that the mentioned file is not there. */
  function missing(t: Token): boolean {
    void answered;
    return t.ref !== null && resolvePaths?.peek(t.ref.path)?.state === "miss";
  }

  let root = $state<HTMLElement | null>(null);

  /** Copying inside this message copies what was sent: a shortened mention
   *  goes back to its whole text (`data-full`). A selection reaching past
   *  the message keeps the browser's own copy. */
  function onCopy(e: ClipboardEvent): void {
    const sel = window.getSelection();
    if (root === null || sel === null || sel.rangeCount === 0 || e.clipboardData === null) return;
    const range = sel.getRangeAt(0);
    if (!root.contains(range.commonAncestorContainer)) return;
    const frag = range.cloneContents();
    const short = frag.querySelectorAll<HTMLElement>("[data-full]");
    if (short.length === 0) return;
    for (const el of short) el.replaceWith(el.dataset.full ?? "");
    e.clipboardData.setData("text/plain", frag.textContent ?? "");
    e.preventDefault();
  }

  function titleFor(t: Token, res: Resolution): string {
    if (res.state === "ambiguous") return `${t.text} matches ${res.matches.length} files — choose one`;
    if (res.state === "hit" && res.hit.kind === "dir") return `browse ${t.text} in the finder`;
    return `open ${t.text}${t.ref?.line !== undefined ? ` at line ${t.ref.line}` : ""} in a pane`;
  }

  /** Open what the daemon answers NOW (the standing answer may predate a
   *  move or a delete); re-read the cache after, so a reference that is
   *  gone stops looking clickable. */
  function activate(e: MouseEvent, t: Token, res: Resolution) {
    if (onOpenPath === undefined || t.ref === null) return;
    const opts = {
      split: e.metaKey || e.ctrlKey,
      reveal: revealOf(t.ref),
      at: menuPoint(e, e.currentTarget as Element),
      label: (p: string) => resolvePaths?.label(p) ?? p,
    };
    void reopenResolution(resolvePaths, t.ref.path, res, onOpenPath, opts).then((now) => {
      if (now !== res) answered += 1; // a fresh answer (unanswered: the stamp stands)
    });
  }
</script>

<!-- Whitespace-tight on purpose: the container is pre-wrap, so any template
     newline/indent between blocks would render as literal extra spacing. -->
<!-- prettier-ignore -->
<span class="usertext" bind:this={root} oncopy={onCopy}
  >{#snippet chip(t: Token, label: string, res: Resolution | undefined)}{#if isDir(t, res)}<FolderIcon size={12} />{:else}<FileIcon path={t.ref?.path ?? label} size={12} />{/if}<span class="chip-name">{label}</span>{/snippet}{#each tokens as t, i (i)}{@const res = resFor(t)}{@const label = chipLabel(t)}{#if t.math !== null}<MathText source={t.math.source} display={t.math.display} />{:else if label !== null && res !== undefined}<button
        class="chip"
        class:quote={t.quote}
        class:ambiguous={res.state === "ambiguous"}
        title={titleFor(t, res)}
        data-full={t.text}
        onclick={(e) => activate(e, t, res)}>{@render chip(t, label, res)}</button>{:else if label !== null}<span
        class="chip inert"
        class:quote={t.quote}
        class:missing={missing(t)}
        title={missing(t) ? `${t.text} · not found` : t.text}
        data-full={t.text}>{@render chip(t, label, res)}</span>{:else if res !== undefined}<button
        class="path"
        class:quote={t.quote}
        class:ambiguous={res.state === "ambiguous"}
        title={titleFor(t, res)}
        onclick={(e) => activate(e, t, res)}>{t.text}</button>{:else if t.quote}<span class="quote">{t.text}</span>{:else}{t.text}{/if}{/each}</span
>

<style>
  .usertext {
    white-space: pre-wrap;
    word-break: break-word;
  }
  .quote {
    color: var(--muted);
  }
  .path {
    display: inline;
    background: none;
    border: none;
    padding: 0;
    margin: 0;
    color: inherit;
    font: inherit;
    font-family: var(--mono, monospace);
    font-size: 0.92em;
    cursor: pointer;
    text-decoration: underline dotted;
    text-underline-offset: 2px;
    text-decoration-color: color-mix(in srgb, var(--fg) 40%, transparent);
    transition: color 0.12s ease;
    word-break: break-all;
  }
  .path.quote {
    color: var(--muted);
  }
  .path:hover {
    color: var(--accent);
    text-decoration-color: var(--accent);
  }
  .path.ambiguous {
    text-decoration-style: dashed;
  }
  /* A mention reads as a file, not code: the doc chip's icon + name in the
     message's own face, one unbroken token (a long name ellipsizes rather
     than wrapping mid-word). */
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    max-width: 100%;
    vertical-align: bottom;
    margin: 0;
    padding: 0 5px 0 4px;
    border: none;
    border-radius: 5px;
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    color: inherit;
    font: inherit;
    font-size: 0.95em;
    cursor: pointer;
    transition:
      color 0.12s ease,
      background-color 0.12s ease;
  }
  .chip:hover {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 20%, transparent);
  }
  .chip.ambiguous {
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 45%, transparent);
  }
  .chip-name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* Not answered yet, or not a link here: the same chip, without the hover. */
  .chip.inert {
    cursor: default;
  }
  .chip.inert:hover {
    color: inherit;
    background: color-mix(in srgb, var(--accent) 12%, transparent);
  }
  /* Not (or no longer) on disk — uploads end with their session — or
     inside a quoted passage, which reads muted throughout. */
  .chip.missing,
  .chip.missing:hover,
  .chip.quote {
    color: var(--muted);
  }
</style>
