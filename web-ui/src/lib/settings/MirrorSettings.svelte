<script lang="ts">
  import { untrack } from "svelte";
  import { pageVisible } from "../shared/visibility";
  import { proMirrorStatus, proSetNeverMirror, proMirrorPreference, type MirrorStatus, type MirrorWorkspace, type MirrorProfile } from "../net/native";
  let { visible = true, recoveryOnly = false }: { visible?: boolean; recoveryOnly?: boolean } = $props();
  let status = $state<MirrorStatus | null>(null);
  let error = $state<string | null>(null);
  let busy = $state<string | null>(null);
  let drafts = $state<Record<string,string>>({});
  let revision = 0;
  async function load(): Promise<void> {
    const current = ++revision;
    try { const next = await proMirrorStatus(); if (current === revision) { status = next; error = null; } }
    catch (reason) { if (current === revision) error = String(reason); }
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
    catch (reason) { error = String(reason); }
    finally { busy = null; }
  }
  function privacy(workspace: MirrorWorkspace, checkbox: HTMLInputElement): void {
    const value = checkbox.checked; checkbox.checked = workspace.never_mirror;
    void act(workspace.workspace_id, () => proSetNeverMirror(workspace.workspace_id, value));
  }
  function saveProfile(workspace: MirrorWorkspace): Promise<void> {
    const profile: MirrorProfile = workspace.profile ?? { setup_command: null, laptop_only: [], deferred: [], missing_environment: [] };
    return proMirrorPreference({ operation: "profile", workspace_id: workspace.workspace_id, profile: { ...profile, setup_command: (drafts[workspace.workspace_id] ?? profile.setup_command ?? "").trim() || null } });
  }
  function bytes(value: number): string {
    return value >= 1e9 ? `${(value/1e9).toFixed(1)} GB` : value >= 1e6 ? `${(value/1e6).toFixed(1)} MB` : `${Math.round(value/1000)} KB`;
  }
  function place(workspace: MirrorWorkspace): string {
    if (workspace.never_mirror) return "Stays on this laptop";
    switch (workspace.ownership?.state) {
      case "local": return "Running on this laptop";
      case "remote": return "Running on your cloud machine";
      case "transferring": return "Moving to your cloud machine…";
      case "privacy_disabled": return "Mirroring is disabled";
      case "hydrating": return "Restoring files and conversations…";
      case "awaiting_verification": return "Checking where this project is running…";
      default: return "Waiting for the first mirror";
    }
  }
</script>
<div class="mirrors">
  <h3>Project mirrors</h3>
  {#if !recoveryOnly}<p class="hint">Open projects are copied automatically. Git history, uncommitted files, agent conversations and selected agent settings travel together. Login files, private keys, environment secrets and ignored files stay here.</p>{:else}<p class="hint">Existing project privacy stays available without an active plan. You can stop copying a project or retry a pending privacy change.</p>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if !recoveryOnly && status && !status.configured}<p class="hint">Mirroring will start when your cloud machine is ready.</p>{/if}
  {#if status?.workspaces.length === 0}<p class="hint">No project mirrors to show yet.</p>{/if}
  {#each status?.workspaces ?? [] as workspace (workspace.workspace_id)}
    <details>
      <summary><span class="title">{workspace.name}</span><span class="hint">{place(workspace)}</span></summary>
      <div class="project">
        <p class="path">{workspace.root}</p>
        <label class="check"><input type="checkbox" checked={workspace.never_mirror} disabled={busy !== null || (recoveryOnly && workspace.never_mirror)} onchange={(event) => privacy(workspace,event.currentTarget)} />Never mirror this project</label>
        {#if workspace.privacy_pending}<p class="error" role="status">Copying from this laptop has stopped. Cloud privacy is still pending; retry to disable it everywhere.</p><button class="btn" disabled={busy !== null} onclick={() => void act(workspace.workspace_id, () => proSetNeverMirror(workspace.workspace_id, true))}>Retry cloud privacy</button>{/if}
        {#if workspace.git_branches?.length}<p class="hint">Cloud changes are saved in {workspace.git_branches.join(", ")} for you to merge.</p>{/if}
        {#if workspace.mirror}
          <p class="hint">{workspace.mirror.files} files · {bytes(workspace.mirror.bytes)} working files · {bytes(workspace.mirror.storage_limit_bytes)} storage limit · {workspace.mirror.excluded} private files excluded{#if workspace.mirror.too_large} · {workspace.mirror.too_large} files exceed the size limit{/if}</p>
          {#if workspace.mirror.last_mirrored_at}<p class="hint">Last copied {new Date(workspace.mirror.last_mirrored_at * 1000).toLocaleString()}</p>{/if}
          {#if workspace.mirror.error}<p class="error" role="status">{workspace.mirror.error}</p>{/if}
        {/if}
        {#if !recoveryOnly}
        <label class="field">Cloud setup command<input value={drafts[workspace.workspace_id] ?? workspace.profile?.setup_command ?? ""} oninput={(event) => { drafts[workspace.workspace_id] = event.currentTarget.value; }} placeholder="For example, npm ci" /></label>
        <button class="btn" disabled={busy !== null} onclick={() => void act(workspace.workspace_id,() => saveProfile(workspace))}>Save setup command</button>
        {#if workspace.profile?.missing_environment.length}<p class="hint">Missing on your cloud machine: {workspace.profile.missing_environment.join(", ")}. Configure these on the cloud machine; their values aren't copied.</p>{/if}
        {#if workspace.profile?.laptop_only.length}<p class="hint">Laptop-only steps: {workspace.profile.laptop_only.join("; ")}</p>{/if}
        {#if workspace.profile?.deferred.length}<p class="hint">Waiting for this laptop: {workspace.profile.deferred.join("; ")}</p>{/if}
        {#each status?.sessions.filter((session) => session.workspace_id === workspace.workspace_id) ?? [] as session (session.id)}
          <label class="check"><input type="checkbox" checked={session.keep_running ?? false} disabled={busy !== null} onchange={(event) => { const value = event.currentTarget.checked; event.currentTarget.checked = session.keep_running ?? false; void act(session.id,() => proMirrorPreference({ operation: "pin",session_id:session.id,keep_running:value })); }} />Keep “{session.display_name ?? session.name}” running when idle</label>
        {/each}
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
  .field { display: flex; flex-direction: column; gap: 6px; font-size: 12px; }
  .field input { width: 100%; box-sizing: border-box; border: 1px solid var(--edge); border-radius: 5px; background: var(--term-bg); color: var(--fg); padding: 8px; font-family: var(--font-mono); }
  .btn { align-self: flex-start; border: 1px solid var(--edge); border-radius: 5px; background: var(--term-bg); color: var(--fg); padding: 6px 10px; cursor: pointer; }
  .btn:disabled { opacity: .5; cursor: default; }
  .error { color: var(--warn); font-size: 12px; overflow-wrap: anywhere; }
</style>
