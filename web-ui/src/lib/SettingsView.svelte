<script lang="ts">
  /**
   * The settings surface (⌘, / gear): a VS Code-grade settings page rendered
   * entirely from the schema registry — category nav, search, typed controls,
   * modified markers — plus a JSON tab editing the settings.json ground truth
   * itself, and a read-only keyboard reference (chords come from keys.ts).
   */
  import { CATEGORIES, SETTINGS, type SettingDef } from "./settings/schema";
  import { isModified, settingsLoaded } from "./settings/store.svelte";
  import SettingRow from "./settings/SettingRow.svelte";
  import SettingsJson from "./settings/SettingsJson.svelte";
  import { KEYS } from "./keys";

  const KEYBOARD = "Keyboard";
  /** Nav sections: schema categories + the keyboard reference. */
  const sections = [...CATEGORIES, KEYBOARD];

  let tab = $state<"ui" | "json">("ui");
  let query = $state("");
  let activeSection = $state<string | null>(null);
  let listEl = $state<HTMLDivElement | null>(null);
  let searchEl = $state<HTMLInputElement | null>(null);

  const q = $derived(query.trim().toLowerCase());

  // Focus the search box when the UI tab shows (VS Code behavior).
  $effect(() => {
    searchEl?.focus();
  });

  function matches(def: SettingDef): boolean {
    if (q === "") return true;
    return (
      def.title.toLowerCase().includes(q) ||
      def.id.toLowerCase().includes(q) ||
      def.category.toLowerCase().includes(q) ||
      def.description.toLowerCase().includes(q)
    );
  }

  const visible = $derived(SETTINGS.filter(matches));
  const modifiedCount = $derived(SETTINGS.filter((d) => isModified(d.id)).length);

  /** Rows grouped by category, registry order, empty groups dropped. */
  const groups = $derived.by(() => {
    const out: { category: string; defs: SettingDef[] }[] = [];
    for (const cat of CATEGORIES) {
      const defs = visible.filter((d) => d.category === cat);
      if (defs.length > 0) out.push({ category: cat, defs });
    }
    return out;
  });

  const keyboardVisible = $derived(q === "" || KEYBOARD.toLowerCase().includes(q));

  function jumpTo(section: string): void {
    activeSection = section;
    const el = listEl?.querySelector<HTMLElement>(`[data-section="${section}"]`);
    el?.scrollIntoView({ block: "start", behavior: "instant" as ScrollBehavior });
  }

  /** Keep the nav highlight on the topmost visible section while scrolling. */
  function onScroll(): void {
    const list = listEl;
    if (list === null) return;
    const top = list.getBoundingClientRect().top + 60;
    let current: string | null = null;
    for (const el of list.querySelectorAll<HTMLElement>("[data-section]")) {
      if (el.getBoundingClientRect().top <= top) current = el.dataset.section ?? null;
    }
    activeSection = current ?? groups[0]?.category ?? null;
  }

  /** The chord reference: label + display string (chords live in keys.ts). */
  const chordDocs: { label: string; chord: string }[] = [
    { label: "Open settings", chord: KEYS.settings },
    { label: "Open folder picker", chord: KEYS.picker },
    { label: "Quick open (files + sessions)", chord: KEYS.quickOpen },
    { label: "Open session 1–9", chord: KEYS.openN },
    { label: "New terminal", chord: KEYS.newTerminal },
    { label: "New agent", chord: KEYS.newAgent },
    { label: "Split right", chord: KEYS.splitRight },
    { label: "Split down", chord: KEYS.splitDown },
    { label: "Close view", chord: KEYS.closeView },
    { label: "Zoom pane", chord: KEYS.zoom },
    { label: "Focus mode (hide sidebar)", chord: KEYS.focusMode },
    { label: "Move pane focus", chord: KEYS.focusArrows },
    { label: "Cycle tabs", chord: KEYS.cycleTabs },
    { label: "Reference selection in agent", chord: KEYS.reference },
    { label: "Terminal text larger", chord: KEYS.fontPlus },
    { label: "Terminal text smaller", chord: KEYS.fontMinus },
    { label: "Terminal text reset", chord: KEYS.fontReset },
  ];
</script>

<div class="settings">
  <header class="top">
    <div class="title-row">
      <h1 class="title">Settings</h1>
      <div class="tabs" role="tablist" aria-label="settings mode">
        <button class="mode" class:on={tab === "ui"} role="tab" aria-selected={tab === "ui"} onclick={() => (tab = "ui")}>
          UI
        </button>
        <button
          class="mode"
          class:on={tab === "json"}
          role="tab"
          aria-selected={tab === "json"}
          onclick={() => (tab = "json")}
        >
          JSON
        </button>
      </div>
    </div>
    <p class="subtitle">
      Ground truth: <code>~/.config/chimaera/settings.json</code> on the daemon host — hand-edits
      and other windows sync here live.
      {#if modifiedCount > 0}
        <span class="mod-count">{modifiedCount} modified</span>
      {/if}
      {#if !settingsLoaded()}
        <span class="loading">loading…</span>
      {/if}
    </p>
    {#if tab === "ui"}
      <input
        class="search"
        type="text"
        placeholder="Search settings"
        bind:value={query}
        bind:this={searchEl}
        aria-label="search settings"
      />
    {/if}
  </header>

  {#if tab === "ui"}
    <div class="body">
      <nav class="nav" aria-label="setting categories">
        {#each sections as section (section)}
          {@const shown =
            section === KEYBOARD
              ? keyboardVisible
              : groups.some((g) => g.category === section)}
          {#if shown}
            <button
              class="nav-item"
              class:on={activeSection === section}
              onclick={() => jumpTo(section)}
            >
              {section}
            </button>
          {/if}
        {/each}
      </nav>

      <div class="list" bind:this={listEl} onscroll={onScroll}>
        {#each groups as group (group.category)}
          <section data-section={group.category}>
            <h2 class="cat">{group.category}</h2>
            {#each group.defs as def (def.id)}
              <SettingRow {def} />
            {/each}
          </section>
        {/each}

        {#if keyboardVisible}
          <section data-section={KEYBOARD}>
            <h2 class="cat">{KEYBOARD}</h2>
            <p class="kbd-note">
              Platform-aware chords — the terminal owns bare Ctrl on every platform. Rebinding
              lands with <code>~/.config/chimaera/keys.toml</code>.
            </p>
            <table class="kbd-table">
              <tbody>
                {#each chordDocs as row (row.label)}
                  <tr>
                    <td class="kbd-label">{row.label}</td>
                    <td class="kbd-chord"><kbd>{row.chord}</kbd></td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </section>
        {/if}

        {#if groups.length === 0 && !keyboardVisible}
          <div class="empty">no settings match “{query}”</div>
        {/if}
      </div>
    </div>
  {:else}
    <div class="json-body">
      <SettingsJson />
    </div>
  {/if}
</div>

<style>
  .settings {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    background: var(--term-bg);
  }

  .top {
    flex: none;
    padding: 18px 22px 12px;
    border-bottom: 1px solid var(--edge);
  }

  .title-row {
    display: flex;
    align-items: center;
    gap: 14px;
  }

  .title {
    margin: 0;
    font-size: 17px;
    font-weight: 600;
    letter-spacing: 0.01em;
  }

  .tabs {
    display: flex;
    border: 1px solid var(--edge);
    border-radius: 7px;
    overflow: hidden;
  }

  .mode {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    letter-spacing: 0.05em;
    color: var(--muted);
    padding: 2px 12px;
    cursor: pointer;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .mode + .mode {
    border-left: 1px solid var(--edge);
  }

  .mode:hover {
    color: var(--fg);
    background: var(--row-hover);
  }

  .mode.on {
    color: var(--fg);
    font-weight: 600;
    background: color-mix(in srgb, var(--accent) 14%, transparent);
  }

  .subtitle {
    margin: 5px 0 0;
    font-size: var(--text-sm);
    color: var(--muted);
  }

  .subtitle code {
    font-family: var(--mono);
    font-size: var(--text-xs);
  }

  .mod-count {
    margin-left: 8px;
    color: var(--accent);
  }

  .loading {
    margin-left: 8px;
    opacity: 0.7;
  }

  .search {
    margin-top: 12px;
    width: min(440px, 100%);
    font: inherit;
    font-size: var(--text-md);
    color: var(--fg);
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 8px;
    padding: 6px 12px;
  }

  .search:focus {
    outline: 2px solid var(--focus-ring);
    outline-offset: 1px;
  }

  .body {
    flex: 1;
    display: flex;
    min-height: 0;
  }

  .nav {
    flex: none;
    width: 148px;
    padding: 14px 8px 14px 14px;
    display: flex;
    flex-direction: column;
    gap: 1px;
    overflow-y: auto;
  }

  .nav-item {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-md);
    color: var(--muted);
    text-align: left;
    padding: 4px 9px;
    border-radius: 5px;
    cursor: pointer;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .nav-item:hover {
    color: var(--fg);
    background: var(--row-hover);
  }

  .nav-item.on {
    color: var(--fg);
    font-weight: 600;
    background: var(--row-active);
  }

  .list {
    flex: 1;
    min-width: 0;
    overflow-y: auto;
    padding: 6px 22px 40vh 8px;
    scroll-padding-top: 8px;
  }

  .cat {
    margin: 18px 0 4px;
    padding: 0 14px;
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--muted);
  }

  .empty {
    padding: 40px 14px;
    color: var(--muted);
    font-size: var(--text-md);
  }

  .json-body {
    flex: 1;
    position: relative;
    min-height: 0;
  }

  /* --- keyboard reference --- */

  .kbd-note {
    margin: 4px 0 10px;
    padding: 0 14px;
    font-size: var(--text-sm);
    color: var(--muted);
    max-width: 60ch;
  }

  .kbd-note code {
    font-family: var(--mono);
    font-size: var(--text-xs);
  }

  .kbd-table {
    margin: 0 14px;
    border-collapse: collapse;
  }

  .kbd-table td {
    padding: 4px 0;
    font-size: var(--text-md);
  }

  .kbd-label {
    color: var(--fg);
    padding-right: 36px;
  }

  .kbd-chord kbd {
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    padding: 1px 6px;
    border: 1px solid var(--edge);
    border-radius: 4px;
    background: none;
    white-space: nowrap;
  }
</style>
