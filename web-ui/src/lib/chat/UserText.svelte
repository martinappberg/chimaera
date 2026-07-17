<script lang="ts">
  import MathText from "./MathText.svelte";
  import { splitUserMath } from "./math";
  import { pathCandidate, trimPathWord, type PathHit, type ResolvePaths } from "./paths";

  /**
   * The user's own message text: plain (never markdown — prompts are not
   * documents), whitespace preserved, with recognized LaTeX spans rendered
   * as math and @-mentions / real paths made clickable through the same
   * /fs/validate flow as agent prose. Mentions render as quiet pills — the
   * visual receipt that the tag landed.
   */
  interface Props {
    text: string;
    onOpenPath?: (path: string, kind: "file" | "dir") => void;
    resolvePaths?: ResolvePaths;
  }

  let { text, onOpenPath, resolvePaths }: Props = $props();

  interface Token {
    /** Verbatim text (separators included for plain runs). */
    text: string;
    /** Residual punctuation after a clickable head. */
    tail: string;
    /** Validation key (mentions strip the "@" and any trailing slash). */
    candidate: string | null;
    mention: boolean;
    math: { source: string; display: boolean } | null;
  }

  function classify(word: string): Token {
    if (word.startsWith("@") && word.length > 1 && !word.startsWith("@term:")) {
      const { head, tail } = trimPathWord(word.slice(1));
      const candidate = head.replace(/\/+$/, "");
      if (candidate.length > 0) {
        return { text: `@${head}`, tail, candidate, mention: true, math: null };
      }
    }
    const { head, tail } = trimPathWord(word);
    if (pathCandidate(head)) {
      return { text: head, tail, candidate: head, mention: false, math: null };
    }
    return { text: word, tail: "", candidate: null, mention: false, math: null };
  }

  function appendPlain(out: Token[], plain: string) {
    let last = 0;
    for (const m of plain.matchAll(/\S+/g)) {
      if (m.index > last) {
        out.push({
          text: plain.slice(last, m.index),
          tail: "",
          candidate: null,
          mention: false,
          math: null,
        });
      }
      out.push(classify(m[0]));
      last = m.index + m[0].length;
    }
    if (last < plain.length) {
      out.push({
        text: plain.slice(last),
        tail: "",
        candidate: null,
        mention: false,
        math: null,
      });
    }
  }

  const tokens = $derived.by(() => {
    const out: Token[] = [];
    for (const run of splitUserMath(text)) {
      if (run.kind === "text") {
        appendPlain(out, run.text);
      } else {
        out.push({
          text: "",
          tail: "",
          candidate: null,
          mention: false,
          math: { source: run.source, display: run.display },
        });
      }
    }
    return out;
  });

  let hits = $state<Map<string, PathHit>>(new Map());
  // Sent messages are immutable, so this resolves once per mount (replays
  // re-mount the component and re-validate — deletions age out naturally).
  $effect(() => {
    const candidates = [...new Set(tokens.filter((t) => t.candidate !== null).map((t) => t.candidate!))];
    if (candidates.length === 0 || resolvePaths === undefined) return;
    let stale = false;
    void resolvePaths(candidates)
      .then((res) => {
        if (!stale) hits = res;
      })
      .catch(() => {});
    return () => {
      stale = true;
    };
  });

  function hitFor(t: Token): PathHit | undefined {
    return t.candidate !== null ? hits.get(t.candidate) : undefined;
  }
</script>

<!-- Whitespace-tight on purpose: the container is pre-wrap, so any template
     newline/indent between blocks would render as literal extra spacing. -->
<!-- prettier-ignore -->
<span class="usertext"
  >{#each tokens as t, i (i)}{@const hit = hitFor(t)}{#if t.math !== null}<MathText source={t.math.source} display={t.math.display} />{:else if hit !== undefined}<button
        class="path"
        class:mention={t.mention}
        title={hit.kind === "dir" ? `browse ${t.text} in the finder` : `open ${t.text} in a pane`}
        onclick={() => onOpenPath?.(hit.path, hit.kind)}>{t.text}</button>{t.tail}{:else}{t.text}{t.tail}{/if}{/each}</span
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
</style>
