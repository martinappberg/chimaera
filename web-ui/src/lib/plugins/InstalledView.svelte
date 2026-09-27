<script lang="ts">
  /**
   * The Plugins segment of the Extensions tab: the chimaera plugins as cards
   * (PluginCard.svelte — one primary control each, everything secondary in
   * its "…" menu), a small form that previews or installs a plugin from a
   * repository (the preview is a PluginCard of its own under the form),
   * then the agent plugins, as each agent CLI reports them. Agent state is
   * read from the agents, never guessed; versions, sources and verification
   * from the daemon.
   */
  import ConfirmDialog from "../shared/ConfirmDialog.svelte";
  import { pageVisible } from "../shared/visibility";
  import { knowledge } from "../workspace/knowledge";
  import { installedOutcome, type Outcome } from "./installCopy";
  import PluginCard from "./PluginCard.svelte";
  import type { AgentsState } from "./requirementsModel";
  import {
    changeWorkbenchPlugin,
    checkedAt,
    installWorkbenchPlugin,
    isMissingRoute,
    previewPlugin,
    type AgentHook,
    type AgentPlugin,
    type AgentPlugins,
    type PluginChange,
    type PluginDetails,
    type WorkspacePlugin,
  } from "./store";

  interface Props {
    /** null while the first list is on its way. */
    plugins: WorkspacePlugin[] | null;
    agentPlugins: AgentPlugins | null;
    agentState: AgentsState;
    agentError: string | null;
    wsId: string | null;
    /** The tab and this view are showing (the "checked …" clock ticks). */
    visible: boolean;
    onAttach: (pluginId: string) => void;
    onOpenSession: (id: string) => void;
    onRefresh: () => void;
  }

  let { plugins: loaded, agentPlugins, agentState, agentError, wsId, visible, onAttach, onOpenSession, onRefresh }: Props =
    $props();

  /** Chimaera's own plugins first (the curated list), then the rest, each
   *  in the daemon's order. */
  const plugins = $derived([...(loaded ?? [])].sort((a, b) => Number(b.first_party) - Number(a.first_party)));

  const k = $derived($knowledge);
  const counts = $derived(k !== null && k.provider !== null ? k.counts : null);
  const anyInstalled = $derived(plugins.some((p) => p.installed));

  // The "checked 2 hours ago" lines age while someone can see them: one
  // tick a minute, only while shown and only once something was checked.
  let now = $state(Date.now());
  $effect(() => {
    if (!visible || !$pageVisible || $checkedAt.size === 0) return;
    now = Date.now();
    const t = setInterval(() => (now = Date.now()), 60_000);
    return () => clearInterval(t);
  });

  // --- Remove, behind one confirm dialog ---------------------------------------

  let removing = $state<WorkspacePlugin | null>(null);
  let removeBusy = $state(false);
  let removeError = $state<string | null>(null);

  function removeBody(p: WorkspacePlugin): string {
    const versions = [p.version, p.previous].filter((v) => v !== null && v !== "").join(" and ");
    const what = `This deletes ${p.name} ${versions} from this host.`;
    return p.first_party ? `${what} You can install it again from here.` : `${what} Workspaces where it is on lose what it adds.`;
  }

  async function confirmRemove(): Promise<void> {
    if (removing === null || removeBusy) return;
    removeBusy = true;
    removeError = null;
    try {
      await changeWorkbenchPlugin("remove", removing.id);
      removing = null;
    } catch (e) {
      removeError = e instanceof Error ? e.message : String(e);
    } finally {
      removeBusy = false;
    }
  }

  // --- install from a repository -----------------------------------------------

  let repo = $state("");
  let adding = $state(false);
  let looking = $state(false);
  let added = $state<(Outcome & { error: boolean }) | null>(null);
  /** The repository last previewed (as typed) and what its release says. */
  let previewed = $state<{ github: string; plugin: PluginDetails } | null>(null);
  const formBusy = $derived(adding || looking);

  async function addFromRepository(): Promise<void> {
    const github = repo.trim();
    if (github === "" || formBusy) return;
    adding = true;
    added = null;
    try {
      const res = await installWorkbenchPlugin(github);
      added = { ...installedOutcome(res, res.id), error: false };
      repo = "";
      previewed = null;
    } catch (e) {
      added = {
        text: isMissingRoute(e) ? "this daemon can't install plugins yet — update chimaera" : e instanceof Error ? e.message : String(e),
        error: true,
      };
    } finally {
      adding = false;
    }
  }

  /** Preview: what the repository's release says, in a card under the form;
   *  nothing is installed. A refusal is the outcome line, as for Install. */
  async function previewRepository(): Promise<void> {
    const github = repo.trim();
    if (github === "" || formBusy) return;
    looking = true;
    added = null;
    try {
      previewed = { github, plugin: await previewPlugin(github) };
    } catch (e) {
      previewed = null;
      added = {
        text: isMissingRoute(e) ? "this daemon can't preview plugins yet — update chimaera" : e instanceof Error ? e.message : String(e),
        error: true,
      };
    } finally {
      looking = false;
    }
  }

  /** The preview's own Install: the repository it previewed. Done, the
   *  preview gives way to the new card and the outcome line; a refusal
   *  stays on the preview (the card shows it). */
  async function installPreviewed(): Promise<PluginChange> {
    if (previewed === null) throw new Error("nothing previewed");
    const github = previewed.github;
    const res = await installWorkbenchPlugin(github);
    added = { ...installedOutcome(res, res.id), error: false };
    previewed = null;
    if (repo.trim() === github) repo = "";
    return res;
  }

  // --- agent plugins ------------------------------------------------------------

  /** The codex hooks of one agent plugin still waiting for the user's trust. */
  function waitingHooks(hooks: AgentHook[] | undefined, pl: AgentPlugin): number {
    const base = pl.id.split("@")[0];
    return (hooks ?? []).filter(
      (h) => (h.plugin_id === pl.id || h.plugin_id === base) && (h.trust === "untrusted" || h.trust === "modified"),
    ).length;
  }

  /** The chimaera plugin that asks for agent plugin `id` (as `agent` lists
   *  it) — its attach sheet is where those hooks get reviewed. */
  function wantedBy(agent: string, id: string): WorkspacePlugin | undefined {
    const base = id.split("@")[0];
    return plugins.find((p) =>
      [...p.requires, ...p.recommends].some((r) => r.agent === agent && (r.id === id || r.id.split("@")[0] === base)),
    );
  }

  /** "2.1.283 (Claude Code)" · "codex-cli 0.157.1" → the version itself. */
  function agentVersion(v: string): string {
    return v.match(/\d+\.\d+\.\d+[\w.+-]*/)?.[0] ?? v;
  }

  function fmtTokens(n: number): string {
    return n >= 1000 ? `~${(n / 1000).toFixed(1).replace(/\.0$/, "")}k` : `~${n}`;
  }

  /** "4 skills · 2 hooks · ~1.2k tokens in every session". */
  function metaLine(pl: AgentPlugin): string {
    const parts: string[] = [];
    if (pl.skills_n !== undefined && pl.skills_n > 0) parts.push(`${pl.skills_n} skill${pl.skills_n === 1 ? "" : "s"}`);
    if (pl.hooks_n !== undefined && pl.hooks_n > 0) parts.push(`${pl.hooks_n} hook${pl.hooks_n === 1 ? "" : "s"}`);
    if (pl.always_on_tokens !== undefined && pl.always_on_tokens > 0) {
      parts.push(`${fmtTokens(pl.always_on_tokens)} tokens in every session`);
    }
    return parts.join(" · ");
  }
</script>

<section class="sec" aria-labelledby="wb-title">
  <h2 id="wb-title" class="lbl">Chimaera plugins</h2>

  {#if loaded === null}
    <p class="empty">looking for plugins…</p>
  {:else if plugins.length === 0}
    <p class="empty">No plugins on this host yet — install one from a repository below.</p>
  {/if}

  {#each plugins as p (p.id)}
    <PluginCard
      plugin={p}
      {agentPlugins}
      {agentState}
      {agentError}
      {counts}
      {wsId}
      {now}
      {onAttach}
      {onOpenSession}
      {onRefresh}
      removing={removing?.id === p.id && removeBusy}
      onRemove={(x) => {
        removeError = null;
        removing = x;
      }}
    />
  {/each}

  {#if plugins.length > 0 && !anyInstalled}
    <p class="empty">Nothing installed yet — pick one above.</p>
  {/if}

  <form
    class="add"
    onsubmit={(e) => {
      e.preventDefault();
      void addFromRepository();
    }}
  >
    <label class="addlabel" for="wb-add-repo">Install from a repository</label>
    <div class="field">
      <input
        id="wb-add-repo"
        type="text"
        placeholder="owner/repo"
        spellcheck="false"
        autocomplete="off"
        autocapitalize="off"
        aria-describedby="wb-add-help"
        readonly={formBusy}
        bind:value={repo}
      />
      <button type="button" class="opt" disabled={repo.trim() === "" || formBusy} onclick={() => void previewRepository()}>
        {looking ? "Looking…" : "Preview"}
      </button>
      <button type="submit" class="opt primary" disabled={repo.trim() === "" || formBusy}>
        {adding ? "Installing…" : "Install"}
      </button>
    </div>
    <p id="wb-add-help" class="help">
      The latest release of a plugin's GitHub repository: Preview shows what it adds, Install puts it on this host. It
      does nothing until you switch it on in a workspace.
    </p>
    {#if added !== null}
      <p class="outcome" class:err={added.error} role={added.error ? "alert" : "status"}>{added.text}</p>
    {/if}
  </form>

  {#if previewed !== null}
    {@const pv = previewed}
    {#key `${pv.github}@${pv.plugin.version}`}
      <PluginCard
        plugin={pv.plugin}
        {agentPlugins}
        {agentState}
        {agentError}
        {counts}
        {wsId}
        {now}
        {onAttach}
        {onOpenSession}
        {onRefresh}
        preview={{ install: installPreviewed, close: () => (previewed = null) }}
      />
    {/key}
  {/if}
</section>

{#if removing !== null}
  <ConfirmDialog
    title="Remove {removing.name}?"
    body={removeBody(removing)}
    confirmLabel={removeBusy ? "Removing…" : "Remove"}
    danger
    error={removeError}
    onConfirm={() => void confirmRemove()}
    onCancel={() => {
      if (!removeBusy) removing = null;
    }}
  />
{/if}

<section class="sec" aria-labelledby="ag-title">
  <h2 id="ag-title" class="lbl">Agent plugins</h2>
  {#if agentState === "unavailable"}
    <p class="empty">This daemon can't ask the agents about their plugins yet — update chimaera.</p>
  {:else if agentState === "error" && agentPlugins === null}
    <p class="empty">
      <span class="err">Couldn't ask the agents: {agentError}</span>
      <button class="link" onclick={onRefresh}>Try again</button>
    </p>
  {:else if agentPlugins === null}
    <p class="empty">asking claude and codex…</p>
  {:else}
    <div class="agents">
      {#each agentPlugins.agents as a (a.agent)}
        <div class="agroup">
          <div class="ghead">
            <span class="gname">{a.agent}</span>
            {#if a.version}<span class="gver" title={a.version}>{agentVersion(a.version)}</span>{/if}
            {#if a.available && !a.error && a.plugins.length > 0}
              <span class="gcount">{a.plugins.length} plugin{a.plugins.length === 1 ? "" : "s"}</span>
            {/if}
          </div>
          {#if a.error}
            <p class="gline err">{a.error}</p>
          {:else if !a.available}
            <p class="gline">{a.agent} isn't installed on this host</p>
          {:else if a.plugins.length === 0}
            <p class="gline">{a.agent} has no plugins</p>
          {:else}
            <ul class="plist">
              {#each a.plugins as pl (pl.id)}
                {@const waiting = waitingHooks(a.hooks, pl)}
                {@const meta = metaLine(pl)}
                {@const wb = waiting > 0 ? wantedBy(a.agent, pl.id) : undefined}
                <li class="prow">
                  <div class="pmain">
                    <span class="pname" title={pl.id}>{pl.id.split("@")[0]}</span>
                    <span class="pver">{[pl.version, pl.scope].filter(Boolean).join(" · ")}</span>
                  </div>
                  <div class="pmeta">
                    {#if waiting > 0}
                      <span class="warn">{waiting} hook{waiting === 1 ? "" : "s"} not trusted</span>
                      {#if wb !== undefined}
                        <button class="opt small" title="See exactly what {a.agent} will run, then trust it" onclick={() => onAttach(wb.id)}
                          >Review</button
                        >
                      {:else}
                        <span>— trust them in codex's /hooks</span>
                      {/if}
                    {:else}
                      {meta}
                    {/if}
                  </div>
                  <span class="pstate" class:off={!pl.enabled}>{pl.enabled ? "enabled" : "disabled"}</span>
                </li>
              {/each}
            </ul>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</section>

<style>
  .sec {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .lbl {
    margin: 0;
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
    font-weight: 600;
  }
  .empty {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--muted);
    line-height: 1.5;
  }
  .err {
    color: var(--err);
  }
  .warn {
    color: var(--warn);
  }
  .link {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    color: var(--accent);
    cursor: pointer;
    border-radius: 3px;
  }
  .link:hover {
    text-decoration: underline;
  }
  .opt.small {
    font-size: var(--text-xs);
    padding: 2px 10px;
  }

  /* --- install from a repository: a small form ------------------------------ */
  .add {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-top: 4px;
    padding: 16px 20px;
    border: 1px solid var(--edge);
    border-radius: 12px;
  }
  .add p {
    margin: 0;
  }
  .addlabel {
    font-size: var(--text-md);
    font-weight: 600;
    color: var(--fg);
  }
  .field {
    display: flex;
    gap: 8px;
    max-width: 440px;
  }
  /* The field and its button share height and radius, like Settings' inputs. */
  .field input {
    flex: 1 1 auto;
    min-width: 0;
    font: inherit;
    font-size: var(--text-sm);
    font-family: var(--mono);
    color: var(--fg);
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 7px;
    padding: 5px 10px;
  }
  .field input:focus {
    outline: 2px solid var(--focus-ring);
    outline-offset: 1px;
  }
  .field input::placeholder {
    color: var(--muted);
  }
  .field .opt {
    flex: none;
    border-radius: 7px;
    padding: 5px 14px;
  }
  .help {
    font-size: var(--text-xs);
    line-height: 1.5;
    color: var(--muted);
    max-width: 70ch;
  }
  /* A refusal can carry a URL: break it rather than widen the page. */
  .outcome {
    font-size: var(--text-sm);
    line-height: 1.45;
    color: var(--muted);
    overflow-wrap: anywhere;
  }
  .outcome.err {
    color: var(--err);
  }

  /* --- agent plugins: one quiet group per agent, rows aligned in columns -- */
  .agents {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .agroup {
    border: 1px solid var(--edge);
    border-radius: 12px;
    background: var(--overlay-bg);
    overflow: hidden;
  }
  .ghead {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 11px 16px;
    border-bottom: 1px solid var(--edge);
  }
  .gname {
    font-family: var(--mono);
    font-size: var(--text-md);
    font-weight: 600;
  }
  .gver {
    font-family: var(--mono);
    font-size: 11.5px;
    color: var(--muted);
  }
  .gcount {
    margin-left: auto;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .gline {
    margin: 0;
    padding: 12px 16px;
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .gline.err {
    color: var(--err);
    overflow-wrap: anywhere;
  }
  .plist {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  /* Name + version · scope | what it brings | enabled — the same columns in
     every row, so the list reads down. */
  .prow {
    display: grid;
    grid-template-columns: minmax(0, 260px) minmax(0, 1fr) auto;
    align-items: baseline;
    column-gap: 18px;
    row-gap: 2px;
    padding: 9px 16px;
    font-size: var(--text-sm);
  }
  .prow + .prow {
    border-top: 1px solid var(--edge);
  }
  .pmain {
    display: flex;
    align-items: baseline;
    gap: 8px;
    min-width: 0;
  }
  .pname {
    font-family: var(--mono);
    font-weight: 600;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .pver {
    font-family: var(--mono);
    font-size: 11.5px;
    color: var(--muted);
    white-space: nowrap;
  }
  .pstate {
    font-size: var(--text-xs);
    color: var(--accent);
  }
  .pstate.off {
    color: var(--muted);
  }
  .pmeta {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 4px 10px;
    font-size: var(--text-xs);
    color: var(--muted);
    min-width: 0;
  }

  @container (max-width: 600px) {
    .add {
      padding: 14px 16px;
    }
    /* Narrow: name and state on the first line, the rest under the name. */
    .prow {
      grid-template-columns: minmax(0, 1fr) auto;
    }
    .pmeta {
      grid-column: 1;
      grid-row: 2;
    }
    .pmeta:empty {
      display: none;
    }
  }
</style>
