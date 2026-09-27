<script lang="ts">
  import { untrack } from "svelte";
  import { api } from "../net/api";
  import { isBrowserGateway, workbenchPath } from "../net/base";
  import { pageVisible } from "../shared/visibility";
  import { connectHost, openWindow, proCloudRequest, writeClipboard, type CloudSetupInfo, type CloudSetupRequest } from "../net/native";

  let { visible = true }: { visible?: boolean } = $props();
  let info = $state<CloudSetupInfo | null>(null);
  let busy = $state<string | null>(null);
  let error = $state<string | null>(null);
  let repository = $state("");
  let copied = $state(false);
  const browser = isBrowserGateway();
  async function cloudRequest(request: CloudSetupRequest): Promise<CloudSetupInfo> {
    if (!browser) return proCloudRequest(request);
    const read = request.operation === "info" || request.operation === "start";
    const path = read ? "/pro/cloud" : `/pro/cloud/${request.operation}`;
    const headers: Record<string, string> = {};
    if (request.operation !== "info") headers["X-Chimaera-Wake"] = "interaction";
    if (!read) headers["Content-Type"] = "application/json";
    const response = await api(path, {
      method: read ? "GET" : "POST", headers,
      body: read ? undefined : JSON.stringify(request),
      signal: AbortSignal.timeout(request.operation === "project" ? 310_000 : 95_000),
    });
    const result = await response.json();
    if (!response.ok) throw new Error(result.error ?? "Cloud machine is unavailable. Try waking it from your account.");
    return result as CloudSetupInfo;
  }
  $effect(() => {
    if (!visible || !$pageVisible) return;
    let cancelled = false;
    untrack(() => { void cloudRequest({ operation: "info" }).then(value => { if (!cancelled) info = value; }).catch(() => {}); });
    return () => { cancelled = true; };
  });
  async function act(label: string, request: CloudSetupRequest): Promise<void> {
    if (busy !== null) return;
    busy = label;
    error = null;
    try {
      const result = await cloudRequest(request);
      if (request.operation === "start") info = result;
      if (result.workspace_id) {
        if (browser) {
          // Keep the host pinned in this tab and avoid popup blockers after an
          // asynchronous clone or login request.
          const params = new URLSearchParams({ ws: result.workspace_id, win: `w-${crypto.randomUUID()}` });
          location.assign(`${workbenchPath()}#${params}`);
          location.reload();
        } else if (result.host_alias) {
          await connectHost(result.host_alias);
          await openWindow(result.host_alias, result.workspace_id, true);
        }
      }
      if (request.operation === "project") repository = "";
    } catch (reason) { error = reason instanceof Error ? reason.message : String(reason); }
    finally { busy = null; }
  }
  async function copyKey(): Promise<void> {
    if (!info?.ssh_public_key) return;
    try { if (!await writeClipboard(info.ssh_public_key)) throw new Error("Could not copy the public key"); copied = true; }
    catch (reason) { error = String(reason); }
  }
</script>

<div class="cloud">
  <h3>Cloud machine</h3>
  <p class="hint">Continue work while your computer is asleep. Cloud time counts while the machine is awake.</p>
  {#if browser}
    <a class="account-link" href="/workbench">Open account and cloud machines</a>
  {/if}
  {#if !browser || info?.available}
  <button class="btn" disabled={busy !== null} onclick={() => void act("start", { operation: "start" })}>
    {busy === "start" ? "Starting cloud machine…" : info?.available ? "Wake cloud machine" : "Set up cloud machine"}
  </button>
  {/if}
  {#if info?.available}
    <p class="hint">Sign in with your own agent subscriptions. Each button opens the provider's login in a cloud terminal.</p>
    <div class="actions">
      <button class="btn" disabled={busy !== null} onclick={() => void act("claude", { operation: "onboard", agent: "claude" })}>Sign in to Claude</button>
      <button class="btn" disabled={busy !== null} onclick={() => void act("codex", { operation: "onboard", agent: "codex" })}>Sign in to Codex</button>
      <button class="btn" disabled={busy !== null} onclick={() => void act("github", { operation: "onboard", agent: "github" })}>Connect GitHub</button>
    </div>
    {#if info.ssh_public_key}
      <label class="key-label" for="cloud-ssh-key">Cloud SSH public key</label>
      <textarea id="cloud-ssh-key" readonly value={info.ssh_public_key} rows="3"></textarea>
      <button class="btn" onclick={() => void copyKey()}>{copied ? "Copied" : "Copy public key"}</button>
    {/if}
    <form onsubmit={(event) => { event.preventDefault(); void act("project", { operation: "project", url: repository.trim() }); }}>
      <label for="cloud-repository">Open a Git repository in the cloud</label>
      <div class="clone-row">
        <input id="cloud-repository" type="url" placeholder="https://github.com/you/project" bind:value={repository} required disabled={busy !== null} />
        <button class="btn" disabled={busy !== null || !repository.trim()}>{busy === "project" ? "Cloning…" : "Clone and open"}</button>
      </div>
    </form>
  {/if}
  {#if busy !== null && busy !== "start" && busy !== "project"}<p class="hint" role="status">Opening the login terminal…</p>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
</div>

<style>
  .cloud { display: grid; gap: 12px; border-top: 1px solid var(--edge); padding-top: 20px; margin-top: 4px; }
  h3 { margin: 0; font-size: 13px; font-weight: 600; }
  .hint { margin: 0; color: var(--muted); font-size: 12px; line-height: 1.6; }
  .actions, .clone-row { display: flex; gap: 8px; flex-wrap: wrap; }
  .btn { justify-self: start; border: 1px solid var(--edge); border-radius: 6px; color: var(--fg); background: var(--bg); padding: 7px 10px; font-size: 12px; cursor: pointer; }
  .btn:hover:not(:disabled) { border-color: var(--accent); }
  .btn:disabled { opacity: .55; cursor: default; }
  form { display: grid; gap: 8px; margin-top: 4px; }
  label { color: var(--fg); font-size: 12px; }
  input, textarea { border: 1px solid var(--edge); border-radius: 6px; background: var(--bg); color: var(--fg); padding: 8px; font: inherit; font-size: 12px; min-width: 0; }
  input { flex: 1; min-width: 170px; }
  textarea { width: 100%; box-sizing: border-box; font-family: monospace; resize: vertical; }
  .account-link { color: var(--accent); font-size: 12px; }
  .error { margin: 0; color: var(--danger); font-size: 12px; line-height: 1.5; }
</style>
