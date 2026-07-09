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
    /** How many trailing WORDS were just revealed (streaming): they get a
     *  fade-in span. 0 = render statically. Re-renders wipe earlier spans,
     *  which is correct — settled words must not re-animate. */
    fadeWords?: number;
    /** Open a VALIDATED path the prose references — files land in a viewer
     *  pane, directories in the Finder. */
    onOpenPath?: (path: string, kind: "file" | "dir") => void;
    /** Batch-validate path candidates against the daemon (the terminal
     *  link provider's mechanism): only real files/dirs get the click
     *  affordance. Returns canonical absolute path + kind per HIT. */
    resolvePaths?: ResolvePaths;
  }

  let { text, fadeWords = 0, onOpenPath, resolvePaths }: Props = $props();

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

  /** Wrap the last `count` words of the rendered tree in fade spans. Walks
   *  text nodes from the END so only the tail is touched (cheap on long
   *  messages); word fragments never cross element boundaries. */
  function fadeTail(root: HTMLElement, count: number) {
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
      acceptNode: (n) =>
        (n.textContent ?? "").trim().length > 0
          ? NodeFilter.FILTER_ACCEPT
          : NodeFilter.FILTER_REJECT,
    });
    const nodes: Text[] = [];
    while (walker.nextNode()) nodes.push(walker.currentNode as Text);
    let remaining = count;
    for (let i = nodes.length - 1; i >= 0 && remaining > 0; i--) {
      const node = nodes[i];
      const words = [...(node.textContent ?? "").matchAll(/\S+/g)];
      if (words.length === 0) continue;
      const take = Math.min(remaining, words.length);
      const splitAt = words[words.length - take].index ?? 0;
      const tail = node.splitText(splitAt);
      const span = document.createElement("span");
      span.className = "stream-fade";
      tail.parentNode?.replaceChild(span, tail);
      span.appendChild(tail);
      remaining -= take;
    }
  }

  // Runs after each {@html} flush; only the freshly revealed batch animates
  // and file-looking code spans get their click affordance re-stamped.
  let fadedHtml = "";
  $effect(() => {
    const current = html;
    if (el === null) return;
    // fadeTail mutates the DOM in place and is NOT idempotent — only wrap when
    // this exact html was just (re)flushed, never on a bare fadeWords change
    // over an already-faded tree (which would nest stream-fade spans).
    if (fadeWords > 0 && current !== fadedHtml) {
      fadedHtml = current;
      fadeTail(el, fadeWords);
    }
    stampPaths(el);
  });
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
