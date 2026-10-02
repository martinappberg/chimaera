<script lang="ts">
  import type { HostState } from "../net/native";
  import Switch from "../shared/Switch.svelte";
  import { modalFocus } from "../shared/modalFocus";
  import { focusOnMount } from "../shared/focusOnMount";
  import { schedulerLabel } from "./clusterRow";

  let { host, firstSetup = false, phase = null, onSave, onRepair, onClose }: {
    host: HostState;
    firstSetup?: boolean;
    phase?: string | null;
    onSave: (hpc: boolean) => Promise<void>;
    onRepair: () => Promise<void>;
    onClose: () => void;
  } = $props();

  // Each opening owns a fresh draft; live host-status events must not reset it.
  // svelte-ignore state_referenced_locally
  let hpc = $state(host.cluster?.login_serve !== true);
  let repairing = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let repaired = $state(false);
  const cluster = $derived(host.cluster !== null);
  const changed = $derived(cluster && hpc !== (host.cluster?.login_serve !== true));
  const needsWarning = $derived(cluster && !hpc && (changed || firstSetup));
  const jobsOnly = $derived(cluster && host.cluster?.login_serve !== true);

  async function save(): Promise<void> {
    if (busy) return;
    busy = true;
    error = null;
    try { await onSave(hpc); onClose(); }
    catch (e) { error = e instanceof Error ? e.message : String(e); }
    finally { busy = false; }
  }

  async function repair(): Promise<void> {
    if (busy) return;
    busy = true;
    error = null;
    try { await onRepair(); repaired = true; repairing = false; }
    catch (e) { error = e instanceof Error ? e.message : String(e); }
    finally { busy = false; }
  }
</script>

<svelte:window onkeydown={(e) => { if (e.key === "Escape" && !busy) { e.stopPropagation(); onClose(); } }} />

<div class="overlay">
  <button class="scrim" aria-label="Close remote settings" tabindex="-1" onclick={() => !busy && onClose()}></button>
  <div class="panel" role="dialog" aria-modal="true" aria-label={firstSetup ? `Set up ${host.alias}` : `Connection settings for ${host.alias}`} tabindex="-1" use:modalFocus>
    <header>
      <p class="eyebrow">{firstSetup ? "REMOTE SETUP" : "CONNECTION SETTINGS"}</p>
      <h2>{host.alias}</h2>
      <p class="intro">{cluster ? `${schedulerLabel(host.cluster?.scheduler)} detected. Choose how you use this host.` : "Manage this remote connection."}</p>
    </header>

    {#if cluster}
      <div class="placement">
        <div class="switch-row">
          <div><h3>Run workspaces in scheduled jobs</h3><p>Use compute nodes for Chimaera and your agents.</p></div>
          <Switch on={hpc} label="Run workspaces in scheduled jobs" disabled={busy} onToggle={(on) => (hpc = on)} />
        </div>
        <p class="explanation">{hpc
          ? "Chimaera connects through this host. Your agents and workspaces run on compute nodes, using your existing agent installations."
          : "Chimaera and your agents run directly on this host, like a regular remote server."}</p>
        {#if needsWarning}
          <div class="warning" role="note">
            <strong>Chimaera will run on the login node.</strong>
            <p>Your agents can keep running after you disconnect. Use this mode when your cluster permits it, and follow its limits for shared login nodes.</p>
          </div>
        {/if}
        {#if changed && hpc}
          <p class="explanation">Any Chimaera server already running on the login node stays running. The cluster page will let you shut it down.</p>
        {/if}
      </div>
    {/if}

    {#if !firstSetup}
      <div class="recovery">
        <h3>Having connection problems?</h3>
        <p>Reinstall Chimaera’s executable on this remote. Your workspaces, settings, saved chats, and agent installations are kept.</p>
        {#if repairing}
          <div class="repair-confirm">
            <strong>Reinstall on {host.alias}?</strong>
            <p>{jobsOnly
              ? "Running jobs keep using their current build. New jobs and reopened workspaces use the repaired installation. Nothing starts on the login node."
              : "Chimaera will restart on this remote. Running agents and shells will stop; supported conversations can be reopened afterward."}</p>
            <div class="repair-actions">
              <button class="quiet" disabled={busy} onclick={() => (repairing = false)}>Cancel</button>
              <button class="action" disabled={busy} onclick={() => void repair()}>{busy ? (phase ?? "Repairing…") : "Reinstall Chimaera"}</button>
            </div>
          </div>
        {:else if repaired}
          <p class="success" role="status">Chimaera reinstalled. {jobsOnly ? "Ready for your next job." : "Connection restored."}</p>
        {:else}
          <button class="repair-link" disabled={busy || changed} onclick={() => (repairing = true)}>Repair Chimaera…</button>
          {#if changed}<p class="explanation">Save the connection change before repairing.</p>{/if}
        {/if}
      </div>
    {/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    <footer>
      <button class="quiet" use:focusOnMount disabled={busy} onclick={onClose}>{firstSetup || changed ? "Cancel" : "Done"}</button>
      {#if firstSetup || changed}
        <button class="action" disabled={busy} onclick={() => void save()}>{busy ? "Saving…" : firstSetup ? "Continue" : "Save connection"}</button>
      {/if}
    </footer>
  </div>
</div>

<style>
  .overlay { position: fixed; inset: 0; z-index: 110; display: grid; place-items: center; padding: 24px; }
  .scrim { position: absolute; inset: 0; border: 0; background: var(--scrim); backdrop-filter: blur(3px); }
  .panel { position: relative; width: min(480px, 100%); max-height: calc(100vh - 48px); overflow-y: auto; background: var(--bg); border: 1px solid var(--edge); border-radius: 14px; box-shadow: 0 18px 56px color-mix(in srgb, var(--fg) 15%, transparent); padding: 24px; display: flex; flex-direction: column; gap: 22px; }
  h2, h3, p { margin: 0; }
  h2 { font-size: 24px; font-weight: 550; margin: 5px 0 8px; overflow-wrap: anywhere; }
  h3 { font-size: var(--text-sm); font-weight: 600; }
  p { font-size: var(--text-sm); line-height: 1.55; }
  .eyebrow { color: var(--muted); font: 10px var(--mono); letter-spacing: .1em; }
  .intro, .explanation, .recovery > p, .switch-row p { color: var(--muted); }
  .switch-row { display: flex; align-items: center; justify-content: space-between; gap: 20px; }
  .switch-row p { margin-top: 3px; }
  .placement, .recovery { display: flex; flex-direction: column; gap: 12px; }
  .recovery { border-top: 1px solid var(--edge); padding-top: 20px; }
  .warning, .repair-confirm { padding: 14px; border: 1px solid var(--edge); border-radius: 9px; background: var(--row-hover); font-size: var(--text-sm); }
  .warning { border-color: color-mix(in srgb, var(--warn) 45%, var(--edge)); }
  .warning strong { color: var(--warn); }
  .warning p, .repair-confirm p { margin-top: 6px; }
  footer, .repair-actions { display: flex; justify-content: flex-end; gap: 8px; }
  .repair-actions { margin-top: 14px; }
  button { font: inherit; cursor: pointer; }
  .quiet, .action { border: 1px solid var(--edge); border-radius: 7px; padding: 7px 12px; font-size: var(--text-sm); background: transparent; color: var(--fg); }
  .action { background: var(--accent); border-color: var(--accent); color: var(--bg); }
  .quiet:hover { background: var(--row-hover); }
  .repair-link { align-self: flex-start; padding: 0; border: 0; background: none; color: var(--accent); font-size: var(--text-sm); }
  .repair-link:hover { text-decoration: underline; }
  button:disabled { opacity: .5; cursor: default; }
  .error { color: var(--err); overflow-wrap: anywhere; }
  .success { color: var(--accent); }
</style>
