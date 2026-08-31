<script module lang="ts">
  import DOMPurify from "dompurify";
  import { marked } from "marked";
  import { markdownMath } from "./math";

  marked.use(markdownMath);

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
  });
</script>

<script lang="ts">
  import { copyText } from "../shared/clipboard";
  import { copyLabel, copyPayload, decorateCopyTargets } from "../shared/copyDecor";
  import { advanceSegments, type SegmenterState } from "./streamSegments";
  import { pathCandidate, trimPathWord, type PathHit, type ResolvePaths } from "./paths";
  import { activateUrl, isWebUrl, urlMenuEntries } from "../shared/urlOpen";
  import { contextMenu } from "../shared/contextMenu.svelte";

  interface Props {
    text: string;
    /** Live streaming block: reveal newly parsed words in fading batches
     *  instead of showing the whole (chunky) text at once. Settled blocks pass
     *  false and render statically. */
    streaming?: boolean;
    /** Open a VALIDATED path the prose references — files land in a viewer
     *  pane, directories in the Finder. */
    onOpenPath?: (path: string, kind: "file" | "dir") => void;
    /** Batch-validate path candidates against the daemon (the terminal
     *  link provider's mechanism): only real files/dirs get the click
     *  affordance. Returns canonical absolute path + kind per HIT. */
    resolvePaths?: ResolvePaths;
    /** Fired after each streaming reveal batch — lets the host keep the
     *  transcript pinned to the bottom as words grow between wire chunks. */
    onReveal?: () => void;
  }

  let { text, streaming = false, onOpenPath, resolvePaths, onReveal }: Props = $props();

  /** candidate text → validated hit or "miss"; lives for the component so
   *  streaming re-renders re-stamp from cache instead of refetching. */
  const resolved = new Map<string, PathHit | "miss">();
  const inflight = new Set<string>();

  // Copy-button chrome comes from the shared decorator (also used by the
  // markdown file preview): injected post-sanitize from literals only, never
  // from agent-derived strings; APPEND-only inside the host (see copyDecor.ts
  // for the {@html}-teardown constraint); for pre the CODE child is the
  // horizontal scroller so the host stays a non-scrolling anchor the button
  // can pin to. Runs BEFORE wrapWords so the reveal bookkeeping hides the
  // button with its still-unrevealed block.

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

  function markPath(node: Element, label: string, hit: PathHit) {
    node.classList.add("md-path");
    node.setAttribute("role", "button");
    // Generated prose/code spans are not naturally focusable. Anchors already
    // are, so only add a tab stop to the synthetic controls.
    if (node.tagName !== "A") node.setAttribute("tabindex", "0");
    node.setAttribute("data-path", hit.path);
    node.setAttribute("data-kind", hit.kind);
    node.setAttribute(
      "title",
      hit.kind === "dir" ? `browse ${label} in the finder` : `open ${label} in a pane`,
    );
  }

  /** Stamp the click affordance onto inline code spans AND bare prose words
   *  that validate as real paths. Unknown candidates batch to the daemon
   *  once; the resolve callback re-stamps from cache. */
  function stampPaths(root: HTMLElement) {
    if (onOpenPath === undefined || resolvePaths === undefined) return;
    const unknownSet = new Set<string>();
    const want = (candidate: string): PathHit | null => {
      const hit = resolved.get(candidate);
      if (hit !== undefined && hit !== "miss") return hit;
      if (hit === undefined && !inflight.has(candidate)) unknownSet.add(candidate);
      return null;
    };
    for (const code of root.querySelectorAll("code")) {
      if (code.closest("pre") !== null || code.classList.contains("md-path")) continue;
      const t = code.textContent ?? "";
      if (!pathCandidate(t)) continue;
      const hit = want(t);
      if (hit !== null) markPath(code, t, hit);
    }
    // Markdown links to a LOCAL path ("[demo.csv](demo-assets/demo.csv)") —
    // agents write these constantly. The href is the candidate; a schemeless
    // (non-http) target that validates routes to a pane instead of trying to
    // navigate the SPA. Local anchors that DON'T validate are neutralized on
    // click (below) so they never blow away the workbench either.
    for (const a of root.querySelectorAll("a")) {
      if (a.classList.contains("md-path")) continue;
      const href = a.getAttribute("href") ?? "";
      if (href === "" || /^[a-z][a-z0-9+.-]*:/i.test(href) || href.startsWith("#")) continue;
      a.classList.add("md-local");
      // Agent-authored hrefs are untrusted. A malformed percent escape makes
      // decodeURI throw; without this guard one bad link aborts the whole
      // post-render effect on every streaming chunk (paths/copy/reveal all
      // stop updating). It is still neutralized as a local link below.
      let decoded: string;
      try {
        decoded = decodeURI(href);
      } catch {
        continue;
      }
      const cand = decoded.replace(/^\.\//, "").replace(/\/+$/, "");
      if (!pathCandidate(cand)) continue;
      const hit = want(cand);
      if (hit !== null) markPath(a, cand, hit);
    }
    // Bare words in prose ("saved to results/plot.png") — same validation,
    // same affordance. Collect first: wrapping mutates the walked tree.
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
      acceptNode: (n) =>
        n.parentElement?.closest("pre, code, a, .md-path, .katex") == null
          ? NodeFilter.FILTER_ACCEPT
          : NodeFilter.FILTER_REJECT,
    });
    const nodes: Text[] = [];
    while (walker.nextNode()) nodes.push(walker.currentNode as Text);
    for (const node of nodes) {
      const words = [...(node.textContent ?? "").matchAll(/\S+/g)];
      // Right-to-left so earlier match indices stay valid across splits.
      for (let i = words.length - 1; i >= 0; i--) {
        const { head } = trimPathWord(words[i][0]);
        if (!pathCandidate(head)) continue;
        const hit = want(head);
        if (hit === null) continue;
        const start = words[i].index;
        const tail = node.splitText(start);
        tail.splitText(head.length);
        const span = document.createElement("span");
        markPath(span, head, hit);
        tail.parentNode?.replaceChild(span, tail);
        span.appendChild(tail);
      }
    }
    if (unknownSet.size > 0) {
      const unknown = [...unknownSet];
      for (const u of unknown) inflight.add(u);
      void resolvePaths(unknown)
        .then((hits) => {
          for (const u of unknown) {
            resolved.set(u, hits.get(u) ?? "miss");
            inflight.delete(u);
          }
          if (el !== null) stampPaths(el);
        })
        .catch(() => {
          for (const u of unknown) inflight.delete(u);
        });
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
    if (node !== null && node !== undefined && onOpenPath !== undefined) {
      // An anchor would navigate the SPA away; a validated path opens a pane.
      if (node.tagName === "A") e.preventDefault();
      const path = node.getAttribute("data-path");
      const kind = node.getAttribute("data-kind");
      if (path !== null && (kind === "file" || kind === "dir")) onOpenPath(path, kind);
      return;
    }
    // A local-path anchor that never validated: still swallow the click so a
    // stale relative href can't replace the whole workbench with a 404.
    const local = target?.closest?.("a.md-local");
    if (local !== null && local !== undefined) {
      e.preventDefault();
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
    if (node === null || node === undefined || onOpenPath === undefined) return;
    const path = node.getAttribute("data-path");
    const kind = node.getAttribute("data-kind");
    if (path === null || (kind !== "file" && kind !== "dir")) return;
    e.preventDefault();
    onOpenPath(path, kind);
  }

  // Agent prose is untrusted model output rendered into the workbench DOM:
  // sanitize EVERYTHING marked emits, always — the full canonical parse AND
  // every per-segment streaming fragment go through here before touching the
  // DOM. The style tag is on DOMPurify's default allowlist, so forbid it
  // explicitly (and the style attribute) — otherwise injected CSS applies
  // document-wide.
  function sanitizeHtml(raw: string): string {
    return DOMPurify.sanitize(raw, { FORBID_TAGS: ["style"], FORBID_ATTR: ["style"] });
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
  // (streamSegments.ts): a closed segment parses, sanitizes, decorates, and
  // word-wraps exactly once and its DOM is never touched again; only the
  // trailing open segment re-renders per chunk, so per-chunk work tracks the
  // tail's size, not the message's. Word-reveal spans exist only for words not
  // yet revealed (the settled prefix stays span-free plain text), and path
  // stamping defers to idle. When `streaming` flips false the template swaps
  // to the canonical `{@html html}` full parse: one whole-message re-parse
  // that also guarantees a span-free settled DOM (word-per-span text copies
  // with a hard newline at every visual wrap point — the canonical swap is
  // what keeps selection-copy clean).
  const REVEAL_TICK_MS = 75;
  const reducedMotion =
    typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;

  let segState: SegmenterState | null = null;
  /** Wrapper of the open segment — always liveEl's last child. */
  let tailEl: HTMLElement | null = null;
  let lastTailSource: string | null = null;
  /** Hidden word spans in CLOSED segments (document order). Their DOM
   *  persists across chunks, so entries stay valid until revealed. */
  let prefixQueue: HTMLElement[] = [];
  /** Hidden word spans in the open tail — rebuilt with it every chunk. */
  let tailQueue: HTMLElement[] = [];
  /** Words of the current tail already revealed, carried across tail rebuilds
   *  (and into a closing segment) so text the reader has seen never re-hides
   *  or double-fades. */
  let tailRevealed = 0;
  /** Blocks whose chrome hides until their first word (the probe) reveals —
   *  a heading or list bullet must not flash its margins/marker above the
   *  reveal point. */
  let hiddenContainers: { el: HTMLElement; probe: HTMLElement }[] = [];
  let revealTimer: ReturnType<typeof setTimeout> | null = null;
  /** Closed-segment roots awaiting deferred (idle) path stamping. */
  let unstamped: HTMLElement[] = [];
  let cancelIdleStamp: (() => void) | null = null;

  function clearReveal() {
    if (revealTimer !== null) {
      clearTimeout(revealTimer);
      revealTimer = null;
    }
  }

  const wordFilter = {
    acceptNode: (n: Node) =>
      (n.textContent ?? "").trim().length > 0 && n.parentElement?.closest(".katex") == null
        ? NodeFilter.FILTER_ACCEPT
        : NodeFilter.FILTER_REJECT,
  };

  /** Count whitespace-delimited runs without touching the tree — the cheap
   *  path for a closing segment whose words are all already revealed. */
  function countWords(root: HTMLElement): number {
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, wordFilter);
    let count = 0;
    while (walker.nextNode()) {
      count += [...(walker.currentNode.textContent ?? "").matchAll(/\S+/g)].length;
    }
    return count;
  }

  /** Wrap every whitespace-delimited run under `root` in a `.rw` span, in
   *  document order. Inline spans preserve flow and whitespace, so a wrapped
   *  word is visually inert until hidden. */
  function wrapWordsIn(root: HTMLElement): HTMLElement[] {
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, wordFilter);
    const nodes: Text[] = [];
    while (walker.nextNode()) nodes.push(walker.currentNode as Text);
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
    prefixQueue = [];
    tailQueue = [];
    tailRevealed = 0;
    hiddenContainers = [];
    unstamped = [];
    freshTail();
  }

  /** Full teardown when the stream ends or the component unmounts. The DOM
   *  itself is Svelte's to remove (the `{#if streaming}` branch). */
  function resetStreamState(): void {
    clearReveal();
    cancelIdleStamp?.();
    cancelIdleStamp = null;
    segState = null;
    tailEl = null;
    lastTailSource = null;
    prefixQueue = [];
    tailQueue = [];
    tailRevealed = 0;
    hiddenContainers = [];
    unstamped = [];
  }

  /** A segment closed: parse + sanitize + decorate + wrap it ONCE, splice it
   *  in before the tail, and never touch its DOM again. */
  function appendClosedSegment(source: string): void {
    if (liveEl === null || tailEl === null) return;
    const root = document.createElement("div");
    root.className = "md-seg";
    root.innerHTML = parseSanitized(source); // sanitized above
    decorateCopyTargets(root);
    if (!reducedMotion) {
      // This segment was the HEAD of the previous open tail — carry the
      // reveal cursor over so already-shown words don't re-hide or re-fade.
      const total = countWords(root);
      if (tailRevealed >= total) {
        tailRevealed -= total; // fully revealed: no spans at all
      } else {
        const spans = wrapWordsIn(root);
        hideFrom(root, spans, tailRevealed);
        for (let i = tailRevealed; i < spans.length; i++) prefixQueue.push(spans[i]);
        tailRevealed = 0;
      }
    }
    liveEl.insertBefore(root, tailEl);
    unstamped.push(root);
  }

  /** Re-render ONLY the open segment — the per-chunk cost. */
  function renderTail(source: string): void {
    if (tailEl === null || source === lastTailSource) return;
    lastTailSource = source;
    tailEl.innerHTML = source.trim().length === 0 ? "" : parseSanitized(source);
    decorateCopyTargets(tailEl);
    tailQueue = [];
    // The swap disconnected the old tail's tracked containers — drop them.
    hiddenContainers = hiddenContainers.filter((h) => h.el.isConnected);
    if (!reducedMotion) {
      const spans = wrapWordsIn(tailEl);
      const shown = Math.min(tailRevealed, spans.length);
      tailRevealed = shown;
      hideFrom(tailEl, spans, shown);
      for (let i = shown; i < spans.length; i++) tailQueue.push(spans[i]);
    }
  }

  /** Stamp path affordances on settled segments at IDLE — the TreeWalker +
   *  per-word regex sweep never runs on the per-chunk hot path. The open tail
   *  is stamped when its segment closes (or by the canonical settle pass). */
  function scheduleIdleStamp(): void {
    if (cancelIdleStamp !== null || unstamped.length === 0) return;
    const run = () => {
      cancelIdleStamp = null;
      const batch = unstamped.splice(0);
      for (const root of batch) {
        if (root.isConnected) stampPaths(root);
      }
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
    for (const source of adv.newlyClosed) appendClosedSegment(source);
    renderTail(adv.open);
    scheduleIdleStamp();
    if (!reducedMotion && prefixQueue.length + tailQueue.length > 0 && revealTimer === null) {
      revealTimer = setTimeout(step, REVEAL_TICK_MS);
    }
  }

  function step() {
    revealTimer = null;
    const pending = prefixQueue.length + tailQueue.length;
    if (pending === 0) return; // caught up — the next chunk resumes us
    // Advance a few words, more when the buffer runs ahead — the stream never
    // lags visibly, it just breathes.
    const take = Math.min(pending, Math.max(2, Math.ceil(pending / 6)));
    for (let k = 0; k < take; k++) {
      let span: HTMLElement;
      if (prefixQueue.length > 0) {
        span = prefixQueue.shift()!;
      } else {
        span = tailQueue.shift()!;
        tailRevealed += 1;
      }
      span.classList.remove("rw-hidden");
      span.classList.add("stream-fade");
    }
    syncHiddenContainers();
    onReveal?.();
    if (prefixQueue.length + tailQueue.length > 0) revealTimer = setTimeout(step, REVEAL_TICK_MS);
  }

  // Streaming: drive the incremental pipeline off every coalesced chunk. Runs
  // post-DOM / pre-paint, so hiding the not-yet-revealed tail never flashes.
  $effect(() => {
    const t = text; // dep: every coalesced chunk
    if (!streaming) return; // dep: live only
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
  });

  // Stop the ticker, the copied-feedback timer, and any pending idle stamp
  // when the component unmounts (a keyed block can be torn down mid-stream).
  $effect(() => () => {
    clearReveal();
    clearCopied();
    cancelIdleStamp?.();
  });
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="md"
  bind:this={el}
  onclick={onClick}
  onkeydown={onKeydown}
  oncontextmenu={onContextMenu}
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
  /* Math is rendered as native MathML inside a KaTeX wrapper. Display math
     scrolls within the reading column instead of widening the workbench. */
  .md :global(.katex) {
    color: inherit;
    font-size: 1.02em;
  }
  .md :global(.katex-display) {
    display: block;
    max-width: 100%;
    overflow-x: auto;
    overflow-y: hidden;
    margin: 0.55em 0;
    padding: 0.1em 0;
  }
  .md :global(.katex-display > .katex) {
    display: block;
    width: max-content;
    min-width: 100%;
  }
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
  .md :global(table) {
    border-collapse: collapse;
    margin: 0.4em 0;
    font-size: var(--text-sm);
  }
  .md :global(th),
  .md :global(td) {
    border: 1px solid var(--edge);
    padding: 3px 8px;
    text-align: left;
  }
  .md :global(th) {
    font-weight: 600;
    background: color-mix(in srgb, var(--fg) 4%, transparent);
  }
  .md :global(hr) {
    border: none;
    border-top: 1px solid var(--edge);
    margin: 0.6em 0;
  }
</style>
