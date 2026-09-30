<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { proCloudProjects, proOpenCloudProject, onProChanged, type CloudProject } from "../net/native";
  import { pageVisible } from "../shared/visibility";
  import { asyncDisposer } from "../shared/asyncDisposer";
  import type { Workspace } from "../workspace/sessions";
  import { RETURN_WINDOW_ENDED_COPY } from "./presentation";
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
  let revision = $state(0);
  let generation = 0;
  /** A project refused because it is mid-step in the cloud opens by itself once
   * it reaches a pause: retried on each list refresh while Home shows, for a
   * bounded time, and only after its folder is saved so no picker reappears. */
  let retry: { id: string; until: number } | null = null;
  const BUSY = "This project is finishing a step in the cloud. It opens here as soon as that step finishes.";
  /** The app's fixed codes for a project that didn't open (`open_code` in
   * shell/pro/projects.rs); anything else reads as the generic line. */
  const OPEN_ERRORS: Record<string, string> = {
    project_busy: BUSY,
    project_folder_not_empty: "That folder already contains files. Choose a new empty project folder so your existing work stays untouched.",
    project_folder_missing: "This project's local folder is unavailable. Restore or reconnect that folder, then try again. No new copy was created.",
    project_folder_nested: "Choose a folder that isn't inside another project or Git repository.",
    account_changed: "Your account changed. Open the project again.",
    project_already_opening: "Another project is opening. Try again when it's done.",
    project_unavailable: "This project isn't available in the cloud right now. Its cloud copy is intact. Try again shortly.",
    return_window_ended: RETURN_WINDOW_ENDED_COPY,
  };
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
    if (browser || !$pageVisible) return;
    untrack(() => void load());
    const timer = setInterval(() => void load(), 30_000);
    return () => { generation += 1; clearInterval(timer); };
  });
  onMount(() => browser ? undefined : asyncDisposer(onProChanged(() => { revision += 1; })));
  async function open(project: CloudProject): Promise<void> {
    if (busy !== null) return;
    busy = project.workspace_id;
    // An automatic retry keeps its explanation on screen while it runs.
    if (retry?.id !== project.workspace_id) error = null;
    try {
      const local = await proOpenCloudProject(project.workspace_id);
      retry = null;
      if (local !== null) onOpen?.({ id: local.workspace_id, root: local.root, name: local.name });
    } catch (reason) {
      const code = reason instanceof Error ? reason.message : String(reason);
      const waiting = code === "project_busy";
      retry = waiting ? { id: project.workspace_id, until: retry?.id === project.workspace_id ? retry.until : Date.now() + 15 * 60_000 } : null;
      error = Object.hasOwn(OPEN_ERRORS, code) ? OPEN_ERRORS[code] : "This project couldn't open here. Its cloud copy is intact. Please try again shortly.";
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
{:else if cloudOnly.length > 0 || error}
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
