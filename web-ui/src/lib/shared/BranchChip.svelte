<script lang="ts">
  /**
   * A quiet branch label: a branch glyph, the branch name, and — when the
   * session works in a linked worktree rather than the main checkout — that
   * worktree's folder name, muted. Hover gives the repository path. With
   * `onOpen` it is a button (the chat's line opens "Changes on this
   * branch"). Shown only where a session IS in a repository.
   */
  import { baseName, branchLabel, type SessionGit } from "../workspace/sessions";

  interface Props {
    git: SessionGit;
    onOpen?: () => void;
  }
  let { git, onOpen }: Props = $props();

  const label = $derived(branchLabel(git));
  /** The worktree's folder, only when it says something the branch name
   *  doesn't (chimaera names its worktrees after their branch). */
  const folder = $derived(
    git.worktree === git.repo ||
      (git.branch !== null && git.branch !== "" && git.worktree.endsWith(`/${git.branch}`))
      ? null
      : baseName(git.worktree),
  );
  const tip = $derived(
    git.worktree === git.repo
      ? git.repo
      : `${git.worktree} — a worktree of ${git.repo}`,
  );
</script>

{#snippet body()}
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
  <span class="bname">{label}{#if folder}<span class="bfolder"> · {folder}</span>{/if}</span>
{/snippet}

{#if onOpen}
  <!-- stopPropagation: the chip sits inside clickable cards (the dashboard
       card opens its session); the chip's own destination wins. -->
  <button
    type="button"
    class="branch-chip link"
    title={`${tip} — changes on this branch`}
    onclick={(e) => {
      e.stopPropagation();
      onOpen?.();
    }}
  >
    {@render body()}
  </button>
{:else}
  <span class="branch-chip" title={tip}>{@render body()}</span>
{/if}

<style>
  .branch-chip {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    min-width: 0;
    max-width: 100%;
    vertical-align: middle;
    font-family: var(--mono);
    font-size: var(--text-xs);
    line-height: 1.2;
    color: var(--muted);
  }
  .branch-chip.link {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    font-family: var(--mono);
    font-size: var(--text-xs);
    cursor: pointer;
  }
  .branch-chip.link:hover {
    color: var(--fg);
  }
  .branch-chip.link:focus-visible {
    outline: 1px solid var(--focus-ring);
    outline-offset: 1px;
    border-radius: 3px;
  }
  .branch-chip svg {
    flex: none;
    opacity: 0.75;
  }
  .bname {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .bfolder {
    opacity: 0.75;
  }
</style>
