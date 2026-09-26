<script lang="ts">
  import { extractFileRefs, revealOf, type FileRef } from "../shared/fileRef";
  import MathText from "./MathText.svelte";
  import { splitUserMath } from "./math";
  import { menuPoint, reopenResolution, type OpenPathFn, type PathResolver, type Resolution } from "./paths";

  /**
   * The user's own message text: plain (never markdown — prompts are not
   * documents), whitespace preserved, with recognized LaTeX spans rendered
   * as math and @-mentions / real paths made clickable through the same
   * resolver as agent prose. Mentions render as quiet pills — the visual
   * receipt that the tag landed.
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
  }

  function plain(t: string): Token {
    return { text: t, ref: null, mention: false, math: null };
  }

  function appendPlain(out: Token[], run: string) {
    let last = 0;
    for (const f of extractFileRefs(run)) {
      if (f.start > last) out.push(plain(run.slice(last, f.start)));
      const t = run.slice(f.start, f.end);
      out.push({ text: t, ref: f.ref, mention: t.startsWith("@"), math: null });
      last = f.end;
    }
    if (last < run.length) out.push(plain(run.slice(last)));
  }

  const tokens = $derived.by(() => {
    const out: Token[] = [];
    for (const run of splitUserMath(text)) {
      if (run.kind === "text") {
        appendPlain(out, run.text);
      } else {
        out.push({ text: "", ref: null, mention: false, math: { source: run.source, display: run.display } });
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
<span class="usertext"
  >{#each tokens as t, i (i)}{@const res = resFor(t)}{#if t.math !== null}<MathText source={t.math.source} display={t.math.display} />{:else if res !== undefined}<button
        class="path"
        class:mention={t.mention}
        class:ambiguous={res.state === "ambiguous"}
        title={titleFor(t, res)}
        onclick={(e) => activate(e, t, res)}>{t.text}</button>{:else}{t.text}{/if}{/each}</span
>

<style>
  .usertext {
    white-space: pre-wrap;
    word-break: break-word;
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
  .path.mention {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    border-radius: 5px;
    padding: 0 4px;
    text-decoration: none;
  }
  .path:hover {
    color: var(--accent);
    text-decoration-color: var(--accent);
  }
  .path.mention:hover {
    background: color-mix(in srgb, var(--accent) 20%, transparent);
  }
  .path.ambiguous {
    text-decoration-style: dashed;
  }
</style>
