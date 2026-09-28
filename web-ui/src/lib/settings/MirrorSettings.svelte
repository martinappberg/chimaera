<script lang="ts">
  import { untrack } from "svelte";
  import { cloudOnboarding } from "../pro/onboarding.svelte";
  import { pageVisible } from "../shared/visibility";
  import { projectCopyError } from "../pro/presentation";
  import { proMirrorStatus, proSetNeverMirror, type MirrorStatus, type MirrorWorkspace } from "../net/native";
  let { visible = true, recoveryOnly = false }: { visible?: boolean; recoveryOnly?: boolean } = $props();
  let status = $state<MirrorStatus | null>(null);
  let error = $state<string | null>(null);
  let busy = $state<string | null>(null);
  let revision = 0;
  async function load(): Promise<void> {
    const current = ++revision;
    try { const next = await proMirrorStatus(); if (current === revision) { status = next; error = null; } }
    catch { if (current === revision) error = "Project status couldn’t refresh. Your saved work and privacy choices haven’t changed."; }
  }
  $effect(() => {
    if (!visible || !$pageVisible) return;
    untrack(() => void load());
    const timer = setInterval(() => void load(), 15000);
    return () => { clearInterval(timer); revision += 1; };
  });
  async function act(id: string, action: () => Promise<void>): Promise<void> {
    if (busy !== null) return;
    busy = id; error = null;
    try { await action(); await load(); }
    catch { error = "This change couldn’t be confirmed. Try again before changing another project setting."; }
    finally { busy = null; }
  }
  function privacy(workspace: MirrorWorkspace, checkbox: HTMLInputElement): void {
    const value = checkbox.checked; checkbox.checked = workspace.never_mirror;
    void act(workspace.workspace_id, () => proSetNeverMirror(workspace.workspace_id, value));
  }
  function place(workspace: MirrorWorkspace): string {
    if (workspace.never_mirror) return "Only on this device";
    switch (workspace.ownership?.state) {
      case "local": return "On this device";
      case "remote": return "Continuing in the cloud";
      case "transferring": return "Preparing to continue in the cloud…";
      case "privacy_disabled": return "Automatic copying is off";
      case "setting_up": return workspace.blocked_providers?.length ? "Waiting for cloud agent sign-in" : "Cloud project setup needs attention";
      case "hydrating": return "Restoring files and conversations…";
      case "awaiting_verification": return "Checking where this project is running…";
      default: return "Waiting for the first copy";
    }
  }
</script>
<div class="mirrors">
  <h3>Project copies</h3>
  {#if !recoveryOnly}<p class="hint">Your files, conversations and supported agent settings stay together across devices. Passwords, private keys and ignored files stay on this device.</p>{:else}<p class="hint">You can still manage project privacy without an active plan.</p>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if !recoveryOnly && status && !status.configured}<p class="hint">Automatic project copying is getting ready. You can keep working here.</p>{/if}
  {#if status?.workspaces.length === 0}<p class="hint">No project copies to show yet.</p>{/if}
  {#each status?.workspaces ?? [] as workspace (workspace.workspace_id)}
    <details>
      <summary><span class="title">{workspace.name}</span><span class="hint">{place(workspace)}</span></summary>
      <div class="project">
        <p class="path">{workspace.root}</p>
        <label class="check"><input type="checkbox" checked={workspace.never_mirror} disabled={busy !== null || (recoveryOnly && workspace.never_mirror)} onchange={(event) => privacy(workspace,event.currentTarget)} />Keep this project on this device</label>
        {#if workspace.never_mirror && !workspace.privacy_pending}<p class="hint">Automatic copying and cloud access are off. Existing saved copies haven’t been deleted.</p>{/if}
        {#if workspace.privacy_pending}<p class="error" role="status">Copying from this device has stopped. Cloud privacy is still pending; retry to disable it everywhere.</p><button class="btn" disabled={busy !== null} onclick={() => void act(workspace.workspace_id, () => proSetNeverMirror(workspace.workspace_id, true))}>Retry cloud privacy</button>{/if}
        {#if workspace.git_branches?.length}<p class="hint">Cloud changes are saved in {workspace.git_branches.join(", ")} for you to merge.</p>{/if}
        {#if !recoveryOnly && workspace.blocked_providers?.length}
          <div class="connection-needed"><p class="hint">Connect the agents this project uses so it can continue automatically.</p><button class="btn" onclick={() => cloudOnboarding.request({ providerIds: workspace.blocked_providers!.map(provider => provider.id), workspaceId: workspace.workspace_id, workspaceName: workspace.name })}>Connect agents to continue</button></div>
        {/if}
        {#if workspace.mirror}
          {#if workspace.mirror.last_mirrored_at}<p class="hint">Last copied {new Date(workspace.mirror.last_mirrored_at * 1000).toLocaleString()}</p>{/if}
          {#if workspace.mirror.too_large}<p class="error" role="status">Some files are too large to include in the cloud copy. They remain available on this device.</p>{/if}
          {#if workspace.mirror.error && workspace.mirror.error !== "cloud_provider_not_ready"}<p class="error" role="status">{projectCopyError(workspace.mirror.error)}</p>{/if}
        {/if}
      </div>
    </details>
  {/each}
</div>
<style>
  .mirrors { border-top: 1px solid var(--edge); padding: 18px 14px 0; margin-top: 18px; }
  h3 { font-size: 13px; margin: 0 0 8px; }
  .hint { color: var(--muted); font-size: 12px; line-height: 1.55; }
  details { border: 1px solid var(--edge); border-radius: 8px; margin: 10px 0; }
  summary { cursor: pointer; padding: 12px; }
  .title { font-weight: 600; margin-right: 12px; }
  .project { padding: 0 12px 14px; display: flex; flex-direction: column; gap: 10px; }
  .path { font-family: var(--font-mono); font-size: 11px; color: var(--muted); overflow-wrap: anywhere; margin: 0; }
  .check { display: flex; gap: 8px; align-items: center; font-size: 12px; }
  .btn { align-self: flex-start; border: 1px solid var(--edge); border-radius: 5px; background: var(--term-bg); color: var(--fg); padding: 6px 10px; cursor: pointer; }
  .btn:disabled { opacity: .5; cursor: default; }
  .error { color: var(--warn); font-size: 12px; overflow-wrap: anywhere; }
</style>
