<script lang="ts">
  /**
   * One node of a plugin screen in the Chimaera format (`ui/1`), and its
   * children, recursively. Every look comes from here: a plugin names
   * semantic props (a tone, a size, an icon name), never colors or CSS, so
   * light and dark, the brand and accessibility are the app's.
   *
   * - Unknown nodes (a newer plugin, an older chimaera) draw their
   *   `fallback` (another node, or `"drop"` for nothing), else a quiet
   *   placeholder that still draws their children.
   * - Actions go to the host (`ScreenCtx.act`): the plugin's, or a built-in
   *   one the host carries out (open a file, save an output to the
   *   workspace…).
   * - Rich nodes compose what core already has: `editor`, `pdf`, `log` and
   *   `image` show a file through the app's own viewers (loaded lazily);
   *   `diagnostics` reads the `diagnostics/1` surface.
   * - Text is text: nothing a plugin sends is inserted as HTML except
   *   `markdown`, which goes through chat's renderer and its sanitizing.
   */
  import { onDestroy, onMount, untrack, type Component } from "svelte";
  import UiNode from "./UiNode.svelte";
  import Switch from "../../shared/Switch.svelte";
  import FileIcon from "../../shared/FileIcon.svelte";
  import Markdown from "../../chat/Markdown.svelte";
  import ExtensionCode from "../../shared/ExtensionCode.svelte";
  import { openInSystemBrowser } from "../../shared/urlOpen";
  import { fetchDiagnostics, onPlatformFrame, type Diagnostic, type UiNodeData } from "../platform";
  import { fsFile } from "../../previews/files";
  import { fetchGitDiff } from "../../workspace/git";
  import {
    KNOWN_NODES,
    diffLines,
    nodes,
    provideForm,
    str,
    tone,
    useForm,
    useScreen,
    type FormCtx,
  } from "./screen";

  interface Props {
    node: { type: string; [prop: string]: unknown };
    depth?: number;
  }

  let { node, depth = 0 }: Props = $props();

  const screen = useScreen();
  const form = useForm();

  const kind = $derived(node.type);
  const children = $derived(nodes(node.children));
  const t = $derived(tone(node.tone));
  const known = $derived(KNOWN_NODES.has(kind));

  // --- small icon set (semantic names; Tabler-style strokes) ----------------
  const ICONS: Record<string, string[]> = {
    check: ["M5 12l5 5l10 -10"],
    x: ["M18 6l-12 12", "M6 6l12 12"],
    alert: ["M12 9v4", "M12 16v.01", "M10.363 3.591l-8.106 13.534a1.914 1.914 0 0 0 1.636 2.871h16.214a1.914 1.914 0 0 0 1.636 -2.87l-8.106 -13.536a1.914 1.914 0 0 0 -3.274 0"],
    info: ["M12 9h.01", "M11 12h1v4h1", "M3 12a9 9 0 1 0 18 0a9 9 0 0 0 -18 0"],
    clock: ["M3 12a9 9 0 1 0 18 0a9 9 0 0 0 -18 0", "M12 7v5l3 3"],
    file: ["M14 3v4a1 1 0 0 0 1 1h4", "M17 21h-10a2 2 0 0 1 -2 -2v-14a2 2 0 0 1 2 -2h7l5 5v11a2 2 0 0 1 -2 2"],
    folder: ["M5 4h4l3 3h7a2 2 0 0 1 2 2v8a2 2 0 0 1 -2 2h-14a2 2 0 0 1 -2 -2v-11a2 2 0 0 1 2 -2"],
    play: ["M7 4v16l13 -8z"],
    stop: ["M5 5h14v14h-14z"],
    refresh: ["M20 11a8.1 8.1 0 0 0 -15.5 -2m-.5 -4v4h4", "M4 13a8.1 8.1 0 0 0 15.5 2m.5 4v-4h-4"],
    download: ["M4 17v2a2 2 0 0 0 2 2h12a2 2 0 0 0 2 -2v-2", "M7 11l5 5l5 -5", "M12 4l0 12"],
    external: ["M12 6h-6a2 2 0 0 0 -2 2v10a2 2 0 0 0 2 2h10a2 2 0 0 0 2 -2v-6", "M11 13l9 -9", "M15 4h5v5"],
    grid: ["M4 4h6v6h-6z", "M14 4h6v6h-6z", "M4 14h6v6h-6z", "M14 14h6v6h-6z"],
    list: ["M9 6l11 0", "M9 12l11 0", "M9 18l11 0", "M5 6l0 .01", "M5 12l0 .01", "M5 18l0 .01"],
    book: ["M3 19a9 9 0 0 1 9 0a9 9 0 0 1 9 0", "M3 6a9 9 0 0 1 9 0a9 9 0 0 1 9 0", "M3 6l0 13", "M12 6l0 13", "M21 6l0 13"],
    bolt: ["M13 3l0 7l6 0l-8 11l0 -7l-6 0l8 -11"],
    settings: ["M10.325 4.317c.426 -1.756 2.924 -1.756 3.35 0a1.724 1.724 0 0 0 2.573 1.066c1.543 -.94 3.31 .826 2.37 2.37a1.724 1.724 0 0 0 1.065 2.572c1.756 .426 1.756 2.924 0 3.35a1.724 1.724 0 0 0 -1.066 2.573c.94 1.543 -.826 3.31 -2.37 2.37a1.724 1.724 0 0 0 -2.572 1.065c-.426 1.756 -2.924 1.756 -3.35 0a1.724 1.724 0 0 0 -2.573 -1.066c-1.543 .94 -3.31 -.826 -2.37 -2.37a1.724 1.724 0 0 0 -1.065 -2.572c-1.756 -.426 -1.756 -2.924 0 -3.35a1.724 1.724 0 0 0 1.066 -2.573c-.94 -1.543 .826 -3.31 2.37 -2.37c1 .608 2.296 .07 2.572 -1.065", "M9 12a3 3 0 1 0 6 0a3 3 0 0 0 -6 0"],
    search: ["M10 10m-7 0a7 7 0 1 0 14 0a7 7 0 1 0 -14 0", "M21 21l-6 -6"],
  };
  const iconOf = (name: unknown): string[] => ICONS[str(name)] ?? ["M12 12m-2 0a2 2 0 1 0 4 0a2 2 0 1 0 -4 0"];

  // --- actions ---------------------------------------------------------------
  function run(action: unknown, payload: unknown, extra?: { form?: Record<string, unknown>; value?: unknown }): void {
    const a = str(action);
    if (a === "") return;
    void screen.act(a, payload ?? null, extra);
  }

  function openHref(href: string): void {
    if (/^https?:\/\//i.test(href)) openInSystemBrowser(href);
  }

  // --- stateful nodes --------------------------------------------------------
  let tabIndex = $state(0);
  const tabs = $derived(
    Array.isArray(node.tabs)
      ? (node.tabs as { title?: unknown; children?: unknown }[]).map((tb) => ({
          title: str(tb?.title, "—"),
          children: nodes(tb?.children),
        }))
      : [],
  );

  // A form collects its fields' values; its submit sends them. A node's
  // type is fixed for its instance: children are keyed by position and
  // type, so a new tree with another node there mounts a new one.
  const formCtx: FormCtx = $state({ values: {} });
  const isForm = (n: { type: string }): boolean => n.type === "form";
  if (isForm(untrack(() => node))) provideForm(formCtx);

  // An input's own value (seeded from the tree; a new tree reseeds it).
  let inputValue = $state<unknown>(null);
  $effect(() => {
    inputValue = node.value ?? (node.type === "toggle" ? false : "");
    if (form !== undefined && typeof node.name === "string" && node.type !== "button") {
      form.values[node.name] = inputValue;
    }
  });
  function setInput(v: unknown): void {
    inputValue = v;
    if (form !== undefined && typeof node.name === "string") form.values[node.name] = v;
    else if (node.action !== undefined) run(node.action, node.payload, { value: v });
  }

  // `list` and `table` rows, with the pages the plugin's `query` adds.
  let extraRows = $state<unknown[]>([]);
  let more = $state<{ query: string; args: unknown } | null>(null);
  let loadingMore = $state(false);
  let moreError = $state<string | null>(null);
  $effect(() => {
    void node;
    extraRows = [];
    const m = node.more as { query?: unknown; args?: unknown } | undefined;
    more = m && typeof m.query === "string" ? { query: m.query, args: m.args ?? {} } : null;
  });
  async function loadMore(): Promise<void> {
    if (more === null || loadingMore) return;
    loadingMore = true;
    moreError = null;
    try {
      const page = (await screen.query(more.query, more.args)) as { items?: unknown[]; rows?: unknown[]; more?: unknown };
      const rows = Array.isArray(page?.items) ? page.items : Array.isArray(page?.rows) ? page.rows : [];
      extraRows = [...extraRows, ...rows];
      const m = page?.more as { query?: unknown; args?: unknown } | undefined;
      more = m && typeof m.query === "string" ? { query: m.query, args: m.args ?? {} } : null;
    } catch (e) {
      moreError = e instanceof Error ? e.message : String(e);
    } finally {
      loadingMore = false;
    }
  }
  const listItems = $derived(
    [...(Array.isArray(node.items) ? node.items : []), ...(kind === "list" ? extraRows : [])] as Record<string, unknown>[],
  );
  const tableRows = $derived(
    [...(Array.isArray(node.rows) ? node.rows : []), ...(kind === "table" ? extraRows : [])] as Record<string, unknown>[],
  );
  const columns = $derived(
    Array.isArray(node.columns)
      ? (node.columns as Record<string, unknown>[]).map((c) => ({
          key: str(c?.key),
          title: str(c?.title, str(c?.key)),
          align: c?.align === "end" ? "end" : c?.align === "center" ? "center" : "start",
        }))
      : [],
  );

  // Rich nodes: the app's own viewers, loaded on first use, on a path
  // resolved from `output:` or the workspace.
  let FileView = $state<Component<{ path: string; wsRoot?: string | null; plugins?: boolean; pathBar?: boolean }> | null>(null);
  let ImageView = $state<Component<{ path: string }> | null>(null);
  let resolved = $state<string | null>(null);
  let resolveError = $state<string | null>(null);
  const richRef = $derived(
    kind === "editor" || kind === "file"
      ? str(node.path)
      : kind === "pdf" || kind === "log" || kind === "image"
        ? str(node.src)
        : "",
  );
  $effect(() => {
    const ref = richRef;
    resolved = null;
    resolveError = null;
    if (ref === "") return;
    screen.resolve(ref).then(
      (p) => {
        if (ref === richRef) resolved = p;
      },
      (e) => (resolveError = e instanceof Error ? e.message : String(e)),
    );
    if (kind === "image") {
      void import("../../previews/ImageView.svelte").then((m) => (ImageView = m.default));
    } else if (kind !== "file") {
      void import("../../previews/FileView.svelte").then((m) => (FileView = m.default as typeof FileView));
    }
  });

  // `status`: idle | busy | ok | warn | bad.
  const statusState = $derived(
    ["idle", "busy", "ok", "warn", "bad"].includes(str(node.state)) ? str(node.state) : "idle",
  );
  // `segmented`: its options, `[{value, label, icon?, title?}]` or strings.
  const segOptions = $derived(
    (Array.isArray(node.options) ? (node.options as unknown[]) : []).map((o) => {
      const r = (typeof o === "object" && o !== null ? o : { value: o }) as Record<string, unknown>;
      const value = str(r.value);
      return { value, label: str(r.label, value), icon: typeof r.icon === "string" ? r.icon : null, title: str(r.title, "") || undefined };
    }),
  );
  // A callout's buttons.
  const calloutActions = $derived(
    kind === "callout" && Array.isArray(node.actions)
      ? (node.actions as unknown[]).filter((a): a is UiNodeData => typeof a === "object" && a !== null && typeof (a as { type?: unknown }).type === "string")
      : [],
  );

  // `diagnostics`: the problems list, from every active plugin's
  // `diagnostics/1` (or one key of this plugin's: `key`), again whenever one
  // is published. Layout notes (`info`, `hint`: a LaTeX box) wait behind a
  // toggle; `quiet` draws nothing while there is nothing to say.
  let problems = $state<Diagnostic[] | null>(null);
  let problemsError = $state<string | null>(null);
  async function loadProblems(): Promise<void> {
    try {
      const file = typeof node.file === "string" ? node.file : undefined;
      const key = typeof node.key === "string" ? node.key : undefined;
      const all = await fetchDiagnostics(screen.ws, file, key);
      problems = key !== undefined || node.mine === true ? all.filter((d) => d.plugin === screen.plugin) : all;
      problemsError = null;
    } catch (e) {
      problemsError = e instanceof Error ? e.message : String(e);
    }
  }
  let showNotes = $state(false);
  const notes = $derived((problems ?? []).filter((d) => d.severity === "info" || d.severity === "hint"));
  const shownProblems = $derived((problems ?? []).filter((d) => d.severity === "error" || d.severity === "warning"));
  const visibleProblems = $derived(showNotes ? [...shownProblems, ...notes] : shownProblems);
  const counts = $derived({
    error: shownProblems.filter((d) => d.severity === "error").length,
    warning: shownProblems.filter((d) => d.severity === "warning").length,
  });
  let stopFrames: (() => void) | null = null;
  onMount(() => {
    if (node.type !== "diagnostics") return;
    void loadProblems();
    stopFrames = onPlatformFrame((f) => {
      if (f.type === "surface" && f.surface === "diagnostics/1") void loadProblems();
    });
  });
  onDestroy(() => stopFrames?.());

  // `diff`: two texts given, or a file against a base (`head`, `index`,
  // `rev:<ref>`, `output:<path>` — a snapshot the plugin kept).
  let baseTexts = $state<{ before: string; after: string } | null>(null);
  let baseError = $state<string | null>(null);
  $effect(() => {
    if (kind !== "diff" || typeof node.path !== "string" || typeof node.base !== "string") return;
    const file = node.path;
    const base = node.base;
    baseTexts = null;
    baseError = null;
    void (async () => {
      try {
        const abs = await screen.resolve(file);
        if (base.startsWith("output:")) {
          const [was, now] = await Promise.all([screen.resolve(base).then((p) => fsFile(p)), fsFile(abs)]);
          const text = (c: { bytes: Uint8Array }) => new TextDecoder().decode(c.bytes);
          baseTexts = { before: text(was), after: text(now) };
          return;
        }
        const d = base.startsWith("rev:")
          ? await fetchGitDiff(screen.ws, abs, "rev", { rev: base.slice(4) })
          : await fetchGitDiff(screen.ws, abs, base === "index" ? "unstaged" : "head");
        if (d.error) throw new Error(d.error);
        if (d.binary) throw new Error("a binary file has no text to compare");
        baseTexts = { before: d.a, after: d.b };
      } catch (e) {
        baseError = e instanceof Error ? e.message : String(e);
      }
    })();
  });
  const diff = $derived(
    kind !== "diff"
      ? []
      : baseTexts !== null
        ? diffLines(baseTexts.before, baseTexts.after, node.mode !== "code")
        : diffLines(str(node.before), str(node.after), node.mode !== "code"),
  );

  const gap = $derived(node.gap === "small" ? "s" : node.gap === "large" ? "l" : "m");
  const cols = $derived(Math.min(6, Math.max(1, Number(node.columns) || 2)));
  const ratio = $derived(Math.min(0.9, Math.max(0.1, Number(node.ratio) || 0.5)));
  const level = $derived(Math.min(3, Math.max(1, Number(node.level) || 2)));
  const progress = $derived(typeof node.value === "number" ? Math.min(1, Math.max(0, node.value)) : null);
  const severityWord = { error: "error", warning: "warning", info: "note", hint: "hint" } as const;
</script>

{#snippet kids(list: { type: string; [k: string]: unknown }[])}
  {#each list as child, i (`${i}:${child.type}`)}
    <UiNode node={child} depth={depth + 1} />
  {/each}
{/snippet}

{#snippet icon(name: unknown, label: unknown, size = 14)}
  <svg
    class="icon"
    viewBox="0 0 24 24"
    width={size}
    height={size}
    fill="none"
    stroke="currentColor"
    stroke-width="1.8"
    stroke-linecap="round"
    stroke-linejoin="round"
    role={typeof label === "string" ? "img" : undefined}
    aria-label={typeof label === "string" ? label : undefined}
    aria-hidden={typeof label === "string" ? undefined : "true"}
  >
    {#each iconOf(name) as d (d)}<path {d} />{/each}
  </svg>
{/snippet}

{#snippet actionButton(b: Record<string, unknown>)}
  <button
    class="opt small"
    class:primary={b.tone === "accent"}
    class:danger={b.tone === "bad"}
    disabled={b.disabled === true || screen.busy}
    title={typeof b.title === "string" ? b.title : undefined}
    onclick={() => run(b.action, b.payload)}
  >
    {#if b.icon !== undefined}{@render icon(b.icon, undefined, 13)}{/if}
    {str(b.label, "…")}
  </button>
{/snippet}

{#if !known}
  {#if node.fallback === "drop"}
    <!-- dropped: the plugin said to show nothing in its place -->
  {:else if typeof node.fallback === "object" && node.fallback !== null}
    <UiNode node={node.fallback as { type: string }} {depth} />
  {:else}
    <div class="unknown" title="This part of the screen needs a newer chimaera ({kind})">
      <span class="unknown-note">needs a newer chimaera</span>
      {@render kids(children)}
    </div>
  {/if}
{:else if kind === "stack"}
  <div class="stack gap-{gap}">{@render kids(children)}</div>
{:else if kind === "row"}
  <div
    class="row gap-{gap}"
    class:between={node.align === "between"}
    class:center={node.align === "center"}
    class:end={node.align === "end"}
  >
    {@render kids(children)}
  </div>
{:else if kind === "grid"}
  <div class="grid" style:--cols={cols}>{@render kids(children)}</div>
{:else if kind === "split"}
  <div class="split" style:--ratio={ratio}>
    <div class="pane">{#if children[0]}<UiNode node={children[0]} depth={depth + 1} />{/if}</div>
    <div class="pane">{#if children[1]}<UiNode node={children[1]} depth={depth + 1} />{/if}</div>
  </div>
{:else if kind === "tabs"}
  <div class="tabs">
    <div class="tabbar" role="tablist">
      {#each tabs as tb, i (i)}
        <button
          role="tab"
          class="tab"
          class:on={i === Math.min(tabIndex, tabs.length - 1)}
          aria-selected={i === Math.min(tabIndex, tabs.length - 1)}
          onclick={() => (tabIndex = i)}>{tb.title}</button
        >
      {/each}
    </div>
    {#if tabs.length > 0}
      <div class="tabpanel" role="tabpanel">{@render kids(tabs[Math.min(tabIndex, tabs.length - 1)].children)}</div>
    {/if}
  </div>
{:else if kind === "section"}
  <details class="section" open={node.collapsed !== true}>
    <summary>{str(node.title)}</summary>
    <div class="section-body stack gap-m">{@render kids(children)}</div>
  </details>
{:else if kind === "card"}
  <div class="card stack gap-s">
    {#if typeof node.title === "string"}<div class="card-title">{node.title}</div>{/if}
    {@render kids(children)}
  </div>
{:else if kind === "divider"}
  <hr class="divider" />
{:else if kind === "text"}
  <p
    class="text tone-{t}"
    class:small={node.size === "small"}
    class:large={node.size === "large"}
    class:strong={node.emphasis === true}
    class:mono={node.mono === true}
  >
    {str(node.text)}
  </p>
{:else if kind === "heading"}
  {#if level === 1}
    <h2 class="heading">{str(node.text)}</h2>
  {:else if level === 2}
    <h3 class="heading">{str(node.text)}</h3>
  {:else}
    <h4 class="heading">{str(node.text)}</h4>
  {/if}
{:else if kind === "markdown"}
  <div class="markdown"><Markdown text={str(node.text)} /></div>
{:else if kind === "code"}
  <!-- This capped vertical scroller must remain keyboard reachable in WebKit. -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div class="code-region" role="region" aria-label="Code" tabindex="0"><ExtensionCode source={str(node.text)} language={str(node.language)} /></div>
{:else if kind === "keyvalue"}
  <dl class="kv">
    {#each Array.isArray(node.items) ? (node.items as Record<string, unknown>[]) : [] as item, i (i)}
      <dt>{str(item?.key)}</dt>
      <dd class="tone-{tone(item?.tone)}">{str(item?.value)}</dd>
    {/each}
  </dl>
{:else if kind === "badge"}
  <span class="badge tone-{t}">{str(node.text)}</span>
{:else if kind === "status"}
  <!-- A process's state as one pill (a build: building, built, errors),
       an optional muted detail beside it. -->
  <span class="status-wrap">
    <span class="status st-{statusState}" role="status">
      {#if statusState === "busy"}<span class="spin" aria-hidden="true"></span>{:else}{@render icon(
          statusState === "ok" ? "check" : statusState === "idle" ? "clock" : "alert",
          undefined,
          13,
        )}{/if}
      <span>{str(node.text)}</span>
    </span>
    {#if typeof node.detail === "string" && node.detail !== ""}<span class="status-detail">{node.detail}</span>{/if}
  </span>
{:else if kind === "segmented"}
  <div class="segmented" role="radiogroup" aria-label={str(node.label, str(node.name))}>
    {#each segOptions as o (o.value)}
      <button
        role="radio"
        class="seg"
        class:on={o.value === str(inputValue)}
        aria-checked={o.value === str(inputValue)}
        title={o.title}
        disabled={node.disabled === true || screen.busy}
        onclick={() => o.value !== str(inputValue) && setInput(o.value)}
        >{#if o.icon !== null}{@render icon(o.icon, undefined, 13)}{/if}{o.label}</button
      >
    {/each}
  </div>
{:else if kind === "icon"}
  <span class="icon-node tone-{t}">{@render icon(node.name, str(node.label, str(node.name)), 16)}</span>
{:else if kind === "progress"}
  <div class="progress" role="progressbar" aria-label={str(node.label, "progress")} aria-valuemin={0} aria-valuemax={100} aria-valuenow={progress === null ? undefined : Math.round(progress * 100)}>
    <div class="bar" class:indeterminate={progress === null}>
      <span style:width={progress === null ? undefined : `${progress * 100}%`}></span>
    </div>
    {#if typeof node.label === "string"}<span class="progress-label">{node.label}</span>{/if}
  </div>
{:else if kind === "empty"}
  <div class="empty">
    <div class="empty-title">{str(node.title)}</div>
    {#if typeof node.text === "string"}<div class="empty-text">{node.text}</div>{/if}
    {#if typeof node.action === "object" && node.action !== null}
      {@render actionButton({ ...(node.action as Record<string, unknown>), tone: "accent" })}
    {/if}
  </div>
{:else if kind === "callout"}
  <div class="callout tone-{t}" role={t === "bad" || t === "warn" ? "alert" : "note"}>
    <span class="callout-icon">{@render icon(t === "bad" || t === "warn" ? "alert" : "info", undefined, 15)}</span>
    <div class="callout-body">
      {#if typeof node.title === "string"}<div class="callout-title">{node.title}</div>{/if}
      <div class="callout-text">{str(node.text)}</div>
    </div>
    {#if calloutActions.length > 0}
      <div class="callout-actions">
        {#each calloutActions as a, i (i)}<UiNode node={a} depth={depth + 1} />{/each}
      </div>
    {/if}
  </div>
{:else if kind === "list"}
  <div class="list-wrap">
    <ul class="list">
      {#each listItems as item, i (i)}
        <li class="item">
          <div class="item-main">
            {#if typeof item?.action === "string" || typeof item?.file === "string"}
              <button
                class="item-title link"
                onclick={() =>
                  typeof item.action === "string"
                    ? run(item.action, item.payload)
                    : run("open-file", { file: item.file, line: item.line })}>{str(item.title)}</button
              >
            {:else}
              <span class="item-title">{str(item?.title)}</span>
            {/if}
            {#if typeof item?.subtitle === "string"}<span class="item-sub">{item.subtitle}</span>{/if}
          </div>
          {#each Array.isArray(item?.badges) ? (item.badges as Record<string, unknown>[]) : [] as b, j (j)}
            <span class="badge tone-{tone(b?.tone)}">{str(b?.text)}</span>
          {/each}
          {#each nodes(item?.actions) as a, j (j)}
            <UiNode node={a} depth={depth + 1} />
          {/each}
        </li>
      {/each}
    </ul>
    {#if more !== null}
      <button class="opt small" disabled={loadingMore} onclick={() => void loadMore()}>
        {loadingMore ? "loading…" : "Show more"}
      </button>
    {/if}
    {#if moreError !== null}<p class="text tone-bad small">{moreError}</p>{/if}
  </div>
{:else if kind === "table"}
  <div class="table-wrap">
    <table class="table">
      <thead>
        <tr>{#each columns as c (c.key)}<th class="al-{c.align}" scope="col">{c.title}</th>{/each}</tr>
      </thead>
      <tbody>
        {#each tableRows as row, i (i)}
          <tr>{#each columns as c (c.key)}<td class="al-{c.align}">{str(row?.[c.key])}</td>{/each}</tr>
        {/each}
      </tbody>
    </table>
    {#if more !== null}
      <button class="opt small" disabled={loadingMore} onclick={() => void loadMore()}>
        {loadingMore ? "loading…" : "Show more"}
      </button>
    {/if}
    {#if moreError !== null}<p class="text tone-bad small">{moreError}</p>{/if}
  </div>
{:else if kind === "file"}
  <button class="file-card" onclick={() => run("open-file", { file: node.path, line: node.line })}>
    <FileIcon path={str(node.path)} size={16} />
    <span class="file-name">{str(node.label, str(node.path).split("/").pop() ?? "")}</span>
    <span class="file-path">{str(node.path)}</span>
  </button>
{:else if kind === "link"}
  {#if typeof node.href === "string"}
    <button class="link" title={node.href} onclick={() => openHref(str(node.href))}>
      {str(node.text)}{@render icon("external", undefined, 11)}
    </button>
  {:else}
    <button class="link" onclick={() => run("open-file", { file: node.file, line: node.line })}>{str(node.text)}</button>
  {/if}
{:else if kind === "image"}
  <div class="rich image" role="img" aria-label={str(node.alt)}>
    {#if resolveError !== null}
      <p class="text tone-bad small">{resolveError}</p>
    {:else if resolved !== null && ImageView !== null}
      <ImageView path={resolved} />
    {/if}
  </div>
{:else if kind === "button"}
  {@render actionButton(node)}
{:else if kind === "toggle"}
  <div class="field inline">
    <Switch
      on={inputValue === true}
      label={str(node.label)}
      disabled={node.disabled === true || screen.busy}
      onToggle={(next) => setInput(next)}
    />
    <span class="field-label">{str(node.label)}</span>
  </div>
{:else if kind === "select"}
  <label class="field">
    <span class="field-label">{str(node.label)}</span>
    <select
      value={str(inputValue)}
      disabled={node.disabled === true || screen.busy}
      onchange={(e) => setInput((e.currentTarget as HTMLSelectElement).value)}
    >
      {#each Array.isArray(node.options) ? (node.options as unknown[]) : [] as o, i (i)}
        {@const value = typeof o === "object" && o !== null ? str((o as Record<string, unknown>).value) : str(o)}
        {@const label = typeof o === "object" && o !== null ? str((o as Record<string, unknown>).label, value) : str(o)}
        <option {value}>{label}</option>
      {/each}
    </select>
  </label>
{:else if kind === "textfield"}
  <label class="field">
    <span class="field-label">{str(node.label)}</span>
    {#if node.multiline === true}
      <textarea
        rows="4"
        placeholder={str(node.placeholder)}
        value={str(inputValue)}
        oninput={(e) => form !== undefined && setInput((e.currentTarget as HTMLTextAreaElement).value)}
        onchange={(e) => form === undefined && setInput((e.currentTarget as HTMLTextAreaElement).value)}
      ></textarea>
    {:else}
      <input
        type="text"
        placeholder={str(node.placeholder)}
        value={str(inputValue)}
        oninput={(e) => form !== undefined && setInput((e.currentTarget as HTMLInputElement).value)}
        onchange={(e) => form === undefined && setInput((e.currentTarget as HTMLInputElement).value)}
      />
    {/if}
  </label>
{:else if kind === "form"}
  <form
    class="form stack gap-m"
    onsubmit={(e) => {
      e.preventDefault();
      run(node.action, node.payload, { form: { ...formCtx.values } });
    }}
  >
    {@render kids(children)}
    <div class="row gap-s">
      <button class="opt primary small" type="submit" disabled={screen.busy}>{str(node.submit, "Submit")}</button>
    </div>
  </form>
{:else if kind === "editor" || kind === "pdf" || kind === "log"}
  <div class="rich" class:tall={kind !== "log"}>
    {#if resolveError !== null}
      <p class="text tone-bad small">{resolveError}</p>
    {:else if resolved !== null && FileView !== null}
      <FileView path={resolved} wsRoot={screen.wsRoot} plugins={false} pathBar={false} />
    {/if}
  </div>
{:else if kind === "diagnostics"}
  {#if problemsError !== null}
    <p class="text tone-bad small">{problemsError}</p>
  {:else if problems !== null && (shownProblems.length > 0 || notes.length > 0 || node.quiet !== true)}
    <div class="problems" class:compact={node.compact === true}>
      <div class="problems-head">
        <span class="problems-title">{str(node.title, "Problems")}</span>
        {#if counts.error > 0}<span class="count tone-bad">{counts.error} error{counts.error === 1 ? "" : "s"}</span>{/if}
        {#if counts.warning > 0}<span class="count tone-warn">{counts.warning} warning{counts.warning === 1 ? "" : "s"}</span>{/if}
        {#if shownProblems.length === 0 && notes.length === 0}<span class="count tone-good">none</span>{/if}
        <span class="spacer"></span>
        {#if notes.length > 0}
          <button class="link-btn" onclick={() => (showNotes = !showNotes)}>
            {showNotes ? "Hide" : "Show"} {notes.length} layout note{notes.length === 1 ? "" : "s"}
          </button>
        {/if}
      </div>
      {#if visibleProblems.length > 0}
        <ul class="plist">
          {#each visibleProblems as d, i (i)}
            <li class="prow sev-{d.severity}">
              <span class="sev" aria-label={severityWord[d.severity]}
                >{@render icon(d.severity === "error" || d.severity === "warning" ? "alert" : "info", undefined, 14)}</span
              >
              <button class="pmain" title="Go to {d.file}:{d.line}" onclick={() => run("open-file", { file: d.file, line: d.line })}>
                <span class="pmsg">{d.message}</span>
                <span class="ploc"
                  >{d.file}:{d.line}{#if typeof d.context === "string" && d.context !== ""}<code class="pctx">{d.context}</code>{/if}</span
                >
              </button>
              <button
                class="opt small quiet ask"
                title="Put this problem in front of an agent, as a reference to its line"
                onclick={() => run("ask-agent", { file: d.file, line: d.line, text: `${d.message}${d.context ? ` at "${d.context}"` : ""}` })}
                >Ask agent</button
              >
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  {/if}
{:else if kind === "diff"}
  <div class="diff" role="group" aria-label="changes">
    {#if baseError !== null}<p class="text tone-bad small">{baseError}</p>{/if}
    {#each diff as line, i (i)}
      {#if line.kind === "change"}
        <div class="dl change">
          {#each line.words as w, j (j)}<span class="w-{w.kind}">{w.text}</span>{/each}
        </div>
      {:else}
        <div class="dl {line.kind}">{line.text || " "}</div>
      {/if}
    {/each}
  </div>
{/if}

<style>
  .stack {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    min-width: 0;
  }
  .row.between {
    justify-content: space-between;
  }
  .row.center {
    justify-content: center;
  }
  .row.end {
    justify-content: flex-end;
  }
  .gap-s {
    gap: 6px;
  }
  .gap-m {
    gap: 10px;
  }
  .gap-l {
    gap: 18px;
  }
  .grid {
    display: grid;
    gap: 10px;
    grid-template-columns: repeat(auto-fit, minmax(max(200px, calc(100% / var(--cols) - 10px)), 1fr));
  }
  .split {
    display: grid;
    grid-template-columns: calc(var(--ratio) * 100%) 1fr;
    gap: 8px;
    min-height: 420px;
  }
  .split .pane {
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .split .pane > :global(*) {
    flex: 1;
  }
  .split .pane > :global(.rich) {
    height: auto;
  }
  @container plugin-screen (max-width: 560px) {
    .split {
      grid-template-columns: 1fr;
    }
  }
  .tabbar {
    display: flex;
    gap: 2px;
    border-bottom: 1px solid var(--edge);
    margin-bottom: 8px;
  }
  .tab {
    appearance: none;
    background: none;
    border: none;
    border-bottom: 2px solid transparent;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-sm);
    padding: 4px 10px;
    cursor: pointer;
  }
  .tab.on {
    color: var(--fg);
    border-bottom-color: var(--accent);
  }
  .section > summary {
    cursor: pointer;
    font-weight: 600;
    font-size: var(--text-sm);
    color: var(--fg);
    padding: 2px 0;
  }
  .section-body {
    padding-top: 8px;
  }
  .card {
    border: 1px solid var(--edge);
    border-radius: 8px;
    padding: 10px 12px;
    min-width: 0;
  }
  .card-title {
    font-weight: 600;
    font-size: var(--text-sm);
  }
  .divider {
    border: none;
    border-top: 1px solid var(--edge);
    margin: 4px 0;
    width: 100%;
  }
  .text {
    margin: 0;
    font-size: var(--text-md);
    line-height: 1.45;
    overflow-wrap: anywhere;
  }
  .text.small {
    font-size: var(--text-sm);
  }
  .text.large {
    font-size: var(--text-lg);
  }
  .text.strong {
    font-weight: 600;
  }
  .text.mono {
    font-family: var(--mono);
    font-size: var(--text-sm);
  }
  .heading {
    margin: 0;
    font-weight: 600;
    color: var(--fg);
  }
  h2.heading {
    font-size: var(--text-lg);
  }
  h3.heading {
    font-size: var(--text-md);
  }
  h4.heading {
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .tone-neutral {
    color: var(--fg);
  }
  p.tone-neutral.small {
    color: var(--muted);
  }
  .tone-accent {
    color: var(--accent);
  }
  .tone-good {
    color: var(--git-added);
  }
  .tone-warn {
    color: var(--warn);
  }
  .tone-bad {
    color: var(--err);
  }
  .code-region { max-height: 360px; overflow: auto; border-radius: 5px; }
  .code-region:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 2px; }
  .kv {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 3px 14px;
    margin: 0;
    font-size: var(--text-sm);
  }
  .kv dt {
    color: var(--muted);
  }
  .kv dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .badge {
    display: inline-flex;
    align-items: center;
    font-size: var(--text-xs);
    line-height: 1.5;
    padding: 0 7px;
    border-radius: 9px;
    border: 1px solid currentColor;
    white-space: nowrap;
  }
  .badge.tone-neutral {
    color: var(--muted);
  }
  .icon {
    flex: none;
    vertical-align: -2px;
  }
  .icon-node {
    display: inline-flex;
  }
  .progress {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 120px;
  }
  .bar {
    height: 6px;
    border-radius: 3px;
    background: var(--row-hover);
    overflow: hidden;
    position: relative;
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--accent);
    border-radius: 3px;
  }
  .bar.indeterminate span {
    width: 35%;
    animation: slide 1.4s ease-in-out infinite;
  }
  @keyframes slide {
    from {
      transform: translateX(-100%);
    }
    to {
      transform: translateX(300%);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .bar.indeterminate span {
      animation: none;
      width: 100%;
      opacity: 0.4;
    }
  }
  .progress-label {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
    padding: 18px 12px;
    text-align: center;
  }
  .empty-title {
    font-weight: 600;
  }
  .empty-text {
    color: var(--muted);
    font-size: var(--text-sm);
  }
  .callout {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    border: 1px solid color-mix(in srgb, currentColor 45%, transparent);
    border-radius: 8px;
    padding: 8px 10px 8px 12px;
    font-size: var(--text-sm);
    background: color-mix(in srgb, currentColor 7%, var(--bg));
  }
  .callout.tone-neutral {
    color: var(--muted);
  }
  .callout-icon {
    flex: none;
    display: inline-flex;
    padding-top: 1px;
  }
  .callout-body {
    flex: 1;
    min-width: 0;
    color: var(--fg);
    line-height: 1.45;
  }
  .callout-title {
    font-weight: 600;
    margin-bottom: 2px;
  }
  .callout-text {
    overflow-wrap: anywhere;
  }
  .callout-actions {
    flex: none;
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-self: center;
  }
  /* --- status: one pill for a process's state --------------------------- */
  .status-wrap {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .status {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 2px 9px 2px 7px;
    border-radius: 999px;
    font-size: var(--text-sm);
    font-weight: 500;
    line-height: 1.5;
    white-space: nowrap;
    background: color-mix(in srgb, currentColor 12%, transparent);
  }
  .status.st-idle {
    color: var(--muted);
  }
  .status.st-busy {
    color: var(--accent);
  }
  .status.st-ok {
    color: var(--git-added);
  }
  .status.st-warn {
    color: var(--warn);
  }
  .status.st-bad {
    color: var(--err);
  }
  .status-detail {
    font-size: var(--text-sm);
    color: var(--muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }
  .spin {
    width: 11px;
    height: 11px;
    border-radius: 50%;
    border: 1.6px solid currentColor;
    border-right-color: transparent;
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .spin {
      animation: none;
      border-right-color: currentColor;
      opacity: 0.6;
    }
  }
  /* --- segmented: the app's own switch (FileView's Text | view) ---------- */
  .segmented {
    display: inline-flex;
    border: 1px solid var(--edge);
    border-radius: 6px;
    overflow: hidden;
    flex: none;
  }
  .segmented .seg {
    appearance: none;
    border: none;
    background: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    padding: 3px 10px;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    cursor: pointer;
  }
  .segmented .seg + .seg {
    border-left: 1px solid var(--edge);
  }
  .segmented .seg:hover:not(:disabled) {
    color: var(--fg);
  }
  .segmented .seg.on {
    background: var(--row-active);
    color: var(--fg);
  }
  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
  }
  .item {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 2px;
    border-bottom: 1px solid var(--edge);
    min-width: 0;
  }
  .item:last-child {
    border-bottom: none;
  }
  .item-main {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
  }
  .item-title {
    font-size: var(--text-sm);
    text-align: left;
    overflow-wrap: anywhere;
  }
  .item-sub {
    font-size: var(--text-xs);
    color: var(--muted);
    overflow-wrap: anywhere;
  }
  .list-wrap,
  .table-wrap {
    display: flex;
    flex-direction: column;
    gap: 6px;
    align-items: flex-start;
    min-width: 0;
  }
  .list-wrap .list,
  .table-wrap {
    width: 100%;
  }
  .table-wrap {
    overflow-x: auto;
  }
  .table {
    border-collapse: collapse;
    width: 100%;
    font-size: var(--text-sm);
  }
  .table th {
    text-align: left;
    color: var(--muted);
    font-weight: 500;
    border-bottom: 1px solid var(--edge);
    padding: 4px 8px;
  }
  .table td {
    border-bottom: 1px solid var(--edge);
    padding: 4px 8px;
  }
  .al-end {
    text-align: right !important;
  }
  .al-center {
    text-align: center !important;
  }
  .file-card {
    appearance: none;
    display: grid;
    grid-template-columns: auto 1fr;
    column-gap: 8px;
    align-items: center;
    text-align: left;
    border: 1px solid var(--edge);
    background: none;
    color: var(--fg);
    border-radius: 8px;
    padding: 6px 10px;
    cursor: pointer;
    font: inherit;
    min-width: 0;
  }
  .file-card:hover {
    background: var(--row-hover);
  }
  .file-name {
    font-size: var(--text-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .file-path {
    grid-column: 2;
    font-size: var(--text-xs);
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .link {
    appearance: none;
    background: none;
    border: none;
    padding: 0;
    color: var(--accent);
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    gap: 3px;
    text-align: left;
  }
  .link:hover {
    text-decoration: underline;
  }
  .rich {
    /* The app's viewers fill their box absolutely: this is their box. */
    position: relative;
    border: 1px solid var(--edge);
    border-radius: 8px;
    overflow: hidden;
    min-height: 160px;
    height: 240px;
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .rich.tall {
    height: 480px;
  }
  .rich > :global(*) {
    flex: 1;
    min-height: 0;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 3px;
    font-size: var(--text-sm);
    min-width: 0;
  }
  .field.inline {
    flex-direction: row;
    align-items: center;
    gap: 8px;
  }
  .field-label {
    color: var(--muted);
    font-size: var(--text-xs);
  }
  .field.inline .field-label {
    color: var(--fg);
    font-size: var(--text-sm);
  }
  .field input,
  .field select,
  .field textarea {
    font: inherit;
    font-size: var(--text-sm);
    color: var(--fg);
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 6px;
    padding: 4px 8px;
    min-width: 0;
    max-width: 420px;
  }
  .field textarea {
    resize: vertical;
  }
  .field input:focus-visible,
  .field select:focus-visible,
  .field textarea:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 1px;
  }
  .opt.danger {
    color: var(--err);
  }
  .unknown {
    border: 1px dashed var(--edge);
    border-radius: 8px;
    padding: 6px 10px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .unknown-note {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  /* --- the problems list ------------------------------------------------ */
  .problems {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--edge);
    border-radius: 8px;
    min-height: 0;
    overflow: hidden;
    background: var(--bg);
  }
  .problems-head {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    font-size: var(--text-xs);
    border-bottom: 1px solid var(--edge);
    flex: none;
  }
  .problems-title {
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .problems-head .count {
    font-weight: 500;
  }
  .problems-head .spacer {
    flex: 1;
  }
  .link-btn {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
  }
  .link-btn:hover {
    color: var(--accent);
  }
  .problems:has(.plist) .problems-head:only-child {
    border-bottom: none;
  }
  .problems:not(:has(.plist)) .problems-head {
    border-bottom: none;
  }
  .plist {
    list-style: none;
    margin: 0;
    padding: 2px 0;
    overflow: auto;
    max-height: 240px;
  }
  .problems.compact .plist {
    max-height: 168px;
  }
  .prow {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 3px 8px 3px 10px;
    min-width: 0;
  }
  .prow:hover {
    background: var(--row-hover);
  }
  .sev {
    flex: none;
    display: inline-flex;
    color: var(--muted);
  }
  .sev-error .sev {
    color: var(--err);
  }
  .sev-warning .sev {
    color: var(--warn);
  }
  .pmain {
    appearance: none;
    border: none;
    background: none;
    padding: 2px 0;
    font: inherit;
    color: inherit;
    text-align: left;
    cursor: pointer;
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .pmsg {
    font-size: var(--text-sm);
    color: var(--fg);
    overflow-wrap: anywhere;
  }
  .ploc {
    font-size: var(--text-xs);
    color: var(--muted);
    display: flex;
    gap: 8px;
    min-width: 0;
    white-space: nowrap;
  }
  .pctx {
    font-family: var(--mono);
    font-size: var(--text-xs);
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }
  .prow .ask {
    flex: none;
    opacity: 0;
  }
  .prow:hover .ask,
  .prow .ask:focus-visible {
    opacity: 1;
  }
  @media (hover: none) {
    .prow .ask {
      opacity: 1;
    }
  }
  .diff {
    font-family: var(--mono);
    font-size: var(--text-sm);
    border: 1px solid var(--edge);
    border-radius: 8px;
    overflow: auto;
    max-height: 420px;
  }
  .dl {
    padding: 1px 10px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .dl.add {
    background: color-mix(in srgb, var(--git-added) 14%, transparent);
  }
  .dl.del {
    background: color-mix(in srgb, var(--git-deleted) 14%, transparent);
    text-decoration: line-through;
    text-decoration-color: color-mix(in srgb, var(--git-deleted) 60%, transparent);
  }
  .dl.change {
    background: color-mix(in srgb, var(--git-modified) 8%, transparent);
  }
  .w-add {
    background: color-mix(in srgb, var(--git-added) 28%, transparent);
    border-radius: 2px;
  }
  .w-del {
    background: color-mix(in srgb, var(--git-deleted) 24%, transparent);
    text-decoration: line-through;
    border-radius: 2px;
  }
  .markdown {
    font-size: var(--text-md);
    min-width: 0;
  }
</style>
