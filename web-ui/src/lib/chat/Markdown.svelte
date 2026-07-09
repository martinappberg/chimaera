<script module lang="ts">
  import DOMPurify from "dompurify";

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
  import { marked } from "marked";
  import { pathCandidate, trimPathWord, type PathHit, type ResolvePaths } from "./paths";

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

  function markPath(node: Element, label: string, hit: PathHit) {
    node.classList.add("md-path");
    node.setAttribute("role", "button");
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
      const cand = decodeURI(href).replace(/^\.\//, "").replace(/\/+$/, "");
      if (!pathCandidate(cand)) continue;
      const hit = want(cand);
      if (hit !== null) markPath(a, cand, hit);
    }
    // Bare words in prose ("saved to results/plot.png") — same validation,
    // same affordance. Collect first: wrapping mutates the walked tree.
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
      acceptNode: (n) =>
        n.parentElement?.closest("pre, code, a, .md-path") == null
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
    if (local !== null && local !== undefined) e.preventDefault();
  }

  // Agent prose is untrusted model output rendered into the workbench DOM:
  // sanitize EVERYTHING marked emits, always. The style tag is on DOMPurify's
  // default allowlist, so forbid it explicitly (and the style attribute) —
  // otherwise injected CSS applies document-wide.
  const html = $derived(
    DOMPurify.sanitize(marked.parse(text, { async: false, breaks: true }) as string, {
      FORBID_TAGS: ["style"],
      FORBID_ATTR: ["style"],
    }),
  );

  let el = $state<HTMLElement | null>(null);

  // --- streaming reveal -------------------------------------------------------
  // Wire chunks arrive coalesced (2 KiB / 100 ms); rendering them raw makes text
  // land in ugly slabs. But re-slicing + re-parsing the whole message on a fast
  // reveal ticker is O(n²). So we parse/sanitize ONCE per chunk (the `html`
  // derived changes only when the full text does), wrap the rendered words in
  // spans, and unhide them a batch at a time on a ~75 ms cadence — the same fade
  // cascade, driven off the already-rendered DOM instead of a re-parse.
  const REVEAL_TICK_MS = 75;
  const reducedMotion =
    typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
  let words: HTMLElement[] = [];
  /** Every element that CONTAINS words, with the index of its first word — so a
   *  block whose words are all still hidden (a heading, an empty list bullet)
   *  is hidden WHOLE, never flashing its margins/marker above the reveal. */
  let containers: { el: HTMLElement; first: number }[] = [];
  let revealed = 0;
  let lastHtml = "";
  let revealTimer: ReturnType<typeof setTimeout> | null = null;

  function clearReveal() {
    if (revealTimer !== null) {
      clearTimeout(revealTimer);
      revealTimer = null;
    }
  }

  /** Wrap every whitespace-delimited run in the tree in a `.rw` span, in
   *  document order, and record the containing elements. Inline spans preserve
   *  flow and whitespace, so a wrapped word is visually inert until hidden. */
  function wrapWords(root: HTMLElement): void {
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
      acceptNode: (n) =>
        (n.textContent ?? "").trim().length > 0
          ? NodeFilter.FILTER_ACCEPT
          : NodeFilter.FILTER_REJECT,
    });
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
    words = spans;
    // Walk each word's ancestors up to (not including) root; the first word to
    // reach an ancestor stamps its index. Stop at an already-stamped ancestor —
    // its parents were stamped by whatever word reached it first.
    const firstOf = new Map<HTMLElement, number>();
    words.forEach((span, i) => {
      let a: HTMLElement | null = span.parentElement;
      while (a !== null && a !== root && !firstOf.has(a)) {
        firstOf.set(a, i);
        a = a.parentElement;
      }
    });
    containers = [...firstOf].map(([el2, first]) => ({ el: el2, first }));
  }

  /** Hide whole blocks that haven't started revealing (their first word is past
   *  the cursor) so their chrome never shows above the reveal point. */
  function syncContainers() {
    for (const c of containers) c.el.classList.toggle("rw-hidden", c.first >= revealed);
  }

  function step() {
    revealTimer = null;
    const total = words.length;
    if (revealed >= total) return; // caught up — the next chunk resumes us
    const remaining = total - revealed;
    // Advance a few words, more when the buffer runs ahead — the stream never
    // lags visibly, it just breathes.
    const take = Math.min(remaining, Math.max(2, Math.ceil(remaining / 6)));
    for (let i = revealed; i < revealed + take && i < total; i++) {
      words[i].classList.remove("rw-hidden");
      words[i].classList.add("stream-fade");
    }
    revealed += take;
    syncContainers();
    onReveal?.();
    if (revealed < total) revealTimer = setTimeout(step, REVEAL_TICK_MS);
  }

  // One effect drives both concerns off `html` (a re-parse — per chunk) and
  // `streaming`. It runs post-DOM / pre-paint, so hiding the not-yet-revealed
  // tail here never flashes the full text.
  $effect(() => {
    const current = html; // dep: re-parse only when the FULL text changes
    const live = streaming && !reducedMotion; // dep
    if (el === null) return;
    if (current !== lastHtml) {
      lastHtml = current;
      // The {@html} flush rebuilt the subtree: (re)wrap words for a live block
      // and re-stamp path affordances — both once per chunk, not per tick.
      if (live) wrapWords(el);
      else {
        words = [];
        containers = [];
      }
      stampPaths(el);
    }
    if (!live) {
      // Settled (or reduced-motion): make sure nothing stays hidden from an
      // earlier streaming pass, then idle.
      clearReveal();
      if (words.length > 0 || containers.length > 0) {
        for (const w of words) w.classList.remove("rw-hidden");
        for (const c of containers) c.el.classList.remove("rw-hidden");
      } else {
        el.querySelectorAll(".rw-hidden").forEach((n) => n.classList.remove("rw-hidden"));
      }
      words = [];
      containers = [];
      revealed = 0;
      return;
    }
    // Show the settled prefix immediately (no fade), hide the rest until the
    // ticker reaches it.
    for (let i = 0; i < words.length; i++) {
      words[i].classList.toggle("rw-hidden", i >= revealed);
    }
    syncContainers();
    if (revealed < words.length && revealTimer === null) {
      revealTimer = setTimeout(step, REVEAL_TICK_MS);
    }
  });

  // Stop the ticker when the component unmounts (a keyed message block can be
  // torn down mid-stream).
  $effect(() => () => clearReveal());
</script>

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="md" bind:this={el} onclick={onClick}>
  <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized above -->
  {@html html}
</div>

<style>
  .md {
    line-height: 1.55;
    font-size: var(--text-md);
    word-break: break-word;
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
  .md :global(h1),
  .md :global(h2),
  .md :global(h3),
  .md :global(h4) {
    margin: 0.7em 0 0.3em;
    font-size: 1.05em;
    font-weight: 600;
  }
  .md :global(ul),
  .md :global(ol) {
    margin: 0.3em 0;
    padding-left: 1.4em;
  }
  .md :global(li) {
    margin: 0.15em 0;
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
    background: color-mix(in srgb, var(--fg) 5%, transparent);
    border: 1px solid var(--edge);
    border-radius: 6px;
    padding: 8px 10px;
    overflow-x: auto;
    margin: 0.4em 0;
  }
  .md :global(pre code) {
    background: none;
    padding: 0;
    font-size: var(--text-sm);
  }
  .md :global(blockquote) {
    margin: 0.4em 0;
    padding-left: 10px;
    border-left: 2px solid var(--edge);
    color: var(--muted);
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
  .md :global(hr) {
    border: none;
    border-top: 1px solid var(--edge);
    margin: 0.6em 0;
  }
</style>
