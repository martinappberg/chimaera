<script lang="ts">
  import { ApiError } from "../net/api";
  import { onMount } from "svelte";
  import { modalFocus } from "../shared/modalFocus";
  import { pageVisible } from "../shared/visibility";
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import { agentCatalog, listAgents, pollAgents, type LaunchPick } from "./launcher";
  import { cancelAgentSetup, getAgentSetup, setupRunning, startAgentSetup, type SetupDetails, type SetupRequest } from "./agentSetup";

  let { request, onclose, onlaunch }: { request: SetupRequest; onclose(): void; onlaunch(pick: LaunchPick): void } = $props();
  let details = $state<SetupDetails | null>(null);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let refreshError = $state<string | null>(null);
  let connected = $state(false);
  let requestId: string | null = null;
  let checking = $state(false);
  let initial = true;
  let ignoredOperationId: string | null = null;
  let disposed = false;
  const operation = $derived(details?.operation ?? null);
  const running = $derived(setupRunning(operation));
  const agent = $derived($agentCatalog.find(a => a.id === request.agent.id) ?? request.agent);
  const verb = $derived(({ install: "Install", update: "Update", reinstall: "Reinstall" })[request.action]);
  const title = $derived(operation?.phase === "succeeded" ? `${agent.name} installed` : operation?.phase === "failed" ? `${agent.name} setup needs attention` : operation?.phase === "cancelled" ? "Installation cancelled" : operation?.phase === "cancelling" ? "Stopping installation" : running ? `${operation?.action === "update" ? "Updating" : operation?.action === "reinstall" ? "Reinstalling" : "Installing"} ${agent.name}` : `${verb} ${agent.name}`);

  async function check(): Promise<void> {
    if (checking) return;
    checking = true;
    try {
      const next = await getAgentSetup(agent.id);
      if (disposed) return;
      if (initial && !request.showResult && !setupRunning(next.operation)) ignoredOperationId = next.operation?.id ?? null;
      if (next.operation?.id === ignoredOperationId && !setupRunning(next.operation)) next.operation = null;
      initial = false;
      if (next.operation?.id === requestId) requestId = null;
      const changed = next.operation?.phase !== details?.operation?.phase;
      details = next;
      connected = true;
      refreshError = null;
      if (changed && next.operation && !setupRunning(next.operation)) void listAgents(true).catch(() => {});
    } catch (e) {
      if (!disposed) {
        connected = false;
        refreshError = `Couldn't refresh setup. ${e instanceof Error ? e.message : "Check the connection."} The installer may still be running.`;
      }
    } finally { checking = false; }
  }
  onMount(() => { void check(); return () => { disposed = true; }; });
  $effect(() => {
    if (!$pageVisible || (!running && connected)) return;
    const timer = setInterval(() => void check(), 2000);
    return () => clearInterval(timer);
  });
  async function start(): Promise<void> {
    if (busy || running || !connected || !details) return;
    busy = true; error = null;
    requestId ??= crypto.randomUUID();
    try {
      const next = await startAgentSetup({ ...request, action: operation?.action ?? request.action }, requestId);
      if (!disposed) details = { ...details, operation: next };
      requestId = null;
      void pollAgents(AbortSignal.timeout(15_000)).catch(() => {});
    } catch (e) {
      if (!disposed) {
        error = e instanceof Error ? e.message : "Couldn't start installation.";
        // Preserve the idempotency ID for an ambiguous reply; fetch before
        // enabling another start so reconnect cannot duplicate a running job.
        if (!(e instanceof ApiError)) connected = false;
      }
    } finally { busy = false; }
  }
  async function cancel(): Promise<void> {
    if (!operation || !details || busy) return;
    busy = true;
    try { details = { ...details, operation: await cancelAgentSetup(agent.id, operation.id) }; }
    catch (e) { error = e instanceof Error ? e.message : "Couldn't stop the installer."; }
    finally { busy = false; }
  }
  function launch(ui: "chat" | "term"): void {
    // Start while the request still exists: closing clears the shared store
    // and invalidates this component’s derived agent synchronously.
    onlaunch({ agent: agent.id, ui, explicit: true });
    onclose();
  }
</script>

<div class="backdrop" role="presentation" onclick={onclose} onkeydown={e => { if (e.key === "Escape") { e.stopPropagation(); onclose(); } }}>
  <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="setup-title" tabindex="-1" use:modalFocus onclick={e => e.stopPropagation()} onkeydown={e => { if (e.key === "Escape") onclose(); e.stopPropagation(); }}>
    <header>
      <span class="glyph"><SessionGlyph kind="agent" agentKind={agent.id} size={25} title={agent.name} /></span>
      <div><p class="eyebrow">AGENT SETUP · {details?.host ?? "This workspace’s host"}</p><h2 id="setup-title">{title}</h2></div>
      <button class="close" aria-label="Close setup" onclick={onclose}>×</button>
    </header>
    <div class="body">
      <p class="intro">{running ? "You can keep working. Close this dialog and return through Agents settings to see the result." : "Chimaera installs its own copy on this workspace’s host. Your account data and conversations are kept."}</p>
      <div class="steps" aria-label="Setup stages">
        <div class:active={running} class:done={operation?.phase === "succeeded"}><span>1</span><strong>Install files</strong><small>{operation?.phase === "succeeded" ? "Finished" : running ? "In progress" : "Official release"}</small></div>
        <div><span>2</span><strong>Sign in</strong><small>In the agent’s terminal</small></div>
        <div><span>3</span><strong>Start chat</strong><small>Checked when opened</small></div>
      </div>
      <div class="location"><span>Install location</span><code>{details?.root ?? "Checking…"}</code></div>
      {#if operation}
        <div class="result" class:failed={operation.phase === "failed"} role="status">
          <strong>{operation.message}</strong>
          {#if operation.exit_status !== null}<span>Installer exit code: {operation.exit_status}</span>{/if}
        </div>
        <details class="output" open={operation.phase === "failed" || running}>
          <summary>Installer output{operation.truncated ? " · latest 64 KB" : ""}</summary>
          {#if agent.path}<div class="selected-path">Executable for new sessions: <code>{agent.path}</code></div>{/if}
          <!-- svelte-ignore a11y_no_noninteractive_tabindex (keyboard users must be able to scroll the bounded output region) -->
          <pre role="region" aria-label="Installer output" tabindex="0">{operation.output || (running ? "Waiting for installer output…" : "The installer produced no output.")}</pre>
        </details>
      {:else}
        <p class="note">Installation does not sign you in or verify chat. Provider availability and host compatibility are checked when you open the agent.</p>
      {/if}
      {#if error}<p class="error" role="alert">{error}</p>{/if}
      {#if refreshError}<p class="error" role="alert">{refreshError}</p>{/if}
      {#if !connected}<button class="link" disabled={checking} onclick={() => void check()}>Refresh status</button>{/if}
    </div>
    <footer>
      <button class="btn" onclick={onclose}>{running ? "Keep working" : "Close"}</button>
      <div class="next">
        {#if running}
          <button class="btn" disabled={busy || operation?.phase === "cancelling" || !connected} onclick={() => void cancel()}>{operation?.phase === "cancelling" ? "Stopping…" : "Cancel installation"}</button>
        {:else if operation?.phase === "succeeded"}
          {#if agent.installed}<button class="btn" onclick={() => launch("term")}>Open terminal to sign in</button>{/if}
          {#if agent.chatCapable}<button class="btn primary" onclick={() => launch("chat")}>Try chat</button>{/if}
        {:else}
          <button class="btn primary" disabled={busy || !connected} onclick={() => void start()}>{busy ? "Starting…" : operation ? "Retry installation" : verb}</button>
        {/if}
      </div>
    </footer>
  </div>
</div>

<style>
  .backdrop { position:fixed; inset:0; z-index:1500; background:var(--scrim); display:grid; place-items:center; padding:24px; }
  .dialog { width:min(640px, 100%); max-height:calc(100vh - 48px); overflow:auto; background:var(--bg); color:var(--fg); border:1px solid var(--edge); border-radius:16px; box-shadow:0 24px 80px var(--scrim); }
  header { display:flex; align-items:center; gap:14px; padding:24px 24px 18px; border-bottom:1px solid var(--edge); }
  .glyph { color:var(--accent); padding:12px; background:var(--rail-bg); border-radius:12px; }
  .eyebrow { margin:0 0 6px; font-size:10px; font-weight:650; letter-spacing:.08em; color:var(--muted); overflow-wrap:anywhere; }
  h2 { font-size:20px; line-height:1.3; margin:0; font-weight:600; letter-spacing:-.025em; }
  .close { margin-left:auto; align-self:flex-start; background:none; border:0; color:var(--muted); font-size:24px; cursor:pointer; }
  .body { padding:20px 24px; }
  .intro,.note { color:var(--muted); font-size:13px; line-height:1.6; margin:0 0 20px; }
  .steps { display:grid; grid-template-columns:repeat(3,1fr); gap:8px; margin-bottom:20px; }
  .steps div { display:flex; flex-direction:column; gap:7px; padding:14px 10px; border:1px solid var(--edge); border-radius:10px; }
  .steps span { width:22px; height:22px; display:grid; place-items:center; border-radius:50%; background:var(--rail-bg); color:var(--muted); font-size:11px; }
  .steps strong { font-size:12px; font-weight:600; }
  .steps small { color:var(--muted); font-size:11px; }
  .steps .active,.steps .done { border-color:var(--accent); }
  .steps .active span,.steps .done span { background:var(--accent); color:var(--bg); }
  .location { display:flex; flex-direction:column; gap:7px; margin-bottom:18px; font-size:11px; color:var(--muted); }
  code { overflow-wrap:anywhere; color:var(--fg); font-size:11px; }
  .result { border-left:3px solid var(--accent); padding:10px 14px; margin:16px 0; background:var(--rail-bg); line-height:1.6; font-size:12px; }
  .result strong { font-weight:500; } .result span { display:block; color:var(--muted); font-size:11px; margin-top:6px; }
  .failed { border-color:var(--err); }
  .selected-path { padding:0 12px 12px; overflow-wrap:anywhere; color:var(--muted); font-size:11px; line-height:1.6; }
  .output { border:1px solid var(--edge); border-radius:8px; overflow:hidden; }
  summary { cursor:pointer; padding:12px; color:var(--muted); font-size:12px; }
  pre { margin:0; padding:12px; max-height:220px; overflow:auto; white-space:pre-wrap; overflow-wrap:anywhere; background:var(--rail-bg); font-size:11px; line-height:1.6; }
  .error { color:var(--err); font-size:12px; line-height:1.6; }
  footer { display:flex; justify-content:space-between; flex-wrap:wrap; gap:10px; padding:16px 24px; border-top:1px solid var(--edge); }
  .next { display:flex; gap:8px; flex-wrap:wrap; }
  .btn { font:inherit; font-size:12px; padding:9px 13px; border:1px solid var(--edge); border-radius:7px; background:var(--bg); color:var(--fg); cursor:pointer; }
  .primary { background:var(--accent); border-color:var(--accent); color:var(--bg); }
  button:disabled { opacity:.5; cursor:default; }
  .link { background:none; border:0; padding:0; color:var(--accent); cursor:pointer; font-size:12px; }
  @media(max-width:520px) { .backdrop { padding:10px; } header,.body { padding:16px; } footer { padding:14px 16px; } .steps { gap:5px; } .steps div { padding:10px 7px; } }
</style>
