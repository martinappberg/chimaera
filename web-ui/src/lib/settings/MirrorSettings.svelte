<script lang="ts">
  import { untrack } from "svelte";
  import { cloudOnboarding } from "../pro/onboarding.svelte";
  import { pageVisible } from "../shared/visibility";
  import { copyIssue, projectCopiesSetupLine, projectCopyError, projectPlace } from "../pro/presentation";
  import { computerSteps, proposedSetup, settleProposal, type ProposalDecision } from "../pro/profile";
  import { proMirrorStatus, proSetNeverMirror, type MirrorStatus, type MirrorWorkspace } from "../net/native";
  let { visible = true, recoveryOnly = false }: { visible?: boolean; recoveryOnly?: boolean } = $props();
  let status = $state<MirrorStatus | null>(null);
  let error = $state<string | null>(null);
  /** A change that didn't go through. Kept apart from `error` so the re-read
   * that follows every change can't clear it. */
  let actionError = $state<string | null>(null);
  let busy = $state<string | null>(null);
  let revision = 0;
  /** Last automatic privacy retry per project. Turning cloud copies off is
   * idempotent, so a pending one is re-sent quietly at most once a minute
   * while this view shows, instead of asking the user to retry. */
  const privacyRetries = new Map<string, number>();
  let privacyFlight = false;
  async function load(): Promise<void> {
    const current = ++revision;
    try {
      const next = await proMirrorStatus();
      if (current !== revision) return;
      status = next; error = null;
      retryPrivacy(next);
    }
    catch { if (current === revision) error = "Project status couldn’t refresh. Your saved work and privacy choices haven’t changed."; }
  }
  function retryPrivacy(next: MirrorStatus): void {
    if (privacyFlight || busy !== null) return;
    const now = Date.now();
    const pending = next.workspaces.find(workspace => workspace.privacy_pending && workspace.never_mirror
      && now - (privacyRetries.get(workspace.workspace_id) ?? 0) >= 60_000);
    if (pending === undefined) return;
    privacyRetries.set(pending.workspace_id, now);
    privacyFlight = true;
    void proSetNeverMirror(pending.workspace_id, true)
      .then(() => load(), () => { /* The next status read shows it still pending. */ })
      .finally(() => { privacyFlight = false; });
  }
  $effect(() => {
    if (!visible || !$pageVisible) return;
    untrack(() => void load());
    const timer = setInterval(() => void load(), 15000);
    return () => { clearInterval(timer); revision += 1; };
  });
  /** The row is re-read after every change, failed or not: the daemon's
   * status is what the switch and its lines show, including a privacy change
   * still waiting for the account (quiet progress, never this error). */
  async function act(id: string, action: () => Promise<void>): Promise<void> {
    if (busy !== null) return;
    busy = id; actionError = null;
    try { await action(); }
    catch { actionError = "This change didn’t go through. Try again in a moment."; }
    finally { busy = null; await load(); }
  }
  /** Set when a decision found the proposal replaced or withdrawn meanwhile:
   * nothing was saved, and the refreshed row shows what is waiting now. */
  let proposalChanged = $state<string | null>(null);
  /** Confirm or dismiss exactly the command shown. Only the user's click
   * turns an agent's proposal into the setup command the cloud runs. */
  function decide(workspace: MirrorWorkspace, shown: string, decision: ProposalDecision): void {
    proposalChanged = null;
    void act(workspace.workspace_id, async () => {
      if (await settleProposal(workspace.workspace_id, shown, decision) === "changed") proposalChanged = workspace.workspace_id;
    });
  }
  function privacy(workspace: MirrorWorkspace, checkbox: HTMLInputElement): void {
    const value = checkbox.checked; checkbox.checked = workspace.never_mirror;
    // This click already asked the account; the quiet re-send waits its minute.
    if (value) privacyRetries.set(workspace.workspace_id, Date.now());
    void act(workspace.workspace_id, () => proSetNeverMirror(workspace.workspace_id, value));
  }
  /** The daemon saves the user's version beside each file and names those
   * copies in `kept_paths` (at most 32). Without names, such as from an older
   * daemon that kept them elsewhere, the sentence claims no location. */
  function keptBoth(mirror: NonNullable<MirrorWorkspace["mirror"]>): string {
    const count = mirror.kept_both ?? 0;
    const paths = mirror.kept_paths ?? [];
    const kept = `Kept both versions of ${count} ${count === 1 ? "file" : "files"} changed here and in the cloud.`;
    if (paths.length === 0) return kept;
    const more = count > paths.length ? `, and ${count - paths.length} more` : "";
    return `${kept} Your version is saved beside ${count === 1 ? "the file" : "each file"}: ${paths.join(", ")}${more}.`;
  }
</script>
<div class="mirrors">
  <h3>Project copies</h3>
  {#if !recoveryOnly}<p class="hint">Your files, conversations and supported agent settings stay together between this computer and the cloud. Connected services are authorized separately.</p>{:else}<p class="hint">You can still manage project privacy without an active plan.</p>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if actionError}<p class="error" role="alert">{actionError}</p>{/if}
  {#if !recoveryOnly && projectCopiesSetupLine(status) !== null}<p class="hint" role="status">{projectCopiesSetupLine(status)}</p>{/if}
  {#if status?.workspaces.length === 0}<p class="hint">No project copies to show yet.</p>{/if}
  {#each status?.workspaces ?? [] as workspace (workspace.workspace_id)}
    {@const steps = computerSteps(workspace.profile)}
    <details>
      <summary><span class="title">{workspace.name}</span><span class="hint">{projectPlace(workspace)}</span></summary>
      <div class="project">
        <p class="path">{workspace.root}</p>
        <label class="check"><input type="checkbox" checked={workspace.never_mirror} disabled={busy !== null || (recoveryOnly && workspace.never_mirror)} onchange={(event) => privacy(workspace,event.currentTarget)} />Keep this project on this computer</label>
        {#if workspace.never_mirror && !workspace.privacy_pending}<p class="hint">Automatic copying and cloud access are off. Existing saved copies haven’t been deleted.</p>{/if}
        {#if workspace.privacy_pending}<p class="hint" role="status">This project now stays on this computer. Chimaera is confirming that with your account and keeps trying on its own.</p>{/if}
        {#if workspace.git_branches?.length}<p class="hint">Cloud changes are saved in {workspace.git_branches.join(", ")} for you to merge.</p>{/if}
        {#if !recoveryOnly && workspace.blocked_providers?.length}
          <div class="connection-needed"><p class="hint">Connect the agents this project uses so it can continue automatically.</p><button class="btn" onclick={() => cloudOnboarding.request({ providerIds: workspace.blocked_providers!.map(provider => provider.id), workspaceId: workspace.workspace_id, workspaceName: workspace.name })}>Connect agents to continue</button></div>
        {/if}
        {#if !recoveryOnly}
          {@const proposal = proposedSetup(workspace.profile)}
          {#if proposal !== null}
            <div class="proposal" role="group" aria-label="Proposed setup command">
              <p class="lead">Your agent proposed a setup command for the cloud</p>
              <pre class="command">{proposal}</pre>
              <p class="hint">Once you confirm it, it runs in the project folder before work continues in the cloud.{#if workspace.profile?.setup_command} It replaces the current setup command.{/if}</p>
              <div class="actions"><button class="btn" disabled={busy !== null} onclick={() => decide(workspace, proposal, "confirm")}>Confirm</button><button class="btn" disabled={busy !== null} onclick={() => decide(workspace, proposal, "dismiss")}>Dismiss</button></div>
            </div>
          {/if}
          {#if proposalChanged === workspace.workspace_id}<p class="hint" role="status">Your agent changed this proposal, so nothing was saved.</p>{/if}
        {/if}
        {#if steps.length}
          <div class="steps">
            <p class="lead">Steps that need your computer</p>
            <p class="hint">These weren’t run in the cloud because they need this computer.</p>
            <ul>{#each steps as step, index (index)}<li>{step}</li>{/each}</ul>
          </div>
        {/if}
        {#if workspace.mirror}
          {#if workspace.mirror.last_mirrored_at}<p class="hint">Last copied {new Date(workspace.mirror.last_mirrored_at * 1000).toLocaleString()}</p>{/if}
          {#if workspace.mirror.kept_both}<p class="hint kept" role="status">{keptBoth(workspace.mirror)}</p>{/if}
          {#if workspace.mirror.too_large}<p class="hint" role="status">Some files are too large for the cloud copy. They stay on this computer.</p>{/if}
          {#if workspace.mirror.error && workspace.mirror.error !== "cloud_provider_not_ready"}<p class={copyIssue(workspace.mirror) === "problem" ? "error" : "hint"} role="status">{projectCopyError(workspace.mirror.error, workspace.mirror.error_code)}</p>{/if}
        {/if}
      </div>
    </details>
  {/each}
</div>
<style>
  .mirrors { border-top: 1px solid var(--edge); padding: 18px 14px 0; margin-top: 18px; }
  h3 { font-size: var(--text-sm); margin: 0 0 8px; }
  .hint { color: var(--muted); font-size: var(--text-xs); line-height: 1.55; }
  .kept { overflow-wrap: anywhere; }
  details { border: 1px solid var(--edge); border-radius: 8px; margin: 10px 0; }
  summary { cursor: pointer; padding: 12px; }
  summary:focus-visible, .btn:focus-visible, input:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 2px; }
  .title { font-weight: 600; margin-right: 12px; }
  .project { padding: 0 12px 14px; display: flex; flex-direction: column; gap: 10px; }
  .path { font-family: var(--mono); font-size: var(--text-xs); color: var(--muted); overflow-wrap: anywhere; margin: 0; }
  .check { display: flex; gap: 8px; align-items: center; font-size: var(--text-xs); }
  input { accent-color: var(--accent); }
  .btn { align-self: flex-start; border: 1px solid var(--edge); border-radius: 6px; background: transparent; color: var(--fg); padding: 6px 10px; font: inherit; font-size: var(--text-sm); cursor: pointer; }
  .btn:hover:not(:disabled) { background: var(--row-hover); }
  .btn:disabled { opacity: .5; cursor: default; }
  .error { color: var(--warn); font-size: var(--text-xs); overflow-wrap: anywhere; }
  .proposal { display: flex; flex-direction: column; gap: 8px; }
  .proposal p { margin: 0; }
  .lead { font-size: var(--text-xs); color: var(--fg); }
  /* Agent-written text: shown whole (the user confirms exactly this), never
     as markup, wrapped and scrollable rather than truncated. */
  .command { margin: 0; padding: 8px 10px; max-height: 12em; overflow: auto; border: 1px solid var(--edge); border-radius: 6px; font-family: var(--mono); font-size: var(--text-xs); white-space: pre-wrap; overflow-wrap: anywhere; user-select: text; }
  .actions { display: flex; gap: 8px; flex-wrap: wrap; }
  .steps { display: flex; flex-direction: column; gap: 6px; }
  .steps p { margin: 0; }
  .steps ul { margin: 0; padding-left: 18px; font-family: var(--mono); font-size: var(--text-xs); overflow-wrap: anywhere; user-select: text; }
  .steps li + li { margin-top: 4px; }
  @media (pointer: coarse) { summary, .btn, .check { min-height: 40px; } .btn { padding: 9px 12px; } }
</style>
