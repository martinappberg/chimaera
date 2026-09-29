<script lang="ts">
  /**
   * A quiet branch chip: the branch a session is on (muted mono text beside a
   * branch glyph), with the checkout it lives in on hover. Shown only where a
   * session IS in a repository — git never asks for attention.
   */
  import { branchLabel, type SessionGit } from "../workspace/sessions";

  interface Props {
    git: SessionGit;
    /** Name the repository too (a workspace holding several). */
    showRepo?: boolean;
  }
  let { git, showRepo = false }: Props = $props();

  const label = $derived(branchLabel(git));
  const repoName = $derived(git.repo.split("/").filter(Boolean).pop() ?? git.repo);
  const tip = $derived(
    git.worktree === git.repo
      ? `${label} — ${git.worktree}`
      : `${label} — worktree ${git.worktree} of ${git.repo}`,
  );
</script>

<span class="branch-chip" class:detached={git.detached} title={tip}>
  <svg viewBox="0 0 16 16" width="10" height="10" aria-hidden="true">
    <path
      d="M5 3v7.5M5 12.5v.5M11 3v3a2.5 2.5 0 0 1-2.5 2.5H5"
      fill="none"
      stroke="currentColor"
      stroke-width="1.4"
      stroke-linecap="round"
    />
    <circle cx="5" cy="12.6" r="1.6" fill="none" stroke="currentColor" stroke-width="1.4" />
    <circle cx="5" cy="2.4" r="1.6" fill="none" stroke="currentColor" stroke-width="1.4" />
    <circle cx="11" cy="2.4" r="1.6" fill="none" stroke="currentColor" stroke-width="1.4" />
  </svg>
  <span class="bname">{#if showRepo}<span class="brepo">{repoName}</span> {/if}{label}</span>
</span>

<style>
  .branch-chip {
    display: inline-flex;
    align-items: center;
    gap: 0.22rem;
    min-width: 0;
    max-width: 100%;
    vertical-align: middle;
    font-family: var(--mono);
    font-size: var(--text-xs);
    line-height: 1.2;
    color: var(--muted);
  }
  .branch-chip svg {
    flex: none;
    opacity: 0.75;
  }
  .branch-chip.detached {
    color: var(--warn);
  }
  .bname {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .brepo {
    opacity: 0.75;
  }
</style>
