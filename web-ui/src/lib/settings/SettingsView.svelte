<script lang="ts">
  /**
   * The settings surface (⌘, / gear): a VS Code-grade settings page rendered
   * entirely from the schema registry — category nav, search, typed controls,
   * modified markers — plus a JSON tab editing the settings.json ground truth
   * itself. Keybindings are ordinary rows (the keys.* settings); the few
   * spec-pinned chords render read-only at the end of the Keyboard section.
   */
  import { CATEGORIES, SETTINGS, type SettingDef } from "./schema";
  import { isModified, settingsLoaded } from "./store.svelte";
  import SettingRow from "./SettingRow.svelte";
  import SettingsJson from "./SettingsJson.svelte";
  import AgentsSettings from "./AgentsSettings.svelte";
  import EnvironmentSettings from "./EnvironmentSettings.svelte";
  import DocumentsSettings from "./DocumentsSettings.svelte";
  import CloudSetup from "./CloudSetup.svelte";
  import BrowserAccount from "../pro/BrowserAccount.svelte";
  import { isBrowserGateway } from "../net/base";
  import { isNativeShell } from "../net/native";
  import BrandMark from "../shared/BrandMark.svelte";
  import { accountPlan, proOffered } from "../net/plan";
  import { isCloudMachine } from "../pro/cloudTransport";
  import { pageVisible } from "../shared/visibility";
  import PluginsSettings from "./PluginsSettings.svelte";
  import { workspacePlugins } from "../plugins/store";
  import NotificationStatus from "./NotificationStatus.svelte";
  import UpdatesStatus from "./UpdatesStatus.svelte";
  import { updateState } from "../workspace/update.svelte";
  import ActivitySettings from "./ActivitySettings.svelte";
  import { settingsJump } from "./jump";
  import { tick, untrack } from "svelte";
  import { APP_MENU, PINNED } from "../shared/keys";
  import { activeModLabel } from "../shared/keybindings";
  import { paneTabHasKeyboardFocus } from "../shared/tabNavigation";

  /** The tab is showing (kept-alive tabs stay mounted while hidden).
   *  `account`: the web's Home, the account's own page. It has no daemon, so
   *  Settings is only Chimaera Pro, whose plan, usage and sign-out show in
   *  place (`pro/BrowserAccount.svelte`). */
  let { visible: tabVisible = true, account = false }: { visible?: boolean; account?: boolean } = $props();

  /**
   * Nav sections: the schema categories, plus the bespoke Environment section
   * (store-backed via /api/v1/environment — it has no schema rows) slotted
   * after Agents, and the bespoke Documents section (/api/v1/agent-docs)
   * after that.
   */
  const sections = (() => {
    if (account) return ["Chimaera Pro"];
    const out = [...CATEGORIES];
    const at = out.indexOf("Agents");
    out.splice(at >= 0 ? at + 1 : out.length, 0, "Environment", "Documents");
    if (isBrowserGateway()) out.splice(out.indexOf("Keyboard"), 0, "Cloud");
    // Plugins' own declared settings, after the Extensions policy.
    const ext = out.indexOf("Extensions");
    out.splice(ext >= 0 ? ext + 1 : out.length, 0, "Plugins");
    // Activity (sessions and tokens across workspaces) is read-only and
    // store-backed (/api/v1/activity), like Environment and Documents. Before
    // Keyboard: the pinned-chords block trails the list and belongs to it.
    const kb = out.indexOf("Keyboard");
    out.splice(kb >= 0 ? kb : out.length, 0, "Activity");
    // Pro is an optional add-on, so it follows every working section rather
    // than opening Settings (and its nav) with an account entry.
    if (isNativeShell() || isBrowserGateway()) out.push("Chimaera Pro");
    return out;
  })();

  /** The Cloud section belongs to a cloud machine's own page. Another daemon
   * behind the gateway (a laptop being viewed) has no cloud status to show. */
  let cloudMachine = $state(false);
  $effect(() => {
    if (!isBrowserGateway() || cloudMachine || !tabVisible || !$pageVisible) return;
    const controller = new AbortController();
    void isCloudMachine(controller.signal)
      .then((value) => { if (!controller.signal.aborted) cloudMachine = value; })
      .catch(() => { /* Passive; the next time Settings shows it asks again. */ });
    return () => controller.abort();
  });

  let tab = $state<"ui" | "json">("ui");
  let query = $state("");
  let activeSection = $state<string | null>(null);
  let listEl = $state<HTMLDivElement | null>(null);
  let searchEl = $state<HTMLInputElement | null>(null);
  let navEl = $state<HTMLElement | null>(null);

  const q = $derived(query.trim().toLowerCase());
  const paid = $derived($accountPlan === "pro" || $accountPlan === "max");
  const proAction = $derived($accountPlan === "free" ? "Get Pro" : "View account");

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

  const PRO_KEYWORDS = ["chimaera pro", "chimaera max", "sessions", "files", "account", "sign in", "plan", "kept", "connected", "devices", "sign out", "cloud", "github", "repository", "ssh public key"];
  const proVisible = $derived(q === "" || PRO_KEYWORDS.some((keyword) => keyword.includes(q)));

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
      if (cat === "Chimaera Pro") {
        // A build without an account endpoint never shows an account group.
        if (account || (proVisible && (isBrowserGateway() || $proOffered === true))) out.push({ category: cat, defs: [] });
        continue;
      }
      if (cat === "Cloud") {
        if (proVisible && cloudMachine) out.push({ category: cat, defs: [] });
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
   * The spec-pinned chords — not rebindable, listed for reference. openN
   * follows the base modifier; the rest shadow browser conventions too
   * carefully to be worth opening up (see keys.ts).
   */
  const pinnedRows = $derived([
    { label: "Open session 1–9", chord: `${activeModLabel()}1–9` },
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
  <header class="top">
    <div class="title-row">
      <h1 class="title">Settings</h1>
      {#if !account}
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
      {/if}
    </div>
    {#if account}
      <p class="subtitle">Your Chimaera account. Each project's own settings open with the project.</p>
    {:else}
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
    {/if}
    {#if tab === "ui" && !account}
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
          {#if group.category === "Chimaera Pro"}
            <section data-section={group.category}>
              <h2 class="cat">Chimaera Pro</h2>
              {#if account}
                <BrowserAccount visible={tabVisible} />
              {:else}
              <button class="pro-entry" aria-label={proAction === "Get Pro" ? "Get Chimaera Pro" : `View Chimaera ${$accountPlan === "max" ? "Max" : "Pro"} account`}
                onclick={() => window.dispatchEvent(new Event("chimaera:open-pro"))}>
                <BrandMark size={30} />
                <span class="pro-copy">
                  {#if $accountPlan === "free"}
                    <strong>Chimaera Pro</strong>
                    <span>Optional: agents keep working in the cloud while you're away, and your work opens on another device.</span>
                  {:else if paid}
                    <strong>Your Chimaera {$accountPlan === "max" ? "Max" : "Pro"}</strong>
                    <span>Your plan, cloud agents and project sync.</span>
                  {:else}
                    <strong>Your Chimaera account</strong>
                    <span>{$accountPlan === "loading" ? "Checking your plan…" : "View your account to check your plan and cloud access."}</span>
                  {/if}
                </span>
                <span class="pro-open" aria-hidden="true">{proAction} <span>→</span></span>
              </button>
              {/if}
            </section>
          {:else if group.category === "Agents"}
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
          {:else if group.category === "Cloud"}
            <section data-section={group.category}>
              <CloudSetup visible={tabVisible} />
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
                 "check now"); the auto-check switch is the generic row. The
                 account's cloud has no switch: its updates are the service's. -->
            <section data-section={group.category}>
              <UpdatesStatus onJump={jumpTo} visible={tabVisible} />
              {#if updateState.daemon?.managed !== true}
                {#each group.defs as def (def.id)}
                  <SettingRow {def} />
                {/each}
              {/if}
            </section>
          {:else if group.category === "Keyboard"}
            <!-- Generic rows, then the reference chords that belong to them. -->
            <section data-section={group.category}>
              <h2 class="cat">{group.category}</h2>
              {#each group.defs as def (def.id)}
                <SettingRow {def} />
              {/each}
              <div class="kbd-reference">
                <div class="kbd-group">
                  <h3 class="kbd-group-title">Pinned chords</h3>
                  <p class="kbd-note">
                    Not rebindable — the terminal owns bare Ctrl on every platform, and these shadow
                    browser conventions too carefully to open up.
                  </p>
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
                  <h3 class="kbd-group-title">chimaera app menu</h3>
                  <p class="kbd-note">
                    The native app's menu bar owns the chords a browser reserves, so these fire only in
                    the chimaera app. Several are a second way to reach a rebindable action above —
                    {APP_MENU.closeView} also closes a view, {APP_MENU.newTerminal} opens a new terminal.
                  </p>
                  <ul class="kbd-list">
                    {#each appMenuRows as row (row.label)}
                      <li class="kbd-row">
                        <span class="kbd-label">{row.label}</span>
                        <kbd class="kbd-pill">{row.chord}</kbd>
                      </li>
                    {/each}
                  </ul>
                </div>
              </div>
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
    font-size: var(--text-lg);
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

  .pro-entry {
    display: grid;
    grid-template-columns: 30px minmax(0, 1fr) auto;
    align-items: center;
    gap: 12px;
    width: calc(100% - 28px);
    margin: 10px 14px 18px;
    padding: 14px;
    border: 1px solid var(--edge);
    border-radius: 8px;
    background: color-mix(in srgb, var(--fg) 2%, transparent);
    color: var(--fg);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .pro-entry:hover { background: var(--row-hover); }
  .pro-entry:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 2px; }
  .pro-copy { display: grid; flex: 1; min-width: 0; gap: 4px; font-size: var(--text-sm); }
  .pro-copy strong { font-size: var(--text-md); font-weight: 550; }
  .pro-copy > span { color: var(--muted); line-height: 1.5; }
  .pro-open { flex: none; display: flex; align-items: center; gap: 8px; font-size: var(--text-sm); color: var(--muted); }
  @container settings (max-width: 460px) {
    .pro-entry { grid-template-columns: 30px minmax(0, 1fr); }
    .pro-open { grid-column: 2; }
  }
  /* A titled block inside a section (Agents → Agent communication). */
  .subcat {
    margin: 18px 0 2px;
    padding: 0 14px;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg);
  }
  .subnote {
    margin: 0 0 4px;
    padding: 0 14px;
    font-size: var(--text-sm);
    line-height: 1.45;
    color: var(--muted);
    max-width: 60ch;
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
  }

  .kbd-row + .kbd-row {
    border-top: 1px solid var(--edge);
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

    .kbd-note,
    .subcat,
    .subnote {
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
