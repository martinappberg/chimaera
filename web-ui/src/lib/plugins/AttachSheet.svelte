<script lang="ts">
  /**
   * "Use <plugin> for Knowledge" — the one sheet, live-checked steps
   * (design §6.2): 0 install the plugin itself when it isn't on this host
   * yet (the card's Install x.y.z flow), 1 the agent plugins it requires or
   * recommends, for the agents installed here (requirementsModel.ts — the
   * CLIs' own installs, in a visible terminal; a recommendation reads as
   * optional), 2 trust codex's hooks right here (§6.6 — each hook in plain
   * words, hash-pinned, never automated), 3 set up this workspace (the
   * plugin's own setup prompt sent to a new agent session of the user's
   * choosing — their click, their billing). Completing also switches the
   * plugin ON for the workspace. Re-checks every time it opens.
   */
  import { onMount } from "svelte";
  import { ApiError } from "../net/api";
  import { focusOnMount } from "../shared/focusOnMount";
  import { modalFocus } from "../shared/modalFocus";
  import { refreshKnowledge } from "../workspace/knowledge";
  import { footprint, footprints, installedOutcome, installTitle, pinnedVersion, type Outcome } from "./installCopy";
  import { agentsForSetup, hooksAwaitingTrust, requirementsModel, sheetText, type AgentsState, type RequirementRow } from "./requirementsModel";
  import {
    agentInstallContinuation,
    fetchAgentPlugins,
    fetchWorkspacePlugins,
    installFirstPartyPlugin,
    installPlugin,
    isMissingRoute,
    putWorkspacePlugin,
    refreshWorkspacePlugins,
    setupPlugin,
    trustHooks,
    type AgentHook,
    type AgentId,
    type AgentPlugins,
    type WorkspacePlugin,
  } from "./store";

  interface Props {
    wsId: string;
    pluginId: string;
    /** An install/setup started a session: open it (the caller closes us). */
    onOpenSession: (id: string) => void;
    onClose: () => void;
  }

  let { wsId, pluginId, onOpenSession, onClose }: Props = $props();

  let plugin = $state<WorkspacePlugin | null>(null);
  /** What its setup leaves here, from the manifest's footprint. */
  const written = $derived(plugin !== null ? footprints(plugin) : []);
  let agents = $state<AgentPlugins | null>(null);
  /** null = still checking; false = the daemon predates agent probes. */
  let agentsAvailable = $state<boolean | null>(null);
  let loadError = $state<string | null>(null);
  let setupAgent = $state<AgentId>("claude");
  /** A check has answered once: later ones keep the chosen agent. */
  let rechecked = false;
  let trust = $state(true);
  let busy = $state<string | null>(null);
  let error = $state<string | null>(null);
  let skipped = $state<{ key: string; reason: string }[]>([]);
  /** The plugin's own install from this sheet: its outcome, or its refusal. */
  let installNote = $state<Outcome | null>(null);
  let installError = $state<string | null>(null);

  function message(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  async function check(): Promise<void> {
    loadError = null;
    try {
      const [wp, ap] = await Promise.all([
        fetchWorkspacePlugins(wsId),
        fetchAgentPlugins(wsId, true).then(
          (a) => {
            agentsAvailable = true;
            return a;
          },
          (e: unknown) => {
            agentsAvailable = !(e instanceof ApiError && e.status === 404);
            if (agentsAvailable) loadError = message(e);
            return null;
          },
        ),
      ]);
      plugin = wp.plugins.find((p) => p.id === pluginId) ?? null;
      agents = ap;
      if (plugin === null) loadError = `no plugin “${pluginId}” on this daemon`;
      // Use the same readiness decision as the chooser, including every
      // required add-on and any ambiguous marketplace report. A re-check
      // keeps the user's pick while it is still eligible — it decides whose
      // account the setup bills.
      const preferred = setupAgents[0];
      if (preferred !== undefined && (!rechecked || !setupAgents.includes(setupAgent))) {
        setupAgent = preferred as AgentId;
      }
      rechecked = true;
    } catch (e) {
      loadError = message(e);
    }
  }
  onMount(() => void check());

  /** The agents report's state, in the card's terms. */
  const agentsState = $derived.by((): AgentsState => {
    if (agentsAvailable === null) return "loading";
    if (agentsAvailable === false) return "unavailable";
    return agents !== null ? "ok" : "error";
  });

  const model = $derived(
    plugin === null
      ? null
      : requirementsModel({
          requires: plugin.requires,
          recommends: plugin.recommends,
          knowledge: plugin.provides.knowledge,
          report: agents,
          state: agentsState,
        }),
  );
  const rows = $derived(model?.rows ?? []);
  const identityUncertain = $derived(rows.some((r) => r.identityAmbiguous));

  /** The plugin itself isn't on this host yet: installing it comes first. */
  const notInstalled = $derived(plugin !== null && plugin.source === "available");
  const showInstallStep = $derived(notInstalled || installNote !== null);

  const agentsDone = $derived(
    model !== null &&
      (model.phase === "ready" || model.phase === "none") &&
      model.notice === null &&
      rows.every((r) => r.status === "installed"),
  );
  const agentsWarn = $derived(
    identityUncertain || (model?.phase === "ready" && model.notice !== null) ||
      rows.some((r) => r.kind === "requires" && (r.status === "missing" || r.status === "disabled")),
  );

  /** Codex hooks of its agent plugins (required or recommended) still
   *  waiting for trust (untrusted or changed since they were trusted). */
  const hooks = $derived.by((): AgentHook[] => (model === null ? [] : hooksAwaitingTrust(model)));
  const needsTrust = $derived(hooks.length > 0);

  const detected = $derived(plugin?.detected === true);
  const canSetup = $derived(plugin?.setup !== null && plugin?.setup !== undefined);
  /** Agents that can run the setup: those with its agent plugin, or — for
   *  a plugin that asks nothing of the agents — any agent on this host. */
  const setupAgents = $derived.by((): AgentId[] => {
    if (model === null) return [];
    const available = (agents?.agents ?? []).filter((a) => a.available).map((a) => a.agent);
    return agentsForSetup(model, available) as AgentId[];
  });
  const needsSetup = $derived(!detected && canSetup);
  // An empty eligible list still means setup is blocked, not optional.
  const setupBlocked = $derived(needsSetup && !setupAgents.includes(setupAgent));

  /** Step numbers: the plugin's own install (when shown) comes first. */
  const nAgents = $derived(showInstallStep ? 2 : 1);
  const nSetup = $derived(nAgents + (needsTrust ? 2 : 1));

  /** Nothing listed and nothing to say: the agents it could use aren't here. */
  function noAgentsLine(p: WorkspacePlugin): string {
    const names = [...new Set(p.recommends.map((r) => r.agent))];
    if (names.length === 1) return `${names[0]} isn't installed on this host; ${p.name} works without it.`;
    if (names.length === 2) return `Neither ${names[0]} nor ${names[1]} is installed on this host; ${p.name} works without them.`;
    return `None of the agents it works with is installed on this host; ${p.name} works without them.`;
  }

  function rowClass(r: RequirementRow): string {
    if (r.status === "installed") return "good-text";
    if (r.tone === "warn") return "warn-text";
    return "muted-text";
  }

  /** Plain words for a hook event + matcher (the codex vocabulary stays
   *  visible in mono beside them). */
  function eventWords(ev: string): string {
    switch (ev) {
      case "SessionStart":
        return "session start";
      case "UserPromptSubmit":
        return "at your prompt";
      case "PreToolUse":
        return "before a tool";
      case "PostToolUse":
        return "after a tool";
      case "Stop":
        return "turn end";
      case "SessionEnd":
        return "session end";
      default:
        return ev;
    }
  }
  function matcherWords(m: string | null | undefined): string {
    if (m == null || m === "" || m === "*") return "always";
    return m.split("|").join(" · ");
  }
  function basename(cmd: string | null | undefined): string {
    if (cmd == null) return "";
    const first = cmd.trim().split(/\s+/)[0] ?? "";
    return first.slice(first.lastIndexOf("/") + 1);
  }

  const primaryLabel = $derived.by(() => {
    if (plugin === null) return "…";
    if (notInstalled) return "Turn on";
    const parts: string[] = [];
    if (needsTrust && trust) parts.push("Trust hooks");
    if (needsSetup) parts.push("set up");
    else if (!plugin.on) parts.push("turn on");
    if (parts.length === 0) return "Done";
    const s = parts.join(" & ");
    return s.charAt(0).toUpperCase() + s.slice(1);
  });

  /** Install the plugin itself (the version chimaera pins), then re-check:
   *  the installed manifest brings its agent plugins and setup with it. */
  async function installThis(): Promise<void> {
    if (plugin === null) return;
    busy = "install-plugin";
    installError = null;
    try {
      const res = await installFirstPartyPlugin(plugin.id);
      installNote = installedOutcome(res, plugin.name);
      await check();
    } catch (e) {
      installError = isMissingRoute(e) ? "this daemon can't install plugins yet — update chimaera" : message(e);
    } finally {
      busy = null;
    }
  }

  async function install(agent: AgentId, agentPluginId: string): Promise<void> {
    busy = `install:${agent}`;
    error = null;
    try {
      const res = await installPlugin(wsId, pluginId, agent, agentPluginId);
      onOpenSession(res.session_id);
    } catch (e) {
      error = isMissingRoute(e) ? `this daemon can't run installs yet — run ${agent}'s own plugin manager instead` : message(e);
      busy = null;
    }
  }

  async function complete(): Promise<void> {
    if (plugin === null || notInstalled || identityUncertain) return;
    if (setupBlocked) {
      error = "Choose an agent with the required plugins enabled.";
      return;
    }
    busy = "complete";
    error = null;
    skipped = [];
    try {
      if (needsTrust && trust) {
        // Exactly the {key, hash} pairs the user saw — the daemon writes
        // only those whose current hash still matches.
        const res = await trustHooks(
          wsId,
          pluginId,
          hooks.map((h) => ({ key: h.key, hash: h.hash })),
        );
        skipped = res.skipped;
        if (skipped.length > 0) {
          // A hook changed after it was shown: nothing past this step
          // runs on a trust the user didn't give. Re-read the hooks so
          // the list (and the next click) carries their current hashes.
          await check();
          busy = null;
          return;
        }
      }
      if (!plugin.on) await putWorkspacePlugin(wsId, pluginId, true);
      refreshWorkspacePlugins();
      if (needsSetup) {
        const res = await setupPlugin(wsId, pluginId, setupAgent);
        refreshKnowledge();
        clearContinuation();
        onOpenSession(res.session_id);
        return;
      }
      refreshKnowledge();
      clearContinuation();
      onClose();
    } catch (e) {
      error = isMissingRoute(e) ? "this daemon can't finish that step yet — update chimaera" : message(e);
      refreshWorkspacePlugins();
      // Steps before the failure may have landed (hooks trusted, switched
      // on): show where things are now, so a retry doesn't redo them.
      await check();
      busy = null;
    }
  }

  function clearContinuation(): void {
    if ($agentInstallContinuation?.workspaceId === wsId && $agentInstallContinuation.pluginId === pluginId) {
      agentInstallContinuation.set(null);
    }
  }

  function onKey(e: KeyboardEvent): void {
    if (e.key === "Escape") {
      e.stopPropagation();
      onClose();
    }
  }
</script>

<div class="backdrop" role="presentation" onclick={onClose} onkeydown={onKey}>
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div
    class="sheet"
    role="dialog"
    aria-modal="true"
    aria-labelledby="attach-title"
    tabindex="-1"
    use:modalFocus
    onclick={(e) => e.stopPropagation()}
  >
    <header class="head">
      <h1 id="attach-title">Use {plugin?.name ?? pluginId} for Knowledge</h1>
      <p>{plugin?.summary ? `${plugin.summary} ` : ""}Each step is checked live.</p>
    </header>

    {#if loadError !== null && plugin === null}
      <p class="err pad">{loadError}</p>
    {:else}
      <ol class="steps">
        <!-- 0 · the plugin itself, when it isn't on this host yet -->
        {#if showInstallStep && plugin !== null}
          <li class="step">
            <span class="num" class:done={!notInstalled}>{notInstalled ? "1" : "✓"}</span>
            <div class="sbody">
              <div class="stitle">Install {plugin.name}</div>
              {#if notInstalled}
                <div class="smuted">
                  Downloads {plugin.name} {pinnedVersion(plugin)} from its release into this host's
                  <span class="mono">~/.chimaera/plugins</span>.
                </div>
                <div class="pills">
                  <button class="opt" disabled={busy !== null} title={installTitle(plugin)} onclick={() => void installThis()}>
                    {busy === "install-plugin" ? "installing…" : `Install ${pinnedVersion(plugin)}`}
                  </button>
                </div>
              {/if}
              {#if installNote !== null}<div class="smuted">{installNote.text}</div>{/if}
              {#if installError !== null}<div class="err">{installError}</div>{/if}
            </div>
          </li>
        {/if}

        <!-- 1 · the agent plugins it requires or recommends -->
        <li class="step">
          <span class="num" class:done={!notInstalled && agentsDone} class:warn={!notInstalled && !agentsDone && agentsWarn}>
            {!notInstalled && agentsDone ? "✓" : nAgents}
          </span>
          <div class="sbody">
            <div class="stitle">For your agents</div>
            {#if plugin === null || model === null || model.phase === "checking"}
              <div class="smuted">checking the agents…</div>
            {:else if notInstalled}
              <div class="smuted">After the install, this shows what {plugin.name} can use from your agents.</div>
            {:else if model.phase === "none"}
              <div class="smuted">nothing to install — this plugin needs no agent plugin</div>
            {:else}
              {@const said = plugin.requires_summary ?? plugin.recommends_summary}
              {#if said !== null}<div class="smuted">{said}</div>{/if}
              {#if model.notice !== null}<div class="smuted warn-text">{model.notice}</div>{/if}
              {#if rows.length === 0 && model.notice === null}
                <div class="smuted">
                  {model.phase === "ready" ? noAgentsLine(plugin) : "can't check the agents' plugins on this daemon"}
                </div>
              {/if}
              {#if rows.length > 0}
                <div class="arows">
                  {#each rows as r (`${r.kind}:${r.agent}:${r.id}`)}
                    <div class="arow">
                      <span class={rowClass(r)} title={r.scope ?? undefined}>{sheetText(r, plugin.provides.knowledge)}</span>
                      {#if r.offerInstall}
                        <button
                          class="opt"
                          disabled={busy !== null}
                          onclick={() => void install(r.agent as AgentId, r.id)}
                          title="runs {r.agent}'s own plugin manager in a visible terminal"
                        >
                          {busy === `install:${r.agent}` ? "starting…" : `Install for ${r.agent}`}
                        </button>
                      {/if}
                    </div>
                  {/each}
                </div>
              {/if}
              {#if rows.some((r) => r.offerInstall)}
                <div class="smuted">An install opens a terminal running the agent's own commands; come back here when it finishes.</div>
              {/if}
            {/if}
            {#if loadError !== null}<div class="err">{loadError}</div>{/if}
          </div>
        </li>

        <!-- 2 · trust codex hooks -->
        {#if needsTrust}
          <li class="step">
            <span class="num warn">{nAgents + 1}</span>
            <div class="sbody">
              <div class="stitle">Trust {plugin?.name ?? "the plugin"}'s hooks in codex</div>
              <div class="smuted">Codex won't run a plugin's hooks until you trust them. These are exactly what it will run:</div>
              <div class="hooks">
                {#each hooks as h (h.key)}
                  <div class="hrow">
                    <span class="hwhen">{eventWords(h.event)}</span>
                    <span class="mono hmatch">{matcherWords(h.matcher)}</span>
                    <span class="hcmd">
                      <span class="mono">{basename(h.command) || h.key}</span>
                      {#if h.trust === "modified"}<span class="hnote warn-text">changed since you last trusted it</span>{/if}
                      {#if h.event === "Stop"}
                        <span class="hnote warn-text">can keep a turn going</span>
                      {/if}
                    </span>
                  </div>
                {/each}
              </div>
              <label class="check">
                <input type="checkbox" bind:checked={trust} />
                <span>
                  Trust these {hooks.length} hook{hooks.length === 1 ? "" : "s"}
                  <span class="smuted inline">— pinned to this version; an update that changes them asks again. You can revoke them in codex's <span class="mono">/hooks</span>.</span>
                </span>
              </label>
              {#if skipped.length > 0}
                <div class="err">
                  Not trusted:
                  {#each skipped as s (s.key)}<div><span class="mono">{s.key}</span> — {s.reason}</div>{/each}
                </div>
              {/if}
            </div>
          </li>
        {/if}

        <!-- 3 · set up this workspace -->
        <li class="step last">
          <span class="num" class:done={detected}>{detected ? "✓" : nSetup}</span>
          <div class="sbody">
            <div class="stitle">Set up this workspace</div>
            {#if notInstalled}
              <div class="smuted">After the install.</div>
            {:else if detected}
              <div class="smuted">Already set up here — {(plugin && footprint(plugin)) ?? "its files"} found. {plugin?.on ? "" : "Turning it on reads them."}</div>
            {:else if !canSetup}
              <div class="smuted">This plugin has no setup step.</div>
            {:else}
              <div class="smuted">
                Sends <span class="fg">“{plugin?.setup?.prompt}”</span> to a new agent session.
                {#if written.length > 0}It writes
                  {#each written as w, i (w)}<span class="mono">{w}</span>{i < written.length - 2 ? ", " : i === written.length - 2 ? " and " : ""}{/each}
                  and may touch other files —{:else}Whatever it writes,{/if} you review it in git; nothing is committed.
              </div>
              <div class="runwith">
                <label for="attach-agent">Run it with</label>
                <select id="attach-agent" bind:value={setupAgent} disabled={setupAgents.length === 0}>
                  {#each setupAgents.length > 0 ? setupAgents : ["claude", "codex"] as a (a)}
                    <option value={a}>{a} · chat</option>
                  {/each}
                </select>
                <span class="smuted">billed to your {setupAgent} account</span>
              </div>
              {#if identityUncertain}
                <div class="smuted warn-text">Resolve the marketplace identity in the agent before continuing.</div>
              {:else if setupAgents.length === 0}
                <div class="smuted">Setup needs an available agent with its required plugins installed and enabled (step {nAgents}).</div>
              {/if}
            {/if}
          </div>
        </li>
      </ol>
    {/if}

    <footer class="foot">
      <span class="fnote"
        >{#if plugin && footprint(plugin)}Knowledge fills as soon as <span class="mono">{footprint(plugin)}</span> appears.{:else}Knowledge
          fills once the setup has run.{/if}</span
      >
      {#if error !== null}<span class="err">{error}</span>{/if}
      <button class="opt quiet" use:focusOnMount onclick={onClose}>Cancel</button>
      <button
        class="opt primary"
        disabled={busy !== null || plugin === null || notInstalled || identityUncertain || setupBlocked}
        title={notInstalled && plugin !== null ? `install ${plugin.name} first` : undefined}
        onclick={() => void complete()}
      >
        {busy === "complete" ? "working…" : primaryLabel}
      </button>
    </footer>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 110;
    display: grid;
    place-items: center;
    padding: 24px;
    background: var(--scrim);
    backdrop-filter: blur(2px);
  }
  .sheet {
    width: min(700px, 100%);
    max-height: calc(100vh - 48px);
    display: flex;
    flex-direction: column;
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 14px;
    box-shadow: 0 24px 60px rgba(0, 0, 0, 0.28);
    overflow: hidden;
    animation: rise 0.18s ease;
  }
  @media (prefers-reduced-motion: reduce) {
    .sheet {
      animation: none;
    }
  }
  .head {
    padding: 20px 26px 14px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    border-bottom: 1px solid var(--edge);
  }
  h1 {
    margin: 0;
    font-size: 18px;
    font-weight: 600;
  }
  .head p {
    margin: 0;
    font-size: var(--text-sm);
    line-height: 1.5;
    color: var(--muted);
  }
  .steps {
    list-style: none;
    margin: 0;
    padding: 6px 26px 4px;
    display: flex;
    flex-direction: column;
    overflow-y: auto;
    min-height: 0;
  }
  .step {
    display: grid;
    grid-template-columns: 28px minmax(0, 1fr);
    column-gap: 12px;
    padding: 16px 0;
    border-bottom: 1px solid var(--edge);
  }
  .step.last {
    border-bottom: none;
  }
  .num {
    width: 22px;
    height: 22px;
    border-radius: 50%;
    border: 2px solid var(--edge);
    color: var(--muted);
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 11px;
    font-weight: 700;
  }
  .num.warn {
    border-color: var(--warn);
    color: var(--warn);
  }
  .num.done {
    border-color: transparent;
    background: color-mix(in srgb, var(--accent) 14%, transparent);
    color: var(--accent);
    font-size: 12px;
  }
  .sbody {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-width: 0;
  }
  .stitle {
    font-size: var(--text-md);
    font-weight: 600;
  }
  .smuted {
    font-size: var(--text-sm);
    color: var(--muted);
    line-height: 1.5;
  }
  .smuted.inline {
    display: inline;
  }
  .fg {
    color: var(--fg);
  }
  .mono {
    font-family: var(--mono);
    font-size: 0.94em;
  }
  .pills {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
    align-items: center;
    font-size: 12.5px;
  }

  .hooks {
    background: color-mix(in srgb, var(--fg) 3%, transparent);
    border-radius: 10px;
    padding: 4px 14px;
    display: flex;
    flex-direction: column;
  }
  .hrow {
    display: grid;
    grid-template-columns: 104px 150px minmax(0, 1fr);
    column-gap: 12px;
    padding: 8px 0;
    font-size: 12.5px;
    border-bottom: 1px solid var(--edge);
    align-items: start;
  }
  .hrow:last-child {
    border-bottom: none;
  }
  .hwhen {
    color: var(--muted);
  }
  .hmatch {
    font-size: 11.5px;
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .hcmd {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  .hcmd .mono {
    font-size: 11.5px;
    overflow-wrap: anywhere;
  }
  .hnote {
    font-size: var(--text-xs);
  }
  .warn-text {
    color: var(--warn);
  }
  .good-text {
    color: var(--accent);
  }
  .muted-text {
    color: var(--muted);
  }
  /* One agent plugin per line: its sentence, then the agent's own install. */
  .arows {
    display: flex;
    flex-direction: column;
    gap: 8px;
    font-size: var(--text-sm);
    line-height: 1.45;
  }
  .arow {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 12px;
  }
  .check {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    font-size: var(--text-sm);
    line-height: 1.45;
    cursor: pointer;
  }
  .check input {
    margin-top: 3px;
    accent-color: var(--accent);
  }

  .runwith {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
    font-size: var(--text-sm);
  }
  .runwith label {
    color: var(--muted);
  }
  .runwith select {
    font: inherit;
    font-size: var(--text-sm);
    color: var(--fg);
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 7px;
    padding: 5px 10px;
  }

  .err {
    font-size: var(--text-xs);
    color: var(--err);
    line-height: 1.45;
  }
  .err.pad {
    padding: 16px 26px;
  }

  .foot {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 14px 26px;
    border-top: 1px solid var(--edge);
    background: color-mix(in srgb, var(--fg) 3%, var(--bg));
    flex-wrap: wrap;
  }
  .fnote {
    font-size: var(--text-xs);
    color: var(--muted);
    margin-right: auto;
  }
  .foot .opt {
    padding: 6px 14px;
  }
</style>
