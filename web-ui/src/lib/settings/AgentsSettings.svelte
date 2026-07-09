<script lang="ts">
  /**
   * The Agents settings section: one row per agent CLI showing what chimaera
   * resolves for it (yours / chimaera-managed / a path you set / not found) with
   * the version and resolved path, an explicit-path override, and — for a
   * chimaera-managed binary — an Uninstall button.
   *
   * Bespoke rather than generic SettingRows because it fuses live daemon state
   * (GET /api/v1/agents) with the `agents.<id>.path` settings and an imperative
   * uninstall. The path override still persists as a normal setting (so the
   * JSON editor and the store stay in sync); only the presentation is custom.
   */
  import { onMount } from "svelte";
  import { listAgents, uninstallAgent, type AgentInfo } from "../workspace/launcher";
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import { flushSettings, getSetting, setSetting } from "./store.svelte";

  // The four per-agent path keys are all string-typed, so this cast is sound.
  type AgentPathKey =
    | "agents.claude.path"
    | "agents.codex.path"
    | "agents.gemini.path"
    | "agents.agy.path";
  const pathKey = (id: string): AgentPathKey => `agents.${id}.path` as AgentPathKey;

  let agents = $state<AgentInfo[]>([]);
  let loading = $state(true);
  let loadError = $state<string | null>(null);
  // Per-agent editable path field + busy/error state, keyed by agent id.
  let inputs = $state<Record<string, string>>({});
  let saving = $state<Record<string, boolean>>({});
  let removing = $state<Record<string, boolean>>({});
  let rowError = $state<Record<string, string | null>>({});

  async function load(): Promise<void> {
    loading = true;
    loadError = null;
    try {
      // refresh=true so the rows reflect the current settings + managed state
      // (the daemon busts its detection cache on an install/uninstall/edit).
      const list = await listAgents(true);
      agents = list;
      const next: Record<string, string> = {};
      for (const a of list) next[a.id] = getSetting(pathKey(a.id));
      inputs = next;
    } catch (e) {
      loadError = e instanceof Error ? e.message : "failed to list agents";
    } finally {
      loading = false;
    }
  }

  onMount(load);

  function provenance(a: AgentInfo): { label: string; cls: string; title: string } {
    if (!a.installed)
      return { label: "not found", cls: "missing", title: "not resolvable — set a path below" };
    if (a.explicit)
      return { label: "set path", cls: "set", title: "resolved from the path you set below" };
    if (a.managed)
      return { label: "chimaera", cls: "managed", title: "installed by chimaera in ~/.chimaera/agents" };
    return { label: "yours", cls: "yours", title: "your own install, resolved from your login shell" };
  }

  async function save(id: string): Promise<void> {
    if (saving[id]) return;
    saving = { ...saving, [id]: true };
    rowError = { ...rowError, [id]: null };
    try {
      setSetting(pathKey(id), (inputs[id] ?? "").trim());
      await flushSettings();
      await load();
    } catch (e) {
      rowError = { ...rowError, [id]: e instanceof Error ? e.message : "failed to save" };
    } finally {
      saving = { ...saving, [id]: false };
    }
  }

  async function uninstall(a: AgentInfo): Promise<void> {
    if (removing[a.id]) return;
    if (
      !confirm(
        `Uninstall the chimaera-managed ${a.name}?\n\n` +
          `This removes only chimaera's own copy under ~/.chimaera/agents. ` +
          `Your own install, if any, is left untouched.`,
      )
    )
      return;
    removing = { ...removing, [a.id]: true };
    rowError = { ...rowError, [a.id]: null };
    try {
      await uninstallAgent(a.id);
      await load();
    } catch (e) {
      rowError = { ...rowError, [a.id]: e instanceof Error ? e.message : "failed to uninstall" };
    } finally {
      removing = { ...removing, [a.id]: false };
    }
  }

  const versionNumber = (v: string): string =>
    v.split(" ").find((t) => /^\d/.test(t)) ?? v.split(" ")[0];
</script>

<section class="agents">
  <div class="head">
    <h2 class="cat">Agents</h2>
    <button class="recheck" title="re-check installed agents" onclick={() => void load()} disabled={loading}>
      {loading ? "checking…" : "re-check"}
    </button>
  </div>
  <p class="intro">
    Which binary chimaera runs for each agent — for both a launched agent and when
    you type its name in a chimaera terminal. Leave a path empty to resolve it
    from your login shell, then a chimaera-managed install. chimaera only shadows
    your own binary when it manages one or you set a path here.
  </p>

  {#if loadError !== null}
    <div class="err" role="alert">{loadError}</div>
  {/if}

  {#each agents as a (a.id)}
    {@const p = provenance(a)}
    <div class="agent">
      <div class="top">
        <span class="glyph"><SessionGlyph kind="agent" agentKind={a.id} size={13} title={a.name} /></span>
        <span class="name">{a.name}</span>
        <span class="prov {p.cls}" title={p.title}>{p.label}</span>
        {#if a.version}<span class="ver" title={a.version}>{versionNumber(a.version)}</span>{/if}
        {#if a.outdated}<span class="prov outdated" title="installed but too old to run usefully">outdated</span>{/if}
        <span class="spacer"></span>
        {#if a.managed}
          <button
            class="uninstall"
            disabled={removing[a.id]}
            title="remove chimaera's managed copy (your own install is untouched)"
            onclick={() => void uninstall(a)}
          >
            {removing[a.id] ? "removing…" : "uninstall"}
          </button>
        {/if}
      </div>
      {#if a.path}
        <div class="resolved" title={a.path}><span class="mono">{a.path}</span></div>
      {/if}
      <div class="form">
        <input
          class="path-input"
          bind:value={inputs[a.id]}
          placeholder="resolve from login shell / PATH"
          spellcheck="false"
          autocapitalize="off"
          autocorrect="off"
          disabled={saving[a.id]}
          onkeydown={(e) => {
            if (e.key === "Enter") void save(a.id);
          }}
        />
        <button
          class="save"
          disabled={saving[a.id] || (inputs[a.id] ?? "") === getSetting(pathKey(a.id))}
          onclick={() => void save(a.id)}
        >
          {saving[a.id] ? "saving…" : "save"}
        </button>
      </div>
      <code class="id" title="settings.json key">{pathKey(a.id)}</code>
      {#if rowError[a.id]}<div class="err row" role="alert">{rowError[a.id]}</div>{/if}
    </div>
  {/each}
</section>

<style>
  .agents {
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
  }
  .head {
    display: flex;
    align-items: baseline;
    gap: 0.6rem;
  }
  .cat {
    margin: 0;
    font-size: 0.95rem;
    font-weight: 600;
    color: var(--fg);
  }
  .recheck {
    appearance: none;
    border: 1px solid var(--edge);
    background: var(--term-bg);
    color: var(--muted);
    font: inherit;
    font-size: 0.68rem;
    cursor: pointer;
    padding: 0.1rem 0.45rem;
    border-radius: 5px;
  }
  .recheck:hover:not(:disabled) {
    color: var(--fg);
    background: var(--row-hover);
  }
  .recheck:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .intro {
    margin: 0 0 0.2rem;
    font-size: 0.74rem;
    line-height: 1.5;
    color: var(--muted);
  }

  .agent {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    padding: 0.6rem 0.7rem;
    border: 1px solid var(--edge);
    border-radius: 8px;
    background: var(--term-bg);
  }
  .top {
    display: flex;
    align-items: center;
    gap: 0.45rem;
  }
  .glyph {
    display: flex;
    align-items: center;
    color: var(--muted);
  }
  .name {
    font-size: 0.82rem;
    font-weight: 600;
    color: var(--fg);
  }
  .spacer {
    flex: 1;
  }

  .prov {
    font-size: 0.6rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    padding: 0.05rem 0.35rem;
    border-radius: 4px;
    color: var(--muted);
    background: var(--row-hover);
  }
  .prov.yours {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 14%, transparent);
  }
  .prov.managed {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 14%, transparent);
  }
  .prov.set {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 14%, transparent);
  }
  .prov.missing,
  .prov.outdated {
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 14%, transparent);
  }

  .ver {
    font-family: var(--mono);
    font-size: 0.66rem;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }

  .uninstall {
    appearance: none;
    border: 1px solid var(--edge);
    background: var(--term-bg);
    color: var(--muted);
    font: inherit;
    font-size: 0.68rem;
    cursor: pointer;
    padding: 0.12rem 0.5rem;
    border-radius: 5px;
  }
  .uninstall:hover:not(:disabled) {
    color: var(--git-deleted, var(--warn));
    border-color: color-mix(in srgb, var(--warn) 45%, var(--edge));
    background: var(--row-hover);
  }
  .uninstall:disabled {
    opacity: 0.6;
    cursor: default;
  }

  .resolved {
    min-width: 0;
  }
  .resolved .mono {
    font-family: var(--mono);
    font-size: 0.68rem;
    color: var(--muted);
    overflow-wrap: anywhere;
  }

  .form {
    display: flex;
    gap: 0.4rem;
  }
  .path-input {
    flex: 1;
    min-width: 0;
    appearance: none;
    background: var(--code-bg, var(--term-bg));
    border: 1px solid var(--edge);
    border-radius: 6px;
    padding: 0.22rem 0.45rem;
    font-family: var(--mono);
    font-size: 0.72rem;
    color: var(--fg);
  }
  .path-input:focus {
    outline: none;
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
  }
  .save {
    appearance: none;
    border: 1px solid color-mix(in srgb, var(--accent) 45%, var(--edge));
    background: var(--term-bg);
    color: var(--accent);
    font: inherit;
    font-size: 0.7rem;
    cursor: pointer;
    padding: 0.12rem 0.6rem;
    border-radius: 6px;
  }
  .save:hover:not(:disabled) {
    background: var(--row-hover);
  }
  .save:disabled {
    opacity: 0.5;
    cursor: default;
    color: var(--muted);
    border-color: var(--edge);
  }

  .id {
    font-family: var(--mono);
    font-size: 0.62rem;
    color: var(--muted);
    opacity: 0.75;
  }

  .err {
    font-size: 0.7rem;
    color: var(--git-deleted, var(--warn));
    background: color-mix(in srgb, var(--warn) 10%, transparent);
    padding: 0.25rem 0.45rem;
    border-radius: 5px;
  }
  .err.row {
    margin-top: 0.1rem;
  }
</style>
