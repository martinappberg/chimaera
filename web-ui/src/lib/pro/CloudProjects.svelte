<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { proCloudProjects, proOpenCloudProject, onProChanged, type CloudProject } from "../net/native";
  import { pageVisible } from "../shared/visibility";
  import { asyncDisposer } from "../shared/asyncDisposer";
  import type { Workspace } from "../workspace/sessions";
  import { projectCopyError } from "./projectCopy";
  import { fetchHomeProjects, type HomeProject } from "./accountHome";
  /** `browser`: the web's Home (the account's own page), which has no local
   *  computer: every cloud project is listed, and opening one goes to its
   *  project view. Otherwise the desktop's section, which adopts a project
   *  into a local folder through the native shell. */
  let { onOpen, knownIds = [], browser = false }: {
    onOpen?: (workspace: Workspace) => void;
    knownIds?: string[];
    browser?: boolean;
  } = $props();
  let projects = $state<CloudProject[]>([]);
  let busy = $state<string | null>(null);
  let error = $state<string | null>(null);
  let listError = $state(false);
  let revision = $state(0);
  let generation = 0;
  // A legacy pending import can have a local registry row without an approved
  // destination. Keep its explicit folder-choice recovery action reachable.
  const cloudOnly = $derived(projects.filter(project => project.local_root === null || !knownIds.includes(project.workspace_id)));
  async function load(): Promise<void> {
    const request = ++generation;
    try {
      const next = await proCloudProjects();
      if (request !== generation) return;
      projects = next;
      listError = false;
    }
    catch { if (request === generation) listError = true; }
  }
  $effect(() => {
    revision;
    if (browser || !$pageVisible) return;
    untrack(() => void load());
    const timer = setInterval(() => void load(), 30_000);
    return () => { generation += 1; clearInterval(timer); };
  });
  onMount(() => browser ? undefined : asyncDisposer(onProChanged(() => { revision += 1; })));
  async function open(project: CloudProject): Promise<void> {
    if (busy !== null) return;
    busy = project.workspace_id;
    error = null;
    try {
      const local = await proOpenCloudProject(project.workspace_id);
      if (local !== null) onOpen?.({ id: local.workspace_id, root: local.root, name: local.name, local_copy: local.local_copy });
    } catch (reason) {
      error = projectCopyError(reason);
    } finally { busy = null; }
  }

  // --- the web's Home ---------------------------------------------------------
  /** Null until the first answer. */
  let homeRows = $state<HomeProject[] | null>(null);
  let homePending = $state(false);
  let homeError = $state(false);
  /** Passive reads while the page shows: every 30 s, sooner while part of the
   * list is still connecting, and at once when the page shows again. */
  $effect(() => {
    if (!browser || !$pageVisible) return;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout> | null = null;
    const read = async (): Promise<void> => {
      let pending = false;
      try {
        const list = await fetchHomeProjects(controller.signal);
        homeRows = list.projects;
        homePending = pending = list.pending;
        homeError = false;
      } catch {
        if (controller.signal.aborted) return;
        homeError = true;
      }
      if (!controller.signal.aborted) timer = setTimeout(() => void read(), pending ? 10_000 : 30_000);
    };
    untrack(() => void read());
    return () => { controller.abort(); if (timer !== null) clearTimeout(timer); };
  });
</script>
{#if browser}
  <section class="cloud-projects" aria-label="Cloud projects">
    <div class="heading"><h2>Cloud projects</h2></div>
    {#if homeRows === null}
      {#if !homeError}<p class="hint" role="status">Loading your projects…</p>{/if}
    {:else if homeRows.length > 0}
      <p class="hint">Open a project to continue its sessions and files here.</p>
      {#each homeRows as project (project.workspace_id)}
        <div class="project"><div><span class="name">{project.name ?? "Saved project"}</span><span class="hint">{project.available ? "Continue your sessions and files." : "Waiting for a connection. Your work stays saved."}</span></div><a class="open" href={project.href}>Open project</a></div>
      {/each}
    {:else if homePending}
      <p class="hint" role="status">Connecting to your saved work…</p>
    {:else}
      <div class="blank"><h3>No cloud projects yet</h3><p class="hint">Projects you keep in sync from the desktop app appear here.</p></div>
    {/if}
    {#if homeError}<p class="error" role="alert">Your cloud projects couldn't {homeRows === null ? "load" : "refresh"} just now. Your work stays saved, and this page keeps trying.</p>{/if}
  </section>
{:else if cloudOnly.length > 0 || error || listError}
  <section class="cloud-projects" aria-label="Synced projects">
    <div class="heading"><h2>Synced projects</h2></div>
    <p class="hint">Open a project to save or update its local copy. Its sessions keep running on their current computer until you choose Take over.</p>
    {#each cloudOnly as project (project.workspace_id)}
      <div class="project"><div><span class="name">{project.name}</span><span class="hint">{(project.destination_saved || project.local_root) ? "Local destination saved" : "No local copy yet"}{#if !project.available} · Temporarily unavailable{/if}</span></div><button disabled={busy !== null || !project.available} onclick={() => void open(project)}>{busy === project.workspace_id ? "Opening…" : "Open project"}</button></div>
    {/each}
    {#if listError}<p class="error" role="alert">Synced projects couldn’t refresh. Your local projects remain available.</p>{/if}
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
  button, .open { background: transparent; color: var(--fg); border: 1px solid var(--edge); border-radius: 6px; padding: 6px 10px; font: inherit; font-size: var(--text-sm); cursor: pointer; flex: none; }
  .open { text-decoration: none; }
  .open:hover { background: var(--row-hover); }
  button:disabled { opacity: .5; cursor: default; }
  button:focus-visible, .open:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 3px; }
  @media (pointer: coarse) { button, .open { min-height: 40px; padding: 9px 12px; box-sizing: border-box; } }
  .blank { margin-top: 8px; border: 1px solid var(--edge); border-radius: 10px; padding: 36px 24px; text-align: center; display: flex; flex-direction: column; align-items: center; gap: 8px; }
  .blank h3 { margin: 0; color: var(--fg); font-size: var(--text-lg); font-weight: 500; letter-spacing: -.02em; }
  .blank p { margin: 0; max-width: 40ch; }
  .error { color: var(--warn); font-size: var(--text-sm); }
  @media (max-width: 480px) { .project { align-items: flex-start; flex-direction: column; gap: 10px; } }
</style>
