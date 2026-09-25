<script lang="ts">
  /**
   * The Notifications section's status line: whether the OS (native app) or
   * the browser (a browser tab) will actually show Chimaera's alerts, with
   * the one action that changes that — ask, or open the system settings
   * where a denial is undone — plus a test alert. The switches below it are
   * ordinary schema rows (they decide WHICH alerts the daemon produces);
   * this line is about whether they can reach the user at all.
   */
  import { onMount } from "svelte";
  import {
    isNativeShell,
    notificationPermission,
    openNotificationSettings,
    requestNotificationPermission,
    testNotification,
    type NativeNotificationPermission,
  } from "../net/native";
  import { isMac } from "../shared/keys";
  import {
    browserPermission,
    requestBrowserPermission,
  } from "../workspace/notices";

  const native = isNativeShell();

  type Status = "loading" | "granted" | "denied" | "ask" | "unsupported";
  let status = $state<Status>("loading");
  let busy = $state(false);
  let tested = $state(false);

  function fromNative(p: NativeNotificationPermission): Status {
    return p === "not_determined" ? "ask" : p;
  }

  function fromBrowser(p: NotificationPermission | "unsupported"): Status {
    return p === "default" ? "ask" : p;
  }

  async function refresh(): Promise<void> {
    status = native
      ? fromNative(await notificationPermission().catch(() => "unsupported" as const))
      : fromBrowser(browserPermission());
  }

  async function allow(): Promise<void> {
    busy = true;
    try {
      status = native
        ? fromNative(await requestNotificationPermission())
        : fromBrowser(await requestBrowserPermission());
    } finally {
      busy = false;
    }
  }

  async function test(): Promise<void> {
    if (native) {
      await testNotification();
    } else if (browserPermission() === "granted") {
      new Notification("Chimaera", {
        body: "Notifications are on. You'll hear from agents here when they finish or need you.",
        tag: "chimaera-test",
      });
    }
    tested = true;
    setTimeout(() => (tested = false), 2500);
    // A native test on a never-asked app raises the OS prompt; pick up the
    // answer once the user has had a moment with it.
    if (native) setTimeout(() => void refresh(), 1500);
  }

  onMount(() => {
    void refresh();
    // Returning from System Settings is the usual way a denial gets undone.
    const onFocus = (): void => void refresh();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  });

  const where = native ? (isMac ? "macOS" : "your desktop") : "this browser";
</script>

<div class="status" class:warn={status === "denied"} data-state={status}>
  <span class="dot" aria-hidden="true"></span>
  <div class="text">
    {#if status === "loading"}
      <span class="line">Checking notification permission…</span>
    {:else if status === "granted"}
      <span class="line">Notifications are on in {where}.</span>
      <span class="sub">
        {#if native}
          They arrive even with every window closed. Style and grouping live in {isMac
            ? "System Settings"
            : "your desktop's settings"}.
        {:else}
          Shown while a Chimaera tab is open. The native app notifies even with every window
          closed.
        {/if}
      </span>
    {:else if status === "ask"}
      <span class="line">Chimaera hasn't asked {where} for permission yet.</span>
      <span class="sub">
        {native
          ? "It asks with the first notification; ask now to be sure you don't miss it."
          : "Browsers only show notifications for sites you allow."}
      </span>
    {:else if status === "denied"}
      <span class="line">Notifications are turned off for Chimaera in {where}.</span>
      <span class="sub">
        {native
          ? isMac
            ? "Turn on “Allow notifications” for Chimaera in System Settings → Notifications."
            : "Allow them for Chimaera in your desktop's notification settings."
          : "Allow notifications for this site in the browser's site settings, then reload."}
      </span>
    {:else}
      <span class="line">Notifications aren't available here.</span>
      <span class="sub">
        {native
          ? "This build isn't running from an app bundle, so macOS won't deliver its notifications."
          : "This browser can't show notifications for this page."}
      </span>
    {/if}
  </div>
  <div class="actions">
    {#if status === "ask"}
      <button class="btn primary" disabled={busy} onclick={() => void allow()}>
        Allow notifications
      </button>
    {:else if status === "denied" && native && isMac}
      <button class="btn" onclick={() => void openNotificationSettings()}>
        Open System Settings
      </button>
    {/if}
    {#if status === "granted" || (native && status === "ask")}
      <button class="btn" onclick={() => void test()}>{tested ? "Sent" : "Send test"}</button>
    {/if}
  </div>
</div>

<style>
  .status {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    margin: 4px 18px 10px 18px;
    padding: 12px 14px;
    border: 1px solid var(--edge);
    border-radius: 8px;
    background: color-mix(in srgb, var(--fg) 2%, transparent);
  }

  .dot {
    flex: none;
    width: 8px;
    height: 8px;
    margin-top: 6px;
    border-radius: 50%;
    background: var(--muted);
  }

  .status[data-state="granted"] .dot {
    background: var(--accent);
  }

  .status.warn {
    border-color: color-mix(in srgb, var(--warn) 40%, var(--edge));
    background: color-mix(in srgb, var(--warn) 6%, transparent);
  }

  .status.warn .dot {
    background: var(--warn);
  }

  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }

  .line {
    font-size: var(--text-md);
    color: var(--fg);
  }

  .sub {
    font-size: var(--text-sm);
    color: var(--muted);
    line-height: 1.45;
  }

  .actions {
    flex: none;
    display: flex;
    gap: 6px;
    align-items: center;
  }

  .btn {
    appearance: none;
    border: 1px solid var(--edge);
    background: var(--term-bg);
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
    padding: 3px 9px;
    border-radius: 6px;
    white-space: nowrap;
    transition:
      color 0.12s ease,
      background-color 0.12s ease;
  }

  .btn:hover:not(:disabled) {
    color: var(--fg);
    background: color-mix(in srgb, var(--fg) 3%, transparent);
  }

  .btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .btn.primary {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 45%, var(--edge));
  }

  .btn.primary:hover:not(:disabled) {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }

  @media (max-width: 560px) {
    .status {
      flex-direction: column;
    }
  }
</style>
