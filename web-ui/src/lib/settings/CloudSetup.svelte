<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { api } from "../net/api";
  import { gatewayWorkspace, workbenchPath } from "../net/base";
  import ProviderConnections from "../pro/ProviderConnections.svelte";
  import { cloudRequest } from "../pro/cloudTransport";
  import { BILLING_PATH } from "../pro/accountHome";
  import { pageVisible } from "../shared/visibility";
  import { cloudAsleep, cloudCopy, cloudPollDelay, cloudProjectStatus, friendlyError, sleepingConnectionsLine } from "../pro/presentation";
  import { isNativeShell, proCloudStatus, proMirrorStatus, writeClipboard, type MirrorStatus, type CloudSetupInfo, type CloudSetupRequest, type CloudProvisioningStatus } from "../net/native";

  let { visible = true, requiredProviders = [], contextLabel, workspaceId, onReady }: { visible?: boolean; requiredProviders?: string[]; contextLabel?: string; workspaceId?: string; onReady?: () => void } = $props();
  let info = $state<CloudSetupInfo | null>(null);
  let status = $state<CloudProvisioningStatus | null>(null);
  let busy = $state<string | null>(null);
  let error = $state<string | null>(null);
  let repository = $state("");
  let copied = $state(false);
  let advanced = $state(false);
  let generation = 0;
  let alive = true;
  let actionGeneration = 0;
  let connectionChecked = $state(false);
  let providersMounted = $state(false);
  let connectionsRequested = $state(false);
  let projects = $state<MirrorStatus | null>(null);
  let refreshFlight: Promise<void> | null = null;
  /** Passive reads in a row that found the cloud unreachable although the
   * account said it was ready. One is usually the machine going to sleep in
   * between, which is not an outage; only a second in a row is reported. */
  let unreachable = $state(0);
  /** From the connection panel's own fresh catalog while it shows. */
  let liveAgents = $state<boolean | null>(null);
  /** When a request that wakes the cloud machine found it still asleep or
   * starting (`cloud_asleep`): the connections section says it is waking, with
   * no error, while the checks below pick up the list. Bounded, so a machine
   * that went back to sleep offers Try again. */
  let wakingSince = $state<number | null>(null);
  /** A browser view's last read found the cloud machine asleep: a state, not
   * an outage. */
  let browserAsleep = $state(false);
  const browser = !isNativeShell();
  const agents = $derived(liveAgents ?? (browser ? null : status?.agents_connected ?? null));
  const connected = $derived(info?.available === true && (browser || status?.state === "ready"));
  const outage = $derived(status?.state === "ready" && !connected && unreachable >= 2);
  const asleep = $derived(status?.state === "sleeping" || status?.state === "ready" && connectionChecked && !connected && !outage);
  const copy = $derived(outage
    ? { title: "Cloud access is temporarily unavailable", detail: "We couldn’t reach your projects and agent connections. We’ll keep checking. You can keep working here." }
    : status?.state === "ready" && !connected
      ? connectionChecked ? cloudCopy("sleeping", status.reason, status.phase, agents) : { title: "Checking availability…", detail: "Checking access to your projects and agent connections." }
      : cloudCopy(status?.state ?? "error", status?.reason ?? null, status?.phase, agents));
  $effect(() => { if (connected) { providersMounted = true; connectionsRequested = false; wakingSince = null; } });
  const preparing = $derived(!browser && (status?.state === "preparing" || status?.state === "ready" && !connected && !connectionChecked));
  // The check mark claims a connected agent, so it needs one on record.
  const available = $derived((connected || asleep) && agents === true);
  const projectStatus = $derived(cloudProjectStatus(projects, workspaceId, browser ? "cloud" : "computer"));
  const needsCheck = $derived(browser ? connectionChecked && !connected && !browserAsleep : status?.state === "error" || status?.state === "unavailable" || outage);
  /** Waking: a request that wakes it is in flight while it sleeps, or one just found it still starting. */
  const wakeNoted = $derived(wakingSince !== null);
  const waking = $derived(wakeNoted || busy === "connections" && (asleep || browserAsleep));

  async function readProjects(signal?: AbortSignal): Promise<MirrorStatus | null> {
    try {
      if (!browser) return await proMirrorStatus();
      const timeout = AbortSignal.timeout(20_000);
      const response = await api("/pro/status", { signal: signal ? AbortSignal.any([signal, timeout]) : timeout });
      if (!response.ok) return null;
      return await response.json() as MirrorStatus;
    } catch { return null; }
  }
  async function refresh(signal?: AbortSignal, force = false): Promise<void> {
    if (refreshFlight) {
      await refreshFlight;
      if (force && alive && !signal?.aborted) return refresh(signal);
      return;
    }
    const current = ++generation;
    const task = (async () => {
      try {
        let reachable = false;
        if (browser) {
          const result = await cloudRequest({ operation: "info" }, signal).catch((reason: unknown) => { if (cloudAsleep(reason)) return null; throw reason; });
          if (!alive || signal?.aborted || current !== generation) return;
          browserAsleep = result === null;
          info = result; connectionChecked = true;
          reachable = result?.available === true;
        } else {
          const result: CloudProvisioningStatus = await proCloudStatus();
          if (!alive || signal?.aborted || current !== generation) return;
          // Passive status and metadata reads never wake a suspended machine.
          if (result.state === "ready") {
            const read = await cloudRequest({ operation: "info" }, signal).catch((reason: unknown) => cloudAsleep(reason) ? "asleep" as const : null);
            if (!alive || signal?.aborted || current !== generation) return;
            const details = read === "asleep" ? null : read;
            reachable = details?.available === true;
            // A machine that says it is asleep or starting is idle, not an
            // outage: it never counts as unreachable.
            if (read === "asleep") { status = result; info = null; connectionChecked = true; unreachable = 0; }
            else {
              // A machine that just went to sleep answers nothing: ask the
              // account again before counting it as unreachable.
              const again: CloudProvisioningStatus | null = reachable ? null : await proCloudStatus().catch(() => null);
              if (!alive || signal?.aborted || current !== generation) return;
              if (again !== null && again.state !== "ready") { status = again; info = null; connectionChecked = false; unreachable = 0; }
              else { status = result; info = details; connectionChecked = true; unreachable = reachable ? 0 : unreachable + 1; }
            }
          } else { status = result; info = null; connectionChecked = false; unreachable = 0; }
        }
        // Native mirror metadata is local and stays useful while compute is idle.
        const nextProjects = !browser || reachable ? await readProjects(signal) : null;
        if (alive && !signal?.aborted && current === generation) {
          projects = nextProjects; error = null;
          if (wakingSince !== null && Date.now() - wakingSince > 120_000) wakingSince = null;
        }
      } catch {
        if (alive && !signal?.aborted && current === generation) {
          status = { state: "error", reason: null }; info = null; projects = null; connectionChecked = true;
          error = "Cloud availability couldn’t refresh. Your local work is still available.";
        }
      }
    })();
    refreshFlight = task;
    void task.finally(() => { if (refreshFlight === task) refreshFlight = null; });
    return task;
  }
  $effect(() => {
    if (!visible || !$pageVisible) return;
    // A noted wake restarts the checks on the fast cadence (and its end on the usual one).
    void wakeNoted;
    const controller = new AbortController();
    const started = Date.now();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const poll = async () => {
      await refresh(controller.signal);
      if (controller.signal.aborted) return;
      // Read after the response, outside reactive tracking: status replacement
      // must not restart an immediate polling loop. Slow after five minutes.
      timer = setTimeout(() => void poll(), cloudPollDelay(preparing || waking, Date.now() - started));
    };
    untrack(() => void poll());
    return () => { generation += 1; controller.abort(); clearTimeout(timer); };
  });
  onDestroy(() => { alive = false; generation += 1; actionGeneration += 1; });
  async function act(label: string, request: CloudSetupRequest): Promise<void> {
    if (busy !== null) return;
    const action = ++actionGeneration;
    busy = label; error = null;
    if (request.operation === "start") { connectionsRequested = true; generation += 1; }
    try {
      const result = await cloudRequest(request);
      if (!alive || action !== actionGeneration) return;
      if (request.operation === "start") generation += 1;
      if (result.available !== undefined) info = result;
      // Only the cloud machine's own page opens a repository there; it is
      // navigation to the new project, never a way to move work.
      if (result.workspace_id && browser) {
        if (gatewayWorkspace() !== null) {
          // A project tab is bound to its path; a fragment would reopen the old one.
          location.assign(`/workspace/${encodeURIComponent(result.workspace_id)}/`);
        } else {
          const params = new URLSearchParams({ ws: result.workspace_id, win: `w-${crypto.randomUUID()}` });
          location.assign(`${workbenchPath()}#${params}`);
          location.reload();
        }
      }
      if (!alive || action !== actionGeneration) return;
      if (request.operation === "project") repository = "";
      if (request.operation === "start") await refresh(undefined, true);
    } catch (reason) {
      if (!alive || action !== actionGeneration) return;
      // Still asleep or starting after the wake: say it is waking, keep checking.
      if (request.operation === "start" && cloudAsleep(reason)) { wakingSince = Date.now(); return; }
      error = friendlyError(reason, request.operation === "project" ? "This repository couldn’t open in the cloud. Check the URL and your Git access, then try again." : "Your agent connections couldn’t load. Try again in a moment.");
    }
    finally { if (alive && action === actionGeneration) busy = null; }
  }
  async function copyKey(): Promise<void> {
    if (!info?.ssh_public_key) return;
    try { if (!await writeClipboard(info.ssh_public_key)) throw new Error("clipboard unavailable"); copied = true; } catch { error = "The public key couldn’t be copied. You can select it below."; }
  }
</script>

<section class="cloud" aria-label="Cloud status">
  <div class="machine" class:attention={needsCheck}>
    <div class="heading"><div><span class="eyebrow">Cloud</span><h2>{browser ? connected || browserAsleep ? "Available when you need it" : connectionChecked ? "Cloud access is temporarily unavailable" : "Checking availability…" : status ? copy.title : "Checking availability…"}</h2></div><span class="status-mark" class:connected={available} class:preparing aria-hidden="true">{#if available}<svg viewBox="0 0 24 24"><path d="m6 12 4 4 8-8" /></svg>{:else}<svg viewBox="0 0 24 24"><path d="M7 17a4 4 0 0 1-1-7.9 6 6 0 0 1 11.4-1.5A4.7 4.7 0 0 1 18 17H7Z" /></svg>{/if}</span></div>
    <p class="hint" role="status">{browser ? connected ? agents ? "Agents connected here can keep working while your computer sleeps." : "Agents you connect here can keep working while your computer sleeps." : browserAsleep ? sleepingConnectionsLine(waking) : connectionChecked ? "We couldn’t reach your projects and agent connections. We’ll keep checking." : "Checking access to your projects and agent connections." : status ? copy.detail : "Your projects and conversations come with you."}</p>
    {#if needsCheck && browser}<div class="recovery"><a class="account-link" href={BILLING_PATH}>Open your account →</a></div>{/if}
  </div>
  {#if projectStatus}
    <div class="project-status" class:attention={projectStatus.state === "attention"} role="status"><span class="project-dot" class:active={projectStatus.state === "active"} aria-hidden="true"></span><div><h3>{projectStatus.title}</h3><p class="hint">{projectStatus.detail}</p></div></div>
  {/if}
  {#if !connected && (!browser && asleep || browserAsleep || connectionsRequested)}
    <details class="sleeping-connections" ontoggle={(event) => { if (event.currentTarget.open && !connectionsRequested) void act("connections", { operation: "start" }); }}>
      <summary>Agent connections</summary>
      <!-- A browser view already says it is waking in the line above. -->
      {#if waking && !browserAsleep}<p class="hint" role="status">{sleepingConnectionsLine(true)}</p>
      {:else if busy === "connections" || preparing || waking}<p class="hint" role="status">Loading your agent connections…</p>
      {:else}<p class="hint">Your agent connections couldn’t load yet.</p><button class="text-button" disabled={busy !== null} onclick={() => void act("connections", { operation: "start" })}>Try again</button>{/if}
    </details>
  {/if}
  {#if providersMounted}
    <div class="provider-section" hidden={!connected}>
      <ProviderConnections visible={visible && connected} {requiredProviders} {contextLabel} {workspaceId} {onReady} {waking} onAgents={(value) => (liveAgents = value)} compact />
    </div>
  {/if}
  {#if connected && (browser || info?.ssh_public_key)}
    <details class="advanced" ontoggle={(event) => (advanced = event.currentTarget.open)}><summary>{browser ? "Repositories and advanced connections" : "Advanced connections"}</summary>{#if advanced}{#if browser}<p class="hint">For a project that starts in the cloud, open a Git repository here. Private repositories may need the optional repository connection above.</p><form onsubmit={(event) => { event.preventDefault(); void act("project", { operation: "project", url: repository.trim() }); }}><label for="cloud-repository">Repository URL</label><div class="clone-row"><input id="cloud-repository" type="url" placeholder="https://github.com/you/project" bind:value={repository} required disabled={busy !== null} /><button class="btn" disabled={busy !== null || !repository.trim()}>{busy === "project" ? "Opening repository…" : "Open repository"}</button></div></form>{/if}{#if info?.ssh_public_key}<details><summary>SSH public key</summary><p class="hint">Use this public key only if your Git host or connection needs it.</p><textarea aria-label="Cloud SSH public key" readonly value={info.ssh_public_key} rows="3"></textarea><button class="btn" onclick={() => void copyKey()}>{copied ? "Copied" : "Copy public key"}</button></details>{/if}{/if}</details>
  {/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
</section>

<style>
  .cloud { container: cloud-setup / inline-size; min-width: 0; display: grid; gap: 25px; border: 1px solid var(--edge); border-radius: 10px; padding: clamp(18px, 4%, 25px); margin: 22px 0; }
  .machine { display: grid; gap: 9px; }
  .eyebrow { display: block; color: var(--muted); font-size: var(--text-xs); letter-spacing: .06em; text-transform: uppercase; margin-bottom: 7px; }
  .status-mark { flex: none; width: 36px; height: 36px; display: grid; place-items: center; color: var(--muted); border: 1px solid var(--edge); border-radius: 50%; }
  .status-mark svg { width: 22px; height: 22px; fill: none; stroke: currentColor; stroke-width: 1.4; stroke-linecap: round; stroke-linejoin: round; }
  .status-mark.connected { color: var(--accent); border-color: color-mix(in srgb, var(--accent) 30%, var(--edge)); }
  .status-mark.preparing { color: var(--accent); }
  .recovery { display: flex; align-items: center; flex-wrap: wrap; gap: 18px; margin-top: 5px; }
  .project-status { display: flex; gap: 11px; align-items: baseline; padding: 17px 0 0; border-top: 1px solid var(--edge); }
  .project-status h3 { margin: 0 0 4px; font-size: var(--text-sm); font-weight: 550; }
  .project-dot { width: 6px; height: 6px; border-radius: 50%; background: var(--muted); flex: none; }
  .project-dot.active { background: var(--accent); }
  .project-status.attention .project-dot { background: var(--warn); }
  .provider-section { border-top: 1px solid var(--edge); padding-top: 22px; }
  .sleeping-connections { border-top: 1px solid var(--edge); }
  @container cloud-setup (max-width: 420px) { .heading > div { flex-basis: 180px; } }
  .heading { display: flex; justify-content: space-between; align-items: center; gap: 12px; flex-wrap: wrap; }
  .heading > div { flex: 1 1 230px; min-width: 0; }
  h2 { margin: 0; font-size: var(--text-lg); font-weight: 600; }
  .hint { margin: 0; color: var(--muted); font-size: var(--text-sm); line-height: 1.6; }
  .clone-row { display: flex; gap: 8px; flex-wrap: wrap; margin-top: 12px; }
  .btn { justify-self: start; border: 1px solid var(--edge); border-radius: 6px; color: var(--fg); background: var(--bg); padding: 8px 11px; font: inherit; font-size: var(--text-sm); cursor: pointer; }
  .btn:hover:not(:disabled) { border-color: var(--accent); }
  .btn:disabled { opacity: .55; cursor: default; }
  .text-button { border: 0; background: transparent; color: var(--accent); font: inherit; font-size: var(--text-sm); cursor: pointer; }
  button:focus-visible, input:focus-visible, summary:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 3px; }
  form { display: grid; gap: 8px; margin: 16px 0; }
  label { font-size: var(--text-sm); }
  input, textarea { border: 1px solid var(--edge); border-radius: 6px; background: var(--bg); color: var(--fg); padding: 8px; font: inherit; min-width: 0; }
  input { flex: 1; width: 100%; }
  textarea { width: 100%; box-sizing: border-box; font-family: var(--mono); resize: vertical; }
  .account-link { color: var(--accent); font-size: var(--text-sm); }
  summary { cursor: pointer; color: var(--muted); font-size: var(--text-sm); padding: 10px 0; }
  details p { margin-bottom: 12px; }
  .advanced { border-top: 1px solid var(--edge); }
  .error { margin: 0; color: var(--warn); font-size: var(--text-sm); line-height: 1.5; }
  @media (max-width: 520px) { .cloud { padding: 16px; } }
</style>
