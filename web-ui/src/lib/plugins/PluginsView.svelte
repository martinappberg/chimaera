<script lang="ts">
  /**
   * The Extensions tab (design §6; the layout surface, wire and store keep
   * the id "plugins"): three equal segments — Plugins (chimaera's own
   * plugins with Install / the switch and plain sentences, then the agent
   * plugins each CLI reports), Skills (every skill each agent can use here),
   * Browse (later — rendered disabled, honestly). Per workspace, naming the
   * host, because plugins are installed per host. Agent state always comes
   * from the agents themselves (the daemon's probes), never re-derived here.
   */
  import Segmented from "../shared/Segmented.svelte";
  import { pageVisible } from "../shared/visibility";
  import { getHostLabel } from "../net/api";
  import { ApiError } from "../net/api";
  import type { DashCtx } from "../dashboard/dash";
  import type { LayoutCtrl } from "../layout/dnd";
  import InstalledView from "./InstalledView.svelte";
  import SkillsView from "./SkillsView.svelte";
  import {
    fetchAgentPlugins,
    fetchSkills,
    openAttachSheet,
    pluginsAvailable,
    refreshWorkspacePlugins,
    workspacePlugins,
    type AgentPlugins,
    type SkillsReport,
  } from "./store";

  interface Props {
    dash: DashCtx;
    wsId: string | null;
    wsRoot: string | null;
    paneId: string;
    ctrl: LayoutCtrl;
    /** False while this retained tab is behind another pane tab. */
    visible?: boolean;
  }

  let { dash, wsId, wsRoot, paneId, ctrl, visible = true }: Props = $props();

  let view = $state<"plugins" | "skills" | "browse">("plugins");

  type Fetch<T> = { state: "idle" | "loading" | "ok" | "unavailable" | "error"; data: T | null; error: string | null };
  let agentPlugins = $state<Fetch<AgentPlugins>>({ state: "idle", data: null, error: null });
  let skills = $state<Fetch<SkillsReport>>({ state: "idle", data: null, error: null });

  let apSeq = 0;
  async function loadAgentPlugins(): Promise<void> {
    if (wsId === null) return;
    const mine = ++apSeq;
    agentPlugins = { ...agentPlugins, state: "loading" };
    try {
      const data = await fetchAgentPlugins(wsId);
      if (mine !== apSeq) return;
      agentPlugins = { state: "ok", data, error: null };
    } catch (e) {
      if (mine !== apSeq) return;
      agentPlugins =
        e instanceof ApiError && e.status === 404
          ? { state: "unavailable", data: null, error: null }
          : { state: "error", data: agentPlugins.data, error: e instanceof Error ? e.message : String(e) };
    }
  }

  let skSeq = 0;
  async function loadSkills(): Promise<void> {
    if (wsId === null) return;
    const mine = ++skSeq;
    skills = { ...skills, state: "loading" };
    try {
      const data = await fetchSkills(wsId);
      if (mine !== skSeq) return;
      skills = { state: "ok", data, error: null };
    } catch (e) {
      if (mine !== skSeq) return;
      skills =
        e instanceof ApiError && e.status === 404
          ? { state: "unavailable", data: null, error: null }
          : { state: "error", data: skills.data, error: e instanceof Error ? e.message : String(e) };
    }
  }

  // Fetch what the shown view needs, on show and on return (the daemon
  // caches its agent probes, so a return costs at most one cached read).
  let lastShown: string | null = null;
  $effect(() => {
    const on = visible && $pageVisible && wsId !== null;
    const key = on ? `${wsId}:${view}` : null;
    if (key === null) {
      lastShown = null;
      return;
    }
    if (key === lastShown) return;
    lastShown = key;
    refreshWorkspacePlugins();
    if (view === "plugins") void loadAgentPlugins();
    if (view === "skills") void loadSkills();
  });

  const host = $derived(agentPlugins.data?.host || skills.data?.host || getHostLabel());
  const plugins = $derived($workspacePlugins?.plugins ?? []);
</script>

<div class="plugins">
  <div class="inner">
    <header class="head">
      <div class="titles">
        <h1>Extensions</h1>
        <p class="sub">
          {#if view === "skills"}
            What each agent can do in this workspace — as the agents themselves report it.
          {:else}
            Plugins add tools for your agents and views for you. Install one, then switch it on per workspace.
          {/if}
        </p>
      </div>
      <div class="tools">
        <!-- Three equal segments: the wrapper sizes each to the widest. -->
        <div class="segs">
          <Segmented
            label="Extensions sections"
            value={view}
            options={[
              { value: "plugins", label: "Plugins" },
              { value: "skills", label: "Skills" },
              {
                value: "browse",
                label: "Browse",
                disabled: true,
                hint: "later",
                title: "search plugins and skills from the marketplaces your agents already have",
              },
            ]}
            onChange={(v) => (view = v as typeof view)}
          />
        </div>
        <span class="chip mono" title="Plugins are installed per host">on {host}</span>
      </div>
    </header>

    {#if $pluginsAvailable === false}
      <p class="empty">This daemon can't install plugins yet — update chimaera to add them here.</p>
    {:else if view === "plugins"}
      <InstalledView
        {plugins}
        agentPlugins={agentPlugins.data}
        agentState={agentPlugins.state}
        agentError={agentPlugins.error}
        onAttach={(pid) => openAttachSheet(pid)}
        onOpenSession={dash.onOpenSession}
        onRefresh={() => {
          refreshWorkspacePlugins();
          void loadAgentPlugins();
        }}
        {wsId}
      />
    {:else if view === "skills"}
      <SkillsView
        report={skills.data}
        status={skills.state}
        error={skills.error}
        {host}
        {wsRoot}
        onOpenFile={(p) => ctrl.openFileFrom(paneId, p, false)}
        onRefresh={() => void loadSkills()}
      />
    {/if}
  </div>
</div>

<style>
  .plugins {
    position: absolute;
    inset: 0;
    overflow-y: auto;
    background: var(--bg);
    container-type: inline-size;
  }
  .inner {
    max-width: 1100px;
    margin: 0 auto;
    padding: 26px 36px 48px;
    display: flex;
    flex-direction: column;
    gap: 26px;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 20px;
    flex-wrap: wrap;
  }
  .titles {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
  }
  h1 {
    margin: 0;
    font-size: 22px;
    font-weight: 600;
    letter-spacing: -0.01em;
  }
  .sub {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .tools {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: 14px;
  }
  /* Equal-width segments: a grid of 1fr columns sized to the widest
     label (Segmented stays the shared flex recipe everywhere else). */
  .segs :global(.seg) {
    display: grid;
    grid-auto-flow: column;
    grid-auto-columns: 1fr;
  }
  .chip {
    font-size: 11.5px;
    color: var(--muted);
    padding: 3px 9px;
    border: 1px solid var(--edge);
    border-radius: 999px;
    white-space: nowrap;
  }
  .mono {
    font-family: var(--mono);
  }
  .empty {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--muted);
    line-height: 1.5;
  }
</style>
