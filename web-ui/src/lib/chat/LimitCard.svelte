<script lang="ts">
  // The chat's line while its agent reports the account's limit as reached:
  // what was hit and when it resets. Waiting is the default and the agent
  // may have ways of its own (a reset, extra usage), so the app's part — its
  // sign-in, for another account — is a quiet link beside the fact, never a
  // call to action. Shown only where that sign-in can switch accounts; the
  // header's usage chip says the rest.
  import type { LimitNotice } from "./limitCard";

  interface Props {
    notice: LimitNotice;
    agentName: string;
    onSignIn: () => void;
  }
  let { notice, agentName, onSignIn }: Props = $props();
</script>

<div class="limit" role="group" aria-label="{agentName}: {notice.what}">
  <span class="fact">
    <span class="what">{notice.what}</span>{#if notice.resets}<span class="resets">{" · "}{notice.resets}</span>{/if}
  </span>
  <button class="opt-link quiet" title="Sign in to {agentName} with another account — also /login" onclick={onSignIn}>Use another account</button>
</div>

<style>
  /* A transcript notice's weight, not a card's: centred, one line, wrapping
     only in a narrow pane. The link is the shared `.opt-link.quiet`
     (app.css), whose hover underlines without moving anything. */
  .limit {
    display: flex;
    align-items: baseline;
    justify-content: center;
    gap: 2px 14px;
    flex-wrap: wrap;
    font-size: var(--text-sm);
    line-height: 1.5;
    text-align: center;
  }
  .fact {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .what {
    color: var(--fg);
  }
  .resets {
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  button:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 1px;
  }
</style>
