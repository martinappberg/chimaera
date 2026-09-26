<script lang="ts">
  /**
   * A JSON Canvas drawn in world coordinates (BoardView pans and zooms it):
   * groups at the back with their labels above them, then the edges (cubic
   * curves between named sides, arrow ends, labels), then the cards. Text
   * cards are markdown through `marked` + DOMPurify with chat's profile (no
   * style, web links in a new tab without an opener); a relative image loads
   * through a `/raw` ticket and a remote one is never fetched. File cards
   * resolve Obsidian's vault-relative paths by trying the canvas's folder and
   * then each parent, and open in a pane on click (Cmd/Ctrl: beside); image
   * files draw inline. Link cards show the address and open it like any
   * other web link — no preview is fetched. Colors are the theme's own.
   */
  import DOMPurify from "dompurify";
  import { Marked } from "marked";
  import { basename, dirname, fsValidate, isImagePath, rawTicketUrl, resolveDocPath, safeDecodeUri } from "../files";
  import { openPath } from "../../shared/openPath";
  import { activateUrl } from "../../shared/urlOpen";
  import { followDocHref, showLinkHint } from "../docLinks";
  import FileIcon from "../../shared/FileIcon.svelte";
  import {
    arrowPath,
    edgeGeometry,
    facingSides,
    linkLabel,
    type CanvasDoc,
    type CanvasNode,
    type EdgeGeometry,
  } from "./canvas";

  interface Props {
    doc: CanvasDoc;
    path: string;
    wsRoot?: string | null;
  }

  let { doc, path, wsRoot = null }: Props = $props();

  const b = $derived(doc.bounds);
  const byId = $derived(new Map(doc.nodes.map((n) => [n.id, n])));
  const groups = $derived(doc.nodes.filter((n) => n.type === "group"));
  const cards = $derived(doc.nodes.filter((n) => n.type !== "group"));

  interface DrawnEdge {
    id: string;
    geo: EdgeGeometry;
    color: string;
    fromArrow: boolean;
    toArrow: boolean;
    label: string | null;
  }
  const edges = $derived.by<DrawnEdge[]>(() => {
    const out: DrawnEdge[] = [];
    for (const e of doc.edges) {
      const from = byId.get(e.from);
      const to = byId.get(e.to);
      if (from === undefined || to === undefined) continue;
      const auto = facingSides(from, to);
      const geo = edgeGeometry(from, e.fromSide ?? auto[0], to, e.toSide ?? auto[1]);
      out.push({
        id: e.id,
        geo,
        color: e.color ?? "color-mix(in srgb, var(--fg) 45%, transparent)",
        fromArrow: e.fromEnd === "arrow",
        toArrow: e.toEnd === "arrow",
        label: e.label,
      });
    }
    return out;
  });

  // --- file cards -----------------------------------------------------------------

  interface Resolved {
    path: string;
    kind: "file" | "dir";
  }
  /** file field → where it is (null: not found); absent while checking. */
  let resolved = $state<Record<string, Resolved | null>>({});
  let imageUrls = $state<Record<string, string>>({});

  /** The canvas's folder and its parents, nearest first: Obsidian writes
   *  file paths relative to the vault root, wherever the canvas sits. */
  function bases(): string[] {
    const out: string[] = [];
    let dir = dirname(path);
    const stop = wsRoot !== null && dir.startsWith(wsRoot) ? wsRoot : null;
    for (let i = 0; i < 8; i++) {
      out.push(dir);
      if (dir === "/" || dir === stop) break;
      const up = dirname(dir);
      if (up === dir) break;
      dir = up;
    }
    return out;
  }

  $effect(() => {
    const files = [...new Set(doc.nodes.flatMap((n) => (n.type === "file" ? [n.file] : [])))];
    if (files.length === 0) return;
    let stale = false;
    const [base, ...rest] = bases();
    void fsValidate(files, base, null, rest, { strict: true }).then(
      (res) => {
        if (stale) return;
        const next: Record<string, Resolved | null> = {};
        for (const f of files) next[f] = res.valid[f] ?? null;
        resolved = next;
        for (const f of files) {
          const hit = next[f];
          if (hit !== null && hit.kind === "file" && isImagePath(hit.path)) {
            void rawTicketUrl(hit.path).then(
              (u) => {
                if (!stale) imageUrls[f] = u;
              },
              () => {},
            );
          }
        }
      },
      () => {
        if (stale) return;
        resolved = Object.fromEntries(files.map((f) => [f, null]));
      },
    );
    return () => {
      stale = true;
    };
  });

  function openFile(n: CanvasNode, e: MouseEvent): void {
    if (n.type !== "file") return;
    const hit = resolved[n.file];
    if (hit === null || hit === undefined) return;
    openPath(hit.path, hit.kind, { split: e.metaKey || e.ctrlKey });
  }

  function openLink(url: string, e: MouseEvent): void {
    if (linkLabel(url).web) activateUrl(url, e.metaKey || e.ctrlKey);
  }

  // --- text cards -----------------------------------------------------------------

  // Obsidian renders a single newline in a card as a line break.
  const marked = new Marked({ gfm: true, breaks: true, async: false });
  // A private sanitizer instance, so its hook neither leaks into nor depends
  // on chat's global one.
  const purify = DOMPurify(window);
  purify.addHook("afterSanitizeAttributes", (node) => {
    if (node instanceof Element && node.tagName === "A" && /^https?:/i.test(node.getAttribute("href") ?? "")) {
      node.setAttribute("target", "_blank");
      node.setAttribute("rel", "noopener noreferrer");
    }
  });

  let rootEl = $state<HTMLDivElement | null>(null);

  function renderText(node: HTMLElement, source: string): void {
    // Obsidian's `![[embed]]` / `[[link]]` read as plain text here.
    const frag = purify.sanitize(marked.parse(source) as string, {
      FORBID_TAGS: ["style", "form", "input", "button", "textarea", "select"],
      FORBID_ATTR: ["style"],
      RETURN_DOM_FRAGMENT: true,
    });
    // Fixed up while detached: nothing loads from a fragment.
    for (const img of frag.querySelectorAll("img")) {
      const src = img.getAttribute("src") ?? "";
      img.removeAttribute("src");
      if (/^data:image\//i.test(src)) {
        img.setAttribute("src", src);
      } else if (src !== "" && !/^[a-z][a-z0-9+.-]*:/i.test(src) && !src.startsWith("//")) {
        const target = resolveDocPath(path, safeDecodeUri(src.split("#")[0]));
        void rawTicketUrl(target).then(
          (url) => img.setAttribute("src", url),
          () => img.setAttribute("alt", `${img.alt || src} (not found)`),
        );
      } else {
        // A remote image is never fetched: say what it was instead.
        const note = document.createElement("span");
        note.className = "cv-remote";
        note.textContent = `image not loaded: ${img.alt || src}`;
        note.title = src;
        img.replaceWith(note);
      }
    }
    node.replaceChildren(frag);
  }

  function markdown(node: HTMLElement, source: string) {
    renderText(node, source);
    return { update: (next: string) => renderText(node, next) };
  }

  /** Links inside text cards: web links open like any other, relative ones
   *  resolve against the canvas's folder (the markdown view's routing). */
  function onTextClick(e: MouseEvent): void {
    const a = (e.target as Element | null)?.closest?.("a[href]");
    if (a === null || a === undefined) return;
    e.preventDefault();
    e.stopPropagation();
    const href = a.getAttribute("href") ?? "";
    const host = rootEl;
    void followDocHref(href, e.metaKey || e.ctrlKey, {
      docPath: path,
      wsRoot,
      workspaceId: null,
      toAnchor: () => false,
      toLines: () => {},
      hint: (text) => {
        if (host !== null) showLinkHint(host, e.clientX, e.clientY, text);
      },
    });
  }

  const fileName = (f: string) => basename(f) || f;
  const fileDir = (f: string) => {
    const d = f.includes("/") ? f.slice(0, f.lastIndexOf("/")) : "";
    return d;
  };
</script>

<div class="cv-root" bind:this={rootEl} style:width="{b.w}px" style:height="{b.h}px">
  {#each groups as g (g.id)}
    {#if g.type === "group"}
      <div
        class="cv-group"
        class:tinted={g.color !== null}
        style:left="{g.x - b.x}px"
        style:top="{g.y - b.y}px"
        style:width="{g.w}px"
        style:height="{g.h}px"
        style:--c={g.color ?? "var(--muted)"}
      >
        {#if g.label !== null && g.label !== ""}<span class="cv-glabel">{g.label}</span>{/if}
      </div>
    {/if}
  {/each}

  <svg
    class="cv-edges"
    width={b.w}
    height={b.h}
    viewBox="{b.x} {b.y} {b.w} {b.h}"
    aria-hidden="true"
  >
    {#each edges as e (e.id)}
      <path d={e.geo.d} fill="none" stroke={e.color} stroke-width="2" stroke-linecap="butt" />
      {#if e.toArrow}<path d={arrowPath(e.geo.end, e.geo.endAngle)} fill={e.color} />{/if}
      {#if e.fromArrow}<path d={arrowPath(e.geo.start, e.geo.startAngle)} fill={e.color} />{/if}
    {/each}
  </svg>

  {#each cards as n (n.id)}
    <div
      class="cv-card kind-{n.type}"
      class:tinted={n.color !== null}
      style:left="{n.x - b.x}px"
      style:top="{n.y - b.y}px"
      style:width="{n.w}px"
      style:height="{n.h}px"
      style:--c={n.color ?? "var(--edge)"}
    >
      {#if n.type === "text"}
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="cv-md cv-scroll" use:markdown={n.text} onclick={onTextClick}></div>
      {:else if n.type === "file"}
        {@const hit = resolved[n.file]}
        {@const img = imageUrls[n.file]}
        {#if img !== undefined}
          <button class="cv-image" onclick={(e) => openFile(n, e)} title="{n.file} — open (⌘-click: beside)">
            <img src={img} alt={fileName(n.file)} draggable="false" />
          </button>
        {:else}
          <button
            class="cv-file"
            class:missing={hit === null}
            disabled={hit === null}
            onclick={(e) => openFile(n, e)}
            title={hit === null ? `${n.file} — not found` : `${hit?.path ?? n.file} — open (⌘-click: beside)`}
          >
            <span class="cv-row">
              <FileIcon path={n.file} size={16} />
              <span class="cv-fname">{fileName(n.file)}{#if n.subpath !== null}<span class="cv-sub">{n.subpath}</span>{/if}</span>
            </span>
            {#if fileDir(n.file) !== "" || hit === null}
              <span class="cv-fdir"
                >{fileDir(n.file)}{#if hit === null}<span class="cv-miss">{fileDir(n.file) !== "" ? " · " : ""}not found</span>{/if}</span
              >
            {/if}
          </button>
        {/if}
      {:else if n.type === "link"}
        {@const l = linkLabel(n.url)}
        <button class="cv-link" disabled={!l.web} onclick={(e) => openLink(n.url, e)} title={n.url}>
          <span class="cv-row">
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"
              ><circle cx="12" cy="12" r="9" /><path d="M3.6 9h16.8M3.6 15h16.8M12 3a15 15 0 0 1 0 18M12 3a15 15 0 0 0 0 18" /></svg
            >
            <span class="cv-host">{l.host}</span>
          </span>
          {#if l.rest !== "" && l.rest !== "/"}<span class="cv-path">{l.rest}</span>{/if}
        </button>
      {/if}
    </div>
  {/each}

  {#each edges as e (e.id)}
    {#if e.label !== null && e.label !== ""}
      <span class="cv-elabel" style:left="{e.geo.mid.x - b.x}px" style:top="{e.geo.mid.y - b.y}px">{e.label}</span>
    {/if}
  {/each}
</div>

<style>
  .cv-root {
    position: relative;
    font-size: 14px;
    color: var(--fg);
  }

  .cv-group {
    position: absolute;
    border: 2px solid color-mix(in srgb, var(--c) 45%, var(--edge));
    border-radius: 10px;
    background: color-mix(in srgb, var(--c) 5%, transparent);
  }

  .cv-group.tinted {
    border-color: color-mix(in srgb, var(--c) 70%, transparent);
    background: color-mix(in srgb, var(--c) 9%, transparent);
  }

  .cv-glabel {
    position: absolute;
    left: 2px;
    bottom: calc(100% + 6px);
    max-width: 100%;
    padding: 2px 9px;
    border-radius: 6px;
    background: color-mix(in srgb, var(--c) 18%, var(--term-bg));
    color: var(--fg);
    font-size: 15px;
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .cv-edges {
    position: absolute;
    left: 0;
    top: 0;
    overflow: visible;
    pointer-events: none;
  }

  .cv-card {
    position: absolute;
    display: flex;
    border: 2px solid color-mix(in srgb, var(--c) 100%, transparent);
    border-radius: 8px;
    background: var(--term-bg);
    overflow: hidden;
    box-shadow: 0 1px 4px color-mix(in srgb, var(--fg) 8%, transparent);
  }

  .cv-card.tinted {
    background: color-mix(in srgb, var(--c) 8%, var(--term-bg));
  }

  .cv-md {
    flex: 1;
    min-width: 0;
    overflow: auto;
    scrollbar-width: thin;
    padding: 10px 16px;
    line-height: 1.5;
    overflow-wrap: anywhere;
  }

  .cv-md :global(:first-child) {
    margin-top: 0;
  }

  .cv-md :global(:last-child) {
    margin-bottom: 0;
  }

  .cv-md :global(h1),
  .cv-md :global(h2),
  .cv-md :global(h3) {
    margin: 0.4em 0 0.3em;
    line-height: 1.25;
  }

  .cv-md :global(h1) {
    font-size: 1.5em;
  }

  .cv-md :global(h2) {
    font-size: 1.25em;
  }

  .cv-md :global(p),
  .cv-md :global(ul),
  .cv-md :global(ol),
  .cv-md :global(pre),
  .cv-md :global(blockquote) {
    margin: 0.4em 0;
  }

  .cv-md :global(ul),
  .cv-md :global(ol) {
    padding-left: 1.3em;
  }

  .cv-md :global(code) {
    font-family: var(--mono);
    font-size: 0.88em;
    padding: 0.05em 0.3em;
    border-radius: 4px;
    background: color-mix(in srgb, var(--fg) 8%, transparent);
  }

  .cv-md :global(pre) {
    padding: 0.5em 0.7em;
    border-radius: 6px;
    background: color-mix(in srgb, var(--fg) 6%, transparent);
    overflow: auto;
  }

  .cv-md :global(pre code) {
    padding: 0;
    background: none;
  }

  .cv-md :global(blockquote) {
    padding-left: 0.8em;
    border-left: 3px solid var(--edge);
    color: var(--muted);
  }

  .cv-md :global(a) {
    color: var(--accent);
  }

  .cv-md :global(img) {
    max-width: 100%;
  }

  .cv-md :global(.cv-remote) {
    display: inline-block;
    padding: 0.1em 0.5em;
    border: 1px dashed var(--edge);
    border-radius: 4px;
    color: var(--muted);
    font-size: 0.85em;
  }

  .cv-md :global(table) {
    border-collapse: collapse;
  }

  .cv-md :global(th),
  .cv-md :global(td) {
    border: 1px solid var(--edge);
    padding: 0.2em 0.5em;
  }

  .cv-file,
  .cv-link,
  .cv-image {
    appearance: none;
    flex: 1;
    min-width: 0;
    border: none;
    background: none;
    font: inherit;
    color: inherit;
    cursor: pointer;
    text-align: left;
  }

  .cv-file,
  .cv-link {
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 3px;
    padding: 10px 14px;
    position: relative;
  }

  .cv-row {
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
  }

  .cv-file :global(.ficon),
  .cv-link svg {
    flex: none;
    color: var(--muted);
  }

  .cv-fname,
  .cv-host {
    min-width: 0;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .cv-fdir,
  .cv-path {
    padding-left: 23px;
  }

  .cv-sub {
    font-weight: 400;
    color: var(--muted);
    margin-left: 0.3em;
  }

  .cv-fdir,
  .cv-path {
    color: var(--muted);
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .cv-file:hover .cv-fname,
  .cv-link:hover .cv-host {
    color: var(--accent);
  }

  .cv-file.missing,
  .cv-link:disabled {
    cursor: default;
  }

  .cv-file.missing .cv-fname {
    color: var(--muted);
    text-decoration: line-through;
  }

  .cv-miss {
    color: var(--err);
    font-size: 12px;
  }

  .cv-image {
    padding: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    background: color-mix(in srgb, var(--fg) 3%, transparent);
  }

  .cv-image img {
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
    display: block;
  }

  .cv-elabel {
    position: absolute;
    transform: translate(-50%, -50%);
    max-width: 240px;
    padding: 2px 8px;
    border-radius: 5px;
    background: var(--term-bg);
    box-shadow: 0 0 0 1px var(--edge);
    color: var(--fg);
    font-size: 13px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    pointer-events: none;
  }
</style>
