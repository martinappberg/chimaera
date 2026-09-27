<script lang="ts">
  /**
   * The Plugins segment of the Extensions tab: first the chimaera plugins as
   * cards — glyph tile · name · version · quiet tags (`chimaera` for a
   * first-party plugin, `verified` / `unverified · local`) · an **Update to
   * x.y.z** chip when a check found a newer release · summary · then the
   * state: an **Install x.y.z** button for a first-party plugin not
   * installed on this host, else the on/off switch for THIS workspace.
   * Below it, plain sentences: what it found here, what it adds "For
   * agents" and "For you" (what makes opt-in honest), the agent plugins it
   * requires or recommends (requirementsModel.ts — only agents installed on
   * this host), the installed line with Use previous / Check now / Remove,
   * the fault when it can't run, one outcome line per change. A quiet row
   * installs a plugin from a repository. Then the agent plugins, as each
   * agent CLI reports them. Agent state is read from the agents, never
   * guessed; versions, sources and verification from the daemon.
   */
  import ConfirmDialog from "../shared/ConfirmDialog.svelte";
  import Switch from "../shared/Switch.svelte";
  import { knowledge } from "../workspace/knowledge";
  import { installedOutcome, installTitle, pinnedVersion, updatedOutcome, type Outcome } from "./installCopy";
  import { requirementsModel, type AgentsState, type PillTone } from "./requirementsModel";
  import {
    changeWorkbenchPlugin,
    installFirstPartyPlugin,
    installPlugin,
    installWorkbenchPlugin,
    isMissingRoute,
    setWorkspacePluginOn,
    type PluginChange,
    type PluginUpdate,
    type AgentId,
    type AgentPlugins,
    type WorkspacePlugin,
  } from "./store";

  interface Props {
    plugins: WorkspacePlugin[];
    agentPlugins: AgentPlugins | null;
    agentState: AgentsState;
    agentError: string | null;
    wsId: string | null;
    onAttach: (pluginId: string) => void;
    onOpenSession: (id: string) => void;
    onRefresh: () => void;
  }

  let { plugins, agentPlugins, agentState, agentError, wsId, onAttach, onOpenSession, onRefresh }: Props =
    $props();

  /** Per-plugin in-flight/failed switch state. */
  let busy = $state(new Set<string>());
  let errors = $state(new Map<string, string>());

  function message(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  async function toggle(p: WorkspacePlugin, on: boolean): Promise<void> {
    busy = new Set(busy).add(p.id);
    errors = without(errors, p.id);
    try {
      await setWorkspacePluginOn(p.id, on);
    } catch (e) {
      errors = new Map(errors).set(p.id, message(e));
    } finally {
      const next = new Set(busy);
      next.delete(p.id);
      busy = next;
    }
  }

  /** The agent's own install of an agent plugin, in a visible terminal. */
  async function install(p: WorkspacePlugin, agent: AgentId): Promise<void> {
    if (wsId === null) return;
    const key = `${p.id}:${agent}`;
    busy = new Set(busy).add(key);
    try {
      const res = await installPlugin(wsId, p.id, agent);
      onOpenSession(res.session_id);
    } catch (e) {
      errors = new Map(errors).set(
        p.id,
        isMissingRoute(e) ? "this daemon can't run installs yet — use the agent's own plugin manager" : message(e),
      );
    } finally {
      const next = new Set(busy);
      next.delete(key);
      busy = next;
    }
  }

  /** The tile's two letters: a fixed pair for the first-party ids, else
   *  the id's first two characters. */
  function tile(id: string): string {
    if (id === "mycelium") return "my";
    if (id === "agent-notes") return "an";
    if (id === "latex") return "TeX";
    return id.slice(0, 2);
  }

  type Change = "install" | "update" | "rollback" | "remove" | "check";

  /** One-line outcomes of the last change per plugin (the verified
   *  checksum after an install or update, whole in the title). */
  let notes = $state(new Map<string, Outcome>());
  /** The plugin whose Remove waits for the confirm dialog. */
  let removing = $state<WorkspacePlugin | null>(null);
  let removeError = $state<string | null>(null);

  function without<V>(m: Map<string, V>, key: string): Map<string, V> {
    return new Map([...m].filter(([k]) => k !== key));
  }

  /** The outcome line; null when there is no card left to carry it (a
   *  removed plugin that chimaera doesn't pin). */
  function outcome(
    kind: Change,
    p: WorkspacePlugin,
    res: PluginChange | { update: PluginUpdate | null },
  ): Outcome | null {
    if (kind === "check") {
      const u = (res as { update: PluginUpdate | null }).update;
      return { text: u !== null ? `${u.version} is available` : `no release newer than ${p.version}` };
    }
    const c = res as PluginChange;
    if (kind === "install") return installedOutcome(c, p.name);
    if (kind === "rollback") return { text: `back to ${c.version} — Use previous returns to ${c.previous}` };
    if (kind === "remove") return c.plugin ? { text: "removed — Install brings it back" } : null;
    return updatedOutcome(c);
  }

  async function change(p: WorkspacePlugin, kind: Change): Promise<boolean> {
    const key = `${p.id}:change`;
    busy = new Set(busy).add(key);
    errors = without(errors, p.id);
    notes = without(notes, p.id);
    try {
      const res = kind === "install" ? await installFirstPartyPlugin(p.id) : await changeWorkbenchPlugin(kind, p.id);
      const line = outcome(kind, p, res);
      if (line !== null) notes = new Map(notes).set(p.id, line);
      return true;
    } catch (e) {
      const text =
        kind === "install" && isMissingRoute(e) ? "this daemon can't install plugins yet — update chimaera" : message(e);
      if (kind === "remove") removeError = text;
      else errors = new Map(errors).set(p.id, text);
      return false;
    } finally {
      const next = new Set(busy);
      next.delete(key);
      busy = next;
    }
  }

  /** The install-from-a-repository row: what was typed, whether its install
   *  is in flight, and its one outcome line (the error flag picks the style). */
  let repo = $state("");
  let adding = $state(false);
  let added = $state<(Outcome & { error: boolean }) | null>(null);

  async function addFromRepository(): Promise<void> {
    const github = repo.trim();
    if (github === "" || adding) return;
    adding = true;
    added = null;
    try {
      const res = await installWorkbenchPlugin(github);
      added = { ...installedOutcome(res, res.id), error: false };
      repo = "";
    } catch (e) {
      added = {
        text: isMissingRoute(e) ? "this daemon can't install plugins yet — update chimaera" : message(e),
        error: true,
      };
    } finally {
      adding = false;
    }
  }

  async function confirmRemove(): Promise<void> {
    if (removing === null) return;
    if (await change(removing, "remove")) removing = null;
  }

  function removeBody(p: WorkspacePlugin): string {
    const versions = [p.version, p.previous].filter((v) => v !== null && v !== "").join(" and ");
    const what = `This deletes ${p.name} ${versions} from this host.`;
    return p.first_party ? `${what} You can install it again from here.` : `${what} Workspaces where it is on lose what it adds.`;
  }

  /** The version's tooltip: an installed first-party copy running another
   *  version than the one chimaera pins says which one that is. */
  function pinTitle(p: WorkspacePlugin): string | undefined {
    return p.installed && p.first_party && p.pinned_version !== null && p.pinned_version !== p.version
      ? `chimaera pins ${p.pinned_version}`
      : undefined;
  }

  function verifiedTitle(p: WorkspacePlugin): string {
    const base = "This copy matches its release's SHA256SUMS";
    return p.sha256_wasm !== null ? `${base} · plugin.wasm ${p.sha256_wasm}` : base;
  }

  const k = $derived($knowledge);

  /** What the plugin found in this workspace, when it found something. */
  function hereLine(p: WorkspacePlugin): string | null {
    if (typeof p.provides.knowledge === "string" && p.active && k !== null && k.provider !== null) {
      const c = k.counts;
      return `.living/ found — ${c.findings} finding${c.findings === 1 ? "" : "s"} · ${c.decisions} decision${c.decisions === 1 ? "" : "s"} · ${c.learnings} learning${c.learnings === 1 ? "" : "s"} · ${c.open} open`;
    }
    if (p.detect.length === 0) return null;
    if (p.detected) return `${p.detect[0]} found`;
    return null;
  }

  function reqModel(p: WorkspacePlugin) {
    return requirementsModel({
      requires: p.requires,
      recommends: p.recommends,
      knowledge: p.provides.knowledge,
      report: agentPlugins,
      state: agentState,
    });
  }

  function toneClass(t: PillTone): string {
    return `pill ${t}`;
  }

  /** The workbench plugin whose agent plugin `id` (as `agent` lists it) is
   *  required or recommended — the agent card's "review hooks" link. */
  function wantedBy(agent: string, id: string): WorkspacePlugin | undefined {
    const base = id.split("@")[0];
    return plugins.find((p) =>
      [...p.requires, ...p.recommends].some(
        (r) => r.agent === agent && (r.id === id || r.id.split("@")[0] === base),
      ),
    );
  }

  function fmtTokens(n: number): string {
    return n >= 1000 ? `~${(n / 1000).toFixed(1).replace(/\.0$/, "")}k` : `~${n}`;
  }
</script>

<section class="wb" aria-labelledby="wb-title">
  <div class="shead">
    <h2 id="wb-title" class="lbl">Chimaera plugins</h2>
    <span class="hint">sandboxed, inside chimaera</span>
  </div>

  {#if plugins.length === 0}
    <p class="empty">No plugins on this host yet — install one from a repository below.</p>
  {/if}

  {#each plugins as p (p.id)}
    {@const here = hereLine(p)}
    {@const changing = busy.has(`${p.id}:change`)}
    {@const available = p.source === "available"}
    {@const req = reqModel(p)}
    {@const note = notes.get(p.id)}
    <article class="card" class:active={p.active}>
      <div class="top">
        <span class="tile mono" class:on={p.active} aria-hidden="true">{tile(p.id)}</span>
        <div class="ident">
          <div class="nameline">
            <span class="name">{p.name}</span>
            {#if p.version !== ""}<span class="ver mono" title={pinTitle(p)}>{p.version}</span>{/if}
            {#if p.first_party}
              <span class="pill neutral small" title="Maintained with Chimaera and pinned in its plugins.lock">chimaera</span>
            {/if}
            {#if p.verified}
              <span class="pill neutral small" title={verifiedTitle(p)}>verified</span>
            {:else if p.local_path !== null}
              <span class="pill neutral small" title="installed from {p.local_path}">unverified · local</span>
            {/if}
            {#if p.installed && p.update !== null}
              <button
                class="pill good small chip"
                disabled={changing}
                title="download {p.update.version} from its release, verify its checksum, and switch to it"
                onclick={() => void change(p, "update")}
              >
                {changing ? "updating…" : `Update to ${p.update.version}`}
              </button>
            {/if}
          </div>
          <div class="summary">{p.summary}</div>
        </div>
        {#if available}
          <button class="opt primary install" disabled={changing} title={installTitle(p)} onclick={() => void change(p, "install")}>
            {changing ? "installing…" : `Install ${pinnedVersion(p)}`}
          </button>
        {:else}
          <span class="state" class:good={p.active}>
            {#if p.active}
              <span class="dot"></span>active here
            {:else if p.on}
              on · nothing detected yet
            {:else if p.detected && p.detect.length > 0}
              off · {p.detect[0]} found
            {:else}
              off
            {/if}
          </span>
          <Switch
            on={p.on}
            label="{p.name} {p.on ? 'on' : 'off'} in this workspace"
            disabled={busy.has(p.id)}
            onToggle={(next) => void toggle(p, next)}
          />
        {/if}
      </div>

      {#if !available || note !== undefined || errors.has(p.id) || req.rows.length > 0 || req.notice !== null}
        <div class="lines">
          {#if here !== null}
            <p>{here}</p>
          {:else if p.on && !p.detected && p.setup !== null}
            <p class="muted">
              nothing detected yet —
              <button class="link" onclick={() => onAttach(p.id)}>set it up →</button>
            </p>
          {/if}
          {#if p.adds.agents.length > 0}
            <p><span class="muted">For agents:</span> {p.adds.agents.join(" · ")}</p>
          {/if}
          {#if p.adds.ui.length > 0}
            <p><span class="muted">For you:</span> {p.adds.ui.join(" · ")}</p>
          {/if}
          {#if req.notice !== null}
            <p class="muted">{req.notice}</p>
          {/if}
          {#each req.rows as r (`${r.kind}:${r.agent}:${r.id}`)}
            <div class="req">
              <span class:muted={r.kind === "recommends" && r.status === "missing"} class:warn-text={r.status === "disabled" && r.kind === "recommends"}
                >{r.text}</span
              >
              {#if r.pill !== null}
                <span class={toneClass(r.pill.tone)} title={r.scope ?? undefined}>{r.pill.text}</span>
              {/if}
              {#if r.untrustedHooks.length > 0}
                <span class="pill warn"
                  >{r.agent} · {r.untrustedHooks.length} hook{r.untrustedHooks.length === 1 ? "" : "s"} not trusted</span
                >
                <button class="link strong" onclick={() => onAttach(p.id)}>Review &amp; trust →</button>
              {/if}
              {#if r.offerInstall}
                <button
                  class="opt"
                  disabled={busy.has(`${p.id}:${r.agent}`)}
                  title="runs {r.agent}'s own plugin manager in a visible terminal"
                  onclick={() => void install(p, r.agent as AgentId)}
                >
                  {busy.has(`${p.id}:${r.agent}`) ? "starting…" : r.kind === "requires" ? `Install for ${r.agent}` : "Install"}
                </button>
              {/if}
            </div>
          {/each}
          {#if agentState === "error" && agentError !== null && req.phase !== "none"}
            <p><span class="err">{agentError}</span> <button class="link" onclick={onRefresh}>retry</button></p>
          {/if}
          {#if p.installed}
            <div class="acts">
              <span class="muted">
                <span title={p.path ?? undefined}>installed <span class="mono">{p.version}</span></span
                >{#if p.previous !== null}{" "}· <span class="mono">{p.previous}</span> available to go back to{/if}
              </span>
              {#if p.previous !== null}
                <button class="link" disabled={changing} onclick={() => void change(p, "rollback")}>Use previous</button>
              {/if}
              <button class="link" disabled={changing} onclick={() => void change(p, "check")}>
                {changing ? "working…" : "Check now"}
              </button>
              <button
                class="link danger"
                disabled={changing}
                onclick={() => {
                  removeError = null;
                  removing = p;
                }}>Remove</button
              >
            </div>
          {/if}
          {#if p.fault !== null}
            <p class="err">{p.fault}</p>
          {/if}
          {#if note !== undefined}
            <p class="muted" title={note.title}>{note.text}</p>
          {/if}
          {#if errors.has(p.id)}
            <p class="err">{errors.get(p.id)}</p>
          {/if}
        </div>
      {/if}
    </article>
  {/each}

  <form
    class="add"
    onsubmit={(e) => {
      e.preventDefault();
      void addFromRepository();
    }}
  >
    <div class="addrow">
      <label class="addlabel" for="wb-add-repo">Install from a repository</label>
      <input
        id="wb-add-repo"
        type="text"
        placeholder="owner/repo"
        spellcheck="false"
        autocomplete="off"
        autocapitalize="off"
        readonly={adding}
        bind:value={repo}
      />
      <button
        type="submit"
        class="opt"
        disabled={repo.trim() === "" || adding}
        title="download the latest release, verify its checksums, and install it on this host"
      >
        {adding ? "installing…" : "Install"}
      </button>
    </div>
    <p class="hint">
      Downloads the latest release (<span class="mono">plugin.wasm</span>, <span class="mono">plugin.toml</span>,
      verified against its <span class="mono">SHA256SUMS</span>) into <span class="mono">~/.chimaera/plugins</span> on
      this host. It runs sandboxed inside chimaera and does nothing until you switch it on in a workspace.
    </p>
    {#if added !== null}
      <p class={added.error ? "err" : "muted"} title={added.title}>{added.text}</p>
    {/if}
  </form>
</section>

{#if removing !== null}
  <ConfirmDialog
    title="Remove {removing.name}?"
    body={removeBody(removing)}
    confirmLabel={busy.has(`${removing.id}:change`) ? "removing…" : "Remove"}
    danger
    error={removeError}
    onConfirm={() => void confirmRemove()}
    onCancel={() => {
      if (removing !== null && !busy.has(`${removing.id}:change`)) removing = null;
    }}
  />
{/if}

<section class="ag" aria-labelledby="ag-title">
  <div class="shead">
    <h2 id="ag-title" class="lbl">Agent plugins</h2>
    <span class="hint">inside each agent, managed with its own plugin manager</span>
  </div>
  {#if agentState === "unavailable"}
    <p class="empty">This daemon can't ask the agents about their plugins yet — update chimaera.</p>
  {:else if agentState === "error" && agentPlugins === null}
    <p class="empty err">{agentError} <button class="link" onclick={onRefresh}>retry</button></p>
  {:else if agentPlugins === null}
    <p class="empty">asking claude and codex…</p>
  {:else}
    <div class="agents">
      {#each agentPlugins.agents as a (a.agent)}
        <div class="acard">
          <div class="ahead">
            <span class="aname">{a.agent}</span>
            {#if a.version}<span class="ver mono">{a.version}</span>{/if}
            <span class="acount">
              {#if !a.available}not installed on this host{:else}{a.plugins.length} plugin{a.plugins.length === 1 ? "" : "s"}{/if}
            </span>
          </div>
          {#if a.error}
            <div class="abody err">{a.error}</div>
          {:else if !a.available}
            <div class="abody muted">{a.agent} isn't installed on this host, so it has no plugins here.</div>
          {:else if a.plugins.length === 0}
            <div class="abody muted">no plugins</div>
          {:else}
            {#each a.plugins as pl (pl.id)}
              {@const waiting = (a.hooks ?? []).filter(
                (h) => (h.plugin_id === pl.id || h.plugin_id === pl.id.split("@")[0]) && (h.trust === "untrusted" || h.trust === "modified"),
              ).length}
              <div class="abody">
                <div class="prow">
                  <span class="mono pname">{pl.id.split("@")[0]}</span>
                  <span class="mono ver">{[pl.version, pl.scope].filter(Boolean).join(" · ")}</span>
                  <span class="pstate" class:good={pl.enabled}>{pl.enabled ? "enabled" : "disabled"}</span>
                </div>
                {#if pl.skills_n !== undefined || pl.hooks_n !== undefined || pl.always_on_tokens !== undefined}
                  <div class="pmeta">
                    {#if pl.skills_n !== undefined}{pl.skills_n} skill{pl.skills_n === 1 ? "" : "s"}{/if}
                    {#if pl.hooks_n !== undefined}
                      {#if pl.skills_n !== undefined} · {/if}
                      {#if waiting > 0}<span class="warn-text">{waiting} hook{waiting === 1 ? "" : "s"} waiting for your trust</span>{:else}{pl.hooks_n} hook{pl.hooks_n === 1 ? "" : "s"}{/if}
                    {/if}
                    {#if pl.always_on_tokens !== undefined}
                      {#if pl.skills_n !== undefined || pl.hooks_n !== undefined} · {/if}
                      <span class="fg">{fmtTokens(pl.always_on_tokens)} tokens in every session</span>
                    {/if}
                  </div>
                {/if}
                {#if waiting > 0}
                  {@const wb = wantedBy(a.agent, pl.id)}
                  {#if wb !== undefined}
                    <div class="plinks"><button class="link" onclick={() => onAttach(wb.id)}>review hooks</button></div>
                  {/if}
                {/if}
              </div>
            {/each}
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</section>

<style>
  .wb,
  .ag {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .shead {
    display: flex;
    align-items: baseline;
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
  .hint {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .mono {
    font-family: var(--mono);
  }
  .muted {
    color: var(--muted);
  }
  .fg {
    color: var(--fg);
  }
  .empty {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--muted);
    line-height: 1.5;
  }
  .err {
    color: var(--err);
    font-size: var(--text-xs);
  }
  .warn-text {
    color: var(--warn);
  }

  .card {
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 12px;
    padding: 16px 20px;
    display: flex;
    flex-direction: column;
    gap: 14px;
    transition: border-color 0.12s ease;
  }
  .card.active {
    border-color: color-mix(in srgb, var(--accent) 35%, var(--edge));
  }
  .top {
    display: flex;
    align-items: center;
    gap: 12px;
    min-width: 0;
  }
  .tile {
    flex: none;
    width: 34px;
    height: 34px;
    border-radius: 8px;
    background: color-mix(in srgb, var(--fg) 5%, transparent);
    color: var(--muted);
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: var(--text-sm);
    font-weight: 600;
  }
  .tile.on {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    color: var(--accent);
  }
  .ident {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .nameline {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 4px 8px;
  }
  .name {
    font-size: var(--text-lg);
    font-weight: 600;
  }
  .ver {
    font-size: 11.5px;
    color: var(--muted);
  }
  .summary {
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .state {
    margin-left: auto;
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: var(--text-xs);
    color: var(--muted);
    white-space: nowrap;
  }
  .state.good {
    color: var(--accent);
  }
  /* Install x.y.z stands where the switch stands on an installed card. */
  .install {
    margin-left: auto;
    flex: none;
    white-space: nowrap;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--accent);
  }

  /* Plain sentences under the name, aligned with it (past the tile). */
  .lines {
    display: flex;
    flex-direction: column;
    gap: 7px;
    font-size: var(--text-sm);
    padding-left: 46px;
    line-height: 1.45;
    min-width: 0;
  }
  /* A fault or refusal can carry a 64-hex checksum or a path. */
  .lines p {
    margin: 0;
    overflow-wrap: anywhere;
  }
  @container (max-width: 640px) {
    .lines {
      padding-left: 0;
    }
  }
  /* One agent plugin: its sentence, then the pill / trust / install. */
  .req {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 10px;
    align-items: center;
  }
  .pill {
    font-size: 12.5px;
    padding: 2px 10px;
    border-radius: 999px;
    white-space: nowrap;
  }
  .pill.good {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    color: var(--accent);
  }
  .pill.warn {
    background: color-mix(in srgb, var(--warn) 11%, transparent);
    color: var(--warn);
  }
  .pill.neutral {
    border: 1px solid var(--edge);
    color: var(--muted);
  }
  .pill.small {
    font-size: 11px;
    padding: 1px 8px;
  }
  /* The Update chip: a pill that is a button. */
  .pill.chip {
    appearance: none;
    border: none;
    font-family: inherit;
    font-weight: 500;
    cursor: pointer;
  }
  .pill.chip:not(:disabled):hover {
    background: color-mix(in srgb, var(--accent) 20%, transparent);
  }
  .pill.chip:disabled {
    cursor: default;
    opacity: 0.7;
  }
  .acts {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 6px 14px;
  }
  .link {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--accent);
    cursor: pointer;
  }
  .link:hover {
    text-decoration: underline;
  }
  .link:disabled {
    color: var(--muted);
    cursor: default;
    text-decoration: none;
  }
  .link.danger {
    color: var(--err);
  }
  .link.strong {
    font-weight: 500;
  }

  /* Add from a repository: a quiet row under the cards, not another card.
     The field and its button share padding, radius and line-height so they
     stand the same height side by side. */
  .add {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 4px 2px 0;
    font-size: var(--text-sm);
    line-height: 1.45;
  }
  /* A refusal can carry a 64-hex checksum or a URL: break it rather than
     widen the page at phone width. */
  .add p {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .addrow {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 12px;
  }
  .addlabel {
    color: var(--fg);
  }
  .addrow input {
    flex: 1 1 200px;
    min-width: 0;
    max-width: 320px;
    font: inherit;
    font-family: var(--mono);
    color: var(--fg);
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 6px;
    padding: 4px 10px;
    outline: none;
  }
  .addrow input:focus {
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
  }
  .addrow input::placeholder {
    color: var(--muted);
  }
  .addrow .opt {
    padding: 4px 14px;
    border-radius: 6px;
  }

  .agents {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 14px;
  }
  @container (max-width: 700px) {
    .agents {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  .acard {
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 12px;
    overflow: hidden;
  }
  .ahead {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 11px 16px;
    border-bottom: 1px solid var(--edge);
  }
  .aname {
    font-size: var(--text-md);
    font-weight: 600;
  }
  .acount {
    margin-left: auto;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .abody {
    padding: 12px 16px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: var(--text-sm);
  }
  .abody + .abody {
    border-top: 1px solid var(--edge);
  }
  .prow {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .pname {
    font-size: var(--text-sm);
    font-weight: 600;
  }
  .pstate {
    margin-left: auto;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .pstate.good {
    color: var(--accent);
  }
  .pmeta {
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .plinks {
    display: flex;
    gap: 14px;
    font-size: 12.5px;
    padding-top: 2px;
  }
</style>
