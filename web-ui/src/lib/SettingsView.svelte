<script lang="ts">
  /**
   * The settings surface (⌘, / gear): a VS Code-grade settings page rendered
   * entirely from the schema registry — category nav, search, typed controls,
   * modified markers — plus a JSON tab editing the settings.json ground truth
   * itself, and a read-only keyboard reference (chords come from keys.ts).
   */
  import { onMount } from "svelte";
  import { CATEGORIES, SETTINGS, type SettingDef } from "./settings/schema";
  import { isModified, settingsLoaded } from "./settings/store.svelte";
  import SettingRow from "./settings/SettingRow.svelte";
  import SettingsJson from "./settings/SettingsJson.svelte";
  import { KEYS, isAppChord, isLayer2, chordDigit, fontChord } from "./keys";

  const KEYBOARD = "Keyboard";
  /** Nav sections: schema categories + the keyboard reference. */
  const sections = [...CATEGORIES, KEYBOARD];

  let tab = $state<"ui" | "json">("ui");
  let query = $state("");
  let activeSection = $state<string | null>(null);
  let listEl = $state<HTMLDivElement | null>(null);
  let searchEl = $state<HTMLInputElement | null>(null);
  let navEl = $state<HTMLElement | null>(null);
  /** The keyboard row lit by a live chord press (id from chordGroups). */
  let litId = $state<string | null>(null);

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

  // When the nav is a horizontal chip bar (narrow), keep the active category
  // scrolled into view so the highlight is never off-screen. Only the nav
  // itself scrolls here (siblings/ancestors don't overflow), so this is safe.
  $effect(() => {
    const active = activeSection;
    if (active === null || navEl === null) return;
    navEl
      .querySelector<HTMLElement>(`[data-nav="${active}"]`)
      ?.scrollIntoView({ block: "nearest", inline: "nearest" });
  });

  /**
   * The chord reference, grouped for scannability. `id` matches the keys.ts
   * field and drives the live-highlight below; labels/chords are unchanged.
   */
  const chordGroups: {
    label: string;
    rows: { id: string; label: string; chord: string }[];
  }[] = [
    {
      label: "Navigation & panels",
      rows: [
        { id: "settings", label: "Open settings", chord: KEYS.settings },
        { id: "picker", label: "Open folder picker", chord: KEYS.picker },
        { id: "quickOpen", label: "Quick open (files + sessions)", chord: KEYS.quickOpen },
        { id: "openN", label: "Open session 1–9", chord: KEYS.openN },
        { id: "cycleTabs", label: "Cycle tabs", chord: KEYS.cycleTabs },
        { id: "focusArrows", label: "Move pane focus", chord: KEYS.focusArrows },
        { id: "focusMode", label: "Focus mode (hide sidebar)", chord: KEYS.focusMode },
      ],
    },
    {
      label: "Sessions & layout",
      rows: [
        { id: "newTerminal", label: "New terminal", chord: KEYS.newTerminal },
        { id: "newAgent", label: "New agent", chord: KEYS.newAgent },
        { id: "splitRight", label: "Split right", chord: KEYS.splitRight },
        { id: "splitDown", label: "Split down", chord: KEYS.splitDown },
        { id: "closeView", label: "Close view", chord: KEYS.closeView },
        { id: "zoom", label: "Zoom pane", chord: KEYS.zoom },
      ],
    },
    {
      label: "Selection & terminal",
      rows: [
        { id: "reference", label: "Reference selection in agent", chord: KEYS.reference },
        { id: "fontPlus", label: "Terminal text larger", chord: KEYS.fontPlus },
        { id: "fontMinus", label: "Terminal text smaller", chord: KEYS.fontMinus },
        { id: "fontReset", label: "Terminal text reset", chord: KEYS.fontReset },
      ],
    },
  ];

  /**
   * Live-learning touch: match a real chord press to its reference row so it
   * briefly lights up. We reuse keys.ts's modifier logic verbatim so the
   * mapping stays honest, and only claim the unambiguous chords — the
   * arrow/bracket ones are skipped rather than guessed.
   */
  function matchChord(e: KeyboardEvent): string | null {
    const font = fontChord(e); // Cmd/Ctrl with +, −, 0 → terminal font size
    if (font === 1) return "fontPlus";
    if (font === -1) return "fontMinus";
    if (font === 0) return "fontReset";

    if (!isAppChord(e)) return null;
    const layer2 = isLayer2(e);

    if (chordDigit(e) !== null && !layer2) return "openN";

    switch (e.code) {
      case "Comma":
        return layer2 ? null : "settings";
      case "KeyO":
        return layer2 ? null : "picker";
      case "KeyP":
        return layer2 ? null : "quickOpen";
      case "KeyE":
        return layer2 ? "newAgent" : "newTerminal";
      case "KeyD":
        return layer2 ? "splitDown" : "splitRight";
      case "KeyB":
        return layer2 ? null : "focusMode";
      case "KeyR":
        return "reference"; // R only ever means reference (layer differs per platform)
      case "Backspace":
        return layer2 ? null : "closeView";
      case "Enter":
        return layer2 ? "zoom" : null;
      default:
        return null;
    }
  }

  onMount(() => {
    let clear: ReturnType<typeof setTimeout> | undefined;
    function onKey(e: KeyboardEvent): void {
      if (tab !== "ui" || !keyboardVisible) return; // nothing on screen to light up
      const id = matchChord(e);
      if (id === null) return;
      litId = id;
      clearTimeout(clear);
      clear = setTimeout(() => (litId = null), 1000);
    }
    // Capture so the chord registers even if a pane handler stops propagation;
    // we never preventDefault, so the real command still fires underneath.
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      clearTimeout(clear);
    };
  });
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
      <nav class="nav" aria-label="setting categories" bind:this={navEl}>
        {#each sections as section (section)}
          {@const shown =
            section === KEYBOARD
              ? keyboardVisible
              : groups.some((g) => g.category === section)}
          {#if shown}
            <button
              class="nav-item"
              class:on={activeSection === section}
              data-nav={section}
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
              lands with <code>~/.config/chimaera/keys.toml</code>. Press one to see it light up.
            </p>
            {#each chordGroups as group (group.label)}
              <div class="kbd-group">
                <h3 class="kbd-group-title">{group.label}</h3>
                <ul class="kbd-list">
                  {#each group.rows as row (row.id)}
                    <li class="kbd-row" class:lit={litId === row.id}>
                      <span class="kbd-label">{row.label}</span>
                      <kbd class="kbd-pill">{row.chord}</kbd>
                    </li>
                  {/each}
                </ul>
              </div>
            {/each}
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
    /* The pane width (not the viewport) drives every breakpoint below. A named
       container so the child SettingRow can query the same context. */
    container: settings / inline-size;
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

  /* The UI/JSON mode switch lives in the header's far corner — next to the
     h1 it read as part of the title. */
  .title-row .tabs {
    margin-left: auto;
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
    /* The settings.json path is one long token — let it break when narrow. */
    overflow-wrap: anywhere;
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
    overflow-wrap: anywhere;
  }

  .kbd-group {
    margin: 14px 14px 0;
  }

  .kbd-group:first-of-type {
    margin-top: 8px;
  }

  .kbd-group-title {
    margin: 0 0 5px;
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.06em;
    color: var(--muted);
  }

  .kbd-list {
    list-style: none;
    margin: 0;
    padding: 0;
    border: 1px solid var(--edge);
    border-radius: 8px;
    overflow: hidden;
  }

  .kbd-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: 4px 16px;
    padding: 6px 12px;
    font-size: var(--text-md);
    /* Slow default transition owns the fade-out; .lit owns the quick fade-in. */
    transition: background-color 0.5s ease;
  }

  .kbd-row + .kbd-row {
    border-top: 1px solid var(--edge);
  }

  /* Live-learning flash: a soft accent wash that fades after ~1s. */
  .kbd-row.lit {
    background: color-mix(in srgb, var(--accent) 13%, transparent);
    transition: background-color 0.15s ease;
  }

  .kbd-label {
    color: var(--fg);
    min-width: 0;
  }

  .kbd-pill {
    flex: none;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    padding: 2px 7px;
    border: 1px solid var(--edge);
    border-radius: 5px;
    background: var(--term-bg);
    white-space: nowrap;
  }

  /* --- responsive: the settings pane can be dragged narrow (down to ~200px).
     Everything below keys off the pane width via the `settings` container. --- */

  /* Narrow: the category rail folds into a horizontal chip bar pinned above
     the list, and the search spans the full width. */
  @container settings (max-width: 640px) {
    .top {
      padding: 16px 14px 12px;
    }

    .title-row {
      flex-wrap: wrap;
    }

    .search {
      width: 100%;
    }

    .body {
      flex-direction: column;
    }

    /* The rail folds into a quiet text tab strip (the pane-tab language, not
       buttons): muted labels, the active one carrying an accent underline
       that sits on the strip's own hairline. */
    .nav {
      width: auto;
      flex-direction: row;
      gap: 2px;
      padding: 4px 10px 0;
      overflow-x: auto;
      overflow-y: hidden;
      border-bottom: 1px solid var(--edge);
      /* the strip scrolls by wheel/drag; the scrollbar itself would be noise */
      scrollbar-width: none;
    }

    .nav::-webkit-scrollbar {
      display: none;
    }

    .nav-item {
      flex: none;
      white-space: nowrap;
      font-size: var(--text-sm);
      padding: 4px 9px 7px;
      border-radius: 0;
      background: none;
    }

    .nav-item:hover {
      background: none;
      color: var(--fg);
    }

    .nav-item.on {
      background: none;
      box-shadow: inset 0 -2px 0 var(--accent);
    }

    .list {
      padding: 6px 12px 40vh 12px;
    }

    .cat {
      padding: 0 6px;
    }

    .kbd-note {
      padding: 0 6px;
    }

    .kbd-group {
      margin-left: 6px;
      margin-right: 6px;
    }
  }

  /* Very narrow: let long non-mac chords wrap inside their pill so a sliver of
     a pane still never overflows horizontally. */
  @container settings (max-width: 360px) {
    .kbd-pill {
      white-space: normal;
      overflow-wrap: anywhere;
    }

    /* Min-width floor: below ~200px the sections keep a readable width and the
       list scrolls (vertically as always, horizontally only past the floor)
       rather than crushing content into slivers. The body itself never scrolls
       — it is overflow:hidden — so no page-level horizontal scrollbar appears. */
    .list section {
      min-width: 200px;
    }
  }
</style>
