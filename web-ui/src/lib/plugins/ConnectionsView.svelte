<script lang="ts">
  import { untrack } from "svelte";
  import { ApiError, getHostLabel } from "../net/api";
  import { pageVisible } from "../shared/visibility";
  import { openInSystemBrowser } from "../shared/urlOpen";
  import { agentName, agentPluginsRevision, type AgentId } from "./store";
  import { connectionStatus, fetchConnections, managementUrl, type ConnectionsReport } from "./connections";

  import AgentExtensionControls from "./AgentExtensionControls.svelte";
  import ConnectionDialog from "./ConnectionDialog.svelte";

  let { wsId, visible, onOpenSession }: { wsId: string; visible: boolean; onOpenSession: (id: string) => void } = $props();
  let report = $state<ConnectionsReport | null>(null);
  let loading = $state(false);
  let unavailable = $state(false);
  let error = $state<string | null>(null);
  let signingIn = $state<{agent: AgentId; name: string} | null>(null);
  let sequence = 0;
  $effect(() => () => { sequence++; });
  const host = $derived(report?.host || getHostLabel());
  const agents = $derived(report?.agents.filter(a => a.available) ?? []);

  async function load(refresh = false): Promise<void> {
    const mine = ++sequence;
    loading = true;
    error = null;
    try {
      const result = await fetchConnections(wsId, refresh);
      if (mine !== sequence) return;
      report = result;
      unavailable = false;
    } catch (e) {
      if (mine !== sequence) return;
      unavailable = e instanceof ApiError && e.status === 404;
      error = e instanceof Error ? e.message : String(e);
    } finally {
      if (mine === sequence) loading = false;
    }
  }

  $effect(() => {
    const revision = $agentPluginsRevision;
    if (!visible || !$pageVisible) return;
    untrack(() => { void revision; void load(); });
    return () => { sequence++; loading = false; };
  });

</script>

<div class="connections">
  <div class="intro">
    <p>Services your agents can use in this workspace. Each agent manages its own connections on {host}.</p>
    <button class="opt" onclick={() => void load(true)} disabled={loading}>{loading ? "Checking…" : "Check again"}</button>
  </div>
  {#if unavailable}
    <p class="empty">Update chimaera on this host to see agent connections here.</p>
  {:else}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if report === null && loading}
      <p class="empty" role="status">Asking your agents about their connections…</p>
    {:else if report !== null && agents.length === 0}
      <p class="empty">Install an agent on this host to see its connections.</p>
    {/if}
    {#each agents as agent, agentIndex (`${agentIndex}:${agent.agent}`)}
      <section aria-label={`${agent.agent} connections`}>
        <header><h2>{agentName(agent.agent)}</h2>{#if agent.version}<span class="version">{agent.version.replace("(Claude Code)", "").replace("codex-cli", "").trim()}</span>{/if}</header>
        {#if agent.actions?.includes("manage_connections")}<AgentExtensionControls agent={agent.agent} {wsId} {onOpenSession} section="connections" />{/if}
        {#if agent.notice}<p class="empty">{agent.notice}</p>{/if}
        {#each agent.errors as problem}<p class="error">{problem}</p>{/each}
        {#if agent.connections.length === 0 && agent.errors.length === 0}
          <p class="empty">No connections reported in this workspace.</p>
        {/if}
        {#each agent.connections as connection, index (`${index}:${connection.kind}:${connection.name}`)}
          {@const manage = managementUrl(connection)}
          <div class="connection">
            <div class="identity"><span class="name">{connection.name}</span><span class="source">{connection.source}</span></div>
            <span class="status" class:attention={["needs_auth", "failed", "needs_approval"].includes(connection.status)}>{connectionStatus(connection.status, connection.source)}</span>
            <div class="actions">
              {#if connection.login}
                <button class="opt" disabled={loading} title={`Connect with ${agent.agent} on ${host}`} onclick={() => signingIn = {agent: agent.agent, name: connection.name}}>{connection.source === "claude.ai" ? "Set up" : "Sign in"}</button>
              {:else if manage}
                <a href={manage} rel="noopener noreferrer" onclick={(event) => { event.preventDefault(); void openInSystemBrowser(manage); }}>Manage ↗</a>
              {/if}
            </div>
          </div>
        {/each}
      </section>
    {/each}
    {#if agents.length > 0}
      <p class="footnote">Set up opens Claude's connector settings; Sign in guides you through an MCP server's browser authorization. “Configured” means the server is listed; it doesn't confirm a live connection. Changes may require reconnecting or reopening an existing agent session.</p>
    {/if}
  {/if}
</div>

{#if signingIn}
  <ConnectionDialog {wsId} {visible} agent={signingIn.agent} name={signingIn.name} onClose={() => signingIn = null} onConnected={() => void load(true)} />
{/if}

<style>
  .connections { display: flex; flex-direction: column; gap: 28px; min-width: 0; }
  .intro { display: flex; align-items: flex-start; justify-content: space-between; gap: 20px; }
  .intro p { margin: 0; color: var(--muted); font-size: var(--text-sm); line-height: 1.5; max-width: 620px; }
  .intro button { flex: none; }
  header { display: flex; align-items: baseline; gap: 12px; margin-bottom: 12px; }
  h2 { font-size: var(--text-sm); font-weight: 600; margin: 0; }
  .version, .source, .status { color: var(--muted); font-size: var(--text-xs); }
  .version { font-family: var(--mono); }
  .connection { display: grid; grid-template-columns: minmax(0, 1fr) minmax(110px, auto) 100px; align-items: center; gap: 20px; padding: 14px 0; border-bottom: 1px solid var(--edge); }
  .identity { display: flex; flex-direction: column; gap: 4px; min-width: 0; }
  .name { font-size: var(--text-sm); overflow-wrap: anywhere; }
  .actions { display: flex; justify-content: flex-end; }
  .actions a { color: var(--muted); font-size: var(--text-xs); text-decoration: none; }
  .actions a:hover { color: var(--fg); text-decoration: underline; }
  .attention, .error { color: var(--warn); }
  .empty, .error, .footnote { margin: 0; font-size: var(--text-sm); line-height: 1.5; }
  .empty, .footnote { color: var(--muted); }
  .error { overflow-wrap: anywhere; }
  .footnote { font-size: var(--text-xs); max-width: 740px; }
  @container (max-width: 520px) {
    .intro { flex-direction: column; gap: 12px; }
    .connection { grid-template-columns: minmax(0, 1fr) auto; gap: 8px 16px; }
    .status { grid-column: 1; grid-row: 2; }
    .actions { grid-column: 2; grid-row: 1 / 3; }
  }
</style>
