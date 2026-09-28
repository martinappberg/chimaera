<script lang="ts">
  import { untrack } from "svelte";
  import { api } from "../net/api";
  import { isBrowserGateway, workbenchPath } from "../net/base";
  import { pageVisible } from "../shared/visibility";
  import { cloudCopy, friendlyError } from "../pro/presentation";
  import { connectHost, openWindow, proCloudRequest, proCloudStatus, writeClipboard, type CloudSetupInfo, type CloudSetupRequest, type CloudProvisioningStatus } from "../net/native";

  let { visible = true }: { visible?: boolean } = $props();
  let info = $state<CloudSetupInfo | null>(null);
  let status = $state<CloudProvisioningStatus | null>(null);
  let busy = $state<string | null>(null);
  let error = $state<string | null>(null);
  let repository = $state("");
  let copied = $state(false);
  let advanced = $state(false);
  let generation = 0;
  let connectionChecked = $state(false);
  const browser = isBrowserGateway();
  const copy = $derived(status?.state === "ready" && info?.available !== true
    ? { title: connectionChecked ? "Cloud connection isn't ready" : "Checking your cloud connection…", detail: connectionChecked ? "Your cloud has started, but Chimaera couldn't reach it yet. Check again shortly. Your local work is available." : "Your cloud has started. We're checking that its workbench is reachable." }
    : cloudCopy(status?.state ?? "error", status?.reason ?? null));
  const canSetUp = $derived(browser ? info?.available === true : status?.state === "ready" || status?.state === "sleeping");

  async function cloudRequest(request: CloudSetupRequest): Promise<CloudSetupInfo> {
    if (!browser) return proCloudRequest(request);
    const read = request.operation === "info" || request.operation === "start";
    const path = read ? "/pro/cloud" : `/pro/cloud/${request.operation}`;
    const headers: Record<string, string> = {};
    if (request.operation !== "info") headers["X-Chimaera-Wake"] = "interaction";
    if (!read) headers["Content-Type"] = "application/json";
    const response = await api(path, { method: read ? "GET" : "POST", headers, body: read ? undefined : JSON.stringify(request), signal: AbortSignal.timeout(request.operation === "project" ? 310_000 : 95_000) });
    if (!response.ok) throw new Error("Cloud operation is unavailable");
    return await response.json() as CloudSetupInfo;
  }
  async function refresh(): Promise<void> {
    const current = ++generation;
    try {
      if (browser) {
        const result = await cloudRequest({ operation: "info" });
        if (current === generation) info = result;
      } else {
        const result = await proCloudStatus();
        if (current !== generation) return;
        status = result;
        // Reading readiness must never wake a suspended machine.
        if (result.state === "ready") {
          const details = await cloudRequest({ operation: "info" }).catch(() => null);
          if (current === generation) { info = details; connectionChecked = true; }
        }
      }
      if (current === generation) error = null;
    } catch {
      if (current === generation) { status = { state: "error", reason: null }; error = "Cloud readiness couldn't refresh. Your local work is still available."; }
    }
  }
  $effect(() => {
    if (!visible || !$pageVisible) return;
    untrack(() => void refresh());
    const timer = setInterval(() => void refresh(), 30_000);
    return () => { generation += 1; clearInterval(timer); };
  });
  async function act(label: string, request: CloudSetupRequest): Promise<void> {
    if (busy !== null) return;
    busy = label; error = null;
    try {
      const result = await cloudRequest(request);
      info = result;
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
      if (request.operation === "project") repository = "";
    } catch (reason) { error = friendlyError(reason, request.operation === "project" ? "This repository couldn't open in the cloud. Check the URL and your Git access, then try again." : "The sign-in terminal couldn't open. Please try again shortly."); }
    finally { busy = null; }
  }
  async function copyKey(): Promise<void> {
    if (!info?.ssh_public_key) return;
    try { if (!await writeClipboard(info.ssh_public_key)) throw new Error("clipboard unavailable"); copied = true; } catch { error = "The public key couldn't be copied. You can select it below."; }
  }
</script>

<section class="cloud" aria-label="Cloud readiness">
  <div class="heading"><h2>{browser ? "Your cloud" : status ? copy.title : "Checking cloud readiness…"}</h2><button class="text-button" disabled={busy !== null} onclick={() => void refresh()}>Refresh</button></div>
  <p class="hint">{browser ? "Cloud placement and handoff happen automatically. Provider sign-ins below authorize this cloud machine." : status ? copy.detail : "This check doesn't wake a sleeping machine."}</p>
  {#if browser && !info?.available}<a class="account-link" href="/account">Open your account →</a>{/if}
  {#if canSetUp}
    <details class="providers"><summary>Use your agents in the cloud</summary><p class="hint">If you haven't signed in on this machine yet, connect the provider you want to use. Each action opens its own sign-in terminal. Your subscriptions stay yours.</p><div class="actions"><button class="btn" disabled={busy !== null} onclick={() => void act("claude", { operation: "onboard", agent: "claude" })}>Sign in to Claude</button><button class="btn" disabled={busy !== null} onclick={() => void act("codex", { operation: "onboard", agent: "codex" })}>Sign in to Codex</button></div></details>
    <details class="advanced" ontoggle={(event) => (advanced = event.currentTarget.open)}><summary>Repositories and advanced connections</summary>{#if advanced}<p class="hint">For a project that starts in the cloud, open a Git repository here. Private GitHub repositories need GitHub sign-in on this machine.</p><button class="btn" disabled={busy !== null} onclick={() => void act("github", { operation: "onboard", agent: "github" })}>Connect GitHub</button><form onsubmit={(event) => { event.preventDefault(); void act("project", { operation: "project", url: repository.trim() }); }}><label for="cloud-repository">Repository URL</label><div class="clone-row"><input id="cloud-repository" type="url" placeholder="https://github.com/you/project" bind:value={repository} required disabled={busy !== null} /><button class="btn" disabled={busy !== null || !repository.trim()}>{busy === "project" ? "Opening repository…" : "Open repository"}</button></div></form>{#if info?.ssh_public_key}<details><summary>SSH public key</summary><p class="hint">Use this public key only if your Git host or connection needs it.</p><textarea aria-label="Cloud SSH public key" readonly value={info.ssh_public_key} rows="3"></textarea><button class="btn" onclick={() => void copyKey()}>{copied ? "Copied" : "Copy public key"}</button></details>{/if}{/if}</details>
  {/if}
  {#if busy !== null && busy !== "project"}<p class="hint" role="status">Opening the provider's sign-in terminal…</p>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
</section>

<style>
  .cloud { display: grid; gap: 12px; border: 1px solid var(--edge); border-radius: 12px; padding: 20px; margin: 18px 0; }
  .heading { display: flex; justify-content: space-between; align-items: center; gap: 12px; }
  h2 { margin: 0; font-size: var(--text-lg); font-weight: 600; }
  .hint { margin: 0; color: var(--muted); font-size: var(--text-sm); line-height: 1.6; }
  .actions, .clone-row { display: flex; gap: 8px; flex-wrap: wrap; margin-top: 12px; }
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
  .providers, .advanced { border-top: 1px solid var(--edge); }
  .error { margin: 0; color: var(--warn); font-size: var(--text-sm); line-height: 1.5; }
  @media (max-width: 520px) { .cloud { padding: 16px; } }
</style>
