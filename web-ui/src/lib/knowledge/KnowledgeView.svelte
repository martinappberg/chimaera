<script lang="ts">
  /**
   * Knowledge — what the agents recorded, through the knowledge plugin that
   * provides it (docs/design/knowledge-redesign-plan.md). It exists only while such
   * a plugin is on; without one this tab says so in one line.
   *
   * Overview first (where we left off · waiting on you · what changed · open
   * work), then each kind as a list with a reader beside it: an entry's body
   * is its own markdown, as written; every id is a chip that previews on
   * hover and opens on click; a status is shown only as the agent wrote it.
   * The words (section names, status vocabulary) are the plugin's. Nothing
   * here writes a file: "open in file" and "Ask an agent" are the ways to
   * change what's recorded.
   */
  import { untrack } from "svelte";
  import type { LayoutCtrl } from "../layout/dnd";
  import { HoverPreviews } from "../previews/doc/hoverController.svelte";
  import { activeTheme } from "../settings/store.svelte";
  import { askAgent } from "../shared/askAgent";
  import { resolveTargets } from "../shared/embed/embed";
  import { referenceMatcher, referenceSources, ReferenceChips } from "../shared/references";
  import { requestReveal } from "../shared/reveal";
  import { pageVisible } from "../shared/visibility";
  import {
    knowledgeAvailable,
    knowledgeError,
    knowledgeFocus,
    refreshKnowledge,
    takeKnowledgeFocus,
  } from "../workspace/knowledge";
  import { knowledgePlugin, knowledgeProviderActive, openAttachSheet } from "../plugins/store";
  import { searchEntries, type Entry } from "./entries";
  import EntryList from "./EntryList.svelte";
  import EntryReader from "./EntryReader.svelte";
  import { filtersFor, listGroups, SECTION_KIND, type ListGroup, type Section } from "./list";
  import Overview from "./Overview.svelte";
  import { isoDay, providerLabels, shortStatusNote } from "./overview";
  import { knowledgeLookup } from "./store";
  import TidyList from "./TidyList.svelte";

  interface Props {
    wsId?: string | null;
    wsRoot: string | null;
    paneId: string;
    ctrl: LayoutCtrl;
    /** False while this retained tab is behind another pane tab. */
    visible?: boolean;
  }

  let { wsId = null, wsRoot, paneId, ctrl, visible = true }: Props = $props();

  type View = "overview" | Section | "tidy";
  const SECTIONS: Section[] = ["findings", "decisions", "learnings", "conventions", "todos", "sessions"];

  let view = $state<View>("overview");
  let query = $state("");
  let selected = $state<string | null>(null);
  /** On narrow panes the reader replaces the list while this is set. */
  let reading = $state(false);
  let history = $state<string[]>([]);
  let hpos = $state(-1);
  let filters = $state<Record<string, string>>({});
  let unfolded = $state<Set<string>>(new Set());
  let root = $state<HTMLDivElement | null>(null);
  let listBox = $state<HTMLDivElement | null>(null);
  let searchEl = $state<HTMLInputElement | null>(null);
  let width = $state(1200);

  const lookup = $derived($knowledgeLookup);
  const k = $derived(lookup?.k ?? null);
  const idx = $derived(lookup?.idx ?? null);
  const labels = $derived(k !== null ? providerLabels(k) : null);
  /** Who reads the project, in its own word (never its id). */
  const providerName = $derived(labels?.source || $knowledgePlugin?.name || "The knowledge plugin");
  const statusNote = $derived(labels?.status_note || "A status is shown as it was written; Chimaera never rates.");
  const providerActive = $derived($knowledgeProviderActive && k !== null);
  const narrow = $derived(width < 820);
  const chips = new ReferenceChips();
  /** Ids to chips: every registered reference source (this snapshot first). */
  const matcher = $derived(referenceMatcher($referenceSources));

  let now = $state(Date.now());
  $effect(() => {
    if (!visible || !$pageVisible) return;
    now = Date.now();
    const t = setInterval(() => (now = Date.now()), 60_000);
    return () => clearInterval(t);
  });
  const today = $derived(isoDay(now));
  const weekStart = $derived(isoDay(now - 6 * 86_400_000));

  // Refetch on return: a hand edit in another window doesn't nudge the
  // Timeline — a focus return is the cheap moment to catch it.
  let wasVisible = false;
  let primed = false;
  $effect(() => {
    const on = visible && $pageVisible;
    if (primed && on && !wasVisible) refreshKnowledge();
    primed = true;
    wasVisible = on;
  });

  $effect(() => {
    const el = root;
    if (el === null) return;
    const ro = new ResizeObserver(() => (width = el.clientWidth));
    ro.observe(el);
    width = el.clientWidth;
    return () => ro.disconnect();
  });

  // ---- counts, lists ---------------------------------------------------------

  const counts = $derived.by(() => {
    const c: Record<string, number> = {};
    if (idx === null) return c;
    for (const s of SECTIONS) c[s] = 0;
    for (const e of idx.entries) {
      const s = SECTIONS.find((x) => SECTION_KIND[x] === e.kind);
      if (s === undefined) continue;
      if (e.kind === "todo" && e.todo?.closed) continue;
      c[s] += 1;
    }
    return c;
  });
  const navSections = $derived(SECTIONS.filter((s) => (idx?.entries.some((e) => e.kind === SECTION_KIND[s]) ?? false)));

  const searching = $derived(query.trim() !== "");
  const listSection = $derived<Section | null>(SECTIONS.includes(view as Section) ? (view as Section) : null);
  /** The browse state: a section's list, or search results across kinds. */
  const browse = $derived.by((): { section: Section | "search"; groups: ListGroup[]; filters: ReturnType<typeof filtersFor>; filter: string } | null => {
    if (idx === null) return null;
    if (listSection !== null) {
      const kind = SECTION_KIND[listSection];
      const all = idx.entries.filter((e) => e.kind === kind);
      const matched = searchEntries(all, query);
      const fs = filtersFor(listSection, matched, weekStart);
      const fid = filters[listSection] ?? "all";
      const f = fs.find((x) => x.id === fid) ?? fs[0];
      return { section: listSection, groups: listGroups(listSection, matched.filter(f.test)), filters: fs, filter: f.id };
    }
    if (searching) {
      const matched = searchEntries(idx.entries, query);
      const groups: ListGroup[] = [];
      for (const s of SECTIONS) {
        const of = matched.filter((e) => e.kind === SECTION_KIND[s]);
        if (of.length === 0) continue;
        for (const g of listGroups(s, of)) {
          groups.push({ ...g, key: `${s}:${g.key}`, title: g.title || (labels?.sections[s] ?? s), folded: false });
        }
      }
      return { section: "search", groups, filters: [], filter: "all" };
    }
    return null;
  });
  const order = $derived(browse?.groups.flatMap((g) => g.entries.map((e) => e.ekey)) ?? []);
  const entry = $derived<Entry | null>(selected !== null ? (idx?.byKey.get(selected) ?? null) : null);

  // ---- navigation ------------------------------------------------------------

  function show(v: View): void {
    view = v;
    reading = false;
    if (SECTIONS.includes(v as Section) && !narrow) {
      // Land on the first entry so the reader is never empty on a wide pane.
      queueMicrotask(() => {
        if (order.length > 0 && (selected === null || !order.includes(selected))) open(order[0], false);
      });
    }
  }

  /** Read an entry: switch to its section, select it, remember it. */
  function open(ekey: string, remember = true): void {
    const e = idx?.byKey.get(ekey);
    if (e === undefined) return;
    const s = SECTIONS.find((x) => SECTION_KIND[x] === e.kind);
    if (s !== undefined && view !== s && !searching) view = s;
    // A search that hides it would make the jump do nothing visible.
    if (searching && !searchEntries([e], query).length) query = "";
    selected = ekey;
    reading = true;
    if (remember && history[hpos] !== ekey) {
      history = [...history.slice(0, hpos + 1), ekey].slice(-100);
      hpos = history.length - 1;
    }
    queueMicrotask(() => {
      const row = listBox?.querySelector(`[data-ekey="${CSS.escape(ekey)}"]`);
      row?.scrollIntoView({ block: "nearest" });
    });
  }

  function back(): void {
    if (hpos <= 0) return;
    hpos -= 1;
    open(history[hpos], false);
  }

  function forward(): void {
    if (hpos >= history.length - 1) return;
    hpos += 1;
    open(history[hpos], false);
  }

  function abs(p: string): string {
    return p.startsWith("/") || p.startsWith("~") || wsRoot === null ? p : `${wsRoot}/${p}`;
  }

  function openFile(path: string, line: number, end: number): void {
    const p = abs(path);
    if (line > 0) requestReveal(p, end > line ? { line, endLine: end } : { line });
    ctrl.openFileFrom(paneId, p, false);
  }

  function openSession(sid: string): void {
    if (wsId !== null) ctrl.revealWorktreeSession(sid, wsId);
  }

  // A chip, a Timeline row, the dashboard asked for an entry: take it when
  // this tab shows and the snapshot is in.
  $effect(() => {
    const req = $knowledgeFocus;
    if (req === null || !visible || idx === null) return;
    untrack(() => {
      const ekey = takeKnowledgeFocus();
      if (ekey !== null) open(ekey);
    });
  });

  // ---- keyboard ----------------------------------------------------------------

  function onKey(e: KeyboardEvent): void {
    const t = e.target as HTMLElement | null;
    const typing = t !== null && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable);
    if (typing) {
      if (e.key === "Escape" && t === searchEl) {
        query = "";
        searchEl?.blur();
      } else if (e.key === "Enter" && t === searchEl && idx !== null) {
        // An id jumps straight to its entry; anything else reads the first hit.
        const hit = idx.byId.get(query.trim().toUpperCase())?.[0] ?? (order[0] !== undefined ? idx.byKey.get(order[0]) : undefined);
        if (hit !== undefined) {
          open(hit.ekey);
          searchEl?.blur();
        }
      } else if (e.key === "ArrowDown" && t === searchEl && order.length > 0) {
        e.preventDefault();
        open(order[0]);
        listBox?.querySelector<HTMLElement>(`[data-ekey="${CSS.escape(order[0])}"]`)?.focus();
      }
      return;
    }
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.key === "/") {
      e.preventDefault();
      searchEl?.focus();
    } else if (e.key === "[") {
      back();
    } else if (e.key === "]") {
      forward();
    } else if ((e.key === "j" || e.key === "k" || e.key === "ArrowDown" || e.key === "ArrowUp") && order.length > 0) {
      e.preventDefault();
      const i = selected !== null ? order.indexOf(selected) : -1;
      const down = e.key === "j" || e.key === "ArrowDown";
      const next = order[Math.max(0, Math.min(order.length - 1, i < 0 ? 0 : i + (down ? 1 : -1)))];
      open(next);
      listBox?.querySelector<HTMLElement>(`[data-ekey="${CSS.escape(next)}"]`)?.focus();
    } else if (e.key === "Escape" && narrow && reading) {
      reading = false;
    }
  }

  // ---- chips: preview on hover, open on click ------------------------------

  $effect(() => {
    const el = root;
    if (el === null) return;
    const h = new HoverPreviews({
      root: el,
      layer: () => el,
      docPath: () => "",
      mode: () => "reading",
      text: () => null,
      links: () => ({ wsRoot, workspaceId: wsId }),
      ask: (ref) =>
        resolveTargets([ref.target], "/").then(
          (r) => r[ref.target] ?? null,
          () => null,
        ),
      theme: () => untrack(() => activeTheme().kind),
      fontSize: () => 14,
      targetOf: (node) => chips.hoverTarget(node),
      anchors: false,
      standalone: true,
    });
    return () => h.destroy();
  });

  /** A chip: this snapshot's entries open here; another source's target
   *  opens where that source says. */
  function follow(t: { key: string; open: (from: { paneId: string | null; newSplit: boolean }) => void }, newSplit: boolean): void {
    if (idx?.byKey.has(t.key)) open(t.key);
    else t.open({ paneId, newSplit });
  }

  function onClick(e: MouseEvent): void {
    const hit = chips.chipAt(e.target);
    if (hit === null) return;
    e.preventDefault();
    follow(hit.targets[0], e.metaKey || e.ctrlKey);
  }

  function onChipKey(e: KeyboardEvent): void {
    if (e.key !== "Enter") return;
    const hit = chips.chipAt(e.target);
    if (hit === null) return;
    e.preventDefault();
    e.stopPropagation();
    follow(hit.targets[0], false);
  }

  const hasSnapshot = $derived(k !== null && (k.left_off !== null || (idx?.entries.length ?? 0) > 0));
  const headline = $derived.by(() => {
    if (k === null) return "";
    const parts: string[] = [];
    const n = (x: number, one: string, many: string): string => `${x} ${x === 1 ? one : many}`;
    if ((counts.findings ?? 0) > 0) parts.push(n(counts.findings, "finding", "findings"));
    if ((counts.decisions ?? 0) > 0) parts.push(n(counts.decisions, "decision", "decisions"));
    if ((counts.learnings ?? 0) > 0) parts.push(n(counts.learnings, "learning", "learnings"));
    if ((counts.todos ?? 0) > 0) parts.push(`${counts.todos} open to-do${counts.todos === 1 ? "" : "s"}`);
    return parts.join(" · ");
  });
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="knowledge" bind:this={root} onkeydown={onKey} onclick={onClick} onkeydowncapture={onChipKey}>
  {#if !$knowledgeProviderActive}
    <div class="off">
      <h1>Knowledge</h1>
      <p>Knowledge shows what your agents record through a knowledge plugin. None is switched on in this workspace.</p>
      {#if $knowledgePlugin !== null}
        {@const kp = $knowledgePlugin}
        <button class="opt" onclick={() => openAttachSheet(kp.id)}>Use {kp.name} for Knowledge →</button>
      {:else}
        <p>Extensions lists the plugins that provide one.</p>
      {/if}
    </div>
  {:else if $knowledgeAvailable === false}
    <p class="off">This daemon has no Knowledge yet — update chimaera to read what your agents record.</p>
  {:else if $knowledgeError !== null && k === null}
    <p class="off err">{$knowledgeError}</p>
  {:else if k === null || idx === null || labels === null}
    <p class="off">loading…</p>
  {:else}
    <header class="head">
      <div class="titles">
        <h1>Knowledge</h1>
        <p class="sub">
          {headline || "Nothing recorded yet — agents record findings, decisions and learnings as they work."}
          {#if labels.source}<span class="source mono">{labels.source}</span>{/if}
        </p>
      </div>
      <label class="search">
        <span class="sr">Search knowledge</span>
        <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true"
          ><circle cx="7" cy="7" r="5" /><path d="m11 11 3.5 3.5" /></svg
        >
        <input
          bind:this={searchEl}
          type="search"
          placeholder="Find an entry, an id like F-12, a path…"
          bind:value={query}
          spellcheck="false"
          autocomplete="off"
        />
        <kbd>/</kbd>
      </label>
    </header>

    {#if providerActive && k.error !== null}
      <p class="stale">
        {#if hasSnapshot}Showing what {providerName} read last — it couldn't refresh just now.{:else}{providerName} couldn't read
          this project just now.{/if}
        <span class="why">{k.error}</span>
      </p>
    {/if}

    <div class="body" class:narrow>
      <nav class="nav" aria-label="Knowledge sections">
        <button class="nrow" aria-current={view === "overview" && !searching} onclick={() => ((query = ""), show("overview"))}>
          {labels.sections.overview}
        </button>
        {#each navSections as s (s)}
          <button class="nrow" aria-current={view === s} onclick={() => show(s)}>
            {labels.sections[s] ?? s}
            <span class="count mono">{counts[s]}</span>
          </button>
        {/each}
        {#if k.tidy.length > 0}
          <span class="sep"></span>
          <button class="nrow tidy" aria-current={view === "tidy"} onclick={() => ((query = ""), show("tidy"))}>
            {labels.sections.tidy}
            <span class="count mono hot">{k.tidy.length}</span>
          </button>
        {/if}
        {#if !narrow}
          <div class="legend">
            <div class="lh">Status</div>
            <p title={statusNote}>{shortStatusNote(statusNote)}</p>
          </div>
        {/if}
      </nav>

      <div class="main">
        {#if browse !== null && (listSection !== null || searching)}
          <div class="browse" class:reading>
            <div class="listbox" bind:this={listBox}>
              <EntryList
                label={browse.section === "search" ? "Search results" : (labels.sections[browse.section] ?? browse.section)}
                groups={browse.groups}
                filters={browse.filters}
                filter={browse.filter}
                onFilter={(id) => {
                  if (listSection !== null) filters = { ...filters, [listSection]: id };
                }}
                {idx}
                {labels}
                {selected}
                onSelect={(ekey) => open(ekey)}
                {unfolded}
                onUnfold={(key) => (unfolded = new Set([...unfolded, key]))}
                {query}
              />
            </div>
            <div class="readerbox">
              {#if entry !== null}
                <EntryReader
                  {entry}
                  {k}
                  {idx}
                  {matcher}
                  {labels}
                  {chips}
                  {wsRoot}
                  {wsId}
                  canBack={hpos > 0}
                  canForward={hpos < history.length - 1}
                  onBack={back}
                  onForward={forward}
                  onOpen={(ekey) => open(ekey)}
                  onOpenFile={openFile}
                  onOpenSession={openSession}
                  onClose={narrow ? () => (reading = false) : undefined}
                />
              {:else}
                <p class="pick">Pick an entry to read it here. <span class="keys">j / k to move · / to search</span></p>
              {/if}
            </div>
          </div>
        {:else if view === "tidy"}
          <div class="pad">
            <TidyList
              rows={k.tidy}
              {idx}
              title={labels.sections.tidy}
              onAsk={(text) => askAgent({ text })}
              onOpen={(ekey) => open(ekey)}
            />
          </div>
        {:else}
          <div class="pad">
            <Overview
              {k}
              {idx}
              {matcher}
              {labels}
              {chips}
              {wsRoot}
              {wsId}
              {today}
              {now}
              onOpen={(ekey) => open(ekey)}
              onOpenFile={openFile}
              onSection={(s) => show(s)}
            />
          </div>
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .knowledge {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    background: var(--bg);
    color: var(--fg);
    container-type: inline-size;
    overflow: hidden;
  }
  .off {
    margin: 0;
    padding: 36px;
    max-width: 640px;
    font-size: var(--text-sm);
    color: var(--muted);
    line-height: 1.55;
  }
  .off h1 {
    color: var(--fg);
    margin-bottom: 8px;
  }
  .off p {
    margin: 0 0 14px;
  }
  .err {
    color: var(--err);
  }
  .opt {
    border: 1px solid var(--edge);
    background: var(--overlay-bg);
    color: var(--fg);
    border-radius: 8px;
    padding: 7px 14px;
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
  }
  .opt:hover {
    border-color: var(--accent);
    color: var(--accent);
  }
  .head {
    display: flex;
    align-items: flex-end;
    gap: 12px 24px;
    flex-wrap: wrap;
    padding: 20px 28px 14px;
    border-bottom: 1px solid var(--edge);
    flex: none;
  }
  .titles {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
    flex: 1 1 320px;
  }
  h1 {
    margin: 0;
    font-size: 20px;
    font-weight: 600;
    letter-spacing: -0.01em;
  }
  .sub {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--muted);
    display: flex;
    flex-wrap: wrap;
    gap: 4px 10px;
    align-items: baseline;
  }
  .source {
    font-size: 11px;
    border: 1px solid var(--edge);
    border-radius: 999px;
    padding: 1px 8px;
    white-space: nowrap;
  }
  .mono {
    font-family: var(--mono);
  }
  .search {
    position: relative;
    display: flex;
    align-items: center;
    flex: 0 1 340px;
    min-width: 200px;
    color: var(--muted);
  }
  .search svg {
    position: absolute;
    left: 10px;
    pointer-events: none;
  }
  .search input {
    width: 100%;
    border: 1px solid var(--edge);
    background: var(--overlay-bg);
    color: var(--fg);
    border-radius: 8px;
    padding: 7px 30px 7px 30px;
    font: inherit;
    font-size: var(--text-sm);
  }
  .search input:focus {
    outline: 2px solid var(--focus-ring);
    outline-offset: 0;
  }
  .search kbd {
    position: absolute;
    right: 8px;
    font-family: var(--mono);
    font-size: 10.5px;
    border: 1px solid var(--edge);
    border-radius: 4px;
    padding: 0 5px;
    color: var(--muted);
    pointer-events: none;
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
  }
  .stale {
    margin: 0;
    padding: 6px 28px;
    font-size: var(--text-xs);
    color: var(--muted);
    border-bottom: 1px solid var(--edge);
  }
  .stale .why {
    display: block;
    font-family: var(--mono);
    overflow-wrap: anywhere;
  }
  .body {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: 184px minmax(0, 1fr);
  }
  .nav {
    border-right: 1px solid var(--edge);
    padding: 12px 10px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .nrow {
    display: flex;
    align-items: baseline;
    gap: 8px;
    width: 100%;
    border: 0;
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    text-align: left;
    padding: 6px 10px;
    border-radius: 6px;
    cursor: pointer;
  }
  .nrow:hover {
    background: var(--row-hover);
  }
  .nrow[aria-current="true"] {
    background: var(--row-active);
    font-weight: 550;
  }
  .nrow.tidy {
    color: var(--muted);
  }
  .count {
    margin-left: auto;
    font-size: 11px;
    color: var(--muted);
    font-weight: 400;
  }
  .count.hot {
    color: var(--warn);
  }
  .sep {
    border-top: 1px solid var(--edge);
    margin: 8px 4px;
  }
  .legend {
    margin: 18px 10px 0;
    font-size: 11.5px;
    line-height: 1.45;
    color: var(--muted);
  }
  .legend .lh {
    font-size: 10.5px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    margin-bottom: 4px;
  }
  .legend p {
    margin: 0;
  }
  .main {
    min-width: 0;
    min-height: 0;
    overflow: hidden;
    position: relative;
  }
  .pad {
    height: 100%;
    overflow-y: auto;
    padding: 20px 28px 48px;
  }
  .browse {
    display: grid;
    grid-template-columns: minmax(300px, 42%) minmax(0, 1fr);
    height: 100%;
  }
  .listbox {
    overflow-y: auto;
    border-right: 1px solid var(--edge);
    min-height: 0;
  }
  .readerbox {
    overflow-y: auto;
    min-height: 0;
    padding: 18px 28px 60px;
  }
  .pick {
    margin: 40px 0;
    color: var(--muted);
    font-size: var(--text-sm);
    text-align: center;
  }
  .keys {
    display: block;
    margin-top: 6px;
    font-size: var(--text-xs);
  }

  /* Narrow panes: the nav becomes a strip, and the reader replaces the list. */
  .body.narrow {
    grid-template-columns: minmax(0, 1fr);
    grid-template-rows: auto minmax(0, 1fr);
  }
  .body.narrow .nav {
    flex-direction: row;
    border-right: 0;
    border-bottom: 1px solid var(--edge);
    overflow-x: auto;
    overflow-y: hidden;
    padding: 6px 10px;
    gap: 2px;
    /* the strip scrolls by wheel/drag; the scrollbar itself would be noise */
    scrollbar-width: none;
  }
  .body.narrow .nav::-webkit-scrollbar {
    display: none;
  }
  .body.narrow .nrow {
    width: auto;
    white-space: nowrap;
  }
  .body.narrow .sep {
    border-top: 0;
    border-left: 1px solid var(--edge);
    margin: 4px 4px;
  }
  .body.narrow .browse {
    grid-template-columns: minmax(0, 1fr);
  }
  .body.narrow .browse.reading .listbox {
    display: none;
  }
  .body.narrow .browse:not(.reading) .readerbox {
    display: none;
  }
  .body.narrow .listbox {
    border-right: 0;
  }
  .body.narrow .pad,
  .body.narrow .readerbox {
    padding: 14px 16px 40px;
  }
  .head:has(~ .body.narrow) {
    padding: 14px 16px 10px;
  }
</style>
