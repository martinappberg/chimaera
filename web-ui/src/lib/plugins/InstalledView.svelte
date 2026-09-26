<script lang="ts">
  /**
   * Installed: the workbench plugins as cards (glyph tile · name · version ·
   * summary · the on/off switch for THIS workspace · Here / Adds / Needs
   * lines — the "Adds" line is mandatory, it is what makes opt-in honest),
   * then what each agent CLI reports about its own plugins. Requirement
   * state ("Needs") is read from the agents, never guessed.
   */
  import Switch from "../shared/Switch.svelte";
  import { knowledge } from "../workspace/knowledge";
  import { ApiError } from "../net/api";
  import {
    installPlugin,
    setWorkspacePluginOn,
    type AgentHook,
    type AgentId,
    type AgentPlugin,
    type AgentPlugins,
    type AgentPluginsEntry,
    type WorkspacePlugin,
  } from "./store";

  interface Props {
    plugins: WorkspacePlugin[];
    agentPlugins: AgentPlugins | null;
    agentState: "idle" | "loading" | "ok" | "unavailable" | "error";
    agentError: string | null;
    wsId: string | null;
    onAttach: (pluginId: string) => void;
    onOpenSession: (id: string) => void;
    onRefresh: () => void;
  }

  let { plugins, agentPlugins, agentState, agentError, wsId, onAttach, onOpenSession, onRefresh }: Props =
    $props();

  /** Per-plugin in-flight/failed switch state. */
  let busy = $state(new Set<string>());
  let errors = $state(new Map<string, string>());

  async function toggle(p: WorkspacePlugin, on: boolean): Promise<void> {
    busy = new Set(busy).add(p.id);
    errors = new Map([...errors].filter(([k]) => k !== p.id));
    try {
      await setWorkspacePluginOn(p.id, on);
    } catch (e) {
      errors = new Map(errors).set(p.id, e instanceof Error ? e.message : String(e));
    } finally {
      const next = new Set(busy);
      next.delete(p.id);
      busy = next;
    }
  }

  async function install(p: WorkspacePlugin, agent: AgentId): Promise<void> {
    if (wsId === null) return;
    const key = `${p.id}:${agent}`;
    busy = new Set(busy).add(key);
    try {
      const res = await installPlugin(wsId, p.id, agent);
      onOpenSession(res.session_id);
    } catch (e) {
      errors = new Map(errors).set(
        p.id,
        e instanceof ApiError && e.status === 404
          ? "this daemon can't run installs yet — use the agent's own plugin manager"
          : e instanceof Error
            ? e.message
            : String(e),
      );
    } finally {
      const next = new Set(busy);
      next.delete(key);
      busy = next;
    }
  }

  function entry(agent: string): AgentPluginsEntry | null {
    return agentPlugins?.agents.find((a) => a.agent === agent) ?? null;
  }
  function installed(agent: string, id: string): AgentPlugin | null {
    return entry(agent)?.plugins.find((p) => p.id === id || p.id === id.split("@")[0]) ?? null;
  }
  /** Codex hooks of a plugin that still wait for the user's trust. */
  function untrustedHooks(agent: string, id: string): AgentHook[] {
    const hooks = entry(agent)?.hooks ?? [];
    const base = id.split("@")[0];
    return hooks.filter(
      (h) =>
        (h.plugin_id === id || h.plugin_id === base) && (h.trust === "untrusted" || h.trust === "modified"),
    );
  }

  /** The tile's two letters: a fixed pair for the first-party ids, else
   *  the id's first two characters. */
  function tile(id: string): string {
    if (id === "mycelium") return "my";
    if (id === "agent-notes") return "an";
    if (id === "latex") return "TeX";
    return id.slice(0, 2);
  }

  /** The version the agents report for a plugin's requirement (claude's
   *  first, then any) — shown beside the name when known. */
  function version(p: WorkspacePlugin): string | null {
    for (const r of p.requires) {
      const v = installed(r.agent, r.id)?.version;
      if (v) return v;
    }
    return null;
  }

  const k = $derived($knowledge);

  function hereLine(p: WorkspacePlugin): string | null {
    if (typeof p.provides.knowledge === "string" && p.active && k !== null && k.provider !== null) {
      const c = k.counts;
      return `.living/ found — ${c.findings} finding${c.findings === 1 ? "" : "s"} · ${c.decisions} decision${c.decisions === 1 ? "" : "s"} · ${c.learnings} learning${c.learnings === 1 ? "" : "s"} · ${c.open} open`;
    }
    if (p.detect.length === 0) return null;
    if (p.detected) return `${p.detect[0]} found`;
    return null;
  }

  function adds(p: WorkspacePlugin): string {
    return [...p.adds.ui, ...p.adds.agents].join(" · ");
  }

  function fmtTokens(n: number): string {
    return n >= 1000 ? `~${(n / 1000).toFixed(1).replace(/\.0$/, "")}k` : `~${n}`;
  }
</script>

<section class="wb" aria-labelledby="wb-title">
  <div class="shead">
    <h2 id="wb-title" class="lbl">Workbench</h2>
    <span class="hint">run inside chimaera</span>
  </div>

  {#if plugins.length === 0}
    <p class="empty">No workbench plugins on this daemon.</p>
  {/if}

  {#each plugins as p (p.id)}
    {@const v = version(p)}
    {@const here = hereLine(p)}
    <article class="card" class:active={p.active}>
      <div class="top">
        <span class="tile mono" class:on={p.active} aria-hidden="true">{tile(p.id)}</span>
        <div class="ident">
          <div class="nameline">
            <span class="name">{p.name}</span>
            {#if v !== null}<span class="ver mono">{v}</span>{/if}
          </div>
          <div class="summary">{p.summary}</div>
        </div>
        <span class="state" class:good={p.active}>
          {#if p.active}
            <span class="dot"></span>active here
          {:else if p.on}
            on · nothing detected yet
          {:else if p.detected && p.detect.length > 0}
            off · {p.detect[0]} found
          {:else}
            off
          {/if}
        </span>
        <Switch
          on={p.on}
          label="{p.name} {p.on ? 'on' : 'off'} in this workspace"
          disabled={busy.has(p.id)}
          onToggle={(next) => void toggle(p, next)}
        />
      </div>

      <div class="lines">
        {#if here !== null}
          <span class="lbl small">Here</span>
          <span>{here}</span>
        {:else if p.on && !p.detected && p.setup !== null}
          <span class="lbl small">Here</span>
          <span class="muted">
            nothing detected yet —
            <button class="link" onclick={() => onAttach(p.id)}>set it up →</button>
          </span>
        {/if}
        <span class="lbl small">{p.on ? "Adds" : "Would add"}</span>
        <span>{adds(p)}</span>
        {#if p.requires.length > 0}
          <span class="lbl small needs">Needs</span>
          <span class="needs-row">
            {#each p.requires as r (r.agent)}
              {@const ag = entry(r.agent)}
              {@const got = installed(r.agent, r.id)}
              {@const hooks = untrustedHooks(r.agent, r.id)}
              {#if agentState === "unavailable"}
                <span class="pill neutral">{r.agent} · can't check on this daemon</span>
              {:else if agentState === "loading" && agentPlugins === null}
                <span class="pill neutral">{r.agent} · checking…</span>
              {:else if ag === null || !ag.available}
                <span class="pill neutral" title={ag?.error ?? undefined}>{r.agent} · not installed on this host</span>
              {:else if got !== null}
                <span class="pill good" class:warn={!got.enabled}>
                  {r.agent} · plugin {got.version ?? ""} {got.enabled ? "✓" : "· disabled"}
                </span>
                {#if hooks.length > 0}
                  <span class="pill warn">{r.agent} · {hooks.length} hook{hooks.length === 1 ? "" : "s"} not trusted</span>
                  <button class="link strong" onclick={() => onAttach(p.id)}>Review &amp; trust →</button>
                {/if}
              {:else}
                <span class="pill warn">{r.agent} · plugin not installed</span>
                <button
                  class="opt"
                  disabled={busy.has(`${p.id}:${r.agent}`)}
                  title="runs {r.agent}'s own plugin manager in a visible terminal"
                  onclick={() => void install(p, r.agent as AgentId)}
                >
                  {busy.has(`${p.id}:${r.agent}`) ? "starting…" : `Install for ${r.agent}`}
                </button>
              {/if}
            {/each}
            {#if agentState === "error" && agentError !== null}
              <span class="err">{agentError}</span>
              <button class="link" onclick={onRefresh}>retry</button>
            {/if}
          </span>
        {/if}
        {#if errors.has(p.id)}
          <span></span>
          <span class="err">{errors.get(p.id)}</span>
        {/if}
      </div>
    </article>
  {/each}
</section>

<section class="ag" aria-labelledby="ag-title">
  <div class="shead">
    <h2 id="ag-title" class="lbl">Agents</h2>
    <span class="hint">run inside each agent — managed with its own plugin manager</span>
  </div>
  {#if agentState === "unavailable"}
    <p class="empty">This daemon can't ask the agents about their plugins yet — update chimaera.</p>
  {:else if agentState === "error" && agentPlugins === null}
    <p class="empty err">{agentError} <button class="link" onclick={onRefresh}>retry</button></p>
  {:else if agentPlugins === null}
    <p class="empty">asking claude and codex…</p>
  {:else}
    <div class="agents">
      {#each agentPlugins.agents as a (a.agent)}
        <div class="acard">
          <div class="ahead">
            <span class="aname">{a.agent}</span>
            {#if a.version}<span class="ver mono">{a.version}</span>{/if}
            <span class="acount">
              {#if !a.available}not installed on this host{:else}{a.plugins.length} plugin{a.plugins.length === 1 ? "" : "s"}{/if}
            </span>
          </div>
          {#if a.error}
            <div class="abody err">{a.error}</div>
          {:else if !a.available}
            <div class="abody muted">{a.agent} isn't installed on this host, so it has no plugins here.</div>
          {:else if a.plugins.length === 0}
            <div class="abody muted">no plugins</div>
          {:else}
            {#each a.plugins as pl (pl.id)}
              {@const waiting = (a.hooks ?? []).filter(
                (h) => (h.plugin_id === pl.id || h.plugin_id === pl.id.split("@")[0]) && (h.trust === "untrusted" || h.trust === "modified"),
              ).length}
              <div class="abody">
                <div class="prow">
                  <span class="mono pname">{pl.id.split("@")[0]}</span>
                  <span class="mono ver">{[pl.version, pl.scope].filter(Boolean).join(" · ")}</span>
                  <span class="pstate" class:good={pl.enabled}>{pl.enabled ? "enabled" : "disabled"}</span>
                </div>
                {#if pl.skills_n !== undefined || pl.hooks_n !== undefined || pl.always_on_tokens !== undefined}
                  <div class="pmeta">
                    {#if pl.skills_n !== undefined}{pl.skills_n} skill{pl.skills_n === 1 ? "" : "s"}{/if}
                    {#if pl.hooks_n !== undefined}
                      {#if pl.skills_n !== undefined} · {/if}
                      {#if waiting > 0}<span class="warn-text">{waiting} hook{waiting === 1 ? "" : "s"} waiting for your trust</span>{:else}{pl.hooks_n} hook{pl.hooks_n === 1 ? "" : "s"}{/if}
                    {/if}
                    {#if pl.always_on_tokens !== undefined}
                      {#if pl.skills_n !== undefined || pl.hooks_n !== undefined} · {/if}
                      <span class="fg">{fmtTokens(pl.always_on_tokens)} tokens in every session</span>
                    {/if}
                  </div>
                {/if}
                {#if waiting > 0}
                  {@const wb = plugins.find((p) => p.requires.some((r) => r.agent === a.agent && (r.id === pl.id || r.id.split("@")[0] === pl.id.split("@")[0])))}
                  {#if wb !== undefined}
                    <div class="plinks"><button class="link" onclick={() => onAttach(wb.id)}>review hooks</button></div>
                  {/if}
                {/if}
              </div>
            {/each}
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</section>

<style>
  .wb,
  .ag {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .shead {
    display: flex;
    align-items: baseline;
    gap: 12px;
  }
  .lbl {
    margin: 0;
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
    font-weight: 600;
  }
  .lbl.small {
    font-size: 10.5px;
    padding-top: 3px;
  }
  .hint {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .mono {
    font-family: var(--mono);
  }
  .muted {
    color: var(--muted);
  }
  .fg {
    color: var(--fg);
  }
  .empty {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--muted);
    line-height: 1.5;
  }
  .err {
    color: var(--err);
    font-size: var(--text-xs);
  }
  .warn-text {
    color: var(--warn);
  }

  .card {
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 12px;
    padding: 16px 20px;
    display: flex;
    flex-direction: column;
    gap: 14px;
    transition: border-color 0.12s ease;
  }
  .card.active {
    border-color: color-mix(in srgb, var(--accent) 35%, var(--edge));
  }
  .top {
    display: flex;
    align-items: center;
    gap: 12px;
    min-width: 0;
  }
  .tile {
    flex: none;
    width: 34px;
    height: 34px;
    border-radius: 8px;
    background: color-mix(in srgb, var(--fg) 5%, transparent);
    color: var(--muted);
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: var(--text-sm);
    font-weight: 600;
  }
  .tile.on {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    color: var(--accent);
  }
  .ident {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .nameline {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .name {
    font-size: var(--text-lg);
    font-weight: 600;
  }
  .ver {
    font-size: 11.5px;
    color: var(--muted);
  }
  .summary {
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .state {
    margin-left: auto;
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: var(--text-xs);
    color: var(--muted);
    white-space: nowrap;
  }
  .state.good {
    color: var(--accent);
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--accent);
  }

  .lines {
    display: grid;
    grid-template-columns: 76px minmax(0, 1fr);
    row-gap: 8px;
    column-gap: 14px;
    font-size: var(--text-sm);
    padding-left: 46px;
    line-height: 1.45;
  }
  @container (max-width: 640px) {
    .lines {
      padding-left: 0;
    }
  }
  .needs-row {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: center;
  }
  .pill {
    font-size: 12.5px;
    padding: 2px 10px;
    border-radius: 999px;
    white-space: nowrap;
  }
  .pill.good {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    color: var(--accent);
  }
  .pill.warn {
    background: color-mix(in srgb, var(--warn) 11%, transparent);
    color: var(--warn);
  }
  .pill.neutral {
    border: 1px solid var(--edge);
    color: var(--muted);
  }
  .link {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--accent);
    cursor: pointer;
  }
  .link:hover {
    text-decoration: underline;
  }
  .link.strong {
    font-weight: 500;
  }

  .agents {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 14px;
  }
  @container (max-width: 700px) {
    .agents {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  .acard {
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 12px;
    overflow: hidden;
  }
  .ahead {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 11px 16px;
    border-bottom: 1px solid var(--edge);
  }
  .aname {
    font-size: var(--text-md);
    font-weight: 600;
  }
  .acount {
    margin-left: auto;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .abody {
    padding: 12px 16px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: var(--text-sm);
  }
  .abody + .abody {
    border-top: 1px solid var(--edge);
  }
  .prow {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .pname {
    font-size: var(--text-sm);
    font-weight: 600;
  }
  .pstate {
    margin-left: auto;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .pstate.good {
    color: var(--accent);
  }
  .pmeta {
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .plinks {
    display: flex;
    gap: 14px;
    font-size: 12.5px;
    padding-top: 2px;
  }
</style>
