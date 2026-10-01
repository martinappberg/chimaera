<script lang="ts">
  import { agentName, agentExtensionAction } from "./store";
  let { agent, wsId, onOpenSession, section = "plugins", canInstall = false }: {
    agent: string; wsId: string; onOpenSession: (id: string) => void;
    section?: "plugins" | "connections"; canInstall?: boolean;
  } = $props();
  let adding = $state(false);
  let source = $state("");
  let busy = $state(false);
  let error = $state<string | null>(null);
  async function act(action: string, target?: string): Promise<void> {
    if (busy) return;
    busy = true; error = null;
    try { onOpenSession(await agentExtensionAction(wsId, agent, action, target)); }
    catch (e) { error = e instanceof Error ? e.message : String(e); }
    finally { busy = false; }
  }
</script>
<div class="controls">
  <button class="opt" disabled={busy} onclick={() => void act(`manage_${section}`)}>{agent === "agy" ? "Command help" : `Manage in ${agentName(agent)}`}</button>
  {#if canInstall}<button class="opt" disabled={busy} aria-expanded={adding} onclick={() => adding = !adding}>Install plugin…</button>{/if}
</div>
{#if adding}
  <form onsubmit={(e) => { e.preventDefault(); void act("install_plugin", source.trim()); }}>
    <label>Plugin source<input aria-label={`Plugin source for ${agentName(agent)}`} bind:value={source} placeholder={agent === "agy" ? "Local folder or plugin@marketplace" : "Repository URL or local folder"} spellcheck="false" /></label>
    <button class="opt" disabled={busy || !source.trim()}>Install</button>
    <p>{agentName(agent)} handles installation and any approval it requires. The command opens in a terminal so you can review the result.</p>
  </form>
{/if}
{#if error}<p class="error" role="alert">{error}</p>{/if}
<style>
  .controls { display: flex; flex-wrap: wrap; gap: 8px; margin: 10px 0; }
  form { display: flex; flex-wrap: wrap; align-items: end; gap: 10px; margin: 12px 0; }
  label { display: flex; flex: 1; min-width: 180px; flex-direction: column; gap: 6px; font-size: var(--text-xs); }
  input { width: 100%; box-sizing: border-box; }
  p { flex-basis: 100%; margin: 0; color: var(--muted); font-size: var(--text-xs); line-height: 1.5; }
  .error { color: var(--warn); }
</style>
