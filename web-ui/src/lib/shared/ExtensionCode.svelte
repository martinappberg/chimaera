<script lang="ts">
  import { boundedCode, sourceCodeRows, unifiedCodeRows } from "./extensionCode";

  /** Presentation only: paths select a grammar; this component never reads files. */
  let { source, language, path, format = "source", startLine, wrap = "wrap" }: {
    source: string;
    language?: string;
    path?: string;
    format?: "source" | "diff";
    startLine?: number;
    wrap?: "wrap" | "truncate-end";
  } = $props();

  type Token = { text: string; classes: string };
  let tokens = $state<Token[][]>([]);
  const bounded = $derived(boundedCode(source));
  const diffRows = $derived(format === "diff" ? unifiedCodeRows(bounded.source) : null);
  const rows = $derived(diffRows ?? sourceCodeRows(bounded.source, format === "diff" ? undefined : startLine));
  const numbered = $derived(rows.some((row) => row.newLine !== undefined || row.oldLine !== undefined));
  const gutterWidth = $derived(rows.reduce((width, row) => Math.max(width, String(row.oldLine ?? "").length, String(row.newLine ?? "").length), 3));

  $effect(() => {
    const text = bounded.source;
    const lang = language;
    const filename = path?.split("/").at(-1);
    tokens = [];
    if (diffRows !== null || (!lang && !filename)) return;
    let stale = false;
    void (async () => {
      const [{ parserFor, codeHighlighter }, { highlightCode }] = await Promise.all([
        import("../previews/highlight"), import("@lezer/highlight"),
      ]);
      let parser;
      if (lang) parser = await parserFor(lang);
      else {
        const [{ LanguageDescription }, { languages }] = await Promise.all([
          import("@codemirror/language"), import("../previews/languages"),
        ]);
        const description = LanguageDescription.matchFilename(languages, filename ?? "");
        parser = description ? (await description.load()).language.parser : null;
      }
      if (stale || !parser) return;
      const highlighted: Token[][] = [[]];
      highlightCode(text, parser.parse(text), codeHighlighter,
        (value, classes) => highlighted[highlighted.length - 1].push({ text: value, classes }),
        () => highlighted.push([]));
      if (!stale) tokens = highlighted;
    })().catch(() => { /* A missing grammar leaves literal source visible. */ });
    return () => { stale = true; };
  });
</script>

<div class="extension-code" class:diff={diffRows !== null} class:truncate={wrap === "truncate-end"} style:--gutter-width={`${gutterWidth}ch`} role="group" aria-label={diffRows !== null ? "Code changes" : "Code excerpt"}>
  {#each rows as row, index (index)}
    {#if row.kind === "hunk"}
      <div class="hunk" title={row.text} aria-label="Next changed section">⋯</div>
    {:else}
      <div class="line" class:added={row.kind === "add"} class:removed={row.kind === "remove"}>
        {#if diffRows !== null}<span class="gutter" aria-hidden="true">{row.oldLine ?? ""}</span>{/if}
        {#if numbered}<span class="gutter" aria-hidden="true">{row.newLine ?? ""}</span>{/if}
        {#if diffRows !== null}<span class="marker" aria-label={row.kind === "add" ? "Added" : row.kind === "remove" ? "Removed" : undefined}>{row.kind === "add" ? "+" : row.kind === "remove" ? "−" : " "}</span>{/if}
        <code title={wrap === "truncate-end" ? row.text : undefined}>{#if tokens[index]}{#each tokens[index] as token, tokenIndex (tokenIndex)}<span class={token.classes}>{token.text}</span>{/each}{:else}{row.text}{/if}</code>
      </div>
    {/if}
  {/each}
  {#if bounded.truncated}<div class="limit" role="status">Code preview truncated.</div>{/if}
</div>

<style>
  .extension-code { min-width: 0; max-width: 100%; padding-block: 8px; border-radius: 5px; background: color-mix(in srgb, var(--fg) 3%, var(--term-bg)); color: var(--fg); font-family: var(--mono); font-size: var(--text-sm); line-height: 1.6; white-space: normal; tab-size: 4; }
  .line { display: flex; min-width: 0; padding-inline: 10px; min-height: 1.6em; }
  code { display: block; flex: 1; min-width: 0; font: inherit; white-space: pre-wrap; overflow-wrap: anywhere; }
  .truncate code { white-space: pre; overflow: hidden; text-overflow: ellipsis; }
  .gutter { box-sizing: content-box; flex: none; width: var(--gutter-width); padding-inline-end: 12px; color: color-mix(in srgb, var(--muted) 80%, transparent); font-variant-numeric: tabular-nums; text-align: end; white-space: pre; user-select: none; }
  .diff .gutter { padding-inline-end: 8px; }
  .marker { flex: none; width: 2ch; user-select: none; white-space: pre; color: var(--muted); }
  .added { background: color-mix(in srgb, var(--git-added) 9%, transparent); }
  .removed { background: color-mix(in srgb, var(--git-deleted) 9%, transparent); }
  .added .marker { color: var(--git-added); }
  .removed .marker { color: var(--git-deleted); }
  .hunk { padding-inline: 10px; margin-block: 4px; color: var(--muted); background: color-mix(in srgb, var(--fg) 3%, transparent); user-select: none; }
  .limit { padding: 6px 10px 0; color: var(--muted); font-family: var(--ui-font, sans-serif); font-size: var(--text-xs); }
  code :global(.hl-keyword) { color: var(--syn-keyword); }
  code :global(.hl-string) { color: var(--syn-string); }
  code :global(.hl-comment) { color: var(--syn-comment); font-style: italic; }
  code :global(.hl-number) { color: var(--syn-number); }
  code :global(.hl-type) { color: var(--syn-type); }
  code :global(.hl-func) { color: var(--syn-func); }
  code :global(.hl-def) { color: var(--syn-def); }
  code :global(.hl-prop) { color: var(--syn-prop); }
  code :global(.hl-invalid) { color: var(--err); }
</style>
