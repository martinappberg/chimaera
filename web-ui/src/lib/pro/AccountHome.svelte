<script lang="ts">
  /**
   * The web's Home: what a browser shows after signing in at the account
   * (`/`, `net/base.ts` `isAccountHome`). It is the desktop app's Home and
   * Settings, not an account portal: the same navigation, Workspaces listing
   * the cloud projects (each opens its project view), and Settings holding
   * Chimaera Pro. There is no local computer here, so nothing that needs a
   * daemon (This Mac, remote machines, workbench settings) is shown, and
   * nothing billing-related shows before Settings → Chimaera Pro.
   */
  import type { Component } from "svelte";
  import HomeNavigation from "../workspace/HomeNavigation.svelte";
  import CloudProjects from "./CloudProjects.svelte";
  import { paidPlan } from "../net/plan";
  import { loadPaneView } from "../layout/lazyViews";
  import { matchAction } from "../shared/keybindings";

  let surface = $state<"workspaces" | "settings">("workspaces");
  let settingsLoad = $state<Promise<Component<any>> | null>(null);

  function openSettings(): void {
    settingsLoad = loadPaneView("settings");
    surface = "settings";
  }
  function openHome(): void {
    surface = "workspaces";
  }
  // As on the desktop's Home: the Settings chord opens Settings, and Escape
  // leaves it unless a field or a dialog owns the key.
  function onKeydown(event: KeyboardEvent): void {
    if (event.defaultPrevented) return;
    if (matchAction(event)?.id === "settings") {
      event.preventDefault();
      openSettings();
      return;
    }
    if (surface !== "settings" || event.key !== "Escape") return;
    const target = event.target;
    if (target instanceof HTMLElement && (target.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName))) return;
    if (document.querySelector("dialog[open], [aria-modal='true']") !== null) return;
    openHome();
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#if surface === "settings"}
  <div class="home-settings-shell">
    <HomeNavigation active="settings" plan={$paidPlan} showPro={false} onHome={openHome} onPro={openSettings} onSettings={openSettings} />
    <div class="home-settings-surface">
      <nav class="home-surface-nav" aria-label="Home navigation">
        <button class="home-settings-back" onclick={openHome}>
          <svg width="16" height="16" viewBox="0 0 16 16" fill="none" aria-hidden="true"><path d="m9.5 4-4 4 4 4" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" /></svg>
          Home
        </button>
        <span class="home-surface-divider" aria-hidden="true">/</span>
        <span aria-current="page">Settings</span>
        <kbd class="home-back-hint">esc</kbd>
      </nav>
      <div class="home-settings-content">
        {#await settingsLoad}
          <p class="loading">Loading settings…</p>
        {:then SettingsView}
          {#if SettingsView}<SettingsView account />{/if}
        {:catch}
          <p class="loading" role="alert">Couldn't open settings.</p>
          <button class="retry" onclick={openSettings}>Retry</button>
        {/await}
      </div>
    </div>
  </div>
{:else}
  <div class="home">
    <HomeNavigation active="workspaces" plan={$paidPlan} showPro={false} onHome={openHome} onPro={openSettings} onSettings={openSettings} />
    <div class="inner">
      <header class="masthead">
        <div class="welcome">
          <h1>Workspaces</h1>
          <p>Pick up where you left off.</p>
        </div>
      </header>
      <CloudProjects browser />
    </div>
  </div>
{/if}

<style>
  /* The desktop Home's own layout (workspace/HomeScreen.svelte) and its
     Settings frame (App.svelte's home-settings shell), so the web reads as
     the same workbench. */
  .home { position: absolute; inset: 0; display: flex; overflow: hidden; background: var(--bg); }
  .inner { flex: 1; min-width: 0; overflow-y: auto; padding: 56px clamp(24px, 4vw, 64px) 24px; display: flex; flex-direction: column; gap: 36px; }
  .inner > :global(*) { width: 100%; max-width: 860px; margin-left: auto; margin-right: auto; box-sizing: border-box; }
  .masthead { display: flex; align-items: center; justify-content: space-between; gap: 20px; flex-wrap: wrap; margin-bottom: 8px; }
  .welcome p { margin: 8px 0 0; color: var(--muted); font-size: var(--text-md); }
  h1 { margin: 0; font-size: clamp(24px, 3vw, 30px); font-weight: 550; letter-spacing: -0.035em; }

  .home-settings-shell { position: absolute; inset: 0; display: flex; background: var(--bg); }
  .home-settings-surface { flex: 1; min-width: 0; min-height: 0; padding: 44px 24px 24px; display: flex; flex-direction: column; gap: 14px; }
  .home-surface-nav { display: flex; align-items: center; gap: 12px; min-height: 34px; font-size: var(--text-sm); color: var(--fg); }
  .home-settings-back { display: inline-flex; align-items: center; gap: 5px; color: var(--muted); background: transparent; border: 0; border-radius: 6px; padding: 8px; font: inherit; cursor: pointer; }
  .home-settings-back:hover { color: var(--fg); background: var(--row-hover); }
  .home-settings-back:focus-visible, .retry:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 2px; }
  .home-surface-divider, .home-back-hint { color: var(--muted); }
  .home-back-hint { margin-left: auto; font-size: var(--text-xs); }
  .home-settings-content { position: relative; flex: 1; min-height: 0; overflow: hidden; border: 1px solid var(--edge); border-radius: 8px; }
  .loading { margin: 24px; color: var(--muted); font-size: var(--text-sm); }
  .retry { margin: 0 24px; padding: 6px 10px; border: 1px solid var(--edge); border-radius: 6px; background: transparent; color: var(--fg); font: inherit; font-size: var(--text-sm); cursor: pointer; }

  @media (max-width: 900px) {
    .inner { padding-left: 24px; padding-right: 24px; }
  }
  /* On a phone the navigation bar above already says where you are. */
  @media (max-width: 700px) {
    .home { flex-direction: column; }
    .inner { padding: 24px 20px 20px; gap: 30px; }
    .masthead { align-items: flex-start; gap: 20px; }
    .home-settings-shell { flex-direction: column; }
    .home-settings-surface { padding: 0; gap: 0; }
    .home-surface-nav { display: none; }
    .home-settings-content { border: 0; border-radius: 0; }
  }
</style>
