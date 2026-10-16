<script lang="ts">
  /**
   * The settings surface (⌘, / gear): a VS Code-grade settings page rendered
   * entirely from the schema registry — category nav, search, typed controls,
   * modified markers — plus a JSON tab editing the settings.json ground truth
   * itself. Keybindings are ordinary rows (the keys.* settings); the few
   * spec-pinned chords render read-only at the end of the Keyboard section.
   */
  import { CATEGORIES, SETTINGS, type SettingDef } from "./schema";
  import {
    flushSettings,
    isModified,
    settingsLoaded,
    settingsSaveRejection,
    settingsSaveState,
  } from "./store.svelte";
  import SettingRow from "./SettingRow.svelte";
  import SettingsJson from "./SettingsJson.svelte";
  import AgentsSettings from "./AgentsSettings.svelte";
  import EnvironmentSettings from "./EnvironmentSettings.svelte";
  import DocumentsSettings from "./DocumentsSettings.svelte";
  import PluginsSettings from "./PluginsSettings.svelte";
  import { workspacePlugins } from "../plugins/store";
  import NotificationStatus from "./NotificationStatus.svelte";
  import UpdatesStatus from "./UpdatesStatus.svelte";
  import ActivitySettings from "./ActivitySettings.svelte";
  import { settingsJump } from "./jump";
  import { tick, untrack } from "svelte";
  import { APP_MENU, PINNED } from "../shared/keys";
  import { activeModLabel, paneMoveHint } from "../shared/keybindings";
  import { paneTabHasKeyboardFocus } from "../shared/tabNavigation";

  /** The tab is showing (kept-alive tabs stay mounted while hidden). */
  let { visible: tabVisible = true }: { visible?: boolean } = $props();

  /**
   * Nav sections: the schema categories, plus the bespoke Environment section
   * (store-backed via /api/v1/environment — it has no schema rows) slotted
   * after Agents, and the bespoke Documents section (/api/v1/agent-docs)
   * after that.
   */
  const sections = (() => {
    const out = [...CATEGORIES];
    const at = out.indexOf("Agents");
    out.splice(at >= 0 ? at + 1 : out.length, 0, "Environment", "Documents");
    // Plugins' own declared settings, after the Extensions policy.
    const ext = out.indexOf("Extensions");
    out.splice(ext >= 0 ? ext + 1 : out.length, 0, "Plugins");
    // Activity (sessions and tokens across workspaces) is read-only and
    // store-backed (/api/v1/activity), like Environment and Documents. Before
    // Keyboard: the pinned-chords block trails the list and belongs to it.
    const kb = out.indexOf("Keyboard");
    out.splice(kb >= 0 ? kb : out.length, 0, "Activity");
    return out;
  })();

  let tab = $state<"ui" | "json">("ui");
  let query = $state("");
  let activeSection = $state<string | null>(null);
  let listEl = $state<HTMLDivElement | null>(null);
  let searchEl = $state<HTMLInputElement | null>(null);
  let navEl = $state<HTMLElement | null>(null);

  const q = $derived(query.trim().toLowerCase());

  // Focus the search box when the UI tab shows (VS Code behavior).
  $effect(() => {
    const input = searchEl;
    if (!paneTabHasKeyboardFocus()) input?.focus();
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

  /** The unsaved indicator's tooltip, by why the write did not land. */
  const unsavedTitle = $derived.by(() => {
    switch (settingsSaveState()) {
      case "failed":
        return "Couldn't reach the daemon — retrying. Click to retry now.";
      case "rejected":
        return `Not saved: ${settingsSaveRejection()}. Click to send again.`;
      default:
        return null;
    }
  });
  const modifiedCount = $derived(SETTINGS.filter((d) => isModified(d.id)).length);

  /**
   * Environment has no schema rows to search over, so it participates in
   * search through its own keyword list — what a user would type looking for
   * per-session setup.
   */
  const ENV_KEYWORDS = ["environment", "prelude", "module", "conda", "micromamba", "startup", "activate", "export"];
  const envVisible = $derived(q === "" || ENV_KEYWORDS.some((k) => k.includes(q)));
  /** Same idea for the Documents section. */
  const DOC_KEYWORDS = ["documents", "markdown", "agents.md", "skill", "claude", "teach", "check"];
  const docsVisible = $derived(q === "" || DOC_KEYWORDS.some((k) => k.includes(q)));
  /** Plugins: its own word, or a plugin or setting it names. */
  const pluginsVisible = $derived(
    q === "" ||
      "plugins".includes(q) ||
      ($workspacePlugins?.plugins ?? []).some(
        (p) =>
          p.platform.settings.length > 0 &&
          (p.name.toLowerCase().includes(q) || p.platform.settings.some((s) => s.label.toLowerCase().includes(q))),
      ),
  );
  const ACTIVITY_KEYWORDS = ["activity", "usage", "sessions", "tokens", "csv", "export"];
  const activityVisible = $derived(q === "" || ACTIVITY_KEYWORDS.some((k) => k.includes(q)));

  /** Rows grouped by category, registry order, empty groups dropped. */
  const groups = $derived.by(() => {
    const out: { category: string; defs: SettingDef[] }[] = [];
    for (const cat of sections) {
      if (cat === "Environment") {
        if (envVisible) out.push({ category: cat, defs: [] });
        continue;
      }
      if (cat === "Documents") {
        if (docsVisible) out.push({ category: cat, defs: [] });
        continue;
      }
      if (cat === "Plugins") {
        if (pluginsVisible) out.push({ category: cat, defs: [] });
        continue;
      }
      if (cat === "Activity") {
        if (activityVisible) out.push({ category: cat, defs: [] });
        continue;
      }
      const defs = visible.filter((d) => d.category === cat);
      if (defs.length > 0) out.push({ category: cat, defs });
    }
    return out;
  });

  /** The pinned-chords block travels with the Keyboard section. */
  const keyboardVisible = $derived(groups.some((g) => g.category === "Keyboard"));

  /** Scroll to a section, or to one setting's row when given its id
   *  ("agents.communication.enabled" — the Mastermind panel's off state),
   *  highlighting the section that holds it. */
  function jumpTo(section: string): void {
    const el =
      listEl?.querySelector<HTMLElement>(`[data-section="${CSS.escape(section)}"]`) ??
      (listEl !== null ? document.getElementById(`setting-${section}`) : null);
    activeSection = el?.closest<HTMLElement>("[data-section]")?.dataset.section ?? section;
    el?.scrollIntoView({ block: "start", behavior: "instant" as ScrollBehavior });
  }

  /** The Agents section's own schema rows: agent communication, a titled
   *  block under the bespoke binaries panel. */
  const isCommsDef = (d: SettingDef) => d.id.startsWith("agents.communication.");

  // A section asked for from elsewhere (Quick Open "Usage", the dashboard's
  // cost line): clear any search that hides it, then scroll there once shown.
  $effect(() => {
    const want = $settingsJump;
    if (want === null || !tabVisible) return;
    untrack(() => {
      settingsJump.set(null);
      tab = "ui";
      query = "";
      void tick().then(() => jumpTo(want));
    });
  });

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
   * The spec-pinned chords — not rebindable, listed for reference. Pane focus
   * follows the base modifier; the rest shadow browser conventions too
   * carefully to be worth opening up (see keys.ts).
   */
  const pinnedRows = $derived([
    { label: "Focus pane 1–9", chord: `${activeModLabel()}1–9` },
    { label: "Move active tab to pane 1–9", chord: `${paneMoveHint(1)}–9` },
    { label: "Reference selection in agent", chord: PINNED.reference },
    { label: "Terminal text larger", chord: PINNED.fontPlus },
    { label: "Terminal text smaller", chord: PINNED.fontMinus },
    { label: "Terminal text reset", chord: PINNED.fontReset },
  ]);

  /**
   * The native app's menu-bar chords — fixed accelerators that fire only in the
   * chimaera app (a browser reserves ⌘W/⌘T/⌘N). Shown so the full map lives in
   * one place: ⌘W is why a view closes when the rebindable Close View chord is
   * elsewhere. Not reactive to keys.modifier — the menu owns concrete chords.
   */
  const appMenuRows = [
    { label: "Close view", chord: APP_MENU.closeView },
    { label: "New terminal", chord: APP_MENU.newTerminal },
    { label: "New agent", chord: APP_MENU.newAgent },
    { label: "New window", chord: APP_MENU.newWindow },
    { label: "Reload window", chord: APP_MENU.reloadWindow },
  ];
</script>

<div class="settings">
  <!-- One header row in focus order: search, then the UI/JSON switch. The
       settings.json path lives with the JSON editor, where it means something. -->
  <header class="top">
    <h1 class="title">Settings</h1>
    {#if tab === "ui"}
      <input
        class="search"
        type="text"
        placeholder="Search settings"
        bind:value={query}
        bind:this={searchEl}
        aria-label="search settings"
        onkeydown={(e) => {
          // Escape clears a query first; the next one leaves — App closes
          // Home's Settings page only over an empty field, and a text input
          // never clears itself on Escape (only type=search does).
          if (e.key === "Escape" && query !== "") {
            e.preventDefault();
            e.stopPropagation();
            query = "";
          }
        }}
      />
    {/if}
    <span class="status">
      {#if unsavedTitle !== null}
        <!-- Only a failed or refused write shows: the debounce and the PUT
             itself are too brief to be worth a flicker. A click re-sends now. -->
        <button class="unsaved" title={unsavedTitle} onclick={() => void flushSettings()}>
          <span class="dot" aria-hidden="true"></span>unsaved
        </button>
      {/if}
      {#if !settingsLoaded()}
        loading…
      {:else if modifiedCount > 0}
        <span title="settings that differ from their default">{modifiedCount} changed</span>
      {/if}
    </span>
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
  </header>

  {#if tab === "ui"}
    <div class="body">
      <nav class="nav" aria-label="setting categories" bind:this={navEl}>
        {#each sections as section (section)}
          {@const shown = groups.some((g) => g.category === section)}
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
          {#if group.category === "Agents"}
            <!-- Bespoke panel: fuses live daemon detection with the
                 agents.<id>.path settings and an uninstall action. It renders
                 its own <h2>, so its generic rows are skipped here. Agent
                 communication's two rows follow as ordinary schema rows. -->
            {@const comms = group.defs.filter(isCommsDef)}
            <section data-section={group.category}>
              {#if q === "" || group.defs.some((d) => !isCommsDef(d))}
                <AgentsSettings visible={tabVisible} />
              {:else}
                <h2 class="cat">{group.category}</h2>
              {/if}
              {#if comms.length > 0}
                <h3 class="subcat">Agent communication</h3>
                <p class="subnote">
                  Let agents share progress and ask each other for help.
                </p>
                {#each comms as def (def.id)}
                  <SettingRow {def} />
                {/each}
              {/if}
            </section>
          {:else if group.category === "Environment"}
            <!-- Bespoke panel: edits the daemon's prelude map over
                 /api/v1/environment (env-profiles.json), not settings.json.
                 It renders its own <h2>; there are no generic rows. -->
            <section data-section={group.category}>
              <EnvironmentSettings />
            </section>
          {:else if group.category === "Documents"}
            <!-- Bespoke panel: the opt-in "teach agents" installs over
                 /api/v1/agent-docs. It renders its own <h2>. -->
            <section data-section={group.category}>
              <DocumentsSettings />
            </section>
          {:else if group.category === "Plugins"}
            <!-- Bespoke panel: each installed plugin's declared settings,
                 kept by the daemon per plugin (not settings.json). -->
            <section data-section={group.category}>
              <PluginsSettings query={"plugins".includes(q) ? "" : q} />
            </section>
          {:else if group.category === "Activity"}
            <!-- Bespoke panel: sessions, tokens and time from the session
                 records (/api/v1/activity), every workspace. Its own <h2>. -->
            <section data-section={group.category}>
              <ActivitySettings visible={tabVisible} />
            </section>
          {:else if group.category === "Notifications"}
            <!-- Generic rows plus a status line: whether the OS/browser will
                 show the alerts at all, with the action that changes it. -->
            <section data-section={group.category}>
              <h2 class="cat">{group.category}</h2>
              <NotificationStatus />
              {#each group.defs as def (def.id)}
                <SettingRow {def} />
              {/each}
            </section>
          {:else if group.category === "Updates"}
            <!-- The status block answers "is there an update?" for the app,
                 the daemon and the agents (it renders its own <h2> with a
                 "check now"); the auto-check switch is the generic row. -->
            <section data-section={group.category}>
              <UpdatesStatus onJump={jumpTo} visible={tabVisible} />
              {#each group.defs as def (def.id)}
                <SettingRow {def} />
              {/each}
            </section>
          {:else}
            <section data-section={group.category}>
              <h2 class="cat">{group.category}</h2>
              {#each group.defs as def (def.id)}
                <SettingRow {def} />
              {/each}
            </section>
          {/if}
        {/each}

        {#if keyboardVisible}
          <div class="kbd-group">
            <h3 class="kbd-group-title">Fixed chords</h3>
            <ul class="kbd-list">
              {#each pinnedRows as row (row.label)}
                <li class="kbd-row">
                  <span class="kbd-label">{row.label}</span>
                  <kbd class="kbd-pill">{row.chord}</kbd>
                </li>
              {/each}
            </ul>
          </div>

          <div class="kbd-group">
            <h3 class="kbd-group-title">App menu</h3>
            <p class="kbd-note">Only in the chimaera app: a browser keeps these chords.</p>
            <ul class="kbd-list">
              {#each appMenuRows as row (row.label)}
                <li class="kbd-row">
                  <span class="kbd-label">{row.label}</span>
                  <kbd class="kbd-pill">{row.chord}</kbd>
                </li>
              {/each}
            </ul>
          </div>
        {/if}

        {#if groups.length === 0}
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

  /* Title, search, status, mode switch: one line, aligned to the nav and the
     reading column below it. */
  .top {
    flex: none;
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 16px 22px 14px;
  }

  .title {
    flex: none;
    width: 148px;
    margin: 0;
    font-size: var(--text-lg);
    font-weight: 600;
  }

  .search {
    flex: 0 1 380px;
    min-width: 0;
    height: 30px;
    box-sizing: border-box;
    font: inherit;
    font-size: var(--text-md);
    color: var(--fg);
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 7px;
    padding: 0 11px;
  }

  .search:focus {
    outline: 2px solid var(--focus-ring);
    outline-offset: 1px;
  }

  .status {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: var(--text-sm);
    color: var(--muted);
    white-space: nowrap;
  }

  .unsaved {
    appearance: none;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    border: 0;
    background: none;
    padding: 2px 6px;
    border-radius: 5px;
    font: inherit;
    color: var(--warn);
    cursor: pointer;
  }

  .unsaved:hover {
    background: color-mix(in srgb, var(--warn) 10%, transparent);
  }

  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: currentColor;
  }

  .tabs {
    flex: none;
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
    letter-spacing: 0.04em;
    color: var(--muted);
    padding: 3px 12px;
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

  .body {
    flex: 1;
    display: flex;
    min-height: 0;
  }

  .nav {
    flex: none;
    width: 162px;
    box-sizing: border-box;
    padding: 4px 8px 14px 14px;
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
    border-radius: 6px;
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
    padding: 0 22px 40vh 8px;
    scroll-padding-top: 4px;
  }

  /* A reading column, like the chat's: on a wide Home page the controls stay
     within reach of their labels instead of drifting to the window edge. */
  .list > :global(*) {
    max-width: 880px;
  }

  /* Every section heading — these and the bespoke panels' own (Agents,
     Environment, Updates, …) — share one recipe: a real title in the
     foreground, spacing above it doing the separating. */
  .cat {
    margin: 30px 0 6px;
    padding: 0 14px;
    font-size: var(--text-lg);
    font-weight: 600;
    color: var(--fg);
  }

  .list > section:first-child :global(h2) {
    margin-top: 4px;
  }

  /* A titled block inside a section (Agents → Agent communication). */
  .subcat {
    margin: 22px 0 0;
    padding: 0 14px;
    font-size: var(--text-md);
    font-weight: 600;
    color: var(--fg);
  }
  .subnote {
    margin: 2px 0 2px;
    padding: 0 14px;
    font-size: var(--text-sm);
    line-height: 1.45;
    color: var(--muted);
    max-width: 62ch;
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

  .kbd-group {
    margin: 22px 0 0;
    padding: 0 14px;
  }

  .kbd-group-title {
    margin: 0;
    font-size: var(--text-md);
    font-weight: 600;
    color: var(--fg);
  }

  .kbd-note {
    margin: 2px 0 0;
    font-size: var(--text-sm);
    color: var(--muted);
    max-width: 62ch;
  }

  .kbd-list {
    list-style: none;
    margin: 6px 0 0;
    padding: 0;
  }

  .kbd-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: 4px 16px;
    min-height: 32px;
    padding: 2px 20px 2px 0;
    font-size: var(--text-md);
  }

  .kbd-row + .kbd-row {
    border-top: 1px solid color-mix(in srgb, var(--edge) 70%, transparent);
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

  /* Narrow: the category rail folds into a horizontal strip pinned above
     the list, and the title yields its fixed width. */
  @container settings (max-width: 640px) {
    .top {
      flex-wrap: wrap;
      gap: 10px;
      padding: 14px 14px 10px;
    }

    .title {
      width: auto;
    }

    .search {
      order: 5;
      flex: 1 1 100%;
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
      padding: 0 10px;
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
      padding: 0 8px 40vh 8px;
    }

    .list > section:first-child :global(h2) {
      margin-top: 14px;
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
