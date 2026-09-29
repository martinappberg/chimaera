<script lang="ts">
  /**
   * One section's entries: filter chips that fit what is there, then one
   * dense row per entry — id, title, date, and underneath its standing, its
   * status as written and what it cites. Findings group by topic, to-dos by
   * status (done folded away), the rest newest first. Selecting a row reads
   * it beside the list. Rows are the shared `Row` (ui/1's row).
   */
  import Badge from "../shared/ui/Badge.svelte";
  import Row from "../shared/ui/Row.svelte";
  import type { BadgeSpec } from "../shared/ui/tone";
  import type { KnowledgeLabels } from "../workspace/knowledge";
  import { qualifiedId, type Entry, type KnowledgeIndex } from "./entries";
  import { todoRest, type ListFilter, type ListGroup } from "./list";
  import { isRetired, priorityTone, stateLabel, todoGroup } from "./overview";
  import StatusMark from "./StatusMark.svelte";

  interface Props {
    label: string;
    groups: ListGroup[];
    filters: ListFilter[];
    filter: string;
    onFilter: (id: string) => void;
    idx: KnowledgeIndex;
    labels: KnowledgeLabels;
    selected: string | null;
    onSelect: (ekey: string) => void;
    /** Folded groups the user opened (by group key). */
    unfolded: Set<string>;
    onUnfold: (key: string) => void;
    query: string;
  }

  let { label, groups, filters, filter, onFilter, idx, labels, selected, onSelect, unfolded, onUnfold, query }: Props =
    $props();

  const total = $derived(groups.reduce((n, g) => n + g.entries.length, 0));

  function badgesOf(e: Entry): BadgeSpec[] {
    const out: BadgeSpec[] = [];
    const st = stateLabel(e);
    if (st !== null) out.push({ text: st.text, tone: st.tone });
    for (const a of e.amends) out.push({ text: `${a.kind} ${a.id}`, tone: "warn" });
    if (e.id !== "" && (idx.byId.get(e.id.toUpperCase())?.filter((x) => x.kind === e.kind).length ?? 0) > 1) {
      out.push({ text: qualifiedId(idx, e), tone: "neutral" });
    }
    if (e.kind === "todo" && e.todo !== undefined) {
      const g = todoGroup(e.todo);
      const s = e.todo.status.trim();
      if (g === "open" && s !== "" && s.toLowerCase() !== "open") out.push({ text: s.replace(/\*/g, "") });
    }
    return out;
  }
</script>

<div class="list" role="listbox" aria-label={label}>
  {#if filters.length > 1}
    <div class="filters" role="toolbar" aria-label="Filter">
      {#each filters as f (f.id)}
        <button class="fchip" aria-pressed={filter === f.id} onclick={() => onFilter(f.id)}>
          {f.label}<span class="fn">{f.count}</span>
        </button>
      {/each}
    </div>
  {/if}

  {#if total === 0}
    <p class="none">{query.trim() !== "" ? `Nothing here matches “${query.trim()}”.` : "Nothing here with this filter."}</p>
  {/if}

  {#each groups as g (g.key)}
    {#if g.title !== ""}
      <div class="ghead">
        <span class="gt" class:mono={g.mono}>{g.title}</span>
        <span class="gn">{g.entries.length}</span>
        {#if g.subtitle}<span class="gs">{g.subtitle}</span>{/if}
      </div>
    {/if}
    {#if g.folded && !unfolded.has(g.key) && query.trim() === ""}
      <button class="unfold" onclick={() => onUnfold(g.key)}>Show {g.entries.length} {g.title.toLowerCase()}</button>
    {:else}
      {#each g.entries as e (e.ekey)}
        <div class="rowwrap" class:todo={e.kind === "todo"}>
          <Row
            title={e.title || e.id || "(untitled)"}
            subtitle={e.kind === "todo" && e.todo !== undefined ? todoRest(e.todo) : undefined}
            badges={badgesOf(e)}
            selected={selected === e.ekey}
            retired={isRetired(e)}
            role="option"
            attrs={{ "data-ekey": e.ekey }}
            onclick={() => onSelect(e.ekey)}
          >
            {#snippet leading()}
              {#if e.kind === "todo" && e.todo !== undefined}
                <span class="prio {priorityTone(e.todo.priority)}" title={e.todo.priority || "no priority"}></span>
              {:else if e.kind === "learning"}
                <span class="cat">{e.learning?.category && e.learning.category !== "other" ? e.learning.category : ""}</span>
              {:else if e.kind === "session"}
                <span class="mono">{e.date.slice(5, 10)}</span>
              {:else}
                <span class="mono">{e.id}</span>
              {/if}
            {/snippet}
            {#snippet trailing()}
              {#if e.kind !== "session"}{e.date.slice(2, 10)}{/if}
            {/snippet}
            {#snippet meta()}
              {#if e.kind === "finding" || e.kind === "decision"}
                <StatusMark stated={e.stated} {labels} short />
              {:else if e.kind === "convention" && e.stated !== ""}
                <span>{e.stated}</span>
              {/if}
              {#if e.kind === "finding" && e.finding !== undefined && e.finding.ledger.length > 0}
                <span class="strip" title="{e.finding.ledger.length} evidence ledger row{e.finding.ledger.length === 1 ? '' : 's'}">
                  {#each e.finding.ledger.slice(0, 8) as r, i (i)}<i class={r.direction}></i>{/each}
                </span>
              {:else if e.cites.length > 0}
                <span>cites {e.cites.length}</span>
              {/if}
              {#if e.kind === "todo" && e.todo !== undefined && e.todo.id !== ""}<span class="mono">{e.todo.id}</span>{/if}
              {#if e.kind === "session" && e.session !== undefined}
                {#if e.session.branch}<span class="mono">{e.session.branch}</span>{/if}
                {#if e.session.duration}<span>{e.session.duration}</span>{/if}
                {#if e.session.status}<Badge text={e.session.status} />{/if}
              {/if}
              {#if e.recordedBy !== null}<span>{e.recordedBy.name}</span>{/if}
            {/snippet}
          </Row>
        </div>
      {/each}
    {/if}
  {/each}
</div>

<style>
  .list {
    display: flex;
    flex-direction: column;
    min-width: 0;
    padding-bottom: 24px;
  }
  .rowwrap {
    --row-lead: 58px;
    display: contents;
  }
  .rowwrap.todo {
    --row-lead: 12px;
  }
  .filters {
    position: sticky;
    top: 0;
    z-index: 1;
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 10px 14px;
    background: var(--bg);
    border-bottom: 1px solid var(--edge);
  }
  .fchip {
    border: 1px solid var(--edge);
    background: var(--overlay-bg);
    color: var(--muted);
    border-radius: 999px;
    padding: 2px 10px;
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
    display: inline-flex;
    gap: 6px;
    align-items: baseline;
  }
  .fchip[aria-pressed="true"] {
    color: var(--fg);
    border-color: color-mix(in srgb, var(--fg) 55%, transparent);
  }
  .fn {
    font-family: var(--mono);
    font-size: 10.5px;
    color: var(--muted);
  }
  .none {
    margin: 0;
    padding: 18px 14px;
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .ghead {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 2px 8px;
    padding: 14px 14px 4px;
  }
  .gt {
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--fg);
  }
  .gt.mono {
    font-family: var(--mono);
    font-weight: 500;
  }
  .gn {
    font-family: var(--mono);
    font-size: 10.5px;
    color: var(--muted);
  }
  .gs {
    flex-basis: 100%;
    font-size: var(--text-xs);
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .unfold {
    margin: 2px 14px 6px;
    align-self: flex-start;
    border: 0;
    background: none;
    padding: 0;
    color: var(--accent);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
  }
  .mono {
    font-family: var(--mono);
  }
  .cat {
    font-size: 10.5px;
  }
  .prio {
    display: inline-block;
    width: 9px;
    height: 9px;
    border-radius: 2px;
    background: color-mix(in srgb, var(--muted) 55%, transparent);
  }
  .prio.bad {
    background: var(--err);
  }
  .prio.warn {
    background: var(--warn);
  }
  .strip {
    display: inline-flex;
    gap: 2px;
  }
  .strip i {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--accent);
  }
  .strip i.refines {
    background: linear-gradient(90deg, var(--accent) 50%, transparent 50%);
    box-shadow: inset 0 0 0 1px var(--accent);
  }
  .strip i.contradicts {
    background: transparent;
    box-shadow: inset 0 0 0 1.5px var(--err);
  }
  .strip i.unknown {
    background: var(--muted);
  }
</style>
