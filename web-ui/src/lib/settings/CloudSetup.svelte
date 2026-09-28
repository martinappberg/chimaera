<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { workbenchPath } from "../net/base";
  import ProviderConnections from "../pro/ProviderConnections.svelte";
  import { cloudRequest } from "../pro/cloudTransport";
  import { pageVisible } from "../shared/visibility";
  import { cloudCopy, friendlyError } from "../pro/presentation";
  import { connectHost, openWindow, isNativeShell, proCloudStatus, writeClipboard, type CloudSetupInfo, type CloudSetupRequest, type CloudProvisioningStatus } from "../net/native";

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
  const browser = !isNativeShell();
  const copy = $derived(status?.state === "ready" && info?.available !== true
    ? { title: connectionChecked ? "Cloud connection isn't ready" : "Checking your cloud connection…", detail: connectionChecked ? "Your cloud has started, but Chimaera couldn't reach it yet. Check again shortly. Your local work is available." : "Your cloud has started. We're checking that its workbench is reachable." }
    : status?.state === "ready" && info?.available ? { title: "Cloud machine connected", detail: "Agents connected here can keep working while this computer sleeps." } : cloudCopy(status?.state ?? "error", status?.reason ?? null));
  const canSetUp = $derived(browser ? info?.available === true : info?.available === true && status?.state === "ready" || status?.state === "sleeping");

  async function refresh(signal?: AbortSignal): Promise<void> {
    if (refreshing) return;
    refreshing = true;
    const current = ++generation;
    try {
      if (browser) {
        const result = await cloudRequest({ operation: "info" }, signal);
        if (alive && !signal?.aborted && current === generation) info = result;
      } else {
        const result = await proCloudStatus();
        if (!alive || signal?.aborted || current !== generation) return;
        status = result;
        // Reading readiness must never wake a suspended machine.
        if (result.state === "ready") {
          const details = await cloudRequest({ operation: "info" }, signal).catch(() => null);
          if (alive && !signal?.aborted && current === generation) { info = details; connectionChecked = true; }
        }
      }
      if (alive && !signal?.aborted && current === generation) error = null;
    } catch {
      if (alive && !signal?.aborted && current === generation) { status = { state: "error", reason: null }; error = "Cloud readiness couldn't refresh. Your local work is still available."; }
    } finally { if (alive) refreshing = false; }
  }
  $effect(() => {
    if (!visible || !$pageVisible) return;
    const controller = new AbortController();
    untrack(() => void refresh(controller.signal));
    const timer = setInterval(() => void refresh(controller.signal), 30_000);
    return () => { generation += 1; controller.abort(); clearInterval(timer); };
  });
  onDestroy(() => { alive = false; generation += 1; actionGeneration += 1; });
  async function act(label: string, request: CloudSetupRequest): Promise<void> {
    if (busy !== null) return;
    const action = ++actionGeneration;
    busy = label; error = null;
    try {
      const result = await cloudRequest(request);
      if (!alive || action !== actionGeneration) return;
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
      if (request.operation === "start") { await refresh(); if (alive && action === actionGeneration) providerRevision += 1; }
    } catch (reason) { if (alive && action === actionGeneration) error = friendlyError(reason, request.operation === "project" ? "This repository couldn't open in the cloud. Check the URL and your Git access, then try again." : "The sign-in terminal couldn't open. Please try again shortly."); }
    finally { if (alive && action === actionGeneration) busy = null; }
  }
  async function copyKey(): Promise<void> {
    if (!info?.ssh_public_key) return;
    try { if (!await writeClipboard(info.ssh_public_key)) throw new Error("clipboard unavailable"); copied = true; } catch { error = "The public key couldn't be copied. You can select it below."; }
  }
</script>

<section class="cloud" aria-label="Cloud setup">
  <div class="machine">
  <div class="heading"><h2>{browser ? "Your cloud" : status ? copy.title : "Checking cloud readiness…"}</h2><button class="text-button" disabled={busy !== null || refreshing} onclick={() => void refresh()}>Refresh</button></div>
  <p class="hint">{browser ? "This is your cloud machine. Agent connections below authorize your provider account here." : status ? copy.detail : "This check doesn't wake a sleeping machine."}</p>
  {#if !browser && (status?.state === "sleeping" || status?.state === "ready" && !info?.available)}<button class="btn" disabled={busy !== null} onclick={() => void act("start", { operation: "start" })}>{busy === "start" ? "Connecting…" : "Connect to cloud"}</button>{/if}
  {#if browser && !info?.available}<a class="account-link" href="/account">Open your account →</a>{/if}
  </div>
  {#if canSetUp}
    {#key providerRevision}<ProviderConnections {visible} {requiredProviders} {contextLabel} {workspaceId} {onReady} />{/key}
    <details class="advanced" ontoggle={(event) => (advanced = event.currentTarget.open)}><summary>Repositories and advanced connections</summary>{#if advanced}<p class="hint">For a project that starts in the cloud, open a Git repository here. Private repositories may need the optional repository connection above.</p><form onsubmit={(event) => { event.preventDefault(); void act("project", { operation: "project", url: repository.trim() }); }}><label for="cloud-repository">Repository URL</label><div class="clone-row"><input id="cloud-repository" type="url" placeholder="https://github.com/you/project" bind:value={repository} required disabled={busy !== null} /><button class="btn" disabled={busy !== null || !repository.trim()}>{busy === "project" ? "Opening repository…" : "Open repository"}</button></div></form>{#if info?.ssh_public_key}<details><summary>SSH public key</summary><p class="hint">Use this public key only if your Git host or connection needs it.</p><textarea aria-label="Cloud SSH public key" readonly value={info.ssh_public_key} rows="3"></textarea><button class="btn" onclick={() => void copyKey()}>{copied ? "Copied" : "Copy public key"}</button></details>{/if}{/if}</details>
  {/if}
  {#if busy === "start"}<p class="hint" role="status">Connecting to your cloud machine…</p>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
</section>

<style>
  .cloud { display: grid; gap: 25px; border: 1px solid var(--edge); border-radius: 10px; padding: 25px; margin: 22px 0; }
  .machine { display: grid; gap: 9px; padding-bottom: 21px; border-bottom: 1px solid var(--edge); }
  .heading { display: flex; justify-content: space-between; align-items: center; gap: 12px; }
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
