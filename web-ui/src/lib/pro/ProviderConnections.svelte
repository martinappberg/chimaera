<script lang="ts">
  import { onDestroy, tick, untrack } from "svelte";
  import { pageVisible } from "../shared/visibility";
  import { isNativeShell, writeClipboard, type CloudProviderConnection, type CloudProviderStatus, type CloudSetupInfo } from "../net/native";
  import { cloudAction, cloudRequest } from "./cloudTransport";
  import { rememberCatalog } from "./catalogMemory";
  import { CHECKING_AFTER_MS, cloudAsleep } from "./presentation";
  import { agentsConnected, canDisconnect, canStartConnection, connectingLabel, connectionError, connectionSuccessCurrent, disconnectConnection, handoffKey, nextReadyHandoff, panelRows, pendingConnection, providerLabel, providerLoginUrl, providersReady, providerStateLabel, recoverDisconnect, sameConnection } from "./providers";

  let { visible = true, requiredProviders = [], contextLabel, workspaceId, onReady, onReadiness, onAgents, compact = false, live = true, remembered = null, pending = false, onOpen }: {
    visible?: boolean; requiredProviders?: string[]; contextLabel?: string; workspaceId?: string; onReady?: () => void; onReadiness?: (ready: boolean | null) => void;
    /** Whether any agent is connected by a fresh catalog; null when unknown. */
    onAgents?: (connected: boolean | null) => void; compact?: boolean;
    /** The cloud answers now: the catalog is polled only then. Otherwise the
     * rows stay as remembered, or as the last read showed them. */
    live?: boolean;
    /** The last catalog read's rows (the app's memory, or this browser's),
     * shown at once until a live read answers. */
    remembered?: CloudProviderStatus[] | null;
    /** Access the user asked for (opening this section) is still coming. */
    pending?: boolean;
    /** The user opened the section, or arrived needing a connection, while
     * the cloud is idle: the owner asks for access. Never on a passive path. */
    onOpen?: () => void;
  } = $props();
  type Handoff = NonNullable<CloudSetupInfo["handoffs"]>[number];
  let providers = $state<CloudProviderStatus[]>([]);
  let handoffs = $state<Handoff[]>([]);
  let focusedHandoff = $state<Handoff | null>(null);
  let connection = $state<CloudProviderConnection | null>(null);
  let connectionElement = $state<HTMLElement>();
  let disconnectCandidate = $state<CloudProviderStatus | null>(null);
  let confirmationElement = $state<HTMLElement>();
  let disconnectTrigger: HTMLElement | undefined;
  let loaded = $state(false);
  let current = $state(false);
  let busy = $state<string | null>(null);
  let requestingDisconnect = $state(false);
  let error = $state<string | null>(null);
  let operationError = $state<string | null>(null);
  let failedDisconnectProvider: string | null = null;
  let connectionNotice = $state<string | null>(null);
  /** The last catalog read found the cloud asleep or still starting
   * (`cloud_asleep`): the rows stay as they were, with no words and no error,
   * while polling continues. */
  let asleep = $state(false);
  /** A live catalog read has answered: its rows replace the remembered ones. */
  let liveAnswered = $state(false);
  /** Access was asked for once for this section (`request`). */
  let requested = $state(false);
  /** A live answer is late enough for the muted "Checking…". */
  let slow = $state(false);
  let copied = $state(false);
  let authorizationCode = $state("");
  let expanded = $state(false);
  let pollingPaused = $state(false);
  let attemptedHandoffs = $state<string[]>([]);
  let resumeFailures = $state<Record<string, string>>({});
  let alive = true;
  let catalogFlight = $state(false);
  let connectionFlight = $state(false);
  let catalogDisconnectBusy = $state(false);
  let catalogAgain = false;
  let mutation = $state(0);
  let visibilityGeneration = $state(0);
  let catalogMutation = $state(-1);
  let catalogVisibility = $state(-1);
  const native = isNativeShell();
  const required = $derived(requiredProviders.length ? requiredProviders : focusedHandoff?.blocked_providers.map(p => p.id) ?? []);
  const projectName = $derived(contextLabel ?? focusedHandoff?.name);
  /** Remembered rows until a live read answers; see `panelRows`. */
  const panel = $derived(panelRows({ providers, remembered, liveAnswered, loaded, current, asleep }));
  const fromMemory = $derived(panel.fromMemory);
  const rows = $derived(panel.rows);
  const known = $derived(panel.known);
  const settled = $derived(panel.settled);
  const agents = $derived(rows.filter(p => p.category === "agent"));
  const repositories = $derived(rows.filter(p => p.category === "repository"));
  const contextHandoff = $derived(handoffs.find(h => h.workspace_id === workspaceId));
  const selectedHandoff = $derived(contextHandoff ?? (focusedHandoff ? handoffs.find(h => h.workspace_id === focusedHandoff!.workspace_id) : undefined));
  const catalogFresh = $derived(current && catalogMutation === mutation && catalogVisibility === visibilityGeneration);
  /** Actions that need a fresh catalog (continuing a project, Back). */
  const ready = $derived(catalogFresh && providersReady(providers, required));
  /** What the words say: the rows as shown. */
  const shownReady = $derived(settled && providersReady(rows, required));
  const uncertain = $derived(!known || !settled || !shownReady && agents.some(p => p.state === "unknown"));
  const heading = $derived(uncertain ? !known ? "Agent connections" : error ? "Agent connections need attention" : "Agent connections aren't confirmed yet" : shownReady ? required.length ? "The required agents are connected" : "Ready for cloud work" : required.length ? `Connect your agents${projectName ? ` for ${projectName}` : " to continue"}` : "Connect an agent to start cloud work");
  const introduction = $derived(uncertain ? !known ? "" : "Chimaera is checking which agents are connected for cloud work." : shownReady ? required.length ? selectedHandoff && !resumeFailures[handoffKey(selectedHandoff)] ? "The agents this project needs are connected. Chimaera will continue it automatically." : "The agents this project needs are connected." : "Your connected agents are ready for cloud work. You can add another whenever you need it." : required.length ? "Connect the agents this project uses so it can continue automatically." : "Choose the agent you want to use. Connect one to get started; you can add others later.");
  const waiting = $derived(pendingConnection(connection));
  const disconnecting = $derived(disconnectConnection(connection));
  /** Connect works from remembered or idle rows too: the press itself wakes
   * the cloud, which checks the request. Disconnecting needs a fresh read. */
  const canStart = $derived(busy === null && !catalogDisconnectBusy && !pendingConnection(connection) && (catalogFresh || fromMemory || asleep));
  const canManage = $derived(busy === null && !catalogDisconnectBusy && canStartConnection(connection, catalogFresh));
  const confirmedSuccess = $derived(connectionSuccessCurrent(connection, providers, catalogFresh));
  const detailsNeeded = $derived(required.length > 0 || handoffs.length > 0 || connection !== null || operationError !== null || known && (!shownReady || error !== null));
  const showDetails = $derived(!compact || expanded || detailsNeeded);
  /** Nothing to show yet, and the access the user asked for did not come. */
  const stalled = $derived(!known && requested && !pending && !live);
  /** A live answer the user is waiting on: opening woke the cloud, or it
   * answers now and its first read is still out. A press speaks for itself. */
  const awaiting = $derived(visible && busy === null && (pending || live && !liveAnswered && !asleep && error === null));
  const connectionId = $derived(connection?.id ?? null);
  const connectionExpires = $derived(connection?.expires_at ?? null);
  const connectingName = $derived(rows.find(p => p.id === (requestingDisconnect ? busy : connection?.provider_id))?.label ?? "your agent");
  /** A repository connection (GitHub) is for Git in the cloud, not for an agent. */
  const connectingRepository = $derived(rows.find(p => p.id === connection?.provider_id)?.category === "repository");
  const action = $derived(connection?.action ?? null);
  const loginUrl = $derived(connection && action && (action.type === "device_code" || action.type === "browser")
    ? providerLoginUrl(connection.provider_id, action.type === "device_code" ? action.verification_url : action.url) : null);

  $effect(() => { const value = current ? ready : null; untrack(() => onReadiness?.(value)); });
  $effect(() => {
    const value = !catalogFresh ? null : agentsConnected(providers);
    untrack(() => onAgents?.(value));
  });
  $effect(() => {
    if (!awaiting) { slow = false; return; }
    const timer = setTimeout(() => (slow = true), CHECKING_AFTER_MS);
    return () => clearTimeout(timer);
  });
  /** Arriving needing a connection (a project's context) with nothing to show
   * is a request too: ask once, so the rows can load. */
  $effect(() => {
    if (visible && showDetails && !known && !live && !pending && !requested) untrack(() => request());
  });
  function request(): void {
    requested = true;
    onOpen?.();
  }
  function toggle(): void {
    expanded = !expanded;
    if (expanded && !live) request();
  }

  $effect(() => {
    if (!visible || !$pageVisible || busy !== null || waiting || disconnectCandidate || catalogDisconnectBusy) return;
    if (catalogMutation !== mutation || catalogVisibility !== visibilityGeneration) return;
    const handoff = nextReadyHandoff(providers, handoffs, attemptedHandoffs, current);
    if (handoff) untrack(() => void resume(handoff, handoff.workspace_id === workspaceId));
  });

  async function load(signal?: AbortSignal): Promise<void> {
    if (catalogFlight) { catalogAgain = true; return; }
    catalogFlight = true;
    const operation = mutation;
    const visibility = visibilityGeneration;
    try {
      const result = await cloudRequest({ operation: "providers" }, signal);
      if (!alive || signal?.aborted || visibility !== visibilityGeneration) return;
      if (operation !== mutation) { catalogAgain = true; return; }
      providers = result.providers ?? [];
      handoffs = result.handoffs ?? [];
      current = result.available === true && result.providers !== undefined;
      catalogDisconnectBusy = current && result.connection?.operation === "disconnect";
      if (catalogDisconnectBusy) {
        connection = recoverDisconnect(connection, result.connection);
        disconnectCandidate = null;
        if (result.connection?.provider_id === failedDisconnectProvider) { operationError = null; failedDisconnectProvider = null; }
      } else if (current && failedDisconnectProvider && providers.some(p => p.id === failedDisconnectProvider && ["needs_sign_in", "missing"].includes(p.state))) {
        operationError = null; failedDisconnectProvider = null;
      }
      catalogMutation = operation;
      catalogVisibility = visibility;
      loaded = true;
      asleep = false;
      if (current) { liveAnswered = true; rememberCatalog(providers); }
      error = current ? null : "We couldn't check your agent sign-ins yet. We'll try again shortly.";
    } catch (cause) {
      if (alive && !signal?.aborted && visibility === visibilityGeneration) {
        if (operation !== mutation) catalogAgain = true;
        // Asleep or still starting is a state: the rows stay as they were
        // (or the neutral loading state), no words, and checks continue.
        else if (cloudAsleep(cause)) { current = false; asleep = true; error = null; }
        else { loaded = true; current = false; asleep = false; error = "We couldn't check your agent sign-ins. We'll try again shortly."; }
      }
    } finally {
      catalogFlight = false;
      if (catalogAgain && alive && visible && $pageVisible) { catalogAgain = false; void load(); }
    }
  }
  async function checkConnection(signal?: AbortSignal): Promise<void> {
    if (!connection || connectionFlight || busy !== null) return;
    const id = connection.id;
    const operation = mutation;
    connectionFlight = true;
    try {
      const result = await cloudRequest({ operation: "provider_connection", connection_id: id }, signal);
      if (!alive || signal?.aborted || operation !== mutation || connection?.id !== id) return;
      if (!sameConnection(connection, result.connection)) throw new Error("invalid connection");
      if (!result.connection) return;
      connection = result.connection;
      connectionNotice = null;
      if (!pendingConnection(connection)) { current = false; void load(); }
    } catch {
      if (alive && !signal?.aborted && operation === mutation && connection?.id === id) connectionNotice = disconnecting ? "We couldn't confirm this disconnection yet. Check its status before trying again." : `We couldn't check this sign-in yet. ${connectingName} may still be waiting for you.`;
    } finally { connectionFlight = false; }
  }
  $effect(() => {
    // Passive reads only while the cloud answers; idle, the rows stay.
    if (!visible || !$pageVisible || !live) return;
    const controller = new AbortController();
    untrack(() => void load(controller.signal));
    const timer = setInterval(() => void load(controller.signal), 30_000);
    return () => { visibilityGeneration += 1; controller.abort(); clearInterval(timer); };
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
  $effect(() => { if (!visible || !$pageVisible || connectionId === null) authorizationCode = ""; });
  $effect(() => { if (!visible || !$pageVisible) disconnectCandidate = null; });
  onDestroy(() => { alive = false; mutation += 1; authorizationCode = ""; });

  async function connect(providerId: string, operation: "connect" | "disconnect" = "connect"): Promise<void> {
    if (operation === "connect" ? !canStart : !canManage) return;
    if (operation === "disconnect" && !providers.some(p => p.id === providerId && canDisconnect(p))) return;
    const request = ++mutation;
    busy = providerId; requestingDisconnect = operation === "disconnect"; error = null; operationError = null; failedDisconnectProvider = null; copied = false; authorizationCode = ""; connectionNotice = null; disconnectCandidate = null;
    try {
      // The press wakes the cloud; while it comes up, the button keeps
      // saying what it does (bounded, then the usual failure below).
      const result = await cloudAction(operation === "disconnect"
        ? { operation: "provider_disconnect", provider_id: providerId, acknowledge_cloud_work: true }
        : { operation: "provider_connect", provider_id: providerId }, () => alive && request === mutation);
      if (!alive || request !== mutation || result === null) return;
      if (!result.connection || result.connection.provider_id !== providerId || (result.connection.operation ?? "connect") !== operation) throw new Error("invalid connection");
      connection = result.connection;
      pollingPaused = false;
      if (!pendingConnection(connection)) { current = false; await load(); }
      await tick();
      if (alive && request === mutation && visible && $pageVisible) {
        connectionElement?.scrollIntoView({ block: "nearest", behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
        connectionElement?.focus({ preventScroll: true });
      }
    } catch (cause) {
      if (alive && request === mutation) {
        operationError = cause === "provider_busy" || cause instanceof Error && cause.message === "provider_busy"
          ? connectionError("provider_busy", operation)
          : operation === "disconnect" ? "Disconnection couldn't be confirmed. Check the connection before trying again." : "Sign-in couldn't start in the cloud. Try again in a moment.";
        failedDisconnectProvider = operation === "disconnect" ? providerId : null;
        current = false; void load();
      }
    } finally { if (alive && request === mutation) { busy = null; requestingDisconnect = false; } }
  }
  async function requestDisconnect(provider: CloudProviderStatus, trigger?: HTMLElement): Promise<void> {
    if (!canManage || !canDisconnect(provider)) return;
    disconnectTrigger = trigger;
    disconnectCandidate = provider;
    await tick();
    if (alive && visible && $pageVisible && disconnectCandidate?.id === provider.id) {
      confirmationElement?.scrollIntoView({ block: "nearest", behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
      confirmationElement?.focus({ preventScroll: true });
    }
  }
  async function keepConnected(): Promise<void> {
    disconnectCandidate = null;
    await tick();
    if (alive && visible && $pageVisible && disconnectTrigger?.isConnected) disconnectTrigger.focus();
  }
  async function cancel(): Promise<void> {
    if (!connection || disconnecting || busy !== null) return;
    const id = connection.id;
    const request = ++mutation;
    busy = "cancel"; authorizationCode = "";
    try {
      const result = await cloudRequest({ operation: "provider_cancel", connection_id: id });
      if (alive && request === mutation && result.connection) { connection = result.connection; if (!pendingConnection(connection)) { current = false; void load(); } }
    } catch { if (alive && request === mutation) connectionNotice = "Cancellation couldn't be confirmed. Check sign-in status before starting again."; }
    finally { if (alive && request === mutation) busy = null; }
  }
  async function openSignIn(terminal = false): Promise<void> {
    if (!connection || disconnecting || busy !== null) return;
    const request = ++mutation;
    busy = "open"; connectionNotice = null;
    try { await cloudRequest({ operation: terminal ? "open_provider_terminal" : "open_provider_browser", connection_id: connection.id }); }
    catch { if (alive && request === mutation) connectionNotice = terminal ? "The sign-in window couldn't open. Try again shortly." : "Your browser couldn't open. Try again shortly."; }
    finally { if (alive && request === mutation) busy = null; }
  }
  async function submitCode(): Promise<void> {
    if (!connection || disconnecting || connection.phase !== "waiting" || action?.type !== "browser" || action.input !== "authorization_code" || busy !== null) return;
    const id = connection.id;
    const code = authorizationCode.trim();
    if (!code || code.length > 4096 || /[\s\x00-\x1f\x7f]/.test(code)) {
      connectionNotice = `Paste only the one-time code from ${connectingName}'s sign-in page.`;
      return;
    }
    const request = ++mutation;
    busy = "submit"; authorizationCode = ""; connectionNotice = null;
    try {
      const result = await cloudRequest({ operation: "provider_submit", connection_id: id, code });
      if (!alive || request !== mutation || connection?.id !== id) return;
      if (!result.connection) throw new Error("missing connection");
      connection = result.connection;
      if (connection.phase === "connected") void load();
    } catch (cause) {
      if (alive && request === mutation) connectionNotice = cause instanceof Error && cause.message === "authorization_code_incomplete"
        ? connectionError("authorization_code_incomplete")
        : "The code couldn't be confirmed. Check sign-in status, or start again for a fresh code.";
    }
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
    if (busy !== null || !nextReadyHandoff(providers, [handoff], [], current)) return;
    const key = handoffKey(handoff);
    if (!attemptedHandoffs.includes(key)) attemptedHandoffs = [...attemptedHandoffs, key];
    delete resumeFailures[key];
    const request = ++mutation;
    busy = `resume:${handoff.workspace_id}`; error = null;
    try {
      await cloudRequest({ operation: "resume_handoff", workspace_id: handoff.workspace_id, expected_epoch: handoff.expected_epoch });
      if (alive && request === mutation) {
        focusedHandoff = null;
        await load();
        if (returnAfter && alive && request === mutation && visible && $pageVisible && workspaceId === handoff.workspace_id) onReady?.();
      }
    } catch { if (alive && request === mutation) resumeFailures[key] = "This project couldn't continue yet. Your saved work is intact. Try again when you're ready."; }
    finally { if (alive && request === mutation) busy = null; }
  }
</script>

<section class="providers" class:compact aria-label="Cloud agent connections">
  {#if compact && !detailsNeeded}
    <button class="management" aria-expanded={showDetails} onclick={toggle}><span><span class="title-row"><strong>Agent connections</strong>{#if slow}<span class="checking">Checking…</span>{/if}</span>{#if known}<span class="management-state">{shownReady ? "Ready for cloud work" : "Checking connection status…"}</span>{/if}</span><span class="chevron" class:expanded aria-hidden="true">›</span></button>
  {/if}
  {#if showDetails}
  <!-- With nothing to show yet, the management row above is the title. -->
  {#if known || waiting || requestingDisconnect || !(compact && !detailsNeeded)}<div class="heading"><div><span class="eyebrow">{waiting && connectingRepository ? "Repository connection" : "Cloud agents"}</span><h2>{requestingDisconnect ? `Disconnecting ${connectingName}…` : waiting ? disconnecting ? `Disconnecting ${connectingName}…` : connection?.phase === "preparing" ? `Preparing ${connectingName} sign-in…` : connection?.phase === "verifying" ? `Connecting ${connectingName}…` : `Connect ${connectingName}` : heading}</h2></div>{#if slow && !(compact && !detailsNeeded)}<span class="checking" role="status">Checking…</span>{/if}</div>{/if}
  {#if requestingDisconnect || waiting || introduction}<p class="intro">{requestingDisconnect || waiting && disconnecting ? "Chimaera is signing this service out in the cloud." : waiting ? connection?.phase === "preparing" ? "Sign-in will appear here when it's ready." : connection?.phase === "verifying" ? `Chimaera is confirming your sign-in with ${connectingName}.` : "Finish sign-in below. Chimaera will confirm the connection automatically." : introduction}</p>{/if}
  <p class="privacy">Use your own accounts and subscriptions. Connected services are available across your cloud projects. Signing in or disconnecting here doesn't change sign-in on your computer.</p>
  {#if !known && !waiting && !requestingDisconnect}
    <!-- Nothing remembered yet (a new account): placeholders, no words, until the rows load. -->
    {#if stalled}<p class="muted" role="status">Your agent connections couldn’t load yet.</p><button class="text-button" onclick={request}>Try again</button>
    {:else}<div class="provider-cards" aria-busy="true" aria-label="Agent connections">
      {#each [0, 1] as slot (slot)}<div class="provider-card placeholder" aria-hidden="true"><span class="bar wide"></span><span class="bar"></span><span class="bar short"></span><span class="bar action"></span></div>{/each}
    </div>{/if}
  {/if}
  {#if known && current && agents.length === 0}<p class="muted">No cloud agent connections are available yet.</p>{/if}
  {#if !waiting && !requestingDisconnect}<div class="provider-cards">
    {#each agents as provider (provider.id)}
      <article class="provider-card" class:connected={settled && provider.state === "signed_in"}>
        <div class="provider-title"><h3>{provider.label}</h3>{#if required.includes(provider.id)}<span class="required">Needed for this project</span>{/if}</div>
        <p class="state" class:positive={settled && provider.state === "signed_in"}>{!settled && provider.state === "signed_in" ? "Previously connected · checking status" : providerStateLabel(provider)}</p>
        <p class="provider-note">{provider.state === "signed_in" ? "Signed in for cloud work." : provider.state === "unknown" ? "Check the connection, or sign in again if needed." : provider.state === "unavailable" ? "This connection isn't available for cloud work yet." : "Connect the account you already use for this agent."}</p>
        <div class="provider-actions">{#if provider.state !== "signed_in"}<button class="button" disabled={!canStart || provider.methods.length === 0 || provider.state === "unavailable"} onclick={() => void connect(provider.id)}>{busy === provider.id ? connectingLabel(provider) : `Connect ${provider.label}`}</button>{/if}{#if canDisconnect(provider)}<button class="text-button" disabled={!canManage} onclick={(event) => void requestDisconnect(provider, event.currentTarget)}>Disconnect {provider.label}</button>{:else if provider.state === "signed_in"}<span class="connected-label">{settled ? "Connected" : "Check connection to confirm"}</span>{/if}</div>
      </article>
    {/each}
  </div>
  {#each [...new Set(required.filter(id => !agents.some(p => p.id === id)))] as id (id)}<p class="error">{providerLabel(id)} isn't available in the cloud yet. This project waits on your computer.</p>{/each}

  {/if}

  {#if disconnectCandidate}
    <section class="connection confirmation" aria-label={`Disconnect ${disconnectCandidate.label}`} tabindex="-1" bind:this={confirmationElement}>
      <h3>Disconnect {disconnectCandidate.label} in the cloud?</h3>
      <p class="muted">This signs {disconnectCandidate.label} out in the cloud. All your cloud projects share this connection. Running work that uses it may lose access and need you to reconnect.</p>
      <p class="muted small">Sign-in on your computer stays as it is.</p>
      <div class="confirmation-actions"><button class="button secondary" onclick={() => void keepConnected()}>Keep connected</button><button class="button" disabled={!canManage || !providers.some(p => p.id === disconnectCandidate?.id && canDisconnect(p))} onclick={() => void connect(disconnectCandidate!.id, "disconnect")}>Disconnect {disconnectCandidate.label}</button></div>
    </section>
  {/if}

  {#if connection}
    {#if connection.phase === "disconnected"}{#if confirmedSuccess}<p class="connection-success" role="status">{connectingName} is signed out in the cloud. Sign-in on your computer hasn't changed.</p>{/if}
    {:else if disconnecting}
    <section class="connection" aria-label={`Disconnect ${connectingName}`} tabindex="-1" bind:this={connectionElement}>
      {#if !waiting}<h3>{connectingName} disconnection needs attention</h3>{/if}
      <p class="muted" role="status">{waiting ? connection.phase === "verifying" ? "Confirming that this service is signed out in the cloud." : "Signing this service out in the cloud. Sign-in on your computer stays as it is." : connectionError(connection.phase === "failed" ? connection.error_code : connection.phase, "disconnect")}</p>
      {#if !waiting || pollingPaused || connectionNotice}<div class="connection-actions"><button class="text-button" disabled={connectionFlight || catalogFlight || busy !== null} onclick={() => waiting ? void checkConnection() : void load()}>{waiting ? "Check disconnection status" : "Check connection"}</button>{#if !waiting && canManage}{@const provider = providers.find(p => p.id === connection?.provider_id)}{#if provider && canDisconnect(provider)}<button class="text-button" onclick={(event) => void requestDisconnect(provider, event.currentTarget)}>Try disconnecting again</button>{/if}{/if}</div>{/if}
      {#if pollingPaused && waiting}<p class="muted small" role="status">This is taking longer than expected. Check the request's status before trying again.</p>{/if}
      {#if connectionNotice}<p class="error" role="status">{connectionNotice}</p>{/if}
    </section>
    {:else if connection.phase === "connected"}{#if confirmedSuccess}<p class="connection-success" role="status">{connectingRepository ? `${connectingName} is connected. Your cloud can now pull and push your repositories.` : `${connectingName} is connected for cloud work.`}</p>{/if}{:else}
    <section class="connection" aria-label={`Connect ${connectingName}`} tabindex="-1" bind:this={connectionElement}>
      <div class="heading">{#if !waiting}<h3>{connection.phase === "failed" ? `${connectingName} sign-in needs attention` : connection.phase === "expired" ? `${connectingName} sign-in expired` : connection.phase === "canceled" ? `${connectingName} sign-in canceled` : `Connect ${connectingName}`}</h3>{/if}{#if waiting}<span class="phase" role="status">{connection.phase === "preparing" ? "Preparing sign-in…" : connection.phase === "verifying" ? "Confirming connection…" : "Waiting for sign-in"}</span>{/if}</div>
      {#if ["failed", "expired", "canceled"].includes(connection.phase)}<p class="muted">{connectionError(connection.phase === "failed" ? connection.error_code : connection.phase)}</p><button class="button" disabled={!canStart} onclick={() => void connect(connection!.provider_id)}>Try again</button>
      {:else if connection.phase === "preparing"}<p class="muted" role="status">Preparing {connectingName} for sign-in. This happens automatically and may take a moment.</p>
      {:else if connection.phase === "verifying"}<p class="muted" role="status">Confirming your connection with {connectingName}…</p>
      {:else if action?.type === "device_code"}
        {#if connectingRepository}<p class="muted">This lets your cloud pull and push your {connectingName} repositories.</p>{/if}
        <ol class="instructions"><li>Copy this one-time code.</li></ol><div class="code-row"><code aria-label="One-time sign-in code">{action.user_code}</code><button class="button secondary" onclick={() => void copyCode()}>{copied ? "Copied" : "Copy code"}</button></div>
        <ol class="instructions" start="2"><li>Open {connectingName}'s sign-in page, enter the code and approve access.</li></ol>
        {#if native}<button class="button" disabled={busy !== null} onclick={() => void openSignIn()}>Open sign-in page</button>{:else if loginUrl}<a class="button" href={loginUrl} target="_blank" rel="noopener noreferrer">Open sign-in page</a>{:else}<p class="error">This sign-in link couldn't be verified.</p>{/if}
        <p class="muted small">Leave this view open while you finish. We'll confirm the connection here.</p>
      {:else if action?.type === "browser"}
        <p class="muted">Sign in to {connectingName} in your browser.{#if action.input === "authorization_code"} It then shows a code — copy it and paste it here.{/if}</p>
        {#if native}<button class="button" disabled={busy !== null} onclick={() => void openSignIn()}>Continue in browser</button>{:else if loginUrl}<a class="button" href={loginUrl} target="_blank" rel="noopener noreferrer">Continue in browser</a>{:else}<p class="error">This sign-in link couldn't be verified.</p>{/if}
        {#if action.input === "authorization_code"}
          <form class="authorization" onsubmit={(event) => { event.preventDefault(); void submitCode(); }}>
            <label for={`provider-code-${connection.id}`}>Code from {connectingName}</label>
            <!-- Visible so a paste can be checked; still cleared on submit, cancel, hide and teardown. -->
            <div class="authorization-row"><input id={`provider-code-${connection.id}`} type="text" bind:value={authorizationCode} autocomplete="off" autocapitalize="off" spellcheck={false} maxlength="4096" placeholder="Paste the code here" disabled={busy !== null} /><button class="button" type="submit" disabled={busy !== null || !authorizationCode.trim()}>{busy === "submit" ? "Confirming…" : "Connect"}</button></div>
            <p class="muted small">The code goes directly to {connectingName}'s sign-in. It isn't saved in Chimaera.</p>
          </form>
        {/if}
      {:else if action?.type === "terminal"}
        <!-- Only an older cloud still asks for this (its GitHub sign-in); current ones show a code above. -->
        <p class="muted">{connectingName} finishes sign-in in a separate window. Open the sign-in window, then come back here. We'll confirm the connection for you.</p><button class="button" disabled={busy !== null} onclick={() => void openSignIn(true)}>Open sign-in window</button>
      {:else}<p class="muted">Preparing sign-in in your cloud. This may take a moment.</p>{/if}
      {#if waiting}<div class="connection-actions">{#if pollingPaused || connectionNotice}<button class="text-button" disabled={connectionFlight || busy !== null} onclick={() => void checkConnection()}>Check sign-in status</button>{/if}<button class="text-button" disabled={busy !== null} onclick={() => void cancel()}>{busy === "cancel" ? "Canceling…" : "Cancel sign-in"}</button></div>{/if}
      {#if pollingPaused && waiting}<p class="muted small" role="status">Automatic checks have paused after this request's time limit. Check its status or cancel before trying again.</p>{/if}
      {#if connectionNotice}<p class="error" role="status">{connectionNotice}</p>{/if}
    </section>
    {/if}
  {/if}

  {#if ready && onReady && !selectedHandoff && !disconnectCandidate && !waiting}<div class="ready"><p>{required.length ? "The required agents are connected." : "Your first agent is connected. You're ready for cloud work."}</p><button class="button" disabled={busy !== null} onclick={() => onReady?.()}>{required.length ? "Back to project" : "Back to projects"}</button></div>{/if}
  {#each handoffs as handoff (handoff.workspace_id)}
    <div class="handoff"><div><h3>{handoff.name}</h3><p class="muted small" role="status">{resumeFailures[handoffKey(handoff)] ?? (current && providersReady(providers, handoff.blocked_providers.map(p => p.id)) ? "Continuing your project…" : "Waiting for an agent connection for cloud work.")}</p></div>{#if resumeFailures[handoffKey(handoff)]}<button class="button" disabled={busy !== null || !nextReadyHandoff(providers, [handoff], [], current)} onclick={() => void resume(handoff, handoff.workspace_id === workspaceId)}>Try again</button>{:else if !current || !providersReady(providers, handoff.blocked_providers.map(p => p.id))}<button class="button secondary" onclick={() => (focusedHandoff = handoff)}>Connect required agents</button>{/if}</div>
  {/each}
  {#if !waiting && !requestingDisconnect && repositories.length && required.length === 0}<details class="optional"><summary>Repository connections <span>Optional</span></summary><p class="muted small">Lets your cloud pull and push your repositories, including private ones.</p>{#each repositories as provider (provider.id)}<div class="repository"><div><h3>{provider.label}</h3><p class="muted small">{providerStateLabel(provider)}</p><p class="muted small repository-use">{settled && provider.state === "signed_in" ? "Your cloud can pull and push your repositories." : `Connect to pull and push your ${provider.label} repositories from your cloud.`}</p></div><div class="provider-actions">{#if provider.state !== "signed_in"}<button class="button secondary" disabled={!canStart || provider.methods.length === 0 || provider.state === "unavailable"} onclick={() => void connect(provider.id)}>{busy === provider.id ? connectingLabel(provider) : `Connect ${provider.label}`}</button>{/if}{#if canDisconnect(provider)}<button class="text-button" disabled={!canManage} onclick={(event) => void requestDisconnect(provider, event.currentTarget)}>Disconnect {provider.label}</button>{/if}</div></div>{/each}</details>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if operationError}<p class="error" role="alert">{operationError}</p>{/if}
  {/if}
</section>

<style>
  .connection-success { margin: 18px 0 0; color: var(--accent); font-size: var(--text-sm); }
  .authorization { margin-top: 24px; }
  .authorization label { display: block; margin-bottom: 9px; font-size: var(--text-sm); font-weight: 550; }
  .authorization-row { display: flex; gap: 10px; flex-wrap: wrap; }
  .authorization input { flex: 1 1 200px; min-width: 0; padding: 10px 12px; border: 1px solid var(--edge); border-radius: 7px; color: var(--fg); background: var(--bg); font: inherit; }
  .authorization input:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 2px; }
  .management { display: flex; align-items: center; justify-content: space-between; gap: 16px; width: 100%; padding: 2px 0; background: transparent; border: 0; color: var(--fg); font: inherit; text-align: left; cursor: pointer; }
  .management strong { font-size: var(--text-sm); font-weight: 550; }
  .title-row { display: flex; align-items: baseline; gap: 8px; flex-wrap: wrap; }
  .checking { color: var(--muted); font-size: var(--text-xs); font-weight: 400; }
  .placeholder { gap: 11px; }
  .placeholder .bar { display: block; width: 58%; height: 11px; border-radius: 4px; background: color-mix(in srgb, var(--fg) 7%, var(--bg)); }
  .placeholder .bar.wide { width: 42%; height: 15px; }
  .placeholder .bar.short { width: 34%; }
  .placeholder .bar.action { width: 38%; height: 36px; margin-top: 12px; border-radius: 7px; }
  .management-state { display: block; color: var(--muted); font-size: var(--text-xs); margin-top: 5px; }
  .chevron { font-size: 24px; color: var(--muted); transform: rotate(0deg); }
  .chevron.expanded { transform: rotate(90deg); }
  .management + .heading { margin-top: 24px; }
  .compact .eyebrow { display: none; }
  .compact h2 { font-size: var(--text-lg); letter-spacing: -.2px; }
  .compact .intro { font-size: var(--text-sm); margin-top: 10px; }
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
  .provider-actions { display: flex; align-items: center; flex-wrap: wrap; gap: 16px; }
  .button { display: inline-flex; justify-content: center; align-items: center; border: 1px solid transparent; border-radius: 7px; padding: 9px 13px; background: var(--fg); color: var(--bg); font: inherit; font-size: var(--text-sm); text-decoration: none; cursor: pointer; }
  .button.secondary { border-color: var(--edge); background: transparent; color: var(--fg); }
  .button:hover:not(:disabled) { opacity: .85; }
  button:disabled { opacity: .5; cursor: default; }
  button:focus-visible, a:focus-visible, summary:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 3px; }
  .text-button { border: 0; padding: 3px 0; background: transparent; color: var(--muted); font: inherit; font-size: var(--text-xs); text-decoration: underline; text-underline-offset: 3px; cursor: pointer; }
  .connected-label { color: var(--muted); font-size: var(--text-xs); padding: 10px 0; }
  .connection { margin-top: 20px; padding: 23px; background: color-mix(in srgb, var(--fg) 2%, var(--bg)); border: 1px solid var(--edge); border-radius: 9px; }
  .connection:focus { outline: none; }
  .confirmation:focus { outline: 2px solid var(--focus-ring); outline-offset: 3px; }
  .confirmation-actions { display: flex; justify-content: flex-end; flex-wrap: wrap; gap: 12px; margin-top: 20px; }
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
  .repository .repository-use { margin-top: 2px; }
  .error { color: var(--warn); font-size: var(--text-sm); line-height: 1.65; margin: 15px 0 0; overflow-wrap: anywhere; }
  @media (max-width: 520px) { .provider-card, .connection { padding: 18px; } h2 { font-size: 20px; } .code-row { gap: 10px; } code { font-size: 23px; } }
  @media (pointer: coarse) { .button, .management, summary { min-height: 40px; } .text-button { min-height: 40px; padding: 8px 0; } }
</style>
