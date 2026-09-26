<script lang="ts">
  /**
   * The Plugins tab (design §6): one tab, three views — Installed (workbench
   * plugins with their switch and "Adds / Needs" lines, then what each agent
   * CLI reports), Skills (every skill each agent can use here), Browse
   * (later — rendered disabled, honestly). Per workspace, naming the host,
   * because installs are per host. Agent state always comes from the agents
   * themselves (the daemon's probes), never re-derived here.
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

  let view = $state<"installed" | "skills" | "browse">("installed");

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
    if (view === "installed") void loadAgentPlugins();
    if (view === "skills") void loadSkills();
  });

  const host = $derived(agentPlugins.data?.host || skills.data?.host || getHostLabel());
  const plugins = $derived($workspacePlugins?.plugins ?? []);
</script>

<div class="plugins">
  <div class="inner">
    <header class="head">
      <div class="titles">
        <h1>Plugins</h1>
        <p class="sub">
          {#if view === "skills"}
            What each agent can do in this workspace — as the agents themselves report it.
          {:else}
            Add-ons for this workspace. Each says what it adds; nothing is on until you turn it on.
          {/if}
        </p>
      </div>
      <div class="tools">
        <Segmented
          label="Plugin views"
          value={view}
          options={[
            { value: "installed", label: "Installed" },
            { value: "skills", label: "Skills" },
            { value: "browse", label: "Browse", disabled: true, hint: "later", title: "the agents' marketplaces — coming later" },
          ]}
          onChange={(v) => (view = v as typeof view)}
        />
        <span class="chip mono" title="installs are per host">on {host}</span>
      </div>
    </header>

    {#if $pluginsAvailable === false}
      <p class="empty">This daemon has no plugins yet — update chimaera to add workbench plugins here.</p>
    {:else if view === "installed"}
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
