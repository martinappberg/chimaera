<script lang="ts">
  import { onDestroy, tick, untrack } from "svelte";
  import { pageVisible } from "../shared/visibility";
  import { isNativeShell, writeClipboard, type CloudProviderConnection, type CloudProviderStatus, type CloudSetupInfo } from "../net/native";
  import { ProviderTransport } from "./personalProviderTransport";
  import { onProChanged } from "../net/native";
  import { rememberCatalog } from "./catalogMemory";
  import { CHECKING_AFTER_MS, CLOUD_ASLEEP, cloudAsleep, friendlyError } from "./presentation";
  import { agentsConnected, awaitingCloudUpdate, canDisconnect, cloudUpdateLine, connectingLabel, connectionError, connectionSuccessCurrent, disconnectConnection, handoffKey, installingAgent, nextReadyHandoff, olderCloudSignIn, panelRows, pendingConnection, providerLabel, providerLoginUrl, providersReady, providerStateLabel, recoverDisconnect, sameConnection, stillAwaitingUpdate } from "./providers";

  let { visible = true, requiredProviders = [], contextLabel, workspaceId, onReady, onReadiness, onAgents, compact = false, live = true, remembered = null }: {
    visible?: boolean; requiredProviders?: string[]; contextLabel?: string; workspaceId?: string; onReady?: () => void; onReadiness?: (ready: boolean | null) => void;
    /** Whether any agent is connected by a fresh catalog; null when unknown. */
    onAgents?: (connected: boolean | null) => void; compact?: boolean;
    /** The cloud answers now: the catalog is polled only then. Otherwise the
     * rows stay as remembered, or as the last read showed them, and showing
     * the section only looks (`peekCatalog`): it never wakes the cloud. */
    live?: boolean;
    /** The last catalog read's rows (the app's memory, or this browser's),
     * shown at once until a live read answers. */
    remembered?: CloudProviderStatus[] | null;
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
  /** This showing of the section has looked once (`peekCatalog`). */
  let peeked = $state(false);
  /** A live answer is late enough for the muted "Checking…". */
  let slow = $state(false);
  let copied = $state(false);
  let authorizationCode = $state("");
  let expanded = $state(false);
  /** Providers whose Connect an older cloud answered with a sign-in the app
   * never opens: their rows wait for the cloud's update until a fresh
   * catalog offers the one-time code (`stillAwaitingUpdate`). */
  let pressedUpdate = $state<string[]>([]);
  /** Repository connections stay open or closed across the sign-in guide,
   * which replaces the rows while it shows. */
  let repositoriesOpen = $state(false);
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
  let transport = new ProviderTransport();
  let personal = $state(false);
  let unlisten: (() => void) | undefined;
  function retireContext(): void {
    transport.clear(); transport = new ProviderTransport();
    mutation += 1; authorizationCode = ""; connection = null; providers = [];
    handoffs = []; disconnectCandidate = null; current = false; loaded = false;
    liveAnswered = false; busy = null; error = null; operationError = null;
    personal = false; if (alive && visible && $pageVisible) void load();
  }
  void onProChanged(() => {
    const selected = transport;
    void selected.revalidate().catch(cause => {
      // Ordinary cloud/status events and routine token refresh never abandon
      // an original attempt. Only confirmed account-context retirement does.
      if (alive && selected === transport && cause instanceof Error && ["providers_context_changed", "providers_sign_in_required"].includes(cause.message)) retireContext();
    });
  }).then(stop => { if (alive) unlisten = stop; else stop(); });
  function contextFailure(cause: unknown): void {
    if (!(cause instanceof Error) || !["providers_context_changed", "providers_sign_in_required"].includes(cause.message)) return;
    authorizationCode = ""; disconnectCandidate = null; connection = null;
    providers = []; handoffs = []; current = false; liveAnswered = false;
  }
  async function cloudRequest(request: Parameters<ProviderTransport["request"]>[0], signal?: AbortSignal): Promise<CloudSetupInfo> {
    try { return await transport.request(request, signal); } catch (cause) { contextFailure(cause); throw cause; }
  }
  async function cloudAction(request: Parameters<ProviderTransport["action"]>[0], wanted: () => boolean): Promise<CloudSetupInfo | null> {
    try { return await transport.action(request, wanted); } catch (cause) { contextFailure(cause); throw cause; }
  }
  async function peekCatalog(signal?: AbortSignal): Promise<CloudSetupInfo> {
    try { return await transport.catalog(signal); } catch (cause) { contextFailure(cause); throw cause; }
  }
  const required = $derived(requiredProviders.length ? requiredProviders : focusedHandoff?.blocked_providers.map(p => p.id) ?? []);
  const projectName = $derived(contextLabel ?? focusedHandoff?.name);
  /** Remembered rows until a live read answers; see `panelRows`. */
  const panel = $derived(panelRows({ providers, remembered, liveAnswered, loaded, current, asleep }));
  const fromMemory = $derived(panel.fromMemory);
  /** Rows named from the shared catalog only: no state is claimed. */
  const unchecked = $derived(panel.unchecked);
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
  const heading = $derived(unchecked ? required.length ? `Connect your agents${projectName ? ` for ${projectName}` : " to continue"}` : "Agent connections" : uncertain ? !known ? "Agent connections" : error ? "Agent connections need attention" : "Agent connections aren't confirmed yet" : shownReady ? required.length ? "The required agents are connected" : "Ready for cloud work" : required.length ? `Connect your agents${projectName ? ` for ${projectName}` : " to continue"}` : "Connect an agent to start cloud work");
  const introduction = $derived(unchecked ? required.length ? "Connect the agents this project uses so it can continue automatically." : "Connect an agent to use it in the cloud. Agents you connected before stay connected." : uncertain ? !known ? "" : "Chimaera is checking which agents are connected for cloud work." : shownReady ? required.length ? selectedHandoff && !resumeFailures[handoffKey(selectedHandoff)] ? "The agents this project needs are connected. Chimaera will continue it automatically." : "The agents this project needs are connected." : "Your connected agents are ready for cloud work. You can add another whenever you need it." : required.length ? "Connect the agents this project uses so it can continue automatically." : "Choose the agent you want to use. Connect one to get started; you can add others later.");
  const waiting = $derived(pendingConnection(connection));
  const disconnecting = $derived(disconnectConnection(connection));
  /** Connect and Disconnect both work from remembered or idle rows: the
   * press itself brings the cloud up, which then checks the request. Nothing
   * on this page is greyed out only because the cloud is idle. */
  const canStart = $derived(busy === null && !catalogDisconnectBusy && !pendingConnection(connection) && (catalogFresh || fromMemory || asleep));
  const canManage = $derived(canStart);
  const confirmedSuccess = $derived(connectionSuccessCurrent(connection, providers, catalogFresh));
  const detailsNeeded = $derived(required.length > 0 || handoffs.length > 0 || connection !== null || pressedUpdate.length > 0 || operationError !== null || known && (!shownReady || error !== null));
  const showDetails = $derived(!compact || expanded || detailsNeeded);
  /** A live answer still out: the first read of a cloud that answers, or a
   * look while it is idle. An idle answer ends it; a press speaks for itself. */
  const awaiting = $derived(visible && busy === null && error === null && !asleep && !liveAnswered && (live || catalogFlight));
  /** Rows waiting for the cloud's update instead of offering Connect: a
   * Connect found an older cloud, or a read offers only its older sign-in. */
  const updating = $derived(new Set([...pressedUpdate, ...providers.filter(awaitingCloudUpdate).map(p => p.id)]));
  const connectionId = $derived(connection?.id ?? null);
  const connectionExpires = $derived(connection?.expires_at ?? null);
  const connectingName = $derived(rows.find(p => p.id === (requestingDisconnect ? busy : connection?.provider_id))?.label ?? "your agent");
  const action = $derived(connection?.action ?? null);
  /** Whether this row's own sign-in or sign-out is under way: its guide
   * shows in the row and the row's buttons step aside. */
  function inProgress(provider: Pick<CloudProviderStatus, "id">): boolean {
    return pendingConnection(connection) && connection?.provider_id === provider.id || requestingDisconnect && busy === provider.id;
  }
  /** The row's button: what it does while pressed, Try again once its last
   * sign-in ended short, Connect otherwise. */
  function connectLabel(provider: CloudProviderStatus): string {
    if (busy === provider.id) return connectingLabel(provider);
    if (connection?.provider_id === provider.id && ["failed", "expired", "canceled"].includes(connection.phase)) return "Try again";
    return `Connect ${provider.label}`;
  }
  /** Whether a sign-in or sign-out ended short: its reason shows in the row. */
  function ended(phase: string): boolean {
    return ["failed", "expired", "canceled"].includes(phase);
  }
  /** What a press that never reached the cloud says: the cloud still coming
   * up past the wake bound, the app's own sentence when it gave one (the
   * cloud unavailable, the account changed), else the generic line. A bare
   * code is never shown. */
  function pressFailure(cause: unknown, operation: "connect" | "disconnect"): string {
    if (cloudAsleep(cause)) return friendlyError(CLOUD_ASLEEP, "");
    const message = cause instanceof Error ? cause.message : typeof cause === "string" ? cause : "";
    if (/^[A-Z][^_{}<>]{15,160}[.!]$/.test(message)) return message;
    return operation === "disconnect" ? "Disconnection couldn't be confirmed. Check the connection before trying again." : "Sign-in couldn't start in the cloud. Try again in a moment.";
  }
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
  /** Showing the section while the cloud is idle looks once, passively: a
   * cloud that happens to be awake refreshes the rows silently; an idle one
   * changes nothing. Only Connect, Disconnect and sign-in steps wake it. */
  $effect(() => {
    if (visible && $pageVisible && showDetails && !live && !peeked) untrack(() => { peeked = true; void load(); });
  });
  function toggle(): void {
    expanded = !expanded;
    // Each opening looks again.
    if (!expanded) peeked = false;
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
      const result = await peekCatalog(signal);
      personal = transport.personal;
      if (!alive || signal?.aborted || visibility !== visibilityGeneration) return;
      if (operation !== mutation) { catalogAgain = true; return; }
      // A look while the cloud is idle that finds no catalog changes nothing.
      if (!live && (result.available !== true || result.providers === undefined)) { current = false; asleep = true; error = null; return; }
      providers = result.providers ?? [];
      handoffs = result.handoffs ?? [];
      current = result.available === true && result.providers !== undefined;
      catalogDisconnectBusy = current && result.connection?.operation === "disconnect" && (!personal || pendingConnection(result.connection));
      if (personal && result.connection) connection = result.connection;
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
      // Connect comes back only once the cloud offers the one-time code.
      if (current && pressedUpdate.length) pressedUpdate = stillAwaitingUpdate(pressedUpdate, providers);
      error = current ? null : "We couldn't check your agent sign-ins yet. We'll try again shortly.";
    } catch (cause) {
      if (alive && !signal?.aborted && visibility === visibilityGeneration) {
        if (operation !== mutation) catalogAgain = true;
        // Asleep or still starting is a state, and a look while the cloud is
        // idle is never an alarm: the rows stay as they were, with no words.
        else if (!transport.personalRequired && (cloudAsleep(cause) || !live)) { current = false; asleep = true; error = null; }
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
      if (olderCloudSignIn(result.connection)) { awaitUpdate(result.connection); return; }
      connection = result.connection;
      connectionNotice = null;
      if (personal) operationError = null;
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
  onDestroy(() => { alive = false; mutation += 1; authorizationCode = ""; transport.clear(); unlisten?.(); });

  async function connect(providerId: string, operation: "connect" | "disconnect" = "connect"): Promise<void> {
    if (operation === "connect" ? !canStart : !canManage) return;
    if (operation === "disconnect" && !providers.some(p => p.id === providerId && canDisconnect(p))) return;
    const request = ++mutation;
    busy = providerId; requestingDisconnect = operation === "disconnect"; error = null; operationError = null; failedDisconnectProvider = null; copied = false; authorizationCode = ""; connectionNotice = null; disconnectCandidate = null;
    // A repository's sign-in shows in its row, inside the optional list.
    if (rows.some(p => p.id === providerId && p.category === "repository")) repositoriesOpen = true;
    try {
      // The press wakes the cloud; while it comes up, the button keeps
      // saying what it does (bounded, then the usual failure below).
      const result = await cloudAction(operation === "disconnect"
        ? { operation: "provider_disconnect", provider_id: providerId, acknowledge_cloud_work: true }
        : { operation: "provider_connect", provider_id: providerId }, () => alive && request === mutation);
      if (!alive || request !== mutation || result === null) return;
      if (!result.connection || result.connection.provider_id !== providerId || (result.connection.operation ?? "connect") !== operation) throw new Error("invalid connection");
      if (olderCloudSignIn(result.connection)) { awaitUpdate(result.connection); return; }
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
        personal = transport.personal;
        if (personal) connection = transport.pending();
        operationError = cause === "provider_busy" || cause instanceof Error && cause.message === "provider_busy"
          ? connectionError("provider_busy", operation)
          : pressFailure(cause, operation);
        failedDisconnectProvider = operation === "disconnect" ? providerId : null;
        current = false; void load();
      }
    } finally { if (alive && request === mutation) { busy = null; requestingDisconnect = false; } }
  }
  /** An older cloud answered Connect with a sign-in the app never opens (the
   * cloud's own page never shows here). That attempt ends quietly, and the
   * row says the cloud is being updated, with Try again. */
  function awaitUpdate(attempt: CloudProviderConnection): void {
    if (!pressedUpdate.includes(attempt.provider_id)) pressedUpdate = [...pressedUpdate, attempt.provider_id];
    if (rows.some(p => p.id === attempt.provider_id && p.category === "repository")) repositoriesOpen = true;
    connection = null;
    connectionNotice = null;
    void cloudRequest({ operation: "provider_cancel", connection_id: attempt.id }).catch(() => { /* It expires on its own. */ });
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
  async function openSignIn(): Promise<void> {
    if (!connection || disconnecting || busy !== null) return;
    const request = ++mutation;
    busy = "open"; connectionNotice = null;
    try { await cloudRequest({ operation: "open_provider_browser", connection_id: connection.id }); }
    catch { if (alive && request === mutation) connectionNotice = "Your browser couldn't open. Try again shortly."; }
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

<!-- A sign-in lives inside its own row (agent card or repository line): the
     code and one button, a short waiting line and Cancel. Nothing else on the
     page moves while it runs, and the row itself reports how it ended. -->
{#snippet inline(provider: CloudProviderStatus)}
  {#if connection && connection.provider_id === provider.id && (connection.phase !== "connected" && connection.phase !== "disconnected" || confirmedSuccess)}
    {@const label = provider.label}
    <div class="guide" tabindex="-1" bind:this={connectionElement} aria-label={disconnecting || connection.phase === "disconnected" ? `Disconnect ${label}` : `Connect ${label}`}>
      {#if connection.phase === "disconnected"}
        <p class="connection-success" role="status">Signed out in the cloud. Sign-in on your computer hasn't changed.</p>
      {:else if disconnecting}
        {#if waiting}<p class="muted small" role="status">{connection.phase === "verifying" ? "Confirming the sign-out…" : "Signing out in the cloud…"}</p>
        {:else}<p class="error small" role="status">{connectionError(connection.phase === "failed" ? connection.error_code : connection.phase, "disconnect")}</p>{/if}
        {#if !waiting || pollingPaused || connectionNotice}<p class="muted small"><button class="text-button" disabled={connectionFlight || catalogFlight || busy !== null} onclick={() => waiting ? void checkConnection() : void load()}>{waiting ? "Check sign-out status" : "Check connection"}</button>{#if !waiting && canManage && canDisconnect(provider)} · <button class="text-button" onclick={(event) => void requestDisconnect(provider, event.currentTarget)}>Try disconnecting again</button>{/if}</p>{/if}
      {:else if connection.phase === "connected"}
        <p class="connection-success" role="status">{provider.category === "repository" ? "Connected. Your cloud can now pull and push your repositories." : "Connected for cloud work."}</p>
      {:else if ended(connection.phase)}
        <p class="error small" role="status">{connectionError(connection.phase === "failed" ? connection.error_code : connection.phase)}</p>
      {:else if connection.phase === "preparing"}
        <p class="muted small" role="status">{installingAgent(connection) ? `Installing ${label} in your cloud. This takes a moment the first time…` : "Preparing sign-in…"}</p>
      {:else if connection.phase === "verifying"}
        <p class="muted small" role="status">Confirming…</p>
      {:else if action?.type === "device_code"}
        <div class="code-row"><code aria-label="One-time sign-in code">{action.user_code}</code><button class="button secondary" onclick={() => void copyCode()}>{copied ? "Copied" : "Copy"}</button>{#if native || personal}<button class="button" disabled={busy !== null} onclick={() => void openSignIn()}>Open {label}</button>{:else if loginUrl}<a class="button" href={loginUrl} target="_blank" rel="noopener noreferrer">Open {label}</a>{:else}<span class="error small">This sign-in link couldn't be verified.</span>{/if}</div>
        <p class="muted small" role="status">Enter the code on {label}'s sign-in page and approve. Waiting for you… <button class="text-button" disabled={busy !== null} onclick={() => void cancel()}>{busy === "cancel" ? "Canceling…" : "Cancel"}</button></p>
      {:else if action?.type === "browser"}
        <div class="code-row">{#if native || personal}<button class="button" disabled={busy !== null} onclick={() => void openSignIn()}>Open {label} sign-in</button>{:else if loginUrl}<a class="button" href={loginUrl} target="_blank" rel="noopener noreferrer">Open {label} sign-in</a>{:else}<span class="error small">This sign-in link couldn't be verified.</span>{/if}</div>
        {#if action.input === "authorization_code"}
          <form class="authorization" onsubmit={(event) => { event.preventDefault(); void submitCode(); }}>
            <!-- Visible so a paste can be checked; still cleared on submit, cancel, hide and teardown. -->
            <div class="authorization-row"><input id={`provider-code-${connection.id}`} aria-label={`Code from ${label}`} type="text" bind:value={authorizationCode} autocomplete="off" autocapitalize="off" spellcheck={false} maxlength="4096" placeholder={`Paste the code ${label} shows`} disabled={busy !== null} /><button class="button" type="submit" disabled={busy !== null || !authorizationCode.trim()}>{busy === "submit" ? "Confirming…" : "Connect"}</button></div>
          </form>
          <p class="muted small" role="status">Sign in there, then paste the whole code it shows. <button class="text-button" disabled={busy !== null} onclick={() => void cancel()}>{busy === "cancel" ? "Canceling…" : "Cancel"}</button></p>
        {:else}
          <p class="muted small" role="status">Sign in there. Waiting for you… <button class="text-button" disabled={busy !== null} onclick={() => void cancel()}>{busy === "cancel" ? "Canceling…" : "Cancel"}</button></p>
        {/if}
      {:else}
        <p class="muted small" role="status">Preparing sign-in…</p>
      {/if}
      {#if waiting && !disconnecting && (pollingPaused || connectionNotice)}<p class="muted small"><button class="text-button" disabled={connectionFlight || busy !== null} onclick={() => void checkConnection()}>Check sign-in status</button></p>{/if}
      {#if pollingPaused && waiting}<p class="muted small" role="status">{disconnecting ? "This is taking longer than expected. Check its status before trying again." : "Automatic checks paused at this request's time limit. Check its status or cancel, then start again."}</p>{/if}
      {#if connectionNotice}<p class="error small" role="status">{connectionNotice}</p>{/if}
    </div>
  {/if}
{/snippet}

<section class="providers" class:compact aria-label="Cloud agent connections">
  {#if compact && !detailsNeeded}
    <button class="management" aria-expanded={showDetails} onclick={toggle}><span><span class="title-row"><strong>Agent connections</strong>{#if slow}<span class="checking">Checking…</span>{/if}</span>{#if known}<span class="management-state">{shownReady ? "Ready for cloud work" : "Checking connection status…"}</span>{/if}</span><span class="chevron" class:expanded aria-hidden="true">›</span></button>
  {/if}
  {#if showDetails}
  <!-- With nothing to show yet, the management row above is the title. -->
  <!-- A sign-in or sign-out in progress changes nothing up here: it shows
       inside its own row, so the heading and the other rows stay put. -->
  {#if known || waiting || requestingDisconnect || !(compact && !detailsNeeded)}<div class="heading"><div><span class="eyebrow">Cloud agents</span><h2>{heading}</h2></div>{#if slow && !(compact && !detailsNeeded)}<span class="checking" role="status">Checking…</span>{/if}</div>{/if}
  {#if introduction}<p class="intro">{introduction}</p>{/if}
  <p class="privacy">Use your own accounts and subscriptions. Connected services are available across your cloud projects. Signing in or disconnecting here doesn't change sign-in on your computer.</p>
  {#if !known && !waiting && !requestingDisconnect}
    <!-- Nothing remembered and no answer yet: placeholders, no words. -->
    <div class="provider-cards" aria-busy="true" aria-label="Agent connections">
      {#each [0, 1] as slot (slot)}<div class="provider-card placeholder" aria-hidden="true"><span class="bar wide"></span><span class="bar"></span><span class="bar short"></span><span class="bar action"></span></div>{/each}
    </div>
  {/if}
  {#if known && current && agents.length === 0}<p class="muted">No cloud agent connections are available yet.</p>{/if}
  <div class="provider-cards">
    {#each agents as provider (provider.id)}
      {@const waitsForUpdate = provider.state !== "signed_in" && updating.has(provider.id)}
      <article class="provider-card" class:connected={settled && provider.state === "signed_in"}>
        <div class="provider-title"><h3>{provider.label}</h3>{#if required.includes(provider.id)}<span class="required">Needed for this project</span>{/if}</div>
        {#if !unchecked}<p class="state" class:positive={settled && provider.state === "signed_in"}>{!settled && provider.state === "signed_in" ? "Previously connected · checking status" : providerStateLabel(provider)}</p>{/if}
        <p class="provider-note" role={waitsForUpdate ? "status" : undefined}>{waitsForUpdate ? cloudUpdateLine(provider.label) : unchecked ? "Connect the account you already use for this agent." : provider.state === "signed_in" ? "Signed in for cloud work." : provider.state === "unknown" ? "Check the connection, or sign in again if needed." : provider.state === "unavailable" ? "This connection isn't available for cloud work yet." : "Connect the account you already use for this agent."}</p>
        <!-- Try again only looks (a passive catalog read, never a wake). -->
        <div class="provider-actions">{#if waitsForUpdate}<button class="button secondary" disabled={catalogFlight} onclick={() => void load()}>{catalogFlight ? "Checking…" : "Try again"}</button>{:else if provider.state !== "signed_in" && !inProgress(provider)}<button class="button" disabled={!canStart || !unchecked && provider.methods.length === 0 || provider.state === "unavailable"} onclick={() => void connect(provider.id)}>{connectLabel(provider)}</button>{/if}{#if canDisconnect(provider) && !inProgress(provider)}<button class="text-button" disabled={!canManage} onclick={(event) => void requestDisconnect(provider, event.currentTarget)}>Disconnect {provider.label}</button>{:else if provider.state === "signed_in" && !inProgress(provider)}<span class="connected-label">{settled ? "Connected" : "Check connection to confirm"}</span>{/if}</div>
        {@render inline(provider)}
      </article>
    {/each}
  </div>
  {#each [...new Set(required.filter(id => !agents.some(p => p.id === id)))] as id (id)}<p class="error">{providerLabel(id)} isn't available in the cloud yet. This project waits on your computer.</p>{/each}

  {#if disconnectCandidate}
    <section class="connection confirmation" aria-label={`Disconnect ${disconnectCandidate.label}`} tabindex="-1" bind:this={confirmationElement}>
      <h3>Disconnect {disconnectCandidate.label} in the cloud?</h3>
      <p class="muted">This signs {disconnectCandidate.label} out in the cloud. All your cloud projects share this connection. Running work that uses it may lose access and need you to reconnect.</p>
      <p class="muted small">Sign-in on your computer stays as it is.</p>
      <div class="confirmation-actions"><button class="button secondary" onclick={() => void keepConnected()}>Keep connected</button><button class="button" disabled={!canManage || !providers.some(p => p.id === disconnectCandidate?.id && canDisconnect(p))} onclick={() => void connect(disconnectCandidate!.id, "disconnect")}>Disconnect {disconnectCandidate.label}</button></div>
    </section>
  {/if}

  <!-- Only a project waiting on its agents gets a way back from here; the
       plain page has its own navigation, and a row already says Connected. -->
  {#if ready && onReady && required.length > 0 && !selectedHandoff && !disconnectCandidate && !waiting}<div class="ready"><p>{required.length ? "The required agents are connected." : "Your first agent is connected. You're ready for cloud work."}</p><button class="button" disabled={busy !== null} onclick={() => onReady?.()}>{required.length ? "Back to project" : "Back to projects"}</button></div>{/if}
  {#each handoffs as handoff (handoff.workspace_id)}
    <div class="handoff"><div><h3>{handoff.name}</h3><p class="muted small" role="status">{resumeFailures[handoffKey(handoff)] ?? (current && providersReady(providers, handoff.blocked_providers.map(p => p.id)) ? "Continuing your project…" : "Waiting for an agent connection for cloud work.")}</p></div>{#if resumeFailures[handoffKey(handoff)]}<button class="button" disabled={busy !== null || !nextReadyHandoff(providers, [handoff], [], current)} onclick={() => void resume(handoff, handoff.workspace_id === workspaceId)}>Try again</button>{:else if !current || !providersReady(providers, handoff.blocked_providers.map(p => p.id))}<button class="button secondary" onclick={() => (focusedHandoff = handoff)}>Connect required agents</button>{/if}</div>
  {/each}
  {#if repositories.length && required.length === 0}<details class="optional" bind:open={repositoriesOpen}><summary>Repository connections <span>Optional</span></summary><p class="muted small">Lets your cloud pull and push your repositories, including private ones.</p>{#each repositories as provider (provider.id)}{@const waitsForUpdate = provider.state !== "signed_in" && updating.has(provider.id)}<div class="repository"><div><h3>{provider.label}</h3>{#if !unchecked}<p class="muted small">{providerStateLabel(provider)}</p>{/if}<p class="muted small repository-use" role={waitsForUpdate ? "status" : undefined}>{waitsForUpdate ? cloudUpdateLine(provider.label) : settled && provider.state === "signed_in" ? "Your cloud can pull and push your repositories." : `Connect to pull and push your ${provider.label} repositories from your cloud.`}</p></div><div class="provider-actions">{#if waitsForUpdate}<button class="button secondary" disabled={catalogFlight} onclick={() => void load()}>{catalogFlight ? "Checking…" : "Try again"}</button>{:else if provider.state !== "signed_in" && !inProgress(provider)}<button class="button secondary" disabled={!canStart || !unchecked && provider.methods.length === 0 || provider.state === "unavailable"} onclick={() => void connect(provider.id)}>{connectLabel(provider)}</button>{/if}{#if canDisconnect(provider) && !inProgress(provider)}<button class="text-button" disabled={!canManage} onclick={(event) => void requestDisconnect(provider, event.currentTarget)}>Disconnect {provider.label}</button>{/if}</div></div>{@render inline(provider)}{/each}</details>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if operationError}<p class="error" role="alert">{operationError}</p>{/if}
  {/if}
</section>

<style>
  .connection-success { margin: 0; color: var(--accent); font-size: var(--text-sm); line-height: 1.6; }
  /* The in-row guide: a rule above it inside a card, none under a repository line. */
  .guide { display: grid; gap: 10px; width: 100%; margin-top: 16px; padding-top: 14px; border-top: 1px solid var(--edge); }
  .guide:focus { outline: none; }
  .guide p { margin: 0; }
  .guide .error { margin: 0; }
  .repository + .guide { margin-top: 0; padding: 0 0 14px; border-top: 0; }
  .guide .text-button { padding: 0; }
  .authorization { margin: 0; }
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
  .muted { color: var(--muted); font-size: var(--text-sm); line-height: 1.7; }
  .small { font-size: var(--text-xs); }
  .code-row { display: flex; align-items: center; flex-wrap: wrap; gap: 12px; }
  code { font-family: var(--mono); font-size: 22px; letter-spacing: .1em; overflow-wrap: anywhere; user-select: text; }
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
