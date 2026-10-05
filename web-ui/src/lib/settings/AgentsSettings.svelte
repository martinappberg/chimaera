<script lang="ts">
  import { untrack } from "svelte";
  import { getActiveWorkspaceId } from "../net/api";
  import { pageVisible } from "../shared/visibility";
  import { openInSystemBrowser } from "../shared/urlOpen";
  import {
    agentCatalog, listAgents, pollAgents, uninstallAgent,
    versionNumber, type AgentInfo,
  } from "../workspace/launcher";
  import { openAgentSetup, type SetupAction } from "../workspace/agentSetup";
  import ConfirmDialog from "../shared/ConfirmDialog.svelte";
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import { flushSettings, getSetting, setSetting } from "./store.svelte";
  import { agentUpdateStatus } from "./agentStatus";

  let { visible = true }: { visible?: boolean } = $props();
  type AgentPathKey = "agents.claude.path" | "agents.codex.path" | "agents.agy.path" | "agents.grok.path" | "agents.agy.chatPath";
  // Only built-ins own schema keys. Extension identities must never be
  // coerced into settings belonging to an unrelated built-in agent.
  const pathKeys: Partial<Record<string, AgentPathKey>> = {
    claude: "agents.claude.path", codex: "agents.codex.path",
    agy: "agents.agy.path", grok: "agents.grok.path",
  };
  const pathKey = (id: string): AgentPathKey | undefined => pathKeys[id];
  const wsId = getActiveWorkspaceId();
  let loading = $state(true);
  let loadError = $state<string | null>(null);
  let inputs = $state<Partial<Record<AgentPathKey, string>>>({});
  let baseline: Partial<Record<AgentPathKey, string>> = {};
  let saving = $state<Record<string, boolean>>({});
  let rowError = $state<Record<string, string | null>>({});
  let messages = $state<Record<string, string | null>>({});
  let uninstallAsk = $state<AgentInfo | null>(null);
  let uninstallError = $state<string | null>(null);
  let removing = $state(false);

  function syncInputs(list: AgentInfo[]): void {
    const next = { ...inputs };
    const keys = [...list.flatMap(a => pathKey(a.id) ? [pathKey(a.id)!] : []), "agents.agy.chatPath" as const];
    for (const key of keys) {
      const value = getSetting(key);
      // Re-checking or saving another row must preserve an unfinished edit.
      if (next[key] === undefined || next[key] === baseline[key]) next[key] = value;
      baseline[key] = value;
    }
    inputs = next;
  }

  async function load(check = false): Promise<void> {
    loading = true;
    loadError = null;
    try {
      let list = await listAgents(true, check);
      // A fast installer can finish during the initial executable probes.
      // Reconcile once after that result so a reload cannot retain stale
      // “needs setup” rows after successful installation.
      if (list.some(a => (a.installation && !a.installation.running) || (a.setup && !a.setup.running))) list = await pollAgents();
      syncInputs(list);
    }
    catch (e) { loadError = e instanceof Error ? e.message : "Couldn't load agents"; }
    finally { loading = false; }
  }
  $effect(() => {
    if (visible && $pageVisible) untrack(() => { void load(); });
  });

  async function save(a: AgentInfo): Promise<void> {
    const key = pathKey(a.id);
    if (saving[a.id] || key === undefined) return;
    saving[a.id] = true;
    rowError[a.id] = null;
    try {
      const keys = a.id === "agy" ? [key, "agents.agy.chatPath" as const] : [key];
      for (const key of keys) {
        const value = (inputs[key] ?? "").trim();
        setSetting(key, value);
        inputs[key] = baseline[key] = value;
      }
      await flushSettings();
      await load();
    } catch (e) { rowError[a.id] = e instanceof Error ? e.message : "Couldn't save"; }
    finally { saving[a.id] = false; }
  }

  function install(a: AgentInfo, action: SetupAction = "install", showResult = false): void {
    if (wsId !== null) openAgentSetup(a, wsId, action, showResult);
  }

  $effect(() => {
    if (!visible || !$pageVisible || !$agentCatalog.some(a => a.setup?.running || a.installation?.running)) return;
    let pending = false;
    const timer = setInterval(async () => {
      if (pending) return;
      pending = true;
      try { await pollAgents(AbortSignal.timeout(15_000)); } catch { /* Next visible tick retries. */ }
      finally { pending = false; }
    }, 3000);
    return () => clearInterval(timer);
  });

  async function uninstall(a: AgentInfo): Promise<void> {
    if (removing) return;
    removing = true;
    uninstallError = null;
    try {
      await uninstallAgent(a.id);
      uninstallAsk = null;
      messages[a.id] = null;
      await load();
    } catch (e) { uninstallError = e instanceof Error ? e.message : "Couldn't uninstall"; }
    finally { removing = false; }
  }
</script>

<section class="agents">
  <div class="cat-row">
    <h2>Agents</h2>
    <button class="link" disabled={loading} onclick={() => void load(true)}>{loading ? "Checking…" : "Check for updates"}</button>
  </div>
  <p class="intro">Uses each agent’s account, provider, and configuration on this host.</p>
  {#if loadError}<p class="err" role="alert">{loadError}</p>{/if}
  <div class="cards">
    {#each $agentCatalog as a (a.id)}
      {@const status = agentUpdateStatus(a)}
      {@const busy = a.setup?.running || a.installation?.running}
      {@const key = pathKey(a.id)}
      <article>
        <div class="main">
          <span class="glyph"><SessionGlyph kind="agent" agentKind={a.id} size={20} title={a.name} /></span>
          <div class="text">
            <div class="head"><h3>{a.name}</h3>{#if a.version}<span class="version" title={a.version}>{versionNumber(a.version)}</span>{/if}</div>
            <p class="state">{!a.installed ? "Not installed" : a.outdated ? "Needs an update" : a.chatSetupRequired ? "Installed · Chat companion missing" : a.chatCapable ? "Installed · Supports chat and terminal" : "Installed · Terminal only"}</p>
            {#if a.installed}<p class="detail" title={a.latestError ?? undefined}>{a.managed ? "Managed by Chimaera" : "Your installation"}{#if status.text} · {status.text}{/if}</p>{/if}
          </div>
          <div class="actions">
            {#if a.setup?.running}<button class="btn" onclick={() => install(a, "install", true)}>View progress</button>
            {:else if a.installation?.running}<span class="detail" role="status">Installing in a terminal…</span>
            {:else if (!a.installed || a.chatSetupRequired) && a.managedInstall}
              <button class="btn primary" disabled={wsId === null} onclick={() => void install(a)}>{a.chatSetupRequired ? "Set up chat" : "Install"}</button>
            {:else if a.managed && (a.updateAvailable || a.outdated)}
              <button class="btn primary" disabled={wsId === null} onclick={() => void install(a, "update")}>Update</button>
            {:else if (!a.installed || a.updateAvailable || a.outdated) && a.installUrl}
              <button class="link" onclick={() => openInSystemBrowser(a.installUrl!)}>{a.installed ? "Update instructions ↗" : "Install instructions ↗"}</button>
            {/if}
          </div>
        </div>
        {#if a.installation?.running && !a.setup?.running}<p class="notice">Follow this installation in its terminal in the sidebar.</p>{/if}
        {#if a.setup && !busy}<button class="link setup-result" onclick={() => install(a, "install", true)}>{a.setup.phase === "failed" ? "Setup needs attention — view result" : a.setup.phase === "cancelled" ? "Installation cancelled — view result" : "View installation result"}</button>{/if}
        {#if messages[a.id]}<p class="notice" role="status">{messages[a.id]}</p>{/if}
        {#if rowError[a.id]}<p class="err" role="alert">{rowError[a.id]}</p>{/if}
        {#if key !== undefined || a.managed}
        <details>
          <summary>Advanced</summary>
          <div class="advanced">
            {#if a.path}<p class="detail">Using <code>{a.path}</code></p>{/if}
            {#if key !== undefined}
            <label>Custom executable
              <input bind:value={inputs[key]} placeholder="Automatic" spellcheck="false" disabled={saving[a.id]} onkeydown={e => { if (e.key === "Enter") void save(a); }} />
            </label>
            {/if}
            {#if a.id === "agy"}
              <label>Custom chat executable
                <input bind:value={inputs["agents.agy.chatPath"]} placeholder="Automatic" spellcheck="false" disabled={saving[a.id]} />
              </label>
              <p class="detail">Chat uses Google's separate companion. Set up chat installs it alongside Antigravity.</p>
            {/if}
            <div class="advanced-actions">
              {#if key !== undefined}<button class="btn" disabled={saving[a.id] || ((inputs[key] ?? "") === getSetting(key) && (a.id !== "agy" || (inputs["agents.agy.chatPath"] ?? "") === getSetting("agents.agy.chatPath")))} onclick={() => void save(a)}>{saving[a.id] ? "Saving…" : "Save"}</button>{/if}
              {#if a.managed}
                <button class="link" disabled={busy || wsId === null} onclick={() => void install(a, "reinstall")}>Reinstall</button>
                <button class="link danger" disabled={busy || removing} onclick={() => { uninstallError = null; uninstallAsk = a; }}>Uninstall</button>
              {/if}
            </div>
          </div>
        </details>
        {/if}
      </article>
    {/each}
  </div>
</section>

{#if uninstallAsk !== null}
  {@const a = uninstallAsk}
  <ConfirmDialog title="Uninstall {a.name}?" body="This removes Chimaera's copy. Your other installations and conversations are kept." confirmLabel={removing ? "Uninstalling…" : "Uninstall"} danger enterConfirms error={uninstallError} onConfirm={() => void uninstall(a)} onCancel={() => { if (!removing) uninstallAsk = null; }} />
{/if}

<style>
  .setup-result { margin:4px 14px 12px; }
  .cat-row { display:flex; align-items:center; justify-content:space-between; gap:12px; margin:18px 14px 8px; }
  h2 { margin:0; font-size:var(--text-xs); font-weight:600; letter-spacing:.1em; text-transform:uppercase; color:var(--muted); }
  .intro { margin:0 14px 14px; font-size:var(--text-sm); color:var(--muted); line-height:1.5; }
  .cards { margin:0 14px 12px; border:1px solid var(--edge); border-radius:10px; overflow:hidden; }
  article { padding:16px; background:color-mix(in srgb,var(--fg) 2%,transparent); }
  article + article { border-top:1px solid var(--edge); }
  .main { display:flex; align-items:center; gap:12px; flex-wrap:wrap; }
  .glyph { color:var(--muted); align-self:flex-start; margin-top:3px; }
  .text { flex:1; min-width:170px; }
  .head { display:flex; gap:8px; align-items:baseline; flex-wrap:wrap; }
  h3 { margin:0; font-size:var(--text-md); font-weight:550; }
  .version { font:var(--text-xs) var(--mono); color:var(--muted); }
  p { margin:3px 0 0; }
  .state { font-size:var(--text-sm); }
  .detail,.notice { font-size:var(--text-xs); color:var(--muted); line-height:1.5; overflow-wrap:anywhere; }
  .notice { margin-top:8px; }
  button { font:inherit; font-size:var(--text-xs); cursor:pointer; }
  button:disabled { opacity:.5; cursor:default; }
  .link { border:0; padding:3px 0; background:none; color:var(--accent); }
  .link:hover:not(:disabled) { text-decoration:underline; }
  .btn { border:1px solid var(--edge); border-radius:6px; padding:6px 12px; background:var(--term-bg); color:var(--fg); }
  .primary { border-color:color-mix(in srgb,var(--accent) 45%,var(--edge)); color:var(--accent); }
  .btn:hover:not(:disabled) { background:color-mix(in srgb,var(--accent) 8%,transparent); }
  details { margin-top:10px; }
  summary { width:fit-content; cursor:pointer; font-size:var(--text-xs); color:var(--muted); }
  .advanced { display:flex; flex-direction:column; gap:10px; margin-top:12px; }
  label { display:flex; flex-direction:column; gap:5px; font-size:var(--text-xs); color:var(--muted); }
  input { width:100%; box-sizing:border-box; border:1px solid var(--edge); background:var(--term-bg); color:var(--fg); border-radius:6px; padding:7px 9px; font:var(--text-xs) var(--mono); }
  .advanced-actions { display:flex; gap:16px; align-items:center; }
  .danger,.err { color:var(--err); }
  .err { font-size:var(--text-sm); margin:8px 0; overflow-wrap:anywhere; }
  code { font:var(--text-xs) var(--mono); }
</style>
