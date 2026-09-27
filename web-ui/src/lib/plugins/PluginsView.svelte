<script lang="ts">
  /**
   * The Extensions tab (design §6; the layout surface, wire and store keep
   * the id "plugins"): three equal segments — Plugins (chimaera's own
   * plugins with Install / the switch and plain sentences, then the agent
   * plugins each CLI reports), Skills (every skill each agent can use here),
   * Browse (later — rendered disabled, honestly). Per workspace, naming the
   * host, because plugins are installed per host. Agent state always comes
   * from the agents themselves (the daemon's probes), never re-derived here.
   *
   * The header is the same in every view (nothing in it depends on which
   * one is shown), and the views swap in place below it, each in its own
   * scroller, so a switch never moves the chrome or loses a scroll position.
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
  /** Skills mounts on its first show and then stays, parked, like Plugins. */
  let skillsMounted = $state(false);

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
  <!-- The header carries nothing view-specific: each view's lead line lives
       in its body, so switching can't change the header's size. -->
  <div class="bar">
    <header class="head">
      <div class="titles">
        <h1>Extensions</h1>
        <span class="chip mono" title="Plugins are installed per host">on {host}</span>
      </div>
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
          onChange={(v) => {
            view = v as typeof view;
            if (v === "skills") skillsMounted = true;
          }}
        />
      </div>
    </header>
  </div>

  <div class="body">
    {#if $pluginsAvailable === false}
      <div class="view">
        <div class="inner">
          <p class="empty">This daemon can't install plugins yet — update chimaera to add them here.</p>
        </div>
      </div>
    {:else}
      <!-- One scroller per view, both always present (Pane.svelte's layer
           idiom): a switch flips which one shows, so each keeps its scroll
           position — and Skills its filter and search — and no sibling is
           inserted beside the other's content. -->
      <div class="view" class:parked={view !== "plugins"} inert={view !== "plugins"}>
        <div class="inner">
          <p class="lead">Plugins add tools for your agents and views for you. Install one, then switch it on per workspace.</p>
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
        </div>
      </div>
      <div class="view" class:parked={view !== "skills"} inert={view !== "skills"}>
        {#if skillsMounted}
          <div class="inner">
            <p class="lead">What each agent can do in this workspace — as the agents themselves report it.</p>
            <SkillsView
              report={skills.data}
              status={skills.state}
              error={skills.error}
              {host}
              {wsRoot}
              onOpenFile={(p) => ctrl.openFileFrom(paneId, p, false)}
              onRefresh={() => void loadSkills()}
            />
          </div>
        {/if}
      </div>
    {/if}
  </div>
</div>

<style>
  .plugins {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    background: var(--bg);
    container-type: inline-size;
  }
  /* The fixed header. `overflow: hidden` makes it a scroll container so
     `scrollbar-gutter: stable` reserves the same gutter the views' scrollers
     do: its column lines up with theirs whether or not the shown view has a
     scrollbar (a classic scrollbar showing in one view only shifted it). */
  .bar {
    flex: none;
    overflow: hidden;
    scrollbar-gutter: stable;
    border-bottom: 1px solid var(--edge);
  }
  .head {
    max-width: 1100px;
    margin: 0 auto;
    padding: 26px 36px 18px;
    display: flex;
    align-items: center;
    gap: 20px;
  }
  .titles {
    flex: 1 1 auto;
    display: flex;
    align-items: center;
    gap: 12px;
    min-width: 0;
  }
  h1 {
    flex: none;
    margin: 0;
    font-size: 22px;
    font-weight: 600;
    letter-spacing: -0.01em;
    white-space: nowrap;
  }
  /* A long host name ellipsizes rather than wrapping the row. */
  .chip {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
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
  /* In the header's far corner (as Settings' mode switch), never shrunk. */
  .segs {
    flex: none;
    margin-left: auto;
  }
  /* Equal-width segments: a grid of 1fr columns sized to the widest
     label — "Browse later", which is never the bold one, so the bar is as
     wide in every view (Segmented stays the shared flex recipe elsewhere). */
  .segs :global(.seg) {
    display: grid;
    grid-auto-flow: column;
    grid-auto-columns: 1fr;
  }
  /* Narrow: the title row over the bar — chosen by the tab's width alone,
     never by which view shows. 720px keeps a typical host name whole in the
     one-row layout; below it the bar squeezes to equal thirds only when even
     its own width doesn't fit. */
  @container (max-width: 720px) {
    .head {
      flex-direction: column;
      align-items: flex-start;
      gap: 14px;
    }
    .titles,
    .segs {
      max-width: 100%;
    }
    .segs {
      margin-left: 0;
    }
    .segs :global(.seg) {
      grid-auto-columns: minmax(0, 1fr);
    }
  }

  .body {
    flex: 1;
    min-height: 0;
    position: relative;
  }
  .view {
    position: absolute;
    inset: 0;
    overflow-y: auto;
    scrollbar-gutter: stable;
    container-type: inline-size;
  }
  /* Parked, not unmounted: opacity + inert (see .claude/rules/web-ui.md). */
  .view.parked {
    opacity: 0;
  }
  .inner {
    max-width: 1100px;
    margin: 0 auto;
    padding: 22px 36px 48px;
    display: flex;
    flex-direction: column;
    gap: 26px;
  }
  .lead {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .empty {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--muted);
    line-height: 1.5;
  }
</style>
