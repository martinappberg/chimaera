<script lang="ts">
  import { onDestroy } from "svelte";
  import type { Session } from "../workspace/sessions";
  import { manualResumeNote, resumeParkedSession } from "../workspace/manualResume";

  let { session, onResumed }: { session: Session; onResumed: () => void } = $props();
  let busy = $state(false);
  let confirmed = $state(false);
  let error = $state<string | null>(null);
  let mounted = true;
  onDestroy(() => { mounted = false; });

  async function resume(): Promise<void> {
    if (busy || confirmed) return;
    busy = true;
    error = null;
    try {
      await resumeParkedSession(session);
      if (!mounted) return;
      confirmed = true;
      onResumed();
    } catch (failure) {
      if (mounted) error = failure instanceof Error ? failure.message : "Couldn’t confirm the resume. Check this conversation’s status.";
    } finally {
      if (mounted) busy = false;
    }
  }
</script>

<div class="manual-resume">
  <span role="status">{confirmed ? "Resuming this conversation…" : manualResumeNote(session)}</span>
  {#if session.manual_resume_reason === "project_secrets_idle"}
    <button type="button" disabled={busy || confirmed} onclick={resume}>{busy ? "Resuming…" : "Resume"}</button>
  {/if}
  {#if error !== null}<span class="error" role="alert">{error}</span>{/if}
</div>

<style>
  .manual-resume { padding: 8px 12px; color: var(--muted); font-size: var(--text-xs); text-align: center; }
  button { margin-left: 10px; border: 1px solid var(--edge); border-radius: 6px; padding: 3px 9px; color: var(--fg); background: var(--bg); font: inherit; cursor: pointer; }
  button:hover:not(:disabled) { background: var(--row-hover); }
  button:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 2px; }
  button:disabled { opacity: 0.6; cursor: default; }
  .error { display: block; margin-top: 6px; color: var(--err); }
  @media (pointer: coarse) { button { min-height: 40px; padding: 6px 12px; } }
</style>
