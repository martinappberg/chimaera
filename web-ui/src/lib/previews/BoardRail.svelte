<script lang="ts">
  /**
   * BoardView's outline rail + numeric inspector. Purely presentational —
   * plain parsed data and callbacks only, no shared state; every mutation
   * goes back through the parent's commit path.
   */
  import type { ObjInfo } from "./boardInteract";

  interface Props {
    title: string;
    objects: ObjInfo[];
    selected: string | null;
    onselect: (id: string | null) => void;
    oncommitfield: (field: "x" | "y" | "w" | "h", raw: string) => void;
  }
  let { title, objects, selected, onselect, oncommitfield }: Props = $props();

  const selectedObj = $derived(objects.find((o) => o.id === selected) ?? null);
</script>

<aside class="rail">
  <div class="rail-title">{title}</div>
  <div class="outline">
    {#each objects as o (o.id)}
      <button
        class="obj"
        class:on={o.id === selected}
        onclick={() => onselect(o.id === selected ? null : o.id)}
      >
        <span class="obj-kind">{o.kind}</span>
        <span class="obj-id">{o.id}</span>
      </button>
    {/each}
    {#if objects.length === 0}
      <div class="empty">no objects on this page</div>
    {/if}
  </div>

  {#if selectedObj !== null && selectedObj.at !== null && selectedObj.size !== null}
    <div class="inspector">
      <div class="insp-head">{selectedObj.id}</div>
      <div class="insp-grid">
        <label>x <input type="number" step="8" value={selectedObj.at[0]}
          onchange={(e) => oncommitfield("x", (e.currentTarget as HTMLInputElement).value)} /></label>
        <label>y <input type="number" step="8" value={selectedObj.at[1]}
          onchange={(e) => oncommitfield("y", (e.currentTarget as HTMLInputElement).value)} /></label>
        <label>w <input type="number" step="8" value={selectedObj.size[0]}
          onchange={(e) => oncommitfield("w", (e.currentTarget as HTMLInputElement).value)} /></label>
        <label>h <input type="number" step="8" value={selectedObj.size[1]}
          onchange={(e) => oncommitfield("h", (e.currentTarget as HTMLInputElement).value)} /></label>
      </div>
      <div class="insp-unit">pt · snaps to the 8 pt grid</div>
    </div>
  {/if}
</aside>

<style>
  .rail {
    width: 220px;
    flex-shrink: 0;
    border-left: 1px solid var(--edge);
    background: var(--rail-bg);
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .rail-title {
    padding: 10px 12px 6px;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .outline {
    flex: 1;
    overflow-y: auto;
    padding: 0 6px;
  }
  .obj {
    display: flex;
    align-items: baseline;
    gap: 6px;
    width: 100%;
    padding: 4px 6px;
    background: none;
    border: none;
    border-radius: 4px;
    cursor: pointer;
    text-align: left;
    font-size: var(--text-xs);
  }
  .obj:hover {
    background: var(--row-hover);
  }
  .obj.on {
    background: var(--row-active);
  }
  .obj-kind {
    color: var(--muted);
    font-family: var(--mono);
    flex-shrink: 0;
  }
  .obj-id {
    color: var(--fg);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .empty {
    color: var(--muted);
    font-size: var(--text-xs);
    padding: 8px 6px;
  }
  .inspector {
    border-top: 1px solid var(--edge);
    padding: 8px 12px 10px;
  }
  .insp-head {
    font-size: var(--text-xs);
    font-family: var(--mono);
    color: var(--accent);
    margin-bottom: 6px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .insp-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 4px 8px;
  }
  .insp-grid label {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: var(--text-xs);
    color: var(--muted);
    font-family: var(--mono);
  }
  .insp-grid input {
    width: 100%;
    min-width: 0;
    background: var(--term-bg);
    border: 1px solid var(--edge);
    border-radius: 3px;
    color: var(--fg);
    font-size: var(--text-xs);
    font-family: var(--mono);
    padding: 2px 4px;
  }
  .insp-unit {
    margin-top: 6px;
    font-size: var(--text-xs);
    color: var(--muted);
  }
</style>
