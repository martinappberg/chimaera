<script lang="ts">
  /**
   * One plugin's declared settings (`[[settings]]`,
   * docs/design/plugin-platform-plan.md §9), drawn with the same row language as
   * core settings: title and description on the left, the typed control on
   * the right, a quiet accent bar and a reset when changed. Host settings
   * hold on this host; workspace settings for the workspace shown (and
   * need one). Also what its output folders use, with Clear.
   *
   * Values live in the daemon (`GET`/`PUT /plugins/{pid}/settings`), which
   * checks each against its declaration; the plugin hears the change.
   */
  import Switch from "../shared/Switch.svelte";
  import {
    clearOutput,
    fetchOutputUse,
    fetchPluginSettings,
    putPluginSetting,
    sizeWords,
    type OutputUse,
    type SettingValue,
  } from "./platform";

  interface Props {
    plugin: string;
    name: string;
    wsId: string | null;
    /** Settings → Plugins shows the plugin's name as a heading. */
    heading?: boolean;
  }

  let { plugin, name, wsId, heading = false }: Props = $props();

  let rows = $state<SettingValue[] | null>(null);
  let error = $state<string | null>(null);
  let rowError = $state<{ key: string; text: string } | null>(null);
  let use = $state<OutputUse | null>(null);
  let clearing = $state(false);

  async function load(): Promise<void> {
    try {
      rows = await fetchPluginSettings(plugin, wsId);
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
    fetchOutputUse(plugin).then(
      (u) => (use = u),
      () => (use = null),
    );
  }

  $effect(() => {
    void plugin;
    void wsId;
    void load();
  });

  async function set(row: SettingValue, value: unknown): Promise<void> {
    rowError = null;
    try {
      rows = await putPluginSetting(plugin, row.key, value, row.scope === "workspace" ? wsId : null);
    } catch (e) {
      rowError = { key: row.key, text: e instanceof Error ? e.message : String(e) };
    }
  }

  async function clear(): Promise<void> {
    clearing = true;
    try {
      use = await clearOutput(plugin);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      clearing = false;
    }
  }

  /** Local text while typing: committed on change (blur or Enter). */
  function commitText(row: SettingValue, raw: string): void {
    if (row.type === "number") {
      const n = Number(raw);
      if (raw.trim() === "" || !Number.isFinite(n)) return;
      void set(row, n);
    } else {
      void set(row, raw);
    }
  }
</script>

<div class="plugin-settings">
  {#if heading}<h3 class="name">{name}</h3>{/if}
  {#if error !== null}
    <p class="err">{error}</p>
  {:else if rows === null}
    <p class="muted">loading…</p>
  {:else}
    {#each rows as row (row.key)}
      {@const needsWs = row.scope === "workspace" && wsId === null}
      <div class="row" class:modified={row.set}>
        <div class="gutter"></div>
        <div class="text">
          <div class="head">
            <span class="title">{row.label}</span>
            {#if row.scope === "host"}<span class="scope" title="One value for this host">host</span>{/if}
            {#if row.set}
              <button class="reset" title="reset to default" onclick={() => void set(row, null)}>reset</button>
            {/if}
          </div>
          {#if row.description !== null}<p class="desc">{row.description}</p>{/if}
          {#if needsWs}<p class="desc">Set per workspace: open a workspace to change it.</p>{/if}
          {#if rowError?.key === row.key}<p class="err">{rowError.text}</p>{/if}
        </div>
        <div class="control">
          {#if row.type === "bool"}
            <Switch on={row.value === true} label={row.label} disabled={needsWs} onToggle={(v) => void set(row, v)} />
          {:else if row.type === "enum"}
            <select
              aria-label={row.label}
              value={String(row.value ?? "")}
              disabled={needsWs}
              onchange={(e) => void set(row, (e.currentTarget as HTMLSelectElement).value)}
            >
              {#each row.options as o (o)}<option value={o}>{o}</option>{/each}
            </select>
          {:else}
            <input
              aria-label={row.label}
              type={row.type === "number" ? "number" : "text"}
              min={row.min ?? undefined}
              max={row.max ?? undefined}
              placeholder={row.type === "path" ? "a path in this workspace" : ""}
              value={String(row.value ?? "")}
              disabled={needsWs}
              onchange={(e) => commitText(row, (e.currentTarget as HTMLInputElement).value)}
            />
          {/if}
        </div>
      </div>
    {/each}
    {#if use !== null && use.bytes > 0}
      <div class="row">
        <div class="gutter"></div>
        <div class="text">
          <div class="head"><span class="title">Its files</span></div>
          <p class="desc">
            {name} keeps what it makes in its own folder, outside your projects: {sizeWords(use.bytes)} of
            {sizeWords(use.quota)}. Clearing removes them; it makes them again when needed.
          </p>
        </div>
        <div class="control">
          <button class="opt small" disabled={clearing} onclick={() => void clear()}>{clearing ? "Clearing…" : "Clear"}</button>
        </div>
      </div>
    {/if}
  {/if}
</div>

<style>
  .plugin-settings {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .name {
    margin: 10px 0 2px;
    padding: 0 14px;
    font-size: var(--text-md);
    font-weight: 600;
  }
  .row {
    position: relative;
    display: flex;
    align-items: flex-start;
    gap: 24px;
    padding: 10px 12px 10px 24px;
    border-radius: 8px;
  }
  .row:hover {
    background: color-mix(in srgb, var(--fg) 3%, transparent);
  }
  .gutter {
    position: absolute;
    left: 10px;
    top: 10px;
    bottom: 10px;
    width: 3px;
    border-radius: 2px;
  }
  .row.modified .gutter {
    background: color-mix(in srgb, var(--accent) 70%, transparent);
  }
  .text {
    flex: 1;
    min-width: 0;
  }
  .head {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .title {
    font-size: var(--text-sm);
    font-weight: 600;
  }
  .scope {
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    border: 1px solid var(--edge);
    border-radius: 4px;
    padding: 0 5px;
  }
  .reset {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
  }
  .reset:hover {
    color: var(--accent);
  }
  .desc {
    margin: 3px 0 0;
    font-size: var(--text-sm);
    line-height: 1.45;
    color: var(--muted);
    max-width: 60ch;
  }
  .control {
    flex: none;
    display: flex;
    align-items: center;
    min-height: 24px;
  }
  .control input,
  .control select {
    font: inherit;
    font-size: var(--text-sm);
    color: var(--fg);
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 6px;
    padding: 3px 8px;
    width: 200px;
  }
  .control input[type="number"] {
    width: 90px;
  }
  .muted {
    color: var(--muted);
    font-size: var(--text-sm);
    margin: 0;
  }
  .err {
    color: var(--err);
    font-size: var(--text-sm);
    margin: 3px 0 0;
  }
  @container (max-width: 520px) {
    .row {
      flex-direction: column;
      gap: 8px;
    }
  }
</style>
