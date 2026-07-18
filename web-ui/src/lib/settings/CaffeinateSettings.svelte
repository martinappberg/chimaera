<script lang="ts">
  /** Device-local Caffeinate detail; intentionally not daemon settings.json. */
  import { onMount } from "svelte";
  import {
    caffeinateState,
    onCaffeinateChanged,
    setCaffeinate,
    type CaffeinateState,
  } from "../net/native";

  // Conservative until native state arrives so a fast first click still
  // counts as the explicit acknowledgement described on this page.
  let mode = $state<CaffeinateState>({
    enabled: false,
    consent_required: true,
    closed_lid_ready: false,
  });
  let busy = $state(false);
  let error = $state<string | null>(null);

  onMount(() => {
    let unlisten: (() => void) | null = null;
    void onCaffeinateChanged((next) => {
      mode = next;
      error = null;
    }).then((u) => (unlisten = u));
    const refresh = () => void caffeinateState().then((next) => (mode = next));
    refresh();
    // Dock readiness is live hardware state. Keep this quiet status accurate
    // if the user plugs into or leaves a dock while Settings stays open.
    const timer = window.setInterval(refresh, 5_000);
    return () => {
      unlisten?.();
      window.clearInterval(timer);
    };
  });

  async function toggle(): Promise<void> {
    if (busy) return;
    busy = true;
    error = null;
    try {
      // This page is the full explanation. The explicit enable click is the
      // first-use acknowledgement; later clicks simply toggle the held mode.
      mode = await setCaffeinate(!mode.enabled, !mode.enabled && mode.consent_required);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      try {
        mode = await caffeinateState();
      } catch {
        /* best-effort truth refresh */
      }
    } finally {
      busy = false;
    }
  }
</script>

<h2>Caffeinate</h2>
<div class="card">
  <div class="summary">
    <div>
      <div class="name">Keep Chimaera working while this Mac is locked</div>
      <div class="scope">This is a device setting; it is not stored on the current daemon host.</div>
    </div>
    <span class="status" class:on={mode.enabled}>{mode.enabled ? "active" : "off"}</span>
  </div>

  <p>
    Caffeinate holds a macOS power assertion so local chats, terminals, and background commands can
    continue while the display is off or the screen is locked. The display still sleeps and locks
    normally—Caffeinate does not unlock the Mac or automate desktop apps.
  </p>
  <p>
    While active, remote windows keep retrying eligible SSH network failures with capped backoff and
    try again promptly when connectivity returns. Authentication failures pause for manual action,
    so Caffeinate does not repeatedly raise password or 2FA prompts.
  </p>
  {#if mode.closed_lid_ready}
    <div class="closed-lid-ready">
      <div>
        <div class="ready-name">Closed-lid use</div>
        <div class="ready-detail">
          External power and an active external display are connected. With Caffeinate active,
          Chimaera can keep working in macOS closed-display mode.
        </div>
      </div>
      <span class="ready-status">docked</span>
    </div>
  {/if}
  <ul>
    <li>
      Screen lock and display-off use are supported. Closing the lid can sleep an undocked Mac;
      closed-lid readiness appears here only when external power and an active external display are
      detected.
    </li>
    <li>Low battery, thermal protection, shutdown, or quitting Chimaera can stop the work.</li>
    <li>Failed model requests are not replayed automatically; only connectivity is repaired.</li>
  </ul>

  {#if error !== null}
    <div class="error">{error}</div>
  {/if}
  <div class="action-row">
    <span class="persist">Your choice persists across Chimaera restarts.</span>
    <button class:on={mode.enabled} disabled={busy} onclick={() => void toggle()}>
      {busy ? "updating…" : mode.enabled ? "turn off" : "enable Caffeinate"}
    </button>
  </div>
</div>

<style>
  h2 {
    margin: 18px 0 4px;
    padding: 0 14px;
    color: var(--muted);
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.1em;
    text-transform: uppercase;
  }

  .card {
    margin: 8px 14px 18px;
    padding: 14px 16px;
    border: 1px solid var(--edge);
    border-radius: 9px;
    background: color-mix(in srgb, var(--row-hover) 52%, transparent);
  }

  .summary,
  .action-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 14px;
  }

  .name {
    color: var(--fg);
    font-size: var(--text-md);
    font-weight: 600;
  }

  .scope,
  .persist {
    margin-top: 3px;
    color: var(--muted);
    font-size: var(--text-xs);
  }

  .status {
    flex: none;
    padding: 2px 8px;
    border: 1px solid var(--edge);
    border-radius: 999px;
    color: var(--muted);
    font-size: var(--text-xs);
  }

  .status.on {
    border-color: color-mix(in srgb, var(--accent) 52%, var(--edge));
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 9%, transparent);
  }

  .closed-lid-ready {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 14px;
    max-width: 76ch;
    margin-top: 12px;
    padding: 10px 11px;
    border: 1px solid color-mix(in srgb, var(--accent) 35%, var(--edge));
    border-radius: 7px;
    background: color-mix(in srgb, var(--accent) 7%, transparent);
  }

  .ready-name {
    color: var(--fg);
    font-size: var(--text-sm);
    font-weight: 600;
  }

  .ready-detail {
    margin-top: 2px;
    color: var(--muted);
    font-size: var(--text-xs);
    line-height: 1.45;
  }

  .ready-status {
    flex: none;
    color: var(--accent);
    font-size: var(--text-xs);
  }

  p,
  li {
    color: var(--muted);
    font-size: var(--text-sm);
    line-height: 1.55;
  }

  p {
    max-width: 76ch;
    margin: 12px 0 0;
  }

  ul {
    margin: 10px 0 0;
    padding-left: 20px;
  }

  li + li {
    margin-top: 3px;
  }

  .error {
    margin-top: 10px;
    color: var(--err);
    font-size: var(--text-sm);
    overflow-wrap: anywhere;
  }

  .action-row {
    margin-top: 14px;
    padding-top: 12px;
    border-top: 1px solid var(--edge);
  }

  button {
    appearance: none;
    flex: none;
    border: 1px solid color-mix(in srgb, var(--accent) 50%, var(--edge));
    border-radius: 7px;
    padding: 6px 12px;
    color: var(--fg);
    background: color-mix(in srgb, var(--accent) 13%, transparent);
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
  }

  button.on {
    border-color: var(--edge);
    background: transparent;
  }

  button:hover:enabled {
    background: color-mix(in srgb, var(--accent) 21%, transparent);
  }

  button:disabled {
    cursor: default;
    opacity: 0.55;
  }

  @container settings (max-width: 460px) {
    .summary,
    .action-row {
      align-items: flex-start;
      flex-direction: column;
    }
  }
</style>
