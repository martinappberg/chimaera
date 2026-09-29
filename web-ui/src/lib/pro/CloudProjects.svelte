<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { proCloudProjects, proOpenCloudProject, onProChanged, type CloudProject } from "../net/native";
  import { pageVisible } from "../shared/visibility";
  import { asyncDisposer } from "../shared/asyncDisposer";
  import type { Workspace } from "../workspace/sessions";
  let { onOpen, knownIds }: { onOpen: (workspace: Workspace) => void; knownIds: string[] } = $props();
  let projects = $state<CloudProject[]>([]);
  let busy = $state<string | null>(null);
  let error = $state<string | null>(null);
  let revision = $state(0);
  let generation = 0;
  /** A project refused because it is mid-step in the cloud opens by itself once
   * it reaches a pause: retried on each list refresh while Home shows, for a
   * bounded time, and only after its folder is saved so no picker reappears. */
  let retry: { id: string; until: number } | null = null;
  const BUSY = "This project is finishing a step in the cloud. It opens here as soon as that step finishes.";
  // A legacy pending import can have a local registry row without an approved
  // destination. Keep its explicit folder-choice recovery action reachable.
  const cloudOnly = $derived(projects.filter(project => project.local_root === null || !knownIds.includes(project.workspace_id)));
  async function load(): Promise<void> {
    const request = ++generation;
    try {
      const next = await proCloudProjects();
      if (request !== generation) return;
      projects = next;
      if (error !== BUSY) error = null;
      const pending = retry;
      if (pending === null || busy !== null) return;
      const project = next.find(row => row.workspace_id === pending.id);
      if (project === undefined || Date.now() > pending.until) {
        retry = null;
        if (error === BUSY) error = "This project is still busy in the cloud. Open it again whenever you're ready; its cloud copy is intact.";
      } else if (project.available && project.local_root !== null) void open(project);
    }
    catch { if (request === generation && error !== BUSY) error = "Cloud projects couldn't refresh. Your local projects remain available."; }
  }
  $effect(() => {
    revision;
    if (!$pageVisible) return;
    untrack(() => void load());
    const timer = setInterval(() => void load(), 30_000);
    return () => { generation += 1; clearInterval(timer); };
  });
  onMount(() => asyncDisposer(onProChanged(() => { revision += 1; })));
  async function open(project: CloudProject): Promise<void> {
    if (busy !== null) return;
    busy = project.workspace_id;
    // An automatic retry keeps its explanation on screen while it runs.
    if (retry?.id !== project.workspace_id) error = null;
    try {
      const local = await proOpenCloudProject(project.workspace_id);
      retry = null;
      if (local !== null) onOpen({ id: local.workspace_id, root: local.root, name: local.name });
    } catch (reason) {
      const message = String(reason);
      const waiting = /busy|pause|running/i.test(message);
      retry = waiting ? { id: project.workspace_id, until: retry?.id === project.workspace_id ? retry.until : Date.now() + 15 * 60_000 } : null;
      error = /not.empty|empty.*folder|unrelated|already.*files|destination.*conflict/i.test(message)
        ? "That folder already contains files. Choose a new empty project folder so your existing work stays untouched."
        : /missing|moved|not.*exist|unavailable.*folder|folder.*unavailable/i.test(message)
        ? "This project's local folder is unavailable. Restore or reconnect that folder, then try again. No new copy was created."
        : waiting
          ? BUSY
          : "This project couldn't open here. Its cloud copy is intact. Please try again shortly.";
    } finally { busy = null; }
  }
</script>
{#if cloudOnly.length > 0 || error}
  <section class="cloud-projects" aria-label="Cloud projects">
    <div class="heading"><h2>Cloud projects</h2></div>
    <p class="hint">Projects created in the cloud. When you open one here for the first time, choose where to keep its local copy.</p>
    {#each cloudOnly as project (project.workspace_id)}
      <div class="project"><div><span class="name">{project.name}</span><span class="hint">{project.local_root ? "Local destination saved" : "In the cloud · not on this computer"}{#if !project.available} · Temporarily unavailable{/if}</span></div><button disabled={busy !== null || !project.available} onclick={() => void open(project)}>{busy === project.workspace_id ? "Opening…" : "Open project"}</button></div>
    {/each}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
  </section>
{/if}
<style>
  .cloud-projects { margin: 0; }
  .heading { display: flex; justify-content: space-between; align-items: center; gap: 12px; }
  h2 { margin: 0; color: var(--fg); font-size: var(--text-md); letter-spacing: -.01em; font-weight: 550; }
  .hint { color: var(--muted); font-size: var(--text-sm); line-height: 1.6; }
  .project { display: flex; align-items: center; justify-content: space-between; gap: 14px; border: 1px solid var(--edge); border-radius: 10px; padding: 14px 16px; margin-top: 8px; }
  .project > div { display: flex; flex-direction: column; gap: 3px; min-width: 0; overflow-wrap: anywhere; }
  .name { font-size: var(--text-md); }
  button { background: transparent; color: var(--fg); border: 1px solid var(--edge); border-radius: 6px; padding: 6px 10px; font: inherit; font-size: var(--text-sm); cursor: pointer; flex: none; }
  button:disabled { opacity: .5; cursor: default; }
  button:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 3px; }
  @media (pointer: coarse) { button { min-height: 40px; padding: 9px 12px; } }
  .error { color: var(--warn); font-size: var(--text-sm); }
  @media (max-width: 480px) { .project { align-items: flex-start; flex-direction: column; gap: 10px; } }
</style>
