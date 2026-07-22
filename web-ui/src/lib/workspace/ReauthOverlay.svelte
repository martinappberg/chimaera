<script lang="ts">
  import {
    health as fetchHealth,
    refreshTokenFromHash,
    unauthorized,
  } from "../net/api";
  import { focusOnMount } from "../shared/focusOnMount";
  import { modalFocus } from "../shared/modalFocus";

  interface Props {
    /** Native remote windows refresh credentials through their scoped SSH
     *  reconnect instead; browsers/local windows still need this manual path. */
    enabled?: boolean;
  }

  let { enabled = true }: Props = $props();

  let authRetryMsg = $state<string | null>(null);
  let authRetrying = $state(false);

  async function retryAuth(): Promise<void> {
    if (authRetrying) return;
    authRetrying = true;
    authRetryMsg = null;
    refreshTokenFromHash();
    try {
      await fetchHealth();
      // Token works again: a clean reload re-auths every socket and
      // restores the layout from the daemon.
      location.reload();
    } catch {
      authRetryMsg = "still unauthorized — paste a fresh URL from `chimaera connect`, then retry";
    } finally {
      authRetrying = false;
    }
  }
</script>

{#if enabled && $unauthorized}
  <!-- Blocking re-auth overlay: the daemon rejected this window's token
       (restart or expiry). Nothing behind it is trustworthy until re-auth. -->
  <div class="auth-overlay">
    <div
      class="auth-panel"
      role="alertdialog"
      aria-modal="true"
      aria-label="reconnect"
      tabindex="-1"
      use:modalFocus
    >
      <div class="auth-title">disconnected — unauthorized</div>
      <p class="auth-body">
        The daemon rejected this window's token (it likely restarted). Paste a fresh URL from
        <code>chimaera connect</code> into the address bar, then retry.
      </p>
      <button class="auth-retry" use:focusOnMount disabled={authRetrying} onclick={() => void retryAuth()}>
        {authRetrying ? "retrying…" : "retry"}
      </button>
      {#if authRetryMsg}
        <div class="auth-msg">{authRetryMsg}</div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .auth-overlay {
    position: fixed;
    inset: 0;
    z-index: 200;
    display: flex;
    align-items: flex-start;
    justify-content: center;
    background: var(--scrim);
    animation: authfade 0.1s ease-out;
  }

  @keyframes authfade {
    from {
      opacity: 0;
    }
  }

  .auth-panel {
    margin-top: 20vh;
    width: min(420px, calc(100vw - 2rem));
    padding: 20px;
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 8px;
    box-shadow: 0 12px 36px rgba(0, 0, 0, 0.22);
  }

  .auth-title {
    font-size: var(--text-md);
    font-weight: 600;
    margin-bottom: 8px;
  }

  .auth-body {
    margin: 0 0 12px;
    font-size: var(--text-md);
    line-height: 1.5;
    color: var(--muted);
  }

  .auth-body code {
    font-family: var(--mono);
    font-size: var(--text-sm);
    color: var(--fg);
  }

  .auth-retry {
    appearance: none;
    border: 1px solid var(--edge);
    background: none;
    padding: 4px 16px;
    border-radius: 5px;
    font: inherit;
    font-size: var(--text-md);
    color: var(--fg);
    cursor: pointer;
    transition: background-color 0.12s ease;
  }

  .auth-retry:hover:enabled {
    background: var(--row-hover);
  }

  .auth-retry:disabled {
    color: var(--muted);
    cursor: default;
  }

  .auth-msg {
    margin-top: 8px;
    font-size: var(--text-xs);
    color: var(--err);
  }
</style>
