<script lang="ts">
  import { ApiError } from "../net/api";
  import { onMount } from "svelte";
  import { modalFocus } from "../shared/modalFocus";
  import { focusOnMount } from "../shared/focusOnMount";
  import { pageVisible } from "../shared/visibility";
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import { agentCatalog, pollAgents, type LaunchPick } from "./launcher";
  import { acknowledgeSetupResult, cancelAgentSetup, getAgentSetup, pendingSetupId, recoverSetupResult, setupRunning, startAgentSetup } from "./agentSetupRequests";
  import type { SetupDetails, SetupRequest } from "./agentSetup";

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
  const title = $derived(operation?.phase === "succeeded" ? `${agent.name} ${operation.action === "update" ? "updated" : operation.action === "reinstall" ? "reinstalled" : "installed"}` : operation?.phase === "failed" ? `Couldn't install ${agent.name}` : operation?.phase === "cancelled" ? "Installation cancelled" : operation?.phase === "cancelling" ? "Stopping installation…" : running ? `${operation?.action === "update" ? "Updating" : operation?.action === "reinstall" ? "Reinstalling" : "Installing"} ${agent.name}…` : `${verb} ${agent.name}`);

  async function check(): Promise<void> {
    if (checking) return;
    checking = true;
    try {
      const next = await getAgentSetup(agent.id);
      if (disposed) return;
      if (next.operation && !setupRunning(next.operation) && (initial || next.operation.phase !== details?.operation?.phase || pendingSetupId(agent.id) === next.operation.id)) {
        // Refresh before deciding whether this is an old result: nobody polls
        // the catalog while the dialog and Settings are both closed.
        const catalog = await pollAgents(AbortSignal.timeout(15_000));
        if (disposed) return;
        if (initial && !recoverSetupResult(request, next.operation, catalog.find(a => a.id === agent.id)?.installed === true)) {
          ignoredOperationId = next.operation.id;
        }
      }
      if (next.operation?.id === ignoredOperationId && !setupRunning(next.operation)) next.operation = null;
      initial = false;
      if (next.operation?.id === requestId) requestId = null;
      details = next;
      connected = true;
      refreshError = null;
      if (next.operation && !setupRunning(next.operation)) acknowledgeSetupResult(agent.id, next.operation.id);
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
      if (!disposed && !setupRunning(next)) void check();
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
    try {
      details = { ...details, operation: await cancelAgentSetup(agent.id, operation.id) };
      if (!setupRunning(details.operation)) void check();
    }
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
      <h2 id="setup-title" role="status">{title}</h2>
      <button class="close" aria-label="Close setup" use:focusOnMount onclick={onclose}>×</button>
    </header>
    <div class="body">
      {#if operation?.phase === "succeeded"}
        <p class="intro">Already signed in? Open chat. Otherwise, sign in first.</p>
      {:else if running}
        <p class="intro">You can keep working. Check back in Settings → Agents.</p>
      {:else if operation}
        <p class="intro" class:error={operation.phase === "failed"} role="status">{operation.message}</p>
      {:else}
        <p class="intro">Install the latest version. Your account and conversations are kept.</p>
      {/if}
      <details class="details">
        <summary>Details</summary>
        <dl>
          <dt>Host</dt><dd>{details?.host ?? "Checking…"}</dd>
          <dt>Install location</dt><dd><code>{details?.root ?? "Checking…"}</code></dd>
          {#if agent.path}<dt>Executable for new sessions</dt><dd><code>{agent.path}</code></dd>{/if}
          {#if operation?.exit_status !== null && operation?.exit_status !== undefined}<dt>Installer exit code</dt><dd>{operation.exit_status}</dd>{/if}
        </dl>
        {#if operation}
          <p class="output-label">Installer output{operation.truncated ? " · latest 64 KB" : ""}</p>
          <!-- svelte-ignore a11y_no_noninteractive_tabindex (keyboard users must be able to scroll the bounded output region) -->
          <pre role="region" aria-label="Installer output" tabindex="0">{operation.output || (running ? "Waiting for installer output…" : "The installer produced no output.")}</pre>
        {/if}
      </details>
      {#if error}<p class="error" role="alert">{error}</p>{/if}
      {#if refreshError}<p class="error" role="alert">{refreshError}</p>{/if}
      {#if !connected}<button class="link" disabled={checking} onclick={() => void check()}>Refresh status</button>{/if}
    </div>
    <footer>
      <button class="btn" onclick={onclose}>{running ? "Keep working" : operation?.phase === "succeeded" ? "Done" : operation ? "Close" : "Cancel"}</button>
      <div class="next">
        {#if running}
          <button class="btn" disabled={busy || operation?.phase === "cancelling" || !connected} onclick={() => void cancel()}>{operation?.phase === "cancelling" ? "Stopping…" : "Cancel installation"}</button>
        {:else if operation?.phase === "succeeded"}
          {#if agent.installed}<button class="btn" title={`Open ${agent.name} in a terminal to sign in`} onclick={() => launch("term")}>Sign in</button>{/if}
          {#if agent.installed && agent.chatCapable}<button class="btn primary" onclick={() => launch("chat")}>Open chat</button>{/if}
        {:else}
          <button class="btn primary" disabled={busy || !connected} onclick={() => void start()}>{busy ? "Starting…" : operation ? "Retry installation" : verb}</button>
        {/if}
      </div>
    </footer>
  </div>
</div>

<style>
  .backdrop { position:fixed; inset:0; z-index:1500; background:var(--scrim); display:grid; place-items:center; padding:24px; }
  .dialog { width:min(480px, 100%); max-height:calc(100vh - 48px); overflow:auto; background:var(--bg); color:var(--fg); border:1px solid var(--edge); border-radius:16px; box-shadow:0 24px 80px var(--scrim); }
  header { display:flex; align-items:center; gap:12px; padding:24px 24px 0; }
  .glyph { color:var(--accent); display:flex; }
  h2 { font-size:18px; line-height:1.3; margin:0; font-weight:600; letter-spacing:-.025em; }
  .close { margin-left:auto; align-self:flex-start; background:none; border:0; color:var(--muted); font-size:24px; cursor:pointer; }
  .body { padding:20px 24px; }
  .intro { color:var(--muted); font-size:13px; line-height:1.6; margin:0 0 16px; }
  code { overflow-wrap:anywhere; color:var(--fg); font-size:11px; }
  .details { color:var(--muted); font-size:12px; }
  summary { cursor:pointer; width:fit-content; }
  dl { margin:16px 0; font-size:11px; line-height:1.6; }
  dt { margin-top:10px; } dd { margin:2px 0 0; overflow-wrap:anywhere; color:var(--fg); }
  .output-label { font-size:11px; margin:12px 0 6px; }
  pre { margin:0; padding:12px; max-height:220px; overflow:auto; white-space:pre-wrap; overflow-wrap:anywhere; background:var(--rail-bg); border-radius:7px; font-size:11px; line-height:1.6; }
  .error { color:var(--err); font-size:12px; line-height:1.6; }
  footer { display:flex; justify-content:space-between; flex-wrap:wrap; gap:10px; padding:16px 24px; border-top:1px solid var(--edge); }
  .next { display:flex; gap:8px; flex-wrap:wrap; }
  .btn { font:inherit; font-size:12px; padding:9px 13px; border:1px solid var(--edge); border-radius:7px; background:var(--bg); color:var(--fg); cursor:pointer; }
  .primary { background:var(--accent); border-color:var(--accent); color:var(--bg); }
  button:disabled { opacity:.5; cursor:default; }
  .link { background:none; border:0; padding:0; color:var(--accent); cursor:pointer; font-size:12px; }
  @media(max-width:520px) { .backdrop { padding:10px; } header { padding:16px 16px 0; } .body { padding:16px; } footer { padding:14px 16px; } }
</style>
