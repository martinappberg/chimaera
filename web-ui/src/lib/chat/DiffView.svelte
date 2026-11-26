<script lang="ts">
  /**
   * One file's change as changed lines: removals and additions with a little
   * context, unchanged stretches folded. Shared by a tool row's body and the
   * approval card, so an edit reads the same before and after it is allowed.
   */
  import type { ToolContent } from "./store.svelte";
  import ChangeSize from "./ChangeSize.svelte";
  import { diffLines, diffStat } from "./lineDiff";

  interface Props {
    diff: ToolContent;
    /** Name the file above the lines (off when a caller's header does). */
    showPath?: boolean;
    /** The path as the reader should see it (workspace-relative). */
    label?: (path: string) => string;
    onOpen?: (path: string, e: MouseEvent, anchor: Element) => void;
  }

  let { diff, showPath = true, label, onOpen }: Props = $props();

  const rows = $derived(diffLines(diff.old_text ?? "", diff.new_text ?? ""));
  const stat = $derived(diffStat(rows));
  const SIGN = { ctx: " ", del: "−", add: "+", gap: "" } as const;
</script>

<div class="diff">
  {#if diff.path && showPath}
    <div class="diff-head">
      {#if onOpen !== undefined}
        <button class="diff-path" onclick={(e) => onOpen?.(diff.path ?? "", e, e.currentTarget)}
          >{label?.(diff.path) ?? diff.path}</button
        >
      {:else}
        <span class="diff-path static">{label?.(diff.path) ?? diff.path}</span>
      {/if}
      {#if stat.added + stat.removed > 0}<ChangeSize added={stat.added} removed={stat.removed} />{/if}
    </div>
  {/if}
  <div class="lines">
    {#each rows as row, i (i)}
      {#if row.kind === "gap"}
        <div class="gap" title={`${row.text} unchanged line${row.text === "1" ? "" : "s"}`}>⋯</div>
      {:else}
        <div class="line {row.kind}"><span class="sign" aria-hidden="true">{SIGN[row.kind]}</span><span class="text">{row.text}</span></div>
      {/if}
    {/each}
  </div>
  {#if diff.truncated}
    <span class="trunc">truncated — open the file for the full change</span>
  {/if}
</div>

<style>
  .diff + :global(.diff) {
    margin-top: 8px;
  }
  .diff-head {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding-bottom: 4px;
    font-family: var(--mono, monospace);
    font-size: var(--text-sm);
  }
  .diff-path {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    background: none;
    border: none;
    padding: 0;
    color: var(--accent-text);
    font: inherit;
    cursor: pointer;
    text-align: left;
  }
  .diff-path.static {
    cursor: default;
  }
  button.diff-path:hover {
    text-decoration: underline;
  }
  .lines {
    border-radius: 4px;
    overflow: hidden;
    font-family: var(--mono, monospace);
    font-size: var(--text-sm);
    line-height: 1.5;
  }
  .line {
    display: flex;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .sign {
    flex: none;
    width: 2ch;
    text-align: center;
    color: var(--muted);
    user-select: none;
  }
  .text {
    flex: 1;
    min-width: 0;
    padding-right: 6px;
  }
  .line.ctx {
    color: var(--muted);
  }
  .line.del {
    background: color-mix(in srgb, var(--err) 12%, transparent);
  }
  .line.del .sign {
    color: var(--err);
  }
  .line.add {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
  }
  .line.add .sign {
    color: var(--accent-text);
  }
  .gap {
    padding-left: 2ch;
    color: var(--muted);
    line-height: 1.1;
    user-select: none;
  }
  .trunc {
    display: block;
    margin-top: 4px;
    color: var(--muted);
    font-size: var(--text-sm);
  }
</style>
