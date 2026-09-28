<script module lang="ts">
  import DOMPurify from "dompurify";
  import { marked } from "marked";
  import { chatMarkedExtensions } from "./markedExtensions";

  marked.use(...chatMarkedExtensions);

  // Agent markdown is untrusted model output rendered into the workbench DOM.
  // External links are a phishing / navigate-the-SPA-away vector, so force
  // every http(s) anchor to open in a new tab with no opener handle. Registered
  // once per module (the hook is global to DOMPurify); the per-call config
  // below forbids style tags so injected CSS can't restyle the whole workbench
  // (spoofing permission prompts, hiding controls).
  DOMPurify.addHook("afterSanitizeAttributes", (node) => {
    if (node instanceof Element && node.tagName === "A" && node.hasAttribute("href")) {
      if (/^https?:/i.test(node.getAttribute("href") ?? "")) {
        node.setAttribute("target", "_blank");
        node.setAttribute("rel", "noopener noreferrer");
      }
    }
    // A LOCAL image in agent prose (`![](figs/plot.png)`) is an embed, not a
    // URL: left as a src the browser would fetch it against the app's own
    // origin (a broken image, a stray request). Its target moves to a data
    // attribute here, before the HTML can reach the DOM, and the component
    // swaps the inert <img> for an embed card (upgradeEmbeds). Scoped to this
    // component's sanitize calls: the hook is global to DOMPurify.
    if (deferLocalImages && node instanceof Element && node.tagName === "IMG") {
      const src = node.getAttribute("src") ?? "";
      if (src !== "" && !/^(https?:|data:|blob:)/i.test(src) && !src.startsWith("//")) {
        node.removeAttribute("src");
        node.removeAttribute("srcset");
        node.setAttribute("data-md-embed", src);
      }
    }
  });
  /** True only inside this component's own `DOMPurify.sanitize` calls. */
  let deferLocalImages = false;
</script>

<script lang="ts">
  import { copyText } from "../shared/clipboard";
  import { copyLabel, copyPayload, decorateCopyTargets } from "../shared/copyDecor";
  import { markScrollRegions, watchWidth } from "../shared/scrollRegion";
  import { getSetting } from "../settings/store.svelte";
  import { advanceSegments, type SegmenterState } from "./streamSegments";
  import { RevealLedger } from "./revealLedger";
  import {
    codeSpanRefs,
    hrefRef,
    MISS_TTL_MS,
    menuPoint,
    openResolution,
    reopenResolution,
    type OpenPathFn,
    type PathResolver,
    type Resolution,
  } from "./paths";
  import { extractFileRefs, parseFileRef, revealOf, type FileRef, type FoundRef } from "../shared/fileRef";
  import { activateUrl, isWebUrl, urlMenuEntries } from "../shared/urlOpen";
  import { contextMenu } from "../shared/contextMenu.svelte";
  import { safeDecodeUri } from "../previews/files";
  import { parseSizeHint, splitTarget } from "../shared/embed/embed";
  import { mountEmbed, type EmbedHandle } from "../shared/embed/mount.svelte";
  import type { EmbedResolver } from "./embeds";

  interface Props {
    text: string;
    /** Live streaming block: reveal newly parsed words in fading batches
     *  instead of showing the whole (chunky) text at once. Settled blocks pass
     *  false and render statically. This is TURN state ("this row is the
     *  streaming tail"), deliberately independent of visibility. */
    streaming?: boolean;
    /** False while the owning tab/pane is hidden. A hidden live block FREEZES
     *  in place — no per-chunk work, no ticker, no canonical-parse swap (the
     *  hide-tax) — and resumes with its reveal cursor intact on show. */
    visible?: boolean;
    /** Open a VALIDATED path the prose references — files land in a viewer
     *  pane (at the referenced line), directories in the Finder. */
    onOpenPath?: OpenPathFn;
    /** The chat's path resolver (the daemon validates every candidate):
     *  only real files/dirs get the click affordance. */
    resolvePaths?: PathResolver;
    /** Fired after each streaming reveal batch — lets the host keep the
     *  transcript pinned to the bottom as words grow between wire chunks. */
    onReveal?: () => void;
    /** Resolves local images in the prose (`![](figs/plot.png)`) against
     *  the session's directories, so they render as embed cards. */
    embeds?: EmbedResolver;
  }

  let {
    text,
    streaming = false,
    visible = true,
    onOpenPath,
    resolvePaths,
    onReveal,
    embeds,
  }: Props = $props();

  /** Embed slots this component built, each with the (hidden) placeholder
   *  it stands beside and its card. The placeholder belongs to the rendered
   *  HTML, so its leaving the DOM (a re-render, a rewritten stream, the
   *  settle swap) is what retires the slot: the card is destroyed and the
   *  slot removed — even when it sat outside the nodes `{@html}` tracks. */
  const mountedEmbeds = new Map<HTMLElement, { img: HTMLElement; card: EmbedHandle | null }>();

  function sweepEmbeds(all = false): void {
    for (const [slot, { img, card }] of mountedEmbeds) {
      if (all || !img.isConnected || !slot.isConnected) {
        card?.destroy();
        slot.remove();
        mountedEmbeds.delete(slot);
      }
    }
  }

  /** Swap every inert local-image placeholder in `root` (the sanitizer's
   *  `data-md-embed` <img>) for an embed card in a slot this component
   *  builds. Runs on settled/closed content only — the per-chunk open tail
   *  keeps its placeholders, so a card never churns with the stream. */
  function upgradeEmbeds(root: HTMLElement): void {
    sweepEmbeds();
    for (const img of root.querySelectorAll<HTMLImageElement>("img[data-md-embed]:not(.md-embed-src)")) {
      const target = img.getAttribute("data-md-embed") ?? "";
      const { alt, width } = parseSizeHint(img.getAttribute("alt") ?? "");
      const { path, fragment } = splitTarget(target);
      const shown = safeDecodeUri(path);
      const slot = document.createElement("div");
      slot.className = "md-embed";
      // The placeholder stays (hidden) rather than being replaced: it may be
      // a top-level node of the settled `{@html}`, whose teardown walks the
      // nodes it inserted.
      img.before(slot);
      img.classList.add("md-embed-src");
      const resolver = embeds;
      if (resolver === undefined && !shown.startsWith("/")) {
        // Nowhere to resolve a relative path: say what was meant, quietly.
        slot.classList.add("md-embed-text");
        slot.textContent = alt !== "" ? `${alt} (${shown})` : shown;
        mountedEmbeds.set(slot, { img, card: null });
        continue;
      }
      const card = mountEmbed(slot, {
        path: shown,
        fragment,
        alt,
        width,
        ...(resolver !== undefined ? { resolve: () => resolver.resolve(target) } : {}),
        ...(onOpenPath !== undefined
          ? { onOpen: (p, kind, reveal) => onOpenPath(p, kind, reveal !== undefined ? { reveal } : {}) }
          : {}),
      });
      mountedEmbeds.set(slot, { img, card });
    }
  }

  /** What a stamped path affordance opens. Kept off the DOM: sanitized agent
   *  HTML can forge classes and data-* attributes, so a click honors only an
   *  element this component stamped (a forged one opens nothing). */
  const stamps = new WeakMap<Element, { ref: FileRef; res: Resolution }>();
  /** When a stamp pass last left a candidate unlinked (a miss): the pass
   *  that follows the miss TTL, or a turn end, asks again. */
  let missedAt: number | null = null;
  /** A turn ended while this block was hidden: re-stamp its misses on show. */
  let restampOnShow = false;

  // Copy-button chrome comes from the shared decorator (also used by the
  // markdown file preview): injected post-sanitize from literals only, never
  // from agent-derived strings; APPEND-only inside the host (see copyDecor.ts
  // for the {@html}-teardown constraint); for pre the CODE child is the
  // horizontal scroller so the host stays a non-scrolling anchor the button
  // can pin to. During streaming it runs per segment BEFORE that segment's
  // word wrap, so the reveal bookkeeping hides the button with its
  // still-unrevealed block.

  // Copied feedback: one button at a time; a streaming rebuild mid-feedback
  // simply drops the state with the old DOM (the next chunk replaces it).
  let copiedBtn: HTMLElement | null = null;
  let copiedTimer: ReturnType<typeof setTimeout> | null = null;

  function clearCopied() {
    if (copiedTimer !== null) {
      clearTimeout(copiedTimer);
      copiedTimer = null;
    }
    if (copiedBtn !== null && copiedBtn.isConnected) {
      copiedBtn.classList.remove("copied");
      copiedBtn.setAttribute("aria-label", copyLabel(copiedBtn.closest("pre, blockquote") ?? copiedBtn));
      copiedBtn.title = "copy";
    }
    copiedBtn = null;
  }

  function showCopied(btn: HTMLElement) {
    clearCopied();
    copiedBtn = btn;
    btn.classList.add("copied");
    btn.setAttribute("aria-label", "copied");
    btn.title = "copied";
    copiedTimer = setTimeout(() => {
      copiedTimer = null;
      clearCopied();
    }, 1400);
  }

  function markPath(node: Element, label: string, ref: FileRef, res: Resolution) {
    if (res.state === "miss") return;
    node.classList.add("md-path");
    node.classList.toggle("md-ambiguous", res.state === "ambiguous");
    node.setAttribute("role", "button");
    // Generated prose/code spans are not naturally focusable. Anchors already
    // are, so only add a tab stop to the synthetic controls.
    if (node.tagName !== "A") node.setAttribute("tabindex", "0");
    const at = ref.line !== undefined ? ` at line ${ref.line}` : "";
    node.setAttribute(
      "title",
      res.state === "ambiguous"
        ? `${label} matches ${res.matches.length} files — choose one`
        : res.hit.kind === "dir"
          ? `browse ${label} in the finder`
          : `open ${label}${at} in a pane`,
    );
    stamps.set(node, { ref, res });
  }

  /** Undo `markPath`: the reference no longer resolves. */
  function unmarkPath(node: Element) {
    stamps.delete(node);
    node.classList.remove("md-path", "md-ambiguous");
    node.removeAttribute("role");
    node.removeAttribute("title");
    if (node.tagName !== "A") node.removeAttribute("tabindex");
  }

  /** Wrap each found reference in a text node with its affordance. Right to
   *  left, so earlier offsets stay valid across splits. */
  function wrapRefs(node: Text, found: { f: FoundRef; res: Resolution }[]) {
    for (let i = found.length - 1; i >= 0; i--) {
      const { f, res } = found[i];
      const tail = node.splitText(f.start);
      tail.splitText(f.end - f.start);
      const span = document.createElement("span");
      markPath(span, tail.data, f.ref, res);
      tail.parentNode?.replaceChild(span, tail);
      span.appendChild(tail);
    }
  }

  function textNodes(root: Node, skip: string): Text[] {
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
      acceptNode: (n) =>
        n.parentElement?.closest(skip) == null ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_REJECT,
    });
    const nodes: Text[] = [];
    while (walker.nextNode()) nodes.push(walker.currentNode as Text);
    return nodes;
  }

  /** Stamp the click affordance onto inline code spans, local markdown
   *  links AND bare prose references that the daemon resolves. Unknown
   *  candidates batch to the resolver; when any comes back linkable this
   *  root re-stamps (at idle) from its cache. */
  function stampPaths(root: HTMLElement) {
    const resolver = resolvePaths;
    if (onOpenPath === undefined || resolver === undefined) return;
    const unknown = new Set<string>();
    let missed = false;
    const want = (ref: FileRef): Resolution | null => {
      const res = resolver.peek(ref.path);
      if (res === undefined) unknown.add(ref.path);
      else if (res.state === "miss") missed = true;
      else return res;
      return null;
    };
    const stampText = (node: Text) => {
      const found: { f: FoundRef; res: Resolution }[] = [];
      for (const f of extractFileRefs(node.data)) {
        const res = want(f.ref);
        if (res !== null) found.push({ f, res });
      }
      wrapRefs(node, found);
    };
    // Inline code: the whole span when it is a reference, else the
    // references inside it (`cat results/x.csv`).
    for (const code of root.querySelectorAll("code")) {
      if (code.closest("pre, a, .md-path, .md-embed") !== null || code.querySelector(".md-path") !== null) {
        continue;
      }
      const t = code.textContent ?? "";
      const { whole, parts } = codeSpanRefs(t);
      const wholeRes = whole !== null ? want(whole) : null;
      if (whole !== null && wholeRes !== null) {
        markPath(code, t.trim(), whole, wholeRes);
        continue;
      }
      if (parts.length > 0) for (const node of textNodes(code, ".md-path, .katex")) stampText(node);
    }
    // Markdown links to a LOCAL path ("[demo.csv](demo-assets/demo.csv)",
    // "[x](src/a.rs#L10)") — agents write these constantly. A schemeless
    // target that resolves opens in a pane instead of navigating the SPA;
    // one that doesn't is neutralized on click (below). DOMPurify drops an
    // href it cannot classify (`main.rs:12`), so a link left without one
    // offers its text instead.
    for (const a of root.querySelectorAll("a")) {
      if (a.classList.contains("md-path") || a.closest(".md-embed") !== null) continue;
      const href = a.getAttribute("href") ?? "";
      let ref: FileRef | null;
      if (href === "") {
        ref = parseFileRef(a.textContent ?? "", { delimited: true });
      } else {
        if (/^[a-z][a-z0-9+.-]*:/i.test(href) || href.startsWith("#")) continue;
        a.classList.add("md-local");
        ref = hrefRef(href);
      }
      if (ref === null) continue;
      const res = want(ref);
      if (res !== null) markPath(a, ref.path, ref, res);
    }
    // Bare references in prose ("saved to results/plot.png:12"). Collected
    // first: wrapping mutates the walked tree.
    for (const node of textNodes(root, "pre, code, a, .md-path, .katex, .md-embed")) stampText(node);
    if (missed) missedAt = Date.now();
    if (unknown.size === 0) return;
    void resolver.resolve(unknown).then((linkable) => {
      if (!linkable) {
        missedAt = Date.now();
        return;
      }
      // Re-stamp at idle, and ONLY the root this sweep walked: a synchronous
      // whole-tree re-walk here re-created the O(message) per-chunk cost this
      // pipeline removed. If the stream settled while the batch was in
      // flight, that root is gone — the canonical parse replaced it — so the
      // settled root takes the re-stamp (the settle race).
      const target = root.isConnected ? root : !streaming ? el : null;
      if (target !== null) enqueueStamp(target);
    });
  }

  /** Synchronously mark schemeless (non-#) anchors `md-local` so the click
   *  handler swallows them from the FIRST paint of a streamed fragment: an
   *  unclassified relative href would fall through to real SPA navigation
   *  and blow away the workbench. Classification is a cheap class toggle;
   *  the expensive daemon VALIDATION (stampPaths) stays deferred to idle,
   *  which later upgrades these into openable path affordances. */
  function classifyLocalAnchors(root: HTMLElement): void {
    for (const a of root.querySelectorAll("a")) {
      const href = a.getAttribute("href") ?? "";
      if (href === "" || /^[a-z][a-z0-9+.-]*:/i.test(href) || href.startsWith("#")) continue;
      a.classList.add("md-local");
    }
  }

  function onClick(e: MouseEvent) {
    const target = e.target as Element | null;
    // Copy affordance — delegated, so it survives the per-chunk subtree
    // rebuild. SECURITY: the payload is resolved from the live DOM at click
    // time (the sibling pre's innerText), never from an attribute. Sanitized
    // agent HTML may forge the classes (DOMPurify allows <button class=…>),
    // but a forged button can only ever copy its own visible code block —
    // which is the feature. innerText, not textContent: DOMPurify's default
    // allowlist keeps the `hidden` attribute (and class names like our own
    // .rw-hidden are forgeable), so textContent could smuggle invisible text
    // into the payload; innerText copies exactly what is rendered.
    const copyBtn = target?.closest?.("button.md-copy");
    if (copyBtn instanceof HTMLElement) {
      const code = copyPayload(copyBtn);
      if (code.length > 0) {
        void copyText(code).then((ok) => {
          if (ok && copyBtn.isConnected) showCopied(copyBtn);
        });
      }
      return;
    }
    const node = target?.closest?.(".md-path");
    if (node !== null && node !== undefined && stamps.has(node)) {
      // An anchor would navigate the SPA away; a validated path opens a pane.
      if (node.tagName === "A") e.preventDefault();
      activatePath(node, e);
      return;
    }
    // A local-path anchor that never validated: still swallow the click so a
    // stale relative href can't replace the whole workbench with a 404 —
    // and ask again now (the file may exist since the last answer).
    const local = target?.closest?.("a.md-local");
    if (local !== null && local !== undefined) {
      e.preventDefault();
      retryLocal(local, e);
      return;
    }
    // A web link. The anchor carries target=_blank as a fallback, but in the
    // native app nothing receives a new-window request (the shell's navigation
    // guard admits only the daemon origin), so an untouched click went
    // nowhere. Route it: a live local app opens in a browser pane, anything
    // else in the user's real browser via the shell.
    const web = target?.closest?.("a[href]");
    const href = web?.getAttribute("href") ?? "";
    if (web !== null && web !== undefined && isWebUrl(href)) {
      e.preventDefault();
      activateUrl(href, e.metaKey || e.ctrlKey);
    }
  }

  /** Right-click a rendered link: Chimaera / Browser / Copy. */
  function onContextMenu(e: MouseEvent) {
    const target = e.target as Element | null;
    const web = target?.closest?.("a[href]");
    const href = web?.getAttribute("href") ?? "";
    if (web === null || web === undefined || !isWebUrl(href)) return;
    contextMenu.openAt(e, urlMenuEntries(href));
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key !== "Enter" && e.key !== " ") return;
    const target = e.target as Element | null;
    const node = target?.closest?.(".md-path");
    if (node === null || node === undefined || !stamps.has(node)) return;
    e.preventDefault();
    activatePath(node, e);
  }

  /** Open what a stamped affordance names: a file at its line (Cmd/Ctrl:
   *  in a split), a directory in the Finder, an ambiguous name via a menu
   *  of its matches — as the daemon answers NOW (the stamp may predate a
   *  move or a delete). A reference that is gone loses its affordance. */
  function activatePath(node: Element, e: MouseEvent | KeyboardEvent) {
    const stamp = stamps.get(node);
    if (stamp === undefined || onOpenPath === undefined) return;
    const opts = {
      split: e.metaKey || e.ctrlKey,
      reveal: revealOf(stamp.ref),
      at: menuPoint(e, node),
      label: (p: string) => resolvePaths?.label(p) ?? p,
    };
    void reopenResolution(resolvePaths, stamp.ref.path, stamp.res, onOpenPath, opts).then((res) => {
      if (!node.isConnected || stamps.get(node) !== stamp) return;
      if (res.state === "miss") unmarkPath(node);
      else if (res !== stamp.res) markPath(node, node.textContent ?? stamp.ref.path, stamp.ref, res);
    });
  }

  /** A local link with no standing answer: resolve it now (a click always
   *  retries a miss) and open it if it resolves. */
  function retryLocal(a: Element, e: MouseEvent) {
    const ref = hrefRef(a.getAttribute("href") ?? "");
    const resolver = resolvePaths;
    const open = onOpenPath;
    if (ref === null || open === undefined || resolver === undefined) return;
    const opts = { split: e.metaKey || e.ctrlKey, reveal: revealOf(ref), at: menuPoint(e, a) };
    void resolver.resolveNow(ref.path).then((res) => {
      if (res === undefined || res.state === "miss") return;
      if (a.isConnected) markPath(a, ref.path, ref, res); // it links from now on
      openResolution(res, open, { ...opts, label: (p) => resolver.label(p) });
    });
  }

  /** Hovering a message whose misses have expired asks about them again. */
  function onPointerEnter() {
    if (missedAt === null || streaming || el === null) return;
    if (Date.now() - missedAt < MISS_TTL_MS) return;
    missedAt = null;
    enqueueStamp(el);
  }

  // Agent prose is untrusted model output rendered into the workbench DOM:
  // sanitize EVERYTHING marked emits, always — the full canonical parse AND
  // every per-segment streaming fragment go through here before touching the
  // DOM. The style tag is on DOMPurify's default allowlist, so forbid it
  // explicitly (and the style attribute) — otherwise injected CSS applies
  // document-wide.
  function sanitizeHtml(raw: string): string {
    deferLocalImages = true;
    try {
      return DOMPurify.sanitize(raw, { FORBID_TAGS: ["style"], FORBID_ATTR: ["style"] });
    } finally {
      deferLocalImages = false;
    }
  }

  function parseSanitized(source: string): string {
    return sanitizeHtml(marked.parse(source, { async: false, breaks: true }) as string);
  }

  /** Canonical full parse — the settled transcript's single source of truth.
   *  Lazily computed: the template reads it ONLY when not streaming, so a live
   *  block never pays a whole-message parse per chunk, and the moment the
   *  stream settles this one read IS the canonical re-parse that heals any
   *  segmentation artifact — the settled render is identical to a
   *  never-streamed render by construction. */
  const html = $derived(parseSanitized(text));

  let el = $state<HTMLElement | null>(null);
  /** The streaming container (Svelte-owned shell, imperatively-managed
   *  children) — present only while `streaming`. */
  let liveEl = $state<HTMLElement | null>(null);

  // --- incremental streaming render ------------------------------------------
  // Wire chunks arrive coalesced (2 KiB / 100 ms). Re-parsing + re-sanitizing
  // + re-wrapping the WHOLE accumulated message per chunk is O(n²) — a
  // multi-thousand-word reply burns tens of ms per chunk near its end. Instead
  // the source is segmented at SAFE top-level block boundaries
  // (streamSegments.ts — which also owns the security invariant: streaming
  // must never render MORE than the settled parse would): a closed segment
  // parses, sanitizes, decorates, and word-wraps exactly once and its DOM is
  // never touched again; only the trailing open segment re-renders per chunk,
  // so per-chunk work tracks the tail's size, not the message's. Word-reveal
  // spans exist only for words not yet revealed (revealed segments dissolve
  // their spans — see scheduleUnwrap), and path stamping defers to idle.
  // A HIDDEN live block freezes in place: no per-chunk work, no ticker, and
  // crucially no canonical-parse swap at hide time; it resumes with the
  // reveal cursor intact on show. When `streaming` flips false (the row
  // stops being the streaming tail — new block appended, or turn end) the
  // template swaps to the canonical `{@html html}` full parse: one
  // whole-message re-parse that also guarantees a span-free settled DOM
  // (word-per-span text copies with a hard newline at every visual wrap
  // point — the canonical swap is what keeps settled selection-copy clean).
  const REVEAL_TICK_MS = 75;
  /** Slightly past the 0.32s stream-fade, so dissolving a drained segment's
   *  spans never cuts a running fade short. */
  const UNWRAP_DELAY_MS = 400;
  const reducedMotion =
    typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;

  let segState: SegmenterState | null = null;
  /** Wrapper of the open segment — always liveEl's last child. */
  let tailEl: HTMLElement | null = null;
  let lastTailSource: string | null = null;
  /** The reveal-cursor arithmetic (pure, tested in revealLedger.test.ts).
   *  The queues below mirror its counts entry-for-entry. */
  const ledger = new RevealLedger();
  /** Hidden word spans in CLOSED segments (document order), each with its
   *  segment root so a drained segment can dissolve its spans. */
  let prefixQueue: { span: HTMLElement; root: HTMLElement }[] = [];
  /** Hidden word spans in the open tail — rebuilt with it every chunk. */
  let tailQueue: HTMLElement[] = [];
  /** Remaining hidden spans per closed-segment root — the drain detector. */
  const hiddenPerRoot = new Map<HTMLElement, number>();
  /** Blocks whose chrome hides until their first word (the probe) reveals —
   *  a heading or list bullet must not flash its margins/marker above the
   *  reveal point. */
  let hiddenContainers: { el: HTMLElement; probe: HTMLElement }[] = [];
  let revealTimer: ReturnType<typeof setTimeout> | null = null;
  /** Closed-segment roots awaiting deferred (idle) path stamping. */
  let unstamped: HTMLElement[] = [];
  let cancelIdleStamp: (() => void) | null = null;
  /** Pending span-dissolve timers for drained segments (teardown-tracked). */
  const unwrapTimers = new Set<ReturnType<typeof setTimeout>>();

  function clearReveal() {
    if (revealTimer !== null) {
      clearTimeout(revealTimer);
      revealTimer = null;
    }
  }

  const wordFilter = {
    acceptNode: (n: Node) =>
      (n.textContent ?? "").trim().length > 0 && n.parentElement?.closest(".katex, .md-embed") == null
        ? NodeFilter.FILTER_ACCEPT
        : NodeFilter.FILTER_REJECT,
  };

  /** One walk collects the word-bearing text nodes AND the word count, so a
   *  fully-revealed closing segment never pays a second wrap pass. */
  function collectWordNodes(root: HTMLElement): { nodes: Text[]; count: number } {
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, wordFilter);
    const nodes: Text[] = [];
    let count = 0;
    while (walker.nextNode()) {
      const node = walker.currentNode as Text;
      nodes.push(node);
      count += [...(node.textContent ?? "").matchAll(/\S+/g)].length;
    }
    return { nodes, count };
  }

  /** Wrap every whitespace-delimited run of the pre-collected nodes in a
   *  `.rw` span, in document order. Inline spans preserve flow and
   *  whitespace, so a wrapped word is visually inert until hidden. */
  function wrapFromNodes(nodes: Text[]): HTMLElement[] {
    const spans: HTMLElement[] = [];
    for (const node of nodes) {
      const matches = [...(node.textContent ?? "").matchAll(/\S+/g)];
      const local: HTMLElement[] = [];
      // Right-to-left so earlier match indices stay valid across splits.
      for (let i = matches.length - 1; i >= 0; i--) {
        const start = matches[i].index ?? 0;
        const tail = node.splitText(start);
        tail.splitText(matches[i][0].length);
        const span = document.createElement("span");
        span.className = "rw";
        tail.parentNode?.replaceChild(span, tail);
        span.appendChild(tail);
        local.push(span);
      }
      local.reverse();
      spans.push(...local);
    }
    return spans;
  }

  /** Dissolve a drained segment's reveal spans back into plain text nodes.
   *  Merely revealed spans are not enough: the copy/selection serializer
   *  emits a line break instead of the collapsed space wherever text wraps
   *  between inline elements, so word-per-span prose copies out with a hard
   *  newline at every visual wrap point — and closed-segment DOM now
   *  SURVIVES across chunks, making mid-stream selection meaningful.
   *  Children are preserved, not flattened — stampPaths may have nested a
   *  path affordance inside a word span. */
  function unwrapWords(root: HTMLElement) {
    for (const span of root.querySelectorAll("span.rw")) {
      const parent = span.parentNode;
      if (parent === null) continue;
      while (span.firstChild !== null) parent.insertBefore(span.firstChild, span);
      parent.removeChild(span);
    }
    root.normalize();
  }

  /** Dissolve after the last fade finishes, so the final batch's animation
   *  isn't cut short. The timer set is torn down with the stream. */
  function scheduleUnwrap(root: HTMLElement): void {
    const timer = setTimeout(() => {
      unwrapTimers.delete(timer);
      if (root.isConnected) unwrapWords(root);
    }, UNWRAP_DELAY_MS);
    unwrapTimers.add(timer);
  }

  function clearUnwrapTimers(): void {
    for (const timer of unwrapTimers) clearTimeout(timer);
    unwrapTimers.clear();
  }

  /** Hide `spans[shown..]` and every block whose FIRST word is past the
   *  reveal cursor (registering a probe so the ticker can unhide it). */
  function hideFrom(root: HTMLElement, spans: HTMLElement[], shown: number): void {
    for (let i = shown; i < spans.length; i++) spans[i].classList.add("rw-hidden");
    // Walk each word's ancestors up to (not including) root; the first word
    // to reach an ancestor stamps its index. Stop at an already-stamped
    // ancestor — its parents were stamped by whatever word reached it first.
    const firstOf = new Map<HTMLElement, number>();
    spans.forEach((span, i) => {
      let a: HTMLElement | null = span.parentElement;
      while (a !== null && a !== root && !firstOf.has(a)) {
        firstOf.set(a, i);
        a = a.parentElement;
      }
    });
    for (const [box, first] of firstOf) {
      if (first >= shown) {
        box.classList.add("rw-hidden");
        hiddenContainers.push({ el: box, probe: spans[first] });
      }
    }
  }

  /** Unhide containers whose probe word has revealed; drop dead entries. */
  function syncHiddenContainers(): void {
    if (hiddenContainers.length === 0) return;
    hiddenContainers = hiddenContainers.filter((h) => {
      if (!h.el.isConnected || !h.probe.isConnected) return false;
      if (h.probe.classList.contains("rw-hidden")) return true;
      h.el.classList.remove("rw-hidden");
      return false;
    });
  }

  function freshTail(): void {
    if (liveEl === null) return;
    tailEl = document.createElement("div");
    tailEl.className = "md-seg md-tail";
    liveEl.appendChild(tailEl);
    lastTailSource = null;
  }

  /** Drop every imperative node + reveal bookkeeping (prefix-cache
   *  invalidation: the text rewrote earlier content, so the cached DOM lies). */
  function clearLiveDom(): void {
    if (liveEl === null) return;
    liveEl.replaceChildren();
    sweepEmbeds();
    prefixQueue = [];
    tailQueue = [];
    ledger.reset();
    hiddenPerRoot.clear();
    hiddenContainers = [];
    unstamped = [];
    clearUnwrapTimers();
    freshTail();
  }

  /** Full teardown when the stream ends or the component unmounts. The DOM
   *  itself is Svelte's to remove (the `{#if streaming}` branch). */
  function resetStreamState(): void {
    clearReveal();
    clearUnwrapTimers();
    cancelIdleStamp?.();
    cancelIdleStamp = null;
    segState = null;
    tailEl = null;
    lastTailSource = null;
    prefixQueue = [];
    tailQueue = [];
    ledger.reset();
    hiddenPerRoot.clear();
    hiddenContainers = [];
    unstamped = [];
  }

  /** Parse + sanitize a streamed fragment into `target`, falling back to
   *  PLAIN TEXT on a parser throw — a segment must never be silently dropped
   *  (it would already be marked consumed), and textContent is inert, so the
   *  fallback cannot be more permissive than the settled render. */
  function renderFragment(target: HTMLElement, source: string): void {
    try {
      target.innerHTML = parseSanitized(source); // sanitized above
    } catch {
      target.textContent = source;
    }
  }

  /** A segment closed: parse + sanitize + decorate + wrap it ONCE, splice it
   *  in before the tail, and never touch its DOM again. */
  function appendClosedSegment(source: string): void {
    if (liveEl === null || tailEl === null) return;
    const root = document.createElement("div");
    root.className = "md-seg";
    renderFragment(root, source);
    decorateCopyTargets(root);
    classifyLocalAnchors(root);
    if (!reducedMotion) {
      // This segment was the HEAD of the previous open tail — carry the
      // reveal cursor over so already-shown words don't re-hide or re-fade.
      const { nodes, count } = collectWordNodes(root);
      const shown = ledger.closeSegment(count);
      if (shown < count) {
        const spans = wrapFromNodes(nodes);
        hideFrom(root, spans, shown);
        hiddenPerRoot.set(root, count - shown);
        for (let i = shown; i < spans.length; i++) prefixQueue.push({ span: spans[i], root });
      }
      // Fully revealed: no spans at all — born clean for selection-copy.
    }
    liveEl.insertBefore(root, tailEl);
    // After the word wrap (a card's own text must never become reveal
    // spans) and once attached (a card finds its scroller for lazy loads).
    upgradeEmbeds(root);
    unstamped.push(root);
  }

  /** Re-render ONLY the open segment — the per-chunk cost. */
  function renderTail(source: string): void {
    if (tailEl === null || source === lastTailSource) return;
    lastTailSource = source;
    if (source.trim().length === 0) {
      tailEl.replaceChildren();
    } else {
      renderFragment(tailEl, source);
    }
    decorateCopyTargets(tailEl);
    classifyLocalAnchors(tailEl);
    tailQueue = [];
    // The swap disconnected the old tail's tracked containers — drop them.
    hiddenContainers = hiddenContainers.filter((h) => h.el.isConnected);
    if (!reducedMotion) {
      const { nodes, count } = collectWordNodes(tailEl);
      const shown = ledger.rebuildTail(count);
      if (shown < count) {
        const spans = wrapFromNodes(nodes);
        hideFrom(tailEl, spans, shown);
        tailQueue = spans.slice(shown);
      }
    }
  }

  /** Queue a root for the deferred path sweep (idempotent per pending pass). */
  function enqueueStamp(root: HTMLElement): void {
    if (!unstamped.includes(root)) unstamped.push(root);
    scheduleIdleStamp();
  }

  /** Stamp path affordances on settled segments at IDLE — the TreeWalker +
   *  per-word regex sweep never runs on the per-chunk hot path. The open tail
   *  is stamped when its segment closes (or by the canonical settle pass).
   *  Note the setTimeout fallback: WKWebView (the native app) has no
   *  requestIdleCallback, so stamping there lands on a short fixed delay. */
  function scheduleIdleStamp(): void {
    if (cancelIdleStamp !== null || unstamped.length === 0) return;
    const run = () => {
      cancelIdleStamp = null;
      const batch = unstamped.splice(0);
      const attached = batch.filter((root) => root.isConnected);
      for (const root of attached) stampPaths(root);
      // After every stamp, so the batch's layout reads flush once.
      for (const root of attached) markTableRegions(root);
    };
    if (typeof requestIdleCallback === "function") {
      const id = requestIdleCallback(run, { timeout: 500 });
      cancelIdleStamp = () => cancelIdleCallback(id);
    } else {
      const id = setTimeout(run, 150);
      cancelIdleStamp = () => clearTimeout(id);
    }
  }

  function renderStream(t: string): void {
    if (liveEl === null) return;
    if (tailEl === null || tailEl.parentNode !== liveEl) {
      // (Re)entered live mode with a fresh Svelte-owned container.
      resetStreamState();
      freshTail();
    }
    const adv = advanceSegments(segState, t);
    if (!adv.extended && segState !== null) {
      // The text rewrote earlier content (a retraction/reroute): the cached
      // prefix is dead. `adv` already carries the fresh full split.
      clearLiveDom();
    }
    segState = adv.state;
    if (adv.newlyClosed.length > 0) {
      // The closes consumed the reveal cursor; the tail MUST re-render even
      // when its new source is string-equal to the old one (a duplicate
      // paragraph) — see the RevealLedger order contract.
      lastTailSource = null;
      for (const source of adv.newlyClosed) appendClosedSegment(source);
    }
    renderTail(adv.open);
    scheduleIdleStamp();
    if (!reducedMotion && ledger.pending > 0 && revealTimer === null) {
      revealTimer = setTimeout(step, REVEAL_TICK_MS);
    }
  }

  function step() {
    revealTimer = null;
    const { fromPrefix, fromTail } = ledger.take();
    if (fromPrefix + fromTail === 0) return; // caught up — the next chunk resumes us
    for (let k = 0; k < fromPrefix; k++) {
      const entry = prefixQueue.shift();
      if (entry === undefined) break;
      entry.span.classList.remove("rw-hidden");
      entry.span.classList.add("stream-fade");
      const left = (hiddenPerRoot.get(entry.root) ?? 1) - 1;
      if (left <= 0) {
        hiddenPerRoot.delete(entry.root);
        scheduleUnwrap(entry.root); // drained: dissolve spans post-fade
        markTableRegions(entry.root); // every word shown: measure at final width
      } else {
        hiddenPerRoot.set(entry.root, left);
      }
    }
    for (let k = 0; k < fromTail; k++) {
      const span = tailQueue.shift();
      if (span === undefined) break;
      span.classList.remove("rw-hidden");
      span.classList.add("stream-fade");
    }
    syncHiddenContainers();
    onReveal?.();
    if (ledger.pending > 0) revealTimer = setTimeout(step, REVEAL_TICK_MS);
  }

  // Streaming: drive the incremental pipeline off every coalesced chunk. Runs
  // post-DOM / pre-paint, so hiding the not-yet-revealed tail never flashes.
  // A HIDDEN live block does nothing at all — the segment DOM freezes in
  // place (queues, ledger, and segState intact) and the effect re-runs on
  // show, where renderStream catches up on the accumulated delta and the
  // ticker resumes from the preserved cursor.
  $effect(() => {
    const t = text; // dep: every coalesced chunk
    if (!streaming) return; // dep: live only
    if (!visible) {
      // dep: freeze — stop the ticker; keep every queue and the DOM.
      clearReveal();
      return;
    }
    if (liveEl === null) return; // dep: container mounted
    renderStream(t);
  });

  // Settled (and blocks that never streamed): the canonical parse landed in
  // the DOM via `{@html html}` — decorate it fully, once per content change.
  let lastSettledHtml: string | null = null;
  $effect(() => {
    if (streaming) {
      lastSettledHtml = null; // the next settle re-decorates the fresh subtree
      return;
    }
    resetStreamState();
    const current = html; // dep: the canonical parse (lazily computed here)
    if (el === null) return;
    if (current === lastSettledHtml) return;
    lastSettledHtml = current;
    decorateCopyTargets(el);
    stampPaths(el);
    markTableRegions(el);
    upgradeEmbeds(el);
  });

  // A turn ended (the resolver dropped its misses): a settled block that
  // left a reference unlinked asks again — now if shown, else on show.
  $effect(() => {
    const resolver = resolvePaths;
    if (resolver === undefined) return;
    return resolver.onExpire(() => {
      if (missedAt === null || streaming || el === null) return;
      missedAt = null;
      if (visible) enqueueStamp(el);
      else restampOnShow = true;
    });
  });
  $effect(() => {
    if (!visible || streaming || !restampOnShow || el === null) return;
    restampOnShow = false;
    enqueueStamp(el);
  });

  /** Keyboard reach for the transcript's horizontal scrollers
   *  (shared/scrollRegion.ts): a wide table's .md-table host becomes a
   *  focusable group, a wide fence's code box a plain tab stop, each only
   *  while it overflows. Marks land where the node is attached and at its
   *  final width — the idle stamp pass over closed segments, the moment a
   *  segment's reveal has shown its last word, and the settled render —
   *  never the per-chunk open tail (its scrollWidth read would force a
   *  layout per chunk). Overflow moves with the column's width (a pane
   *  resize) or a table's own (the chat font size), so a settled message
   *  that holds a scroller watches both; a hidden pane watches nothing and
   *  catches up when shown. */
  /** A fence's code box scrolls; a bare raw-HTML <pre> (no code child, no
   *  scroller of its own) scrolls itself — a <pre> that holds one never
   *  overflows, so listing both marks exactly the box that moves. */
  const CHAT_SCROLLERS = [
    [".md-table", { role: "group", label: "scrollable table" }],
    ["pre > code, pre", null],
  ] as const;
  function markTableRegions(root: ParentNode): void {
    markScrollRegions(root, CHAT_SCROLLERS);
  }
  $effect(() => {
    if (!visible || streaming) return;
    void html; // dep: a settled render may have changed the scroller set
    // A fence's content width follows the chat font with no element to
    // observe (its text is the content), so the font settings are deps too.
    void getSetting("chat.fontSize");
    void getSetting("appearance.interfaceFontSize");
    const root = el;
    if (root === null || root.querySelector(".md-table, pre") === null) return;
    const recheck = () => markTableRegions(root);
    recheck();
    const stops = [
      watchWidth(root, recheck),
      ...Array.from(root.querySelectorAll(".md-table > table"), (t) => watchWidth(t, recheck)),
    ];
    return () => stops.forEach((stop) => stop());
  });

  // Stop the ticker, the copied-feedback timer, unwrap timers, and any
  // pending idle stamp when the component unmounts (a keyed block can be
  // torn down mid-stream).
  $effect(() => () => {
    clearReveal();
    clearCopied();
    clearUnwrapTimers();
    cancelIdleStamp?.();
    sweepEmbeds(true);
  });
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="md"
  bind:this={el}
  onclick={onClick}
  onkeydown={onKeydown}
  oncontextmenu={onContextMenu}
  onpointerenter={onPointerEnter}
>
  {#if streaming}
    <!-- Streaming: children are managed imperatively (renderStream) — closed
         segments append once, only the open tail rebuilds per chunk. Every
         fragment passes through DOMPurify before touching innerHTML. -->
    <div class="md-live" bind:this={liveEl}></div>
  {:else}
    <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized above -->
    {@html html}
  {/if}
</div>

<style>
  .md {
    line-height: var(--chat-line-height, 1.55);
    font-size: var(--text-md);
    word-break: break-word;
    /* Table spacing over the shared recipe (app.css "Markdown tables"): the
       transcript's tighter rhythm, and an absolute cell size — --text-sm
       keeps a legibility floor at the smallest pane font. */
    --md-table-margin: 0.4em;
    --md-table-font-size: var(--text-sm);
    --md-table-cell-padding: 3px 8px;
  }
  /* Streaming containers are layout-neutral: display:contents removes the
     live shell and each segment wrapper from the box tree, so block margins,
     margin collapsing, and every .md descendant rule behave exactly as in the
     settled (wrapper-free) canonical render. */
  .md-live,
  .md :global(.md-seg) {
    display: contents;
  }
  /* Streaming reveal: words are wrapped in .rw spans; the not-yet-revealed tail
     is display:none (occupies no space, exactly like the old text slice), and
     each freshly revealed batch fades in. */
  .md :global(.rw-hidden) {
    display: none;
  }
  .md :global(.stream-fade) {
    animation: stream-fade-in 0.32s ease-out both;
  }
  @keyframes stream-fade-in {
    from {
      opacity: 0;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .md :global(.stream-fade) {
      animation: none;
    }
  }
  .md :global(p) {
    margin: 0.35em 0;
  }
  /* Equation typography (.katex) is global — app.css; MathML output has no
     .katex-display wrapper, display math is `<math display="block">`. */
  /* Heading ladder: real hierarchy (agents lean on markdown constantly), with
     a hairline under the top two ranks — quiet document structure, not chrome. */
  .md :global(h1),
  .md :global(h2),
  .md :global(h3),
  .md :global(h4) {
    margin: 0.85em 0 0.35em;
    font-weight: 600;
    line-height: 1.3;
  }
  .md :global(h1) {
    font-size: 1.3em;
  }
  .md :global(h2) {
    font-size: 1.18em;
  }
  .md :global(h3) {
    font-size: 1.08em;
  }
  .md :global(h4) {
    font-size: 1em;
  }
  .md :global(h1),
  .md :global(h2) {
    padding-bottom: 0.15em;
    border-bottom: 1px solid color-mix(in srgb, var(--edge) 70%, transparent);
  }
  .md :global(ul),
  .md :global(ol) {
    margin: 0.3em 0;
    padding-left: 1.4em;
  }
  .md :global(li) {
    margin: 0.15em 0;
  }
  .md :global(li)::marker {
    color: color-mix(in srgb, var(--accent) 70%, var(--muted));
  }
  .md :global(code) {
    font-family: var(--mono, monospace);
    font-size: 0.92em;
    background: color-mix(in srgb, var(--fg) 7%, transparent);
    border-radius: 3px;
    padding: 0.05em 0.3em;
  }
  .md :global(.md-path) {
    cursor: pointer;
    text-decoration: underline dotted;
    text-decoration-color: color-mix(in srgb, var(--fg) 35%, transparent);
    text-underline-offset: 2px;
    transition:
      color 0.12s ease,
      background-color 0.12s ease;
  }
  .md :global(.md-path:hover) {
    color: var(--accent);
    text-decoration-color: var(--accent);
  }
  .md :global(code.md-path:hover) {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
  }
  /* Several files answer to this name: the dashed underline says a click
     asks which. */
  .md :global(.md-path.md-ambiguous) {
    text-decoration-style: dashed;
  }
  .md :global(pre) {
    position: relative; /* the copy button's anchor */
    background: color-mix(in srgb, var(--fg) 5%, transparent);
    border: 1px solid var(--edge);
    border-radius: 6px;
    padding: 8px 10px;
    overflow-x: auto;
    margin: 0.4em 0;
  }
  /* The CODE child is the horizontal scroller (not the pre), so the pinned
     copy button never rides away with scrolled content; a bare raw-HTML pre
     (no code child) still scrolls itself and only loses the pinning. */
  .md :global(pre code) {
    display: block;
    overflow-x: auto;
    scrollbar-width: thin;
    background: none;
    padding: 0;
    font-size: var(--text-sm);
  }
  /* Fenced-block copy chrome: hover-reveal (the .rewind-btn language). The
     scrim keeps the icon legible over code beneath it — token-only, so both
     themes hold. */
  .md :global(.md-copy) {
    position: absolute;
    top: 5px;
    right: 5px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 4px;
    background: color-mix(in srgb, var(--term-bg) 82%, transparent);
    border: 1px solid var(--edge);
    border-radius: 5px;
    color: var(--muted);
    cursor: pointer;
    opacity: 0;
    transition:
      opacity 0.12s ease,
      color 0.12s ease;
  }
  .md :global(pre:hover .md-copy),
  .md :global(blockquote:hover > .md-copy),
  .md :global(.md-copy:focus-visible),
  .md :global(.md-copy.copied) {
    opacity: 1;
  }
  .md :global(.md-copy:hover),
  .md :global(.md-copy.copied) {
    color: var(--accent);
  }
  .md :global(.md-copy .ic-check),
  .md :global(.md-copy.copied .ic-copy) {
    display: none;
  }
  .md :global(.md-copy.copied .ic-check) {
    display: block;
  }
  /* Quoted material (agents echo source prose back for review) reads as a
     quiet card — prose-set and wrapped, deliberately unlike code chrome, but
     carrying the same pinned copy affordance. The wash fades accent→neutral so
     the quote sits a half-step off the page in both themes without shouting. */
  .md :global(blockquote) {
    position: relative; /* the copy button's anchor */
    margin: 0.5em 0;
    padding: 8px 14px;
    border-left: 2.5px solid color-mix(in srgb, var(--accent) 60%, transparent);
    border-radius: 0 7px 7px 0;
    background: linear-gradient(
      to right,
      color-mix(in srgb, var(--accent) 5%, transparent),
      color-mix(in srgb, var(--fg) 3%, transparent) 55%
    );
    color: color-mix(in srgb, var(--fg) 45%, var(--muted));
  }
  /* Trim the card's inner rhythm: first/last CONTENT blocks sit flush (the
     appended copy button is the structural last child, hence the `of` form). */
  .md :global(blockquote > :first-child) {
    margin-top: 0;
  }
  .md :global(blockquote > :nth-last-child(1 of :not(.md-copy))) {
    margin-bottom: 0;
  }
  .md :global(a) {
    color: var(--accent);
  }
  /* Chat's one table delta over the shared recipe (app.css "Markdown
     tables"), which styles the .md-table host tables.ts emits and every
     cell — and resets the root's break-anywhere wrapping on hosted cells
     (why: there). Headers stay on one line so column names read as labels.
     An agent's literal <table> HTML has NO host (marked passes it through),
     so it keeps the root's squeeze-to-fit wrapping — never wider than the
     transcript. */
  .md :global(.md-table th) {
    white-space: nowrap;
  }
  .md :global(hr) {
    border: none;
    border-top: 1px solid var(--edge);
    margin: 0.6em 0;
  }
  /* Local images are embed cards (upgradeEmbeds). Until a streaming
     segment closes, its placeholder holds a quiet box of about a card's
     header height; once upgraded it is hidden beside its card. */
  .md :global(img[data-md-embed]) {
    display: block;
    width: min(100%, 420px);
    height: 64px;
    margin: 0.4em 0;
    border: 1px dashed color-mix(in srgb, var(--edge) 80%, transparent);
    border-radius: 8px;
    color: var(--muted);
    font-size: var(--text-xs);
  }
  .md :global(img.md-embed-src) {
    display: none;
  }
  .md :global(.md-embed) {
    display: block;
    max-width: 100%;
  }
  .md :global(.md-embed-text) {
    color: var(--muted);
    font-size: var(--text-sm);
  }
</style>
