<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { api } from "../net/api";
  import { workbenchPath } from "../net/base";
  import ProviderConnections from "../pro/ProviderConnections.svelte";
  import { cloudRequest } from "../pro/cloudTransport";
  import { pageVisible } from "../shared/visibility";
  import { cloudCopy, cloudPollDelay, cloudStages, friendlyError } from "../pro/presentation";
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
  let providerRevision = $state(0);
  let providerReady = $state<boolean | null>(null);
  let projects = $state<MirrorStatus | null>(null);
  let providerElement = $state<HTMLDivElement>();
  let refreshFlight: Promise<void> | null = null;
  const browser = !isNativeShell();
  const copy = $derived(status?.state === "ready" && info?.available !== true
    ? { title: connectionChecked ? "Cloud connection isn't ready" : "Checking your cloud connection…", detail: connectionChecked ? "Your cloud has started, but Chimaera couldn't reach it yet. Check again shortly. Your local work is available." : "Your cloud has started. We're checking that its workbench is reachable." }
    : status?.state === "ready" && info?.available ? { title: "Cloud machine connected", detail: "Agents connected here can keep working while this computer sleeps." } : cloudCopy(status?.state ?? "error", status?.reason ?? null, status?.phase));
  const connected = $derived(info?.available === true && (browser || status?.state === "ready"));
  const canSetUp = $derived(connected || !browser && status?.state === "sleeping");
  const preparing = $derived(!browser && (status?.state === "preparing" || status?.state === "ready" && !connected));
  const stages = $derived(cloudStages(connected, providerReady, projects, browser, workspaceId));

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
  function focusProviders(): void {
    providerElement?.scrollIntoView({ block: "start", behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
    providerElement?.focus({ preventScroll: true });
  }
  function providerStatus(ready: boolean | null): void { providerReady = ready; }
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
      if (request.operation === "start") { await refresh(undefined, true); if (alive && action === actionGeneration) providerRevision += 1; }
    } catch (reason) { if (alive && action === actionGeneration) error = friendlyError(reason, request.operation === "project" ? "This repository couldn't open in the cloud. Check the URL and your Git access, then try again." : "Your cloud connection couldn't open yet. Check its progress and try again shortly."); }
    finally { if (alive && action === actionGeneration) busy = null; }
  }
  async function copyKey(): Promise<void> {
    if (!info?.ssh_public_key) return;
    try { if (!await writeClipboard(info.ssh_public_key)) throw new Error("clipboard unavailable"); copied = true; } catch { error = "The public key couldn't be copied. You can select it below."; }
  }
</script>

<section class="cloud" aria-label="Cloud setup">
  <div class="machine">
    <div class="heading"><div><span class="eyebrow">Your cloud</span><h2>{browser ? connected ? "Cloud machine connected" : connectionChecked ? "Cloud connection unavailable" : "Checking your cloud connection…" : status ? copy.title : "Checking cloud readiness…"}</h2></div><button class="text-button" disabled={busy !== null || refreshing} onclick={() => void refresh()}>{refreshing ? "Checking…" : "Check again"}</button></div>
    <p class="hint" role="status">{browser ? connected ? "Agents connected here can keep working while your computer sleeps." : connectionChecked ? "This workbench has not confirmed an available cloud connection. Check again or open your account." : "Checking that this workbench is reachable." : status ? copy.detail : "This check doesn't wake a sleeping machine."}</p>
  </div>
  <ol class="stages" aria-label="Cloud setup progress">
    {#each stages as stage, index}
      <li class:complete={stage.state === "complete"} class:current={stage.state === "current"} aria-current={stage.state === "current" ? "step" : undefined}>
        <span class="stage-number" aria-hidden="true">{stage.state === "complete" ? "✓" : `0${index + 1}`}</span>
        <div><h3>{stage.title}</h3><p>{stage.detail}</p></div>
      </li>
    {/each}
  </ol>
  {#if preparing}
    <div class="next-step"><div><h3>Next, connect your agent</h3><p class="hint">This step opens once your cloud workbench is reachable. You can keep using your local projects while preparation continues.</p></div><button class="btn" disabled>Waiting for your cloud</button></div>
  {:else if connected && providerReady !== true}
    <div class="next-step"><p class="hint">Choose an agent below to finish connecting your cloud.</p><button class="btn" onclick={focusProviders}>Connect an agent</button></div>
  {/if}
  {#if !browser && (status?.state === "sleeping" || status?.state === "ready" && !info?.available)}<button class="btn" disabled={busy !== null} onclick={() => void act("start", { operation: "start" })}>{busy === "start" ? "Connecting…" : "Connect to cloud"}</button>{/if}
  {#if browser && !info?.available}<a class="account-link" href="/account">Open your account →</a>{/if}
  {#if canSetUp}
    <div class="provider-section" bind:this={providerElement} tabindex="-1">
      {#key providerRevision}<ProviderConnections {visible} {requiredProviders} {contextLabel} {workspaceId} {onReady} onReadiness={providerStatus} />{/key}
    </div>
    <details class="advanced" ontoggle={(event) => (advanced = event.currentTarget.open)}><summary>Repositories and advanced connections</summary>{#if advanced}<p class="hint">For a project that starts in the cloud, open a Git repository here. Private repositories may need the optional repository connection above.</p><form onsubmit={(event) => { event.preventDefault(); void act("project", { operation: "project", url: repository.trim() }); }}><label for="cloud-repository">Repository URL</label><div class="clone-row"><input id="cloud-repository" type="url" placeholder="https://github.com/you/project" bind:value={repository} required disabled={busy !== null} /><button class="btn" disabled={busy !== null || !repository.trim()}>{busy === "project" ? "Opening repository…" : "Open repository"}</button></div></form>{#if info?.ssh_public_key}<details><summary>SSH public key</summary><p class="hint">Use this public key only if your Git host or connection needs it.</p><textarea aria-label="Cloud SSH public key" readonly value={info.ssh_public_key} rows="3"></textarea><button class="btn" onclick={() => void copyKey()}>{copied ? "Copied" : "Copy public key"}</button></details>{/if}{/if}</details>
  {/if}
  {#if busy === "start"}<p class="hint" role="status">Connecting to your cloud machine…</p>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
</section>

<style>
  .cloud { container: cloud-setup / inline-size; min-width: 0; display: grid; gap: 25px; border: 1px solid var(--edge); border-radius: 10px; padding: 25px; margin: 22px 0; }
  .machine { display: grid; gap: 9px; }
  .eyebrow { display: block; color: var(--muted); font-size: var(--text-xs); letter-spacing: .06em; text-transform: uppercase; margin-bottom: 7px; }
  .stages { list-style: none; margin: 0; padding: 0; display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); border: 1px solid var(--edge); border-radius: 8px; overflow: hidden; }
  .stages li { min-width: 0; padding: 19px 16px; display: flex; align-items: flex-start; gap: 11px; color: var(--muted); }
  .stages li + li { border-left: 1px solid var(--edge); }
  .stages li.current { background: color-mix(in srgb, var(--accent) 5%, var(--bg)); color: var(--fg); }
  .stage-number { font-size: var(--text-xs); font-variant-numeric: tabular-nums; border: 1px solid var(--edge); border-radius: 50%; width: 26px; height: 26px; flex: none; display: grid; place-items: center; }
  .complete .stage-number { color: var(--accent); border-color: color-mix(in srgb, var(--accent) 45%, var(--edge)); }
  .current .stage-number { color: var(--accent); }
  h3 { margin: 2px 0 8px; font-size: var(--text-sm); font-weight: 600; color: inherit; }
  .stages p { margin: 0; font-size: var(--text-sm); line-height: 1.6; color: var(--muted); overflow-wrap: anywhere; }
  .next-step { display: flex; gap: 20px; justify-content: space-between; align-items: center; }
  .next-step .btn { flex: none; align-self: center; }
  .provider-section { border-top: 1px solid var(--edge); padding-top: 25px; scroll-margin-top: 20px; }
  .provider-section:focus { outline: none; }
  @container cloud-setup (max-width: 700px) { .stages { grid-template-columns: 1fr; } .stages li + li { border-left: 0; border-top: 1px solid var(--edge); } .stages li { padding: 15px; } .next-step { flex-direction: column; align-items: flex-start; gap: 12px; } .next-step .btn { align-self: flex-start; } }
  .heading { display: flex; justify-content: space-between; align-items: center; gap: 12px; flex-wrap: wrap; }
  .heading > div { flex: 1 1 230px; min-width: 0; }
  .heading .text-button { flex: none; white-space: nowrap; }
  .stages li > div { min-width: 0; }
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
