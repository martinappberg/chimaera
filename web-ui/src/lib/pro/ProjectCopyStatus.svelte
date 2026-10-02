<script lang="ts">
  import { untrack } from "svelte";
  import { pageVisible } from "../shared/visibility";
  import { proMirrorStatus, proTakeOverProject, type MirrorWorkspace } from "../net/native";
  import { projectCopyError, takeoverEpoch } from "./projectCopy";
  let { workspaceId, onTaken }: { workspaceId: string; onTaken?: () => void } = $props();
  let row = $state<MirrorWorkspace | null>(null);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let generation = 0;
  let shownId: string | null = null;
  async function load(id: string): Promise<void> {
    const request = ++generation;
    try {
      const status = await proMirrorStatus();
      if (request !== generation || id !== workspaceId) return;
      row = status.configured ? status.workspaces.find(workspace => workspace.workspace_id === id) ?? null : null;
    } catch { if (request === generation) row = null; }
  }
  $effect(() => {
    const id = workspaceId;
    // A reused strip must never offer the next project's action with the
    // previous project's role/epoch while its native read is in flight.
    row = null;
    if (shownId !== id) { shownId = id; error = null; }
    if (!$pageVisible) { row = null; return; }
    untrack(() => void load(id));
    const timer = setInterval(() => void load(id), 15_000);
    return () => { generation += 1; clearInterval(timer); };
  });
  async function takeOver(): Promise<void> {
    const epoch = takeoverEpoch(row);
    if (busy || epoch === null) return;
    busy = true; error = null; generation += 1;
    const id = workspaceId;
    try {
      await proTakeOverProject(id, epoch);
      if (id === workspaceId) onTaken?.();
    } catch (reason) { if (id === workspaceId) error = projectCopyError(reason, "takeover"); }
    finally { busy = false; if (id === workspaceId) await load(id); }
  }
</script>
{#if row?.local_copy || error}
  <div class="copy-status">
    <span>{busy || row?.local_copy?.state === "taking_over" ? "Moving execution here…" : row?.local_copy?.state === "recovery_needed" ? "Local copy needs recovery. Open this project again from Home." : row?.local_copy?.ready ? "Local copy · Take over to run work here" : "Local copy is updating…"}</span>
    {#if takeoverEpoch(row) !== null}<button disabled={busy} onclick={() => void takeOver()} title="Finish the current step and move execution to this computer">Take over</button>{/if}
    {#if error}<span class="error" role="alert">{error}</span>{/if}
  </div>
{/if}
<style>
  .copy-status { display: flex; align-items: center; flex-wrap: wrap; gap: 6px 10px; color: var(--muted); font-size: var(--text-xs); }
  button { background: transparent; border: 1px solid var(--edge); border-radius: 5px; color: var(--fg); padding: 4px 8px; font: inherit; cursor: pointer; }
  button:hover:not(:disabled) { background: var(--row-hover); }
  button:disabled { opacity: .5; cursor: default; }
  button:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 2px; }
  .error { color: var(--warn); }
  @media (pointer: coarse) { button { min-height: 40px; } }
</style>
