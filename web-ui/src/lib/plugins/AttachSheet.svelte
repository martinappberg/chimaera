<script lang="ts">
  /**
   * "Use mycelium for Knowledge" — the one sheet, three live-checked steps
   * (design §6.2): 1 installed for your agents (the CLIs' own installs, in a
   * visible terminal), 2 trust codex's hooks right here (§6.6 — each hook in
   * plain words, hash-pinned, never automated), 3 set up this workspace (the
   * plugin's own setup prompt sent to a new agent session of the user's
   * choosing — their click, their billing). Completing also switches the
   * plugin ON for the workspace. Re-checks every time it opens.
   */
  import { onMount } from "svelte";
  import { ApiError } from "../net/api";
  import { focusOnMount } from "../shared/focusOnMount";
  import { modalFocus } from "../shared/modalFocus";
  import { refreshKnowledge } from "../workspace/knowledge";
  import {
    fetchAgentPlugins,
    fetchWorkspacePlugins,
    installPlugin,
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
  let agents = $state<AgentPlugins | null>(null);
  /** null = still checking; false = the daemon predates agent probes. */
  let agentsAvailable = $state<boolean | null>(null);
  let loadError = $state<string | null>(null);
  let setupAgent = $state<AgentId>("claude");
  let trust = $state(true);
  let busy = $state<string | null>(null);
  let error = $state<string | null>(null);
  let skipped = $state<{ key: string; reason: string }[]>([]);

  async function check(): Promise<void> {
    loadError = null;
    try {
      const [wp, ap] = await Promise.all([
        fetchWorkspacePlugins(wsId),
        fetchAgentPlugins(wsId).then(
          (a) => {
            agentsAvailable = true;
            return a;
          },
          (e: unknown) => {
            agentsAvailable = !(e instanceof ApiError && e.status === 404);
            if (agentsAvailable) loadError = e instanceof Error ? e.message : String(e);
            return null;
          },
        ),
      ]);
      plugin = wp.plugins.find((p) => p.id === pluginId) ?? null;
      agents = ap;
      if (plugin === null) loadError = `no plugin “${pluginId}” on this daemon`;
      // Default the setup agent to one that is installed and has the plugin.
      const preferred = requirements().find((r) => r.status === "ok")?.agent;
      if (preferred !== undefined) setupAgent = preferred;
    } catch (e) {
      loadError = e instanceof Error ? e.message : String(e);
    }
  }
  onMount(() => void check());

  type ReqStatus = "checking" | "unknown" | "no-agent" | "missing" | "ok" | "disabled";
  interface Req {
    agent: AgentId;
    id: string;
    status: ReqStatus;
    version: string | null;
    scope: string | null;
  }

  function requirements(): Req[] {
    if (plugin === null) return [];
    return plugin.requires.map((r) => {
      const agent = r.agent as AgentId;
      if (agentsAvailable === false) return { agent, id: r.id, status: "unknown", version: null, scope: null };
      if (agents === null) return { agent, id: r.id, status: "checking", version: null, scope: null };
      const entry = agents.agents.find((a) => a.agent === agent);
      if (entry === undefined || !entry.available) return { agent, id: r.id, status: "no-agent", version: null, scope: null };
      const got = entry.plugins.find((p) => p.id === r.id || p.id === r.id.split("@")[0]);
      if (got === undefined) return { agent, id: r.id, status: "missing", version: null, scope: null };
      return {
        agent,
        id: r.id,
        status: got.enabled ? "ok" : "disabled",
        version: got.version ?? null,
        scope: got.scope ?? null,
      };
    });
  }
  const reqs = $derived.by(() => {
    // Re-derive on every input the checks read.
    void plugin;
    void agents;
    void agentsAvailable;
    return requirements();
  });
  const step1Done = $derived(reqs.length > 0 && reqs.every((r) => r.status === "ok"));

  /** Codex hooks of this plugin still waiting for trust (untrusted or
   *  changed since they were trusted). */
  const hooks = $derived.by((): AgentHook[] => {
    if (plugin === null || agents === null) return [];
    const req = plugin.requires.find((r) => r.agent === "codex");
    if (req === undefined) return [];
    const codex = agents.agents.find((a) => a.agent === "codex");
    if (codex === undefined || !codex.available) return [];
    const base = req.id.split("@")[0];
    return (codex.hooks ?? []).filter(
      (h) => (h.plugin_id === req.id || h.plugin_id === base) && (h.trust === "untrusted" || h.trust === "modified"),
    );
  });
  const needsTrust = $derived(hooks.length > 0);

  const detected = $derived(plugin?.detected === true);
  const canSetup = $derived(plugin?.setup !== null && plugin?.setup !== undefined);
  const setupAgents = $derived(
    reqs.filter((r) => r.status === "ok" || r.status === "disabled" || r.status === "unknown").map((r) => r.agent),
  );

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
  function matcherWords(m: string | undefined): string {
    if (m === undefined || m === "" || m === "*") return "always";
    return m.split("|").join(" · ");
  }
  function basename(cmd: string | undefined): string {
    if (cmd === undefined) return "";
    const first = cmd.trim().split(/\s+/)[0] ?? "";
    return first.slice(first.lastIndexOf("/") + 1);
  }

  const primaryLabel = $derived.by(() => {
    if (plugin === null) return "…";
    const parts: string[] = [];
    if (needsTrust && trust) parts.push("Trust hooks");
    if (!detected && canSetup && setupAgents.length > 0) parts.push("set up");
    else if (!plugin.on) parts.push("turn on");
    if (parts.length === 0) return "Done";
    const s = parts.join(" & ");
    return s.charAt(0).toUpperCase() + s.slice(1);
  });

  async function install(agent: AgentId): Promise<void> {
    busy = `install:${agent}`;
    error = null;
    try {
      const res = await installPlugin(wsId, pluginId, agent);
      onOpenSession(res.session_id);
    } catch (e) {
      error =
        e instanceof ApiError && e.status === 404
          ? `this daemon can't run installs yet — run ${agent}'s own plugin manager instead`
          : e instanceof Error
            ? e.message
            : String(e);
      busy = null;
    }
  }

  async function complete(): Promise<void> {
    if (plugin === null) return;
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
      }
      if (!plugin.on) await putWorkspacePlugin(wsId, pluginId, true);
      refreshWorkspacePlugins();
      if (!detected && canSetup && setupAgents.length > 0) {
        const res = await setupPlugin(wsId, pluginId, setupAgent);
        refreshKnowledge();
        onOpenSession(res.session_id);
        return;
      }
      refreshKnowledge();
      if (skipped.length === 0) onClose();
      else busy = null;
    } catch (e) {
      error =
        e instanceof ApiError && e.status === 404
          ? "this daemon can't finish that step yet — update chimaera"
          : e instanceof Error
            ? e.message
            : String(e);
      busy = null;
      refreshWorkspacePlugins();
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
      <p>Your agents record findings, decisions and learnings as they work; chimaera shows them. Three steps, each checked live.</p>
    </header>

    {#if loadError !== null && plugin === null}
      <p class="err pad">{loadError}</p>
    {:else}
      <ol class="steps">
        <!-- 1 · installed for your agents -->
        <li class="step">
          <span class="num" class:done={step1Done} class:warn={!step1Done && reqs.some((r) => r.status === "missing" || r.status === "no-agent")}>
            {step1Done ? "✓" : "1"}
          </span>
          <div class="sbody">
            <div class="stitle">Installed for your agents</div>
            {#if reqs.length === 0}
              <div class="smuted">{plugin === null ? "checking…" : "nothing to install — this plugin needs no agent plugin"}</div>
            {:else}
              <div class="pills">
                {#each reqs as r (r.agent)}
                  {#if r.status === "ok"}
                    <span class="pill good">{r.agent}{#if r.version} · {r.version}{/if}{#if r.scope} · {r.scope}{/if}</span>
                  {:else if r.status === "disabled"}
                    <span class="pill warn">{r.agent} · installed but disabled</span>
                  {:else if r.status === "missing"}
                    <span class="pill warn">{r.agent} · not installed</span>
                    <button class="opt" disabled={busy !== null} onclick={() => void install(r.agent)} title="runs {r.agent}'s own plugin manager in a visible terminal">
                      {busy === `install:${r.agent}` ? "starting…" : `Install for ${r.agent}`}
                    </button>
                  {:else if r.status === "no-agent"}
                    <span class="pill neutral">{r.agent} · not installed on this host</span>
                  {:else if r.status === "unknown"}
                    <span class="pill neutral">{r.agent} · can't check on this daemon</span>
                  {:else}
                    <span class="pill neutral">{r.agent} · checking…</span>
                  {/if}
                {/each}
              </div>
              {#if reqs.some((r) => r.status === "missing")}
                <div class="smuted">An install opens a terminal running the agent's own commands; come back here when it finishes.</div>
              {/if}
            {/if}
            {#if loadError !== null}<div class="err">{loadError}</div>{/if}
          </div>
        </li>

        <!-- 2 · trust codex hooks -->
        {#if needsTrust}
          <li class="step">
            <span class="num warn">2</span>
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
                      {#if pluginId === "mycelium" && h.event === "Stop"}
                        <span class="hnote warn-text">can keep a turn going until .living/ is updated</span>
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
          <span class="num" class:done={detected}>{detected ? "✓" : needsTrust ? "3" : "2"}</span>
          <div class="sbody">
            <div class="stitle">Set up this workspace</div>
            {#if detected}
              <div class="smuted">Already set up here — {plugin?.detect[0] ?? "its files"} found. {plugin?.on ? "" : "Turning it on reads them."}</div>
            {:else if !canSetup}
              <div class="smuted">This plugin has no setup step.</div>
            {:else}
              <div class="smuted">
                Sends <span class="fg">“{plugin?.setup?.prompt}”</span> to a new agent session. It writes
                <span class="mono">.living/</span>, <span class="mono">MYCELIUM.md</span> and small
                <span class="mono">CLAUDE.md</span> / <span class="mono">AGENTS.md</span> adapters — you review them in git, nothing is committed.
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
              {#if setupAgents.length === 0 && reqs.length > 0}
                <div class="smuted">Install the plugin for an agent first (step 1).</div>
              {/if}
            {/if}
          </div>
        </li>
      </ol>
    {/if}

    <footer class="foot">
      <span class="fnote">Knowledge fills as soon as <span class="mono">.living/</span> appears.</span>
      {#if error !== null}<span class="err">{error}</span>{/if}
      <button class="opt quiet" use:focusOnMount onclick={onClose}>Cancel</button>
      <button class="opt primary" disabled={busy !== null || plugin === null} onclick={() => void complete()}>
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
  .pill {
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
