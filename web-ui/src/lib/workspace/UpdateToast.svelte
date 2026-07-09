<!--
  The update toast: one small, dismissible card, bottom-right, shown when
  `currentOffer` has something worth saying. One primary action per offer
  kind — the full app+daemon chain (native), a daemon-only restart (skew),
  or a pointer at the release (browser windows, which cannot apply updates
  themselves). The copy states consequences plainly: layouts and sessions
  come back (ledger + handoff + window registry), running terminal programs
  restart. "later" snoozes until tomorrow; "skip" mutes that version.

  Capability note: this UI is embedded in — and served by — the daemon it
  talks to, so a toast can never over-promise resurrection: if the daemon
  were too old to have the session ledger, it would be serving a UI too old
  to have this toast.
-->
<script lang="ts">
  import type { UpdateOffer } from "./update.svelte";
  import { snoozeUpdate, skipUpdateVersion } from "./update.svelte";
  import { beginUpdate, connectHost, updateLocalDaemon } from "../net/native";

  let { offer }: { offer: UpdateOffer } = $props();

  let busy = $state(false);
  let error = $state<string | null>(null);

  const title = $derived.by(() => {
    switch (offer.kind) {
      case "app":
      case "release":
        return `chimaera ${offer.version} is available`;
      case "daemon-local":
        return "daemon is older than this app";
      case "daemon-remote":
        return `daemon on ${offer.alias} is outdated`;
    }
  });

  const body = $derived.by(() => {
    switch (offer.kind) {
      case "app":
        return "One click updates the app and daemon. Windows, tabs and sessions come back where they were; running terminal programs restart.";
      case "daemon-local":
        return "Restart the daemon into this app's build. Layouts and sessions come back; running terminal programs restart.";
      case "daemon-remote":
        return "Reconnect and update it. Layouts and sessions there come back; running terminal programs restart.";
      case "release":
        return "Update from the chimaera app, or rerun chimaera connect from your machine.";
    }
  });

  const action = $derived.by(() => {
    switch (offer.kind) {
      case "app":
        return "update now";
      case "daemon-local":
        return "update daemon";
      case "daemon-remote":
        return `update ${offer.alias}`;
      case "release":
        return null;
    }
  });

  const skippable = $derived(offer.kind === "app" || offer.kind === "release");

  async function run(): Promise<void> {
    busy = true;
    error = null;
    try {
      switch (offer.kind) {
        case "app":
          // Diverges on success: the app relaunches into the new build and
          // this window comes back via the shell's window registry.
          await beginUpdate();
          break;
        case "daemon-local":
          // Same port + token via the daemon's handoff: sockets reconnect
          // in place; the skew signal clears on the next health poll.
          await updateLocalDaemon();
          break;
        case "daemon-remote":
          await connectHost(offer.alias, true);
          break;
        case "release":
          break;
      }
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  function skip(): void {
    if (offer.kind === "app" || offer.kind === "release") {
      skipUpdateVersion(offer.version);
    }
  }
</script>

<div class="update-toast" role="status" aria-live="polite">
  <div class="head">
    <span class="dot" aria-hidden="true"></span>
    <span class="title">{title}</span>
  </div>
  <p class="body">{body}</p>
  {#if error !== null}
    <p class="error">{error}</p>
  {/if}
  <div class="actions">
    {#if action !== null}
      <button class="primary" disabled={busy} onclick={() => void run()}>
        {busy ? "updating…" : action}
      </button>
    {/if}
    {#if offer.kind === "release" && offer.url !== null}
      <a class="notes" href={offer.url} target="_blank" rel="noreferrer">release notes</a>
    {/if}
    <button class="quiet" disabled={busy} onclick={snoozeUpdate}>later</button>
    {#if skippable}
      <button class="quiet subtle" disabled={busy} onclick={skip}>skip this version</button>
    {/if}
  </div>
</div>

<style>
  .update-toast {
    position: fixed;
    right: 14px;
    bottom: 14px;
    z-index: 180; /* under the auth/reconnect overlays (190+) */
    width: 320px;
    padding: 12px 14px;
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 10px;
    box-shadow: 0 8px 28px color-mix(in srgb, var(--fg) 12%, transparent);
    animation: toastin 0.16s ease-out;
  }

  @keyframes toastin {
    from {
      opacity: 0;
      transform: translateY(6px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }

  .head {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--accent);
    flex: none;
  }

  .title {
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg);
  }

  .body {
    margin: 6px 0 0;
    font-size: var(--text-xs);
    line-height: 1.45;
    color: var(--muted);
  }

  .error {
    margin: 6px 0 0;
    font-size: var(--text-xs);
    color: var(--danger, #c4554d);
    overflow-wrap: anywhere;
  }

  .actions {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: 10px;
  }

  .primary {
    padding: 4px 12px;
    border-radius: 6px;
    border: 1px solid var(--accent);
    background: color-mix(in srgb, var(--accent) 14%, transparent);
    color: var(--fg);
    font-size: var(--text-xs);
    cursor: pointer;
  }

  .primary:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 24%, transparent);
  }

  .primary:disabled {
    opacity: 0.6;
    cursor: default;
  }

  .notes {
    font-size: var(--text-xs);
    color: var(--accent);
    text-decoration: none;
  }

  .notes:hover {
    text-decoration: underline;
  }

  .quiet {
    padding: 4px 6px;
    border: none;
    background: none;
    color: var(--muted);
    font-size: var(--text-xs);
    cursor: pointer;
  }

  .quiet:hover:not(:disabled) {
    color: var(--fg);
  }

  .subtle {
    margin-left: auto;
    opacity: 0.8;
  }
</style>
