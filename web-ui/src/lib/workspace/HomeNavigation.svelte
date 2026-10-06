<script lang="ts">
  import BrandMark from "../shared/BrandMark.svelte";
  import { keyHintSuffix } from "../shared/keybindings";

  let { active, onHome, onSettings }: {
    active: "workspaces" | "settings";
    onHome: () => void;
    onSettings: () => void;
  } = $props();
</script>

<nav class="home-navigation" aria-label="Chimaera">
  <div class="brand"><BrandMark size={23} title="Chimaera" /><span>chimaera</span></div>
  <button class="home-link" class:active={active === "workspaces"} aria-current={active === "workspaces" ? "page" : undefined} onclick={onHome}>
    <svg viewBox="0 0 18 18" width="17" height="17" aria-hidden="true"><rect x="2.5" y="3" width="13" height="12" rx="2" fill="none" stroke="currentColor" stroke-width="1.3" /><path d="M2.5 7h13M7 7v8" fill="none" stroke="currentColor" stroke-width="1.3" /></svg>
    <span>Workspaces</span>
  </button>
  <div class="utilities">
    <button class:active={active === "settings"} aria-current={active === "settings" ? "page" : undefined} onclick={onSettings} title={`Settings${keyHintSuffix("settings")}`}>
      <svg viewBox="0 0 18 18" width="17" height="17" aria-hidden="true"><path d="M3 5h12M3 13h12" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" /><circle cx="6" cy="5" r="2" fill="var(--rail-bg)" stroke="currentColor" stroke-width="1.3" /><circle cx="12" cy="13" r="2" fill="var(--rail-bg)" stroke="currentColor" stroke-width="1.3" /></svg>
      <span>Settings</span>
    </button>
  </div>
</nav>

<style>
  .home-navigation { width: 200px; flex: 0 0 200px; align-self: stretch; display: flex; flex-direction: column; gap: 30px; padding: 54px 12px 20px; box-sizing: border-box; border-right: 1px solid var(--edge); background: var(--rail-bg); }
  .brand { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; padding: 0 8px; color: var(--fg); }
  .brand > span { font-size: 18px; letter-spacing: -.035em; font-weight: 550; }
  button { display: flex; align-items: center; gap: 10px; width: 100%; padding: 10px; border: 0; border-radius: 6px; background: transparent; color: var(--muted); font: inherit; font-size: var(--text-sm); text-align: left; cursor: pointer; white-space: nowrap; }
  button svg { flex: none; }
  button:hover { color: var(--fg); background: var(--row-hover); }
  button.active { background: var(--row-active); color: var(--fg); }
  button:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 2px; }
  .utilities { margin-top: auto; display: flex; flex-direction: column; gap: 4px; }
  @media (max-width: 700px) {
    .home-navigation { width: 100%; flex: none; display: grid; grid-template-columns: 1fr auto; gap: 14px 8px; padding: 14px 16px 10px; border-right: 0; border-bottom: 1px solid var(--edge); }
    /* Only the macOS overlay titlebar needs clearance for its traffic lights. */
    :global(.native-titlebar-overlay) .home-navigation { padding-top: 48px; }
    .brand { padding: 0; }
    .home-link { grid-row: 2; width: fit-content; }
    .utilities { grid-row: 2; grid-column: 2; margin: 0; flex-direction: row; gap: 2px; }
    .utilities button { width: auto; padding: 9px 8px; gap: 6px; }
    .utilities button svg { display: none; }
    button { min-height: 40px; }
  }
  @media (max-width: 380px) {
    .home-navigation { padding-left: 12px; padding-right: 12px; }
    button { font-size: var(--text-xs); gap: 7px; padding: 9px 8px; }
  }
</style>
