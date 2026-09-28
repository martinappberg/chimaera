<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { api } from "../net/api";
  import { workbenchPath } from "../net/base";
  import ProviderConnections from "../pro/ProviderConnections.svelte";
  import { cloudRequest } from "../pro/cloudTransport";
  import { pageVisible } from "../shared/visibility";
  import { cloudCopy, cloudPollDelay, cloudProjectStatus, friendlyError } from "../pro/presentation";
  import { connectHost, openWindow, isNativeShell, proCloudStatus, proMirrorStatus, writeClipboard, type MirrorStatus, type CloudSetupInfo, type CloudSetupRequest, type CloudProvisioningStatus } from "../net/native";

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
  let refreshing = $state(false);
  let actionGeneration = 0;
  let connectionChecked = $state(false);
  let providersMounted = $state(false);
  let projects = $state<MirrorStatus | null>(null);
  let refreshFlight: Promise<void> | null = null;
  const browser = !isNativeShell();
  const copy = $derived(status?.state === "ready" && info?.available !== true
    ? { title: connectionChecked ? "Cloud connection isn't ready" : "Checking your cloud connection…", detail: connectionChecked ? "Your cloud has started, but Chimaera couldn't reach it yet. Check again shortly. Your local work is available." : "Your cloud has started. We're checking that its workbench is reachable." }
    : status?.state === "ready" && info?.available ? { title: "Your cloud is ready", detail: "Agents connected here can keep working while this computer sleeps." } : cloudCopy(status?.state ?? "error", status?.reason ?? null, status?.phase));
  const connected = $derived(info?.available === true && (browser || status?.state === "ready"));
  $effect(() => { if (connected) providersMounted = true; });
  const preparing = $derived(!browser && (status?.state === "preparing" || status?.state === "ready" && !connected));
  const projectStatus = $derived(connected ? cloudProjectStatus(projects, workspaceId) : null);
  const needsCheck = $derived(browser ? connectionChecked && !connected : status?.state === "error" || status?.state === "unavailable" || status?.state === "ready" && connectionChecked && !connected);

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
    refreshing = true;
    const task = (async () => {
      try {
        let reachable = false;
        if (browser) {
          const result = await cloudRequest({ operation: "info" }, signal);
          if (!alive || signal?.aborted || current !== generation) return;
          info = result; connectionChecked = true;
          reachable = result.available === true;
        } else {
          const result = await proCloudStatus();
          if (!alive || signal?.aborted || current !== generation) return;
          status = result;
          // Passive status and metadata reads never wake a suspended machine.
          if (result.state === "ready") {
            const details = await cloudRequest({ operation: "info" }, signal).catch(() => null);
            if (!alive || signal?.aborted || current !== generation) return;
            info = details;
            connectionChecked = true;
            reachable = details?.available === true;
          } else { info = null; connectionChecked = false; }
        }
        const nextProjects = reachable ? await readProjects(signal) : null;
        if (alive && !signal?.aborted && current === generation) { projects = nextProjects; error = null; }
      } catch {
        if (alive && !signal?.aborted && current === generation) {
          status = { state: "error", reason: null }; info = null; projects = null; connectionChecked = true;
          error = "Cloud readiness couldn't refresh. Your local work is still available.";
        }
      } finally { if (alive) refreshing = false; }
    })();
    refreshFlight = task;
    void task.finally(() => { if (refreshFlight === task) refreshFlight = null; });
    return task;
  }
  $effect(() => {
    if (!visible || !$pageVisible) return;
    const controller = new AbortController();
    const started = Date.now();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const poll = async () => {
      await refresh(controller.signal);
      if (controller.signal.aborted) return;
      // Read after the response, outside reactive tracking: status replacement
      // must not restart an immediate polling loop. Slow after five minutes.
      timer = setTimeout(() => void poll(), cloudPollDelay(preparing, Date.now() - started));
    };
    untrack(() => void poll());
    return () => { generation += 1; controller.abort(); clearTimeout(timer); };
  });
  onDestroy(() => { alive = false; generation += 1; actionGeneration += 1; });
  async function act(label: string, request: CloudSetupRequest): Promise<void> {
    if (busy !== null) return;
    const action = ++actionGeneration;
    busy = label; error = null;
    if (request.operation === "start") generation += 1;
    try {
      const result = await cloudRequest(request);
      if (!alive || action !== actionGeneration) return;
      if (request.operation === "start") generation += 1;
      if (result.available !== undefined) info = result;
      if (result.workspace_id) {
        if (browser) {
          const params = new URLSearchParams({ ws: result.workspace_id, win: `w-${crypto.randomUUID()}` });
          location.assign(`${workbenchPath()}#${params}`);
          location.reload();
        } else if (result.host_alias) {
          await connectHost(result.host_alias);
          await openWindow(result.host_alias, result.workspace_id, true);
        }
      }
      if (!alive || action !== actionGeneration) return;
      if (request.operation === "project") repository = "";
      if (request.operation === "start") await refresh(undefined, true);
    } catch (reason) { if (alive && action === actionGeneration) error = friendlyError(reason, request.operation === "project" ? "This repository couldn't open in the cloud. Check the URL and your Git access, then try again." : "Your cloud connection couldn't open yet. Check its progress and try again shortly."); }
    finally { if (alive && action === actionGeneration) busy = null; }
  }
  async function copyKey(): Promise<void> {
    if (!info?.ssh_public_key) return;
    try { if (!await writeClipboard(info.ssh_public_key)) throw new Error("clipboard unavailable"); copied = true; } catch { error = "The public key couldn't be copied. You can select it below."; }
  }
</script>

<section class="cloud" aria-label="Cloud status">
  <div class="machine" class:attention={needsCheck}>
    <div class="heading"><div><span class="eyebrow">Your cloud</span><h2>{browser ? connected ? "Your cloud is ready" : connectionChecked ? "Cloud connection unavailable" : "Checking your cloud connection…" : status ? copy.title : "Checking cloud readiness…"}</h2></div><span class="status-mark" class:connected class:preparing aria-hidden="true">{#if connected}<svg viewBox="0 0 24 24"><path d="m6 12 4 4 8-8" /></svg>{:else}<svg viewBox="0 0 24 24"><path d="M7 17a4 4 0 0 1-1-7.9 6 6 0 0 1 11.4-1.5A4.7 4.7 0 0 1 18 17H7Z" /></svg>{/if}</span></div>
    <p class="hint" role="status">{browser ? connected ? "Agents connected here can keep working while your computer sleeps." : connectionChecked ? "This workbench has not confirmed an available cloud connection. Your local work is still available." : "Checking that this workbench is reachable." : status ? copy.detail : "This check doesn't wake a sleeping machine."}</p>
    {#if preparing}<p class="automatic">Preparation continues automatically. You can keep working here.</p>{/if}
    {#if needsCheck}<div class="recovery"><button class="text-button" disabled={busy !== null || refreshing} onclick={() => void refresh()}>{refreshing ? "Checking…" : "Check again"}</button>{#if browser}<a class="account-link" href="/account">Open your account →</a>{/if}</div>{/if}
  </div>
  {#if projectStatus}
    <div class="project-status" class:attention={projectStatus.state === "attention"} role="status"><span class="project-dot" class:active={projectStatus.state === "active"} aria-hidden="true"></span><div><h3>{projectStatus.title}</h3><p class="hint">{projectStatus.detail}</p></div></div>
  {/if}
  {#if !browser && status?.state === "sleeping"}<details class="sleeping-connections" ontoggle={(event) => { if (event.currentTarget.open) void act("connections", { operation: "start" }); }}><summary>Agent connections</summary><p class="hint">{busy === "connections" ? "Checking your agent connections… Your cloud is opening automatically for this request." : "Close and reopen this section to check your connections again."}</p></details>{/if}
  {#if providersMounted}
    <div class="provider-section" hidden={!connected}>
      <ProviderConnections visible={visible && connected} {requiredProviders} {contextLabel} {workspaceId} {onReady} compact />
    </div>
  {/if}
  {#if connected}
    <details class="advanced" ontoggle={(event) => (advanced = event.currentTarget.open)}><summary>Repositories and advanced connections</summary>{#if advanced}<p class="hint">For a project that starts in the cloud, open a Git repository here. Private repositories may need the optional repository connection above.</p><form onsubmit={(event) => { event.preventDefault(); void act("project", { operation: "project", url: repository.trim() }); }}><label for="cloud-repository">Repository URL</label><div class="clone-row"><input id="cloud-repository" type="url" placeholder="https://github.com/you/project" bind:value={repository} required disabled={busy !== null} /><button class="btn" disabled={busy !== null || !repository.trim()}>{busy === "project" ? "Opening repository…" : "Open repository"}</button></div></form>{#if info?.ssh_public_key}<details><summary>SSH public key</summary><p class="hint">Use this public key only if your Git host or connection needs it.</p><textarea aria-label="Cloud SSH public key" readonly value={info.ssh_public_key} rows="3"></textarea><button class="btn" onclick={() => void copyKey()}>{copied ? "Copied" : "Copy public key"}</button></details>{/if}{/if}</details>
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
  .automatic { margin: 2px 0 0; color: var(--muted); font-size: var(--text-xs); line-height: 1.6; }
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
  textarea { width: 100%; box-sizing: border-box; font-family: monospace; resize: vertical; }
  .account-link { color: var(--accent); font-size: var(--text-sm); }
  summary { cursor: pointer; color: var(--muted); font-size: var(--text-sm); padding: 10px 0; }
  details p { margin-bottom: 12px; }
  .advanced { border-top: 1px solid var(--edge); }
  .error { margin: 0; color: var(--warn); font-size: var(--text-sm); line-height: 1.5; }
  @media (max-width: 520px) { .cloud { padding: 16px; } }
</style>
