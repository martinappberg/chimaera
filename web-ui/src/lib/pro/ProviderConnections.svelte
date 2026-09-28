<script lang="ts">
  import { onDestroy, tick, untrack } from "svelte";
  import { pageVisible } from "../shared/visibility";
  import { isNativeShell, writeClipboard, type CloudProviderConnection, type CloudProviderStatus, type CloudSetupInfo } from "../net/native";
  import { cloudRequest } from "./cloudTransport";
  import { connectionError, pendingConnection, providerLoginUrl, providersReady, providerStateLabel } from "./providers";

  let { visible = true, requiredProviders = [], contextLabel, workspaceId, onReady, onReadiness }: {
    visible?: boolean; requiredProviders?: string[]; contextLabel?: string; workspaceId?: string; onReady?: () => void; onReadiness?: (ready: boolean | null) => void;
  } = $props();
  type Handoff = NonNullable<CloudSetupInfo["handoffs"]>[number];
  let providers = $state<CloudProviderStatus[]>([]);
  let handoffs = $state<Handoff[]>([]);
  let focusedHandoff = $state<Handoff | null>(null);
  let connection = $state<CloudProviderConnection | null>(null);
  let connectionElement = $state<HTMLElement>();
  let loaded = $state(false);
  let current = $state(false);
  let busy = $state<string | null>(null);
  let error = $state<string | null>(null);
  let connectionNotice = $state<string | null>(null);
  let copied = $state(false);
  let pollingPaused = $state(false);
  let alive = true;
  let catalogFlight = $state(false);
  let connectionFlight = $state(false);
  let catalogAgain = false;
  let mutation = 0;
  const native = isNativeShell();
  const required = $derived(requiredProviders.length ? requiredProviders : focusedHandoff?.blocked_providers.map(p => p.id) ?? []);
  const projectName = $derived(contextLabel ?? focusedHandoff?.name);
  const agents = $derived(providers.filter(p => p.category === "agent"));
  const repositories = $derived(providers.filter(p => p.category === "repository"));
  const contextHandoff = $derived(handoffs.find(h => h.workspace_id === workspaceId));
  const selectedHandoff = $derived(contextHandoff ?? (focusedHandoff ? handoffs.find(h => h.workspace_id === focusedHandoff!.workspace_id) : undefined));
  const ready = $derived(current && providersReady(providers, required));
  const waiting = $derived(pendingConnection(connection));
  const connectionId = $derived(connection?.id ?? null);
  const connectionExpires = $derived(connection?.expires_at ?? null);
  const connectingLabel = $derived(providers.find(p => p.id === connection?.provider_id)?.label ?? "your agent");
  const action = $derived(connection?.action ?? null);
  const loginUrl = $derived(connection && action && (action.type === "device_code" || action.type === "browser")
    ? providerLoginUrl(connection.provider_id, action.type === "device_code" ? action.verification_url : action.url) : null);

  $effect(() => { const value = current ? ready : null; untrack(() => onReadiness?.(value)); });

  async function load(signal?: AbortSignal): Promise<void> {
    if (catalogFlight) { catalogAgain = true; return; }
    catalogFlight = true;
    try {
      const result = await cloudRequest({ operation: "providers" }, signal);
      if (!alive || signal?.aborted) return;
      providers = result.providers ?? [];
      handoffs = result.handoffs ?? [];
      current = result.available === true && result.providers !== undefined;
      loaded = true;
      error = current ? null : "Your cloud connections couldn't be checked yet. Check again shortly.";
    } catch {
      if (alive && !signal?.aborted) { loaded = true; current = false; error = "Your provider connections couldn't refresh. Existing connections haven't been changed."; }
    } finally {
      catalogFlight = false;
      if (catalogAgain && alive && visible && $pageVisible) { catalogAgain = false; void load(); }
    }
  }
  async function checkConnection(signal?: AbortSignal): Promise<void> {
    if (!connection || connectionFlight) return;
    const id = connection.id;
    const operation = mutation;
    connectionFlight = true;
    try {
      const result = await cloudRequest({ operation: "provider_connection", connection_id: id }, signal);
      if (!alive || signal?.aborted || operation !== mutation || connection?.id !== id) return;
      if (!result.connection) throw new Error("missing connection");
      connection = result.connection;
      connectionNotice = null;
      if (connection.phase === "connected") void load();
    } catch {
      if (alive && !signal?.aborted && operation === mutation && connection?.id === id) connectionNotice = "We couldn't check this sign-in yet. The provider may still be waiting for you.";
    } finally { connectionFlight = false; }
  }
  $effect(() => {
    if (!visible || !$pageVisible) return;
    const controller = new AbortController();
    untrack(() => void load(controller.signal));
    const timer = setInterval(() => void load(controller.signal), 30_000);
    return () => { controller.abort(); clearInterval(timer); };
  });
  $effect(() => {
    const id = connectionId;
    const expires = connectionExpires;
    if (!visible || !$pageVisible || !waiting || !id || !expires) return;
    const controller = new AbortController();
    const deadline = Math.min(expires * 1000, Date.now() + 15 * 60_000);
    let timer: ReturnType<typeof setTimeout> | undefined;
    const poll = async () => {
      await checkConnection(controller.signal);
      if (controller.signal.aborted) return;
      if (Date.now() >= deadline) { pollingPaused = true; return; }
      timer = setTimeout(() => void poll(), 2000);
    };
    untrack(() => { pollingPaused = false; void poll(); });
    return () => { controller.abort(); clearTimeout(timer); };
  });
  onDestroy(() => { alive = false; mutation += 1; });

  async function connect(providerId: string): Promise<void> {
    if (busy !== null || waiting && !pollingPaused) return;
    const request = ++mutation;
    busy = providerId; error = null; copied = false; connectionNotice = null;
    try {
      const result = await cloudRequest({ operation: "provider_connect", provider_id: providerId });
      if (!alive || request !== mutation) return;
      if (!result.connection) throw new Error("missing connection");
      connection = result.connection;
      pollingPaused = false;
      if (connection.phase === "connected") await load();
      await tick();
      if (alive && request === mutation && visible && $pageVisible) {
        connectionElement?.scrollIntoView({ block: "nearest", behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
        connectionElement?.focus({ preventScroll: true });
      }
    } catch {
      if (alive && request === mutation) error = "Sign-in couldn't start on your cloud machine. Try again in a moment.";
    } finally { if (alive && request === mutation) busy = null; }
  }
  async function cancel(): Promise<void> {
    if (!connection || busy !== null) return;
    const id = connection.id;
    const request = ++mutation;
    busy = "cancel";
    try {
      const result = await cloudRequest({ operation: "provider_cancel", connection_id: id });
      if (alive && request === mutation && result.connection) { connection = result.connection; if (connection.phase === "connected") void load(); }
    } catch { if (alive && request === mutation) connectionNotice = "Cancellation couldn't be confirmed. Check sign-in status before starting again."; }
    finally { if (alive && request === mutation) busy = null; }
  }
  async function openSignIn(terminal = false): Promise<void> {
    if (!connection || busy !== null) return;
    const request = ++mutation;
    busy = "open"; connectionNotice = null;
    try { await cloudRequest({ operation: terminal ? "open_provider_terminal" : "open_provider_browser", connection_id: connection.id }); }
    catch { if (alive && request === mutation) connectionNotice = terminal ? "The sign-in terminal couldn't open. Try again shortly." : "Your browser couldn't open. Try again shortly."; }
    finally { if (alive && request === mutation) busy = null; }
  }
  async function copyCode(): Promise<void> {
    if (action?.type !== "device_code") return;
    try {
      const ok = native ? await writeClipboard(action.user_code) : await navigator.clipboard.writeText(action.user_code).then(() => true);
      if (!ok) throw new Error("clipboard unavailable");
      if (alive) copied = true;
    } catch { if (alive) connectionNotice = "The code couldn't be copied. You can select it below."; }
  }
  async function resume(handoff: Handoff, returnAfter = false): Promise<void> {
    if (busy !== null || !current || !providersReady(providers, handoff.blocked_providers.map(p => p.id))) return;
    const request = ++mutation;
    busy = `resume:${handoff.workspace_id}`; error = null;
    try {
      await cloudRequest({ operation: "resume_handoff", workspace_id: handoff.workspace_id, expected_epoch: handoff.expected_epoch });
      if (alive && request === mutation) { focusedHandoff = null; await load(); if (returnAfter) onReady?.(); }
    } catch { if (alive && request === mutation) error = "This project couldn't continue yet. Its cloud copy is still paused; check its setup and try again."; }
    finally { if (alive && request === mutation) busy = null; }
  }
</script>

<section class="providers" aria-label="Cloud agent connections">
  <div class="heading"><div><span class="eyebrow">Your cloud agents</span><h2>{required.length ? `Connect your agents${projectName ? ` for ${projectName}` : " to continue"}` : ready ? "Your agents are connected" : "Connect your first agent"}</h2></div><button class="text-button" disabled={catalogFlight || busy !== null} onclick={() => void load()}>Check connections</button></div>
  <p class="intro">{required.length ? "This project is paused until the agents it uses are signed in on your cloud machine." : ready ? "Your connected agents can work on your cloud machine. You can add another whenever you need it." : "Choose the agent you want to use. Connect one to get started; you can add others later."}</p>
  <p class="privacy">Use your own provider account and subscription. Sign-in authorizes this cloud machine; credentials from your other devices aren't copied.</p>
  {#if !loaded}<p class="muted" role="status">Checking your cloud connections…</p>{/if}
  {#if loaded && agents.length === 0}<p class="muted">No agent connections are available from this cloud machine yet.</p>{/if}
  <div class="provider-cards">
    {#each agents as provider (provider.id)}
      <article class="provider-card" class:connected={current && provider.state === "signed_in"}>
        <div class="provider-title"><h3>{provider.label}</h3>{#if required.includes(provider.id)}<span class="required">Needed for this project</span>{/if}</div>
        <p class="state" class:positive={current && provider.state === "signed_in"}>{!current && provider.state === "signed_in" ? "Previously connected · checking status" : providerStateLabel(provider)}</p>
        <p class="provider-note">{provider.state === "signed_in" ? "Signed in on your cloud machine." : provider.state === "missing" ? "We'll prepare the agent, then guide you through sign-in." : provider.state === "unknown" ? "Check the connection, or sign in again if needed." : provider.state === "unavailable" ? "This connection isn't available on this cloud machine yet." : "Connect the account you already use for this agent."}</p>
        {#if provider.state !== "signed_in"}<button class="button" disabled={busy !== null || waiting && !pollingPaused || provider.methods.length === 0 || provider.state === "unavailable"} onclick={() => void connect(provider.id)}>{busy === provider.id ? "Starting…" : `Connect ${provider.label}`}</button>{:else}<span class="connected-label">{current ? "Connected on cloud" : "Check connection to confirm"}</span>{/if}
      </article>
    {/each}
  </div>
  {#each required.filter(id => !agents.some(p => p.id === id)) as id}<p class="error">The cloud machine doesn't offer the required provider “{id}” yet. Update its agent support before continuing this project.</p>{/each}

  {#if connection}
    <section class="connection" aria-label={`Connect ${connectingLabel}`} tabindex="-1" bind:this={connectionElement}>
      <div class="heading"><h3>{connection.phase === "connected" ? `${connectingLabel} is connected` : connection.phase === "failed" ? "Sign-in needs attention" : connection.phase === "expired" ? "Sign-in expired" : connection.phase === "canceled" ? "Sign-in canceled" : `Connect ${connectingLabel}`}</h3>{#if waiting}<span class="phase" role="status">{connection.phase === "preparing" ? "Preparing your agent…" : connection.phase === "verifying" ? "Confirming connection…" : "Waiting for sign-in"}</span>{/if}</div>
      {#if connection.phase === "connected"}<p class="muted">Sign-in was confirmed on your cloud machine. Your local provider account hasn't changed.</p>
      {:else if ["failed", "expired", "canceled"].includes(connection.phase)}<p class="muted">{connectionError(connection.phase === "failed" ? connection.error_code : connection.phase)}</p><button class="button" disabled={busy !== null} onclick={() => void connect(connection!.provider_id)}>Try again</button>
      {:else if connection.phase === "preparing"}<p class="muted">We’re preparing the agent on your cloud machine. You can view its progress while this finishes.</p>{#if action?.type === "terminal"}<button class="button secondary" disabled={busy !== null} onclick={() => void openSignIn(true)}>View setup progress</button>{/if}
      {:else if action?.type === "device_code"}
        <ol class="instructions"><li>Copy this one-time code.</li></ol><div class="code-row"><code aria-label="One-time sign-in code">{action.user_code}</code><button class="button secondary" onclick={() => void copyCode()}>{copied ? "Copied" : "Copy code"}</button></div>
        <ol class="instructions" start="2"><li>Open the provider's secure sign-in page and enter the code.</li></ol>
        {#if native}<button class="button" disabled={busy !== null} onclick={() => void openSignIn()}>Open sign-in page</button>{:else if loginUrl}<a class="button" href={loginUrl} target="_blank" rel="noopener noreferrer">Open sign-in page</a>{:else}<p class="error">The provider's sign-in link couldn't be verified.</p>{/if}
        <p class="muted small">Leave this view open while you finish. We'll confirm the connection here.</p>
      {:else if action?.type === "browser"}
        <p class="muted">Complete sign-in on the provider's secure page. We'll confirm the connection when you return.</p>
        {#if native}<button class="button" disabled={busy !== null} onclick={() => void openSignIn()}>Continue in browser</button>{:else if loginUrl}<a class="button" href={loginUrl} target="_blank" rel="noopener noreferrer">Continue in browser</a>{:else}<p class="error">The provider's sign-in link couldn't be verified.</p>{/if}
      {:else if action?.type === "terminal"}
        <p class="muted">This provider completes sign-in in its own terminal. Open it, follow the provider's instructions, then return here. We'll verify the connection for you.</p><button class="button" disabled={busy !== null} onclick={() => void openSignIn(true)}>Open sign-in terminal</button>
      {:else}<p class="muted">We're preparing this provider's sign-in on your cloud machine. This may take a moment.</p>{/if}
      {#if waiting}<div class="connection-actions"><button class="text-button" disabled={connectionFlight || busy !== null} onclick={() => void checkConnection()}>Check sign-in status</button><button class="text-button" disabled={busy !== null} onclick={() => void cancel()}>{busy === "cancel" ? "Canceling…" : "Cancel sign-in"}</button></div>{/if}
      {#if pollingPaused && waiting}<p class="muted small" role="status">Automatic checks have paused after this request's time limit. Check its status or cancel before trying again.</p>{/if}
      {#if connectionNotice}<p class="error" role="status">{connectionNotice}</p>{/if}
    </section>
  {/if}

  {#if ready && onReady}<div class="ready"><p>{required.length ? "The required agents are connected. You're ready to continue." : "Your first agent is connected. You're ready to start cloud work."}</p><button class="button" disabled={busy !== null} onclick={() => selectedHandoff ? void resume(selectedHandoff, true) : onReady?.()}>{selectedHandoff ? "Continue project" : required.length ? "Back to project" : "Continue to projects"}</button></div>{/if}
  {#each handoffs.filter(h => h.workspace_id !== selectedHandoff?.workspace_id) as handoff (handoff.workspace_id)}
    <div class="handoff"><div><h3>{handoff.name}</h3><p class="muted small">{current && providersReady(providers, handoff.blocked_providers.map(p => p.id)) ? "The required agents are connected. Continue this paused project when you're ready." : "Waiting for an agent connection on your cloud machine."}</p></div>{#if current && providersReady(providers, handoff.blocked_providers.map(p => p.id))}<button class="button" disabled={busy !== null} onclick={() => void resume(handoff)}>{busy === `resume:${handoff.workspace_id}` ? "Continuing…" : "Continue project"}</button>{:else}<button class="button secondary" onclick={() => (focusedHandoff = handoff)}>Connect required agents</button>{/if}</div>
  {/each}
  {#if repositories.length && required.length === 0}<details class="optional"><summary>Repository connections <span>Optional</span></summary><p class="muted small">Connect a repository provider when a project needs access to its private repositories.</p>{#each repositories as provider (provider.id)}<div class="repository"><div><h3>{provider.label}</h3><p class="muted small">{providerStateLabel(provider)}</p></div>{#if provider.state !== "signed_in"}<button class="button secondary" disabled={busy !== null || waiting || provider.methods.length === 0 || provider.state === "unavailable"} onclick={() => void connect(provider.id)}>Connect {provider.label}</button>{/if}</div>{/each}</details>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
</section>

<style>
  .providers { color: var(--fg); }
  .heading { display: flex; align-items: center; justify-content: space-between; gap: 14px; flex-wrap: wrap; }
  .eyebrow { display: block; margin-bottom: 8px; color: var(--muted); font-size: var(--text-xs); letter-spacing: .06em; text-transform: uppercase; }
  h2 { margin: 0; font-size: 22px; font-weight: 550; letter-spacing: -.5px; line-height: 1.3; }
  h3 { margin: 0; font-size: var(--text-md); font-weight: 550; line-height: 1.4; }
  .intro { margin: 15px 0 7px; font-size: var(--text-md); line-height: 1.65; max-width: 64ch; }
  .privacy { margin: 0; color: var(--muted); font-size: var(--text-xs); line-height: 1.7; max-width: 78ch; }
  .provider-cards { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 240px), 1fr)); gap: 13px; margin-top: 24px; }
  .provider-card { display: flex; flex-direction: column; align-items: flex-start; padding: 22px; border: 1px solid var(--edge); border-radius: 9px; }
  .provider-card.connected { background: color-mix(in srgb, var(--fg) 2%, var(--bg)); }
  .provider-title { display: flex; gap: 8px; flex-direction: column; }
  .provider-title h3 { font-size: var(--text-lg); }
  .required { color: var(--muted); font-size: var(--text-xs); }
  .state { margin: 14px 0 0; font-size: var(--text-sm); color: var(--muted); }
  .state.positive { color: var(--accent); }
  .provider-note { flex: 1; color: var(--muted); font-size: var(--text-sm); line-height: 1.65; margin: 8px 0 20px; }
  .button { display: inline-flex; justify-content: center; align-items: center; border: 1px solid transparent; border-radius: 7px; padding: 9px 13px; background: var(--fg); color: var(--bg); font: inherit; font-size: var(--text-sm); text-decoration: none; cursor: pointer; }
  .button.secondary { border-color: var(--edge); background: transparent; color: var(--fg); }
  .button:hover:not(:disabled) { opacity: .85; }
  button:disabled { opacity: .5; cursor: default; }
  button:focus-visible, a:focus-visible, summary:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 3px; }
  .text-button { border: 0; padding: 3px 0; background: transparent; color: var(--muted); font: inherit; font-size: var(--text-xs); text-decoration: underline; text-underline-offset: 3px; cursor: pointer; }
  .connected-label { color: var(--muted); font-size: var(--text-xs); padding: 10px 0; }
  .connection { margin-top: 20px; padding: 23px; background: color-mix(in srgb, var(--fg) 2%, var(--bg)); border: 1px solid var(--edge); border-radius: 9px; }
  .connection:focus { outline: none; }
  .phase { color: var(--muted); font-size: var(--text-xs); }
  .muted { color: var(--muted); font-size: var(--text-sm); line-height: 1.7; }
  .small { font-size: var(--text-xs); }
  .instructions { margin: 20px 0 12px; padding-left: 18px; color: var(--fg); font-size: var(--text-sm); line-height: 1.65; }
  .code-row { display: flex; align-items: center; flex-wrap: wrap; gap: 14px; }
  code { font-family: var(--mono); font-size: 25px; letter-spacing: .1em; overflow-wrap: anywhere; user-select: text; }
  .connection-actions { display: flex; flex-wrap: wrap; gap: 20px; margin-top: 22px; padding-top: 17px; border-top: 1px solid var(--edge); }
  .ready, .handoff { display: flex; align-items: center; justify-content: space-between; flex-wrap: wrap; gap: 15px; margin-top: 20px; padding: 18px 0; border-top: 1px solid var(--edge); }
  .ready p { margin: 0; font-size: var(--text-sm); line-height: 1.7; }
  .handoff > div { flex: 1; min-width: 200px; }
  .handoff p { margin: 5px 0 0; }
  .optional { border-top: 1px solid var(--edge); margin-top: 23px; }
  summary { padding: 17px 0; color: var(--fg); font-size: var(--text-sm); cursor: pointer; }
  summary span { margin-left: 8px; color: var(--muted); font-size: var(--text-xs); }
  .repository { display: flex; justify-content: space-between; align-items: center; gap: 12px; flex-wrap: wrap; padding: 12px 0; }
  .repository p { margin: 4px 0 0; }
  .error { color: var(--warn); font-size: var(--text-sm); line-height: 1.65; margin: 15px 0 0; overflow-wrap: anywhere; }
  @media (max-width: 520px) { .provider-card, .connection { padding: 18px; } h2 { font-size: 20px; } .code-row { gap: 10px; } code { font-size: 23px; } }
</style>
