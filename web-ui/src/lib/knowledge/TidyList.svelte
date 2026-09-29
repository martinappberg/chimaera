<script lang="ts">
  /**
   * Tidy up: factual inconsistencies in the recorded knowledge, as the
   * provider found them — never a judgment of a status. Knowledge never
   * edits the files; "Ask an agent" drafts the provider's full request into
   * an agent's chat composer, where the user reads it and sends it (or not).
   */
  import type { TidyRow } from "../workspace/knowledge";
  import { resolveRef, type KnowledgeIndex } from "./entries";

  interface Props {
    rows: TidyRow[];
    idx: KnowledgeIndex;
    title: string;
    /** Draft `text` into an agent's composer (shared/askAgent); the
     *  agent's name, or null when there is no agent to ask here. */
    onAsk: (text: string) => string | null;
    onOpen: (ekey: string) => void;
  }

  let { rows, idx, title, onAsk, onOpen }: Props = $props();

  /** Per row: what the last ask did — by the row's own words, so a refresh
   *  that drops a fixed row doesn't hand its note to the next. */
  let outcome = $state<Record<string, { ok: boolean; text: string }>>({});
  const rowKey = (r: TidyRow): string => `${r.kind}\u0000${r.text}`;

  function ask(r: TidyRow, text: string): void {
    const name = onAsk(text);
    outcome = {
      ...outcome,
      [rowKey(r)]:
        name !== null
          ? { ok: true, text: `Drafted into ${name}'s message box — read it, then send.` }
          : { ok: false, text: "No agent chat is open in this workspace — start one, then ask again." },
    };
  }
</script>

<section class="tidy" aria-labelledby="tidy-title">
  <h2 id="tidy-title" class="lbl">{title} <span class="n">{rows.length}</span></h2>
  <p class="lead">
    Things in the recorded knowledge that don't line up. Knowledge never edits these files — <em>Ask an agent</em> drafts the fix
    into a chat for you to send.
  </p>
  {#if rows.length === 0}
    <p class="quiet">Nothing to tidy.</p>
  {:else}
    <div class="rows">
      {#each rows as r, i (i)}
        <div class="row">
          <div class="what">
            <span class="text">{r.text}</span>
            {#if r.refs.length > 0}
              <span class="refs">
                {#each r.refs.slice(0, 24) as ref, j (j)}
                  {@const found = resolveRef(idx, ref)}
                  {#if found.length > 0}
                    <button class="rchip" onclick={() => onOpen(found[0].ekey)}>{ref.id}</button>
                  {:else}
                    <span class="rchip dead">{ref.id}</span>
                  {/if}
                {/each}
                {#if r.refs.length > 24}<span class="more">+{r.refs.length - 24}</span>{/if}
              </span>
            {/if}
            {#if outcome[rowKey(r)] !== undefined}
              {@const o = outcome[rowKey(r)]}
              <span class="outcome" class:bad={!o.ok}>{o.text}</span>
            {/if}
          </div>
          {#if r.ask !== ""}
            <button class="ask" onclick={() => ask(r, r.ask)} title={r.ask}>Ask an agent</button>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</section>

<style>
  .tidy {
    max-width: 900px;
  }
  .lbl {
    margin: 0 0 6px;
    display: flex;
    gap: 8px;
    align-items: baseline;
    font-size: 10.5px;
    font-weight: 600;
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .n {
    font-family: var(--mono);
    letter-spacing: 0;
  }
  .lead {
    margin: 0 0 12px;
    font-size: var(--text-sm);
    color: var(--muted);
    max-width: 70ch;
    line-height: 1.5;
  }
  .quiet {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .rows {
    border-top: 1px solid var(--edge);
  }
  .row {
    display: flex;
    gap: 8px 18px;
    align-items: center;
    flex-wrap: wrap;
    padding: 12px 4px;
    border-bottom: 1px solid var(--edge);
  }
  .what {
    flex: 1 1 360px;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .text {
    font-size: var(--text-sm);
    line-height: 1.5;
    overflow-wrap: anywhere;
  }
  .refs {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .rchip {
    font-family: var(--mono);
    font-size: 11px;
    line-height: 17px;
    border: 1px solid var(--edge);
    background: var(--overlay-bg);
    color: var(--accent);
    border-radius: 5px;
    padding: 0 5px;
    cursor: pointer;
  }
  .rchip.dead {
    color: var(--muted);
    cursor: default;
  }
  .rchip:not(.dead):hover {
    border-color: var(--accent);
  }
  .more {
    font-size: 11px;
    color: var(--muted);
  }
  .outcome {
    font-size: var(--text-xs);
    color: var(--accent);
  }
  .outcome.bad {
    color: var(--warn);
  }
  .ask {
    border: 1px solid var(--edge);
    background: var(--overlay-bg);
    color: var(--fg);
    border-radius: 7px;
    padding: 5px 12px;
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
    white-space: nowrap;
  }
  .ask:hover {
    border-color: var(--accent);
    color: var(--accent);
  }
</style>
