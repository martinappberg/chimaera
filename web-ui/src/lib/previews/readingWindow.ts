/**
 * Large reading documents retain their nodes in memory, but only blocks near
 * the viewport stay in the live DOM. Inherited pane flags otherwise restyle
 * hundreds of thousands of inline nodes on every tab switch in WebKit.
 * Exact, same-tag placeholders preserve block geometry and scroll position.
 * Only noninteractive prose paragraphs are windowed: keyboard targets,
 * lists/tables, and asynchronous image/math content stay connected.
 */
export function readingWindow(root: HTMLElement, _metrics?: unknown) {
  interface Block {
    node: HTMLElement;
    placeholder: HTMLElement;
    shown: boolean;
  }
  const scroll = root.closest<HTMLElement>(".md-scroll");
  const layer = root.closest<HTMLElement>(".layer");
  let blocks: Block[] = [];
  let targets = new WeakMap<Element, Block>();
  let frame = 0;
  let width = 0;
  let selecting = false;
  let finding = false;
  let printing = false;

  function show(block: Block): void {
    if (block.shown || !root.contains(block.placeholder)) return;
    observer.unobserve(block.placeholder);
    block.placeholder.replaceWith(block.node);
    block.shown = true;
    observer.observe(block.node);
  }

  const observer = new IntersectionObserver((entries) => {
    if (selecting || finding || printing) return;
    const hide: Array<{ block: Block; height: number }> = [];
    // Read all departing geometry before changing any nodes.
    for (const entry of entries) {
      const block = targets.get(entry.target);
      if (!block || entry.target !== (block.shown ? block.node : block.placeholder)) continue;
      if (!entry.isIntersecting && block.shown) {
        if (block.node.contains(document.activeElement)) continue;
        hide.push({ block, height: entry.boundingClientRect.height });
      }
    }
    for (const { block, height } of hide) {
      block.placeholder.style.height = `${height}px`;
      observer.unobserve(block.node);
      block.node.replaceWith(block.placeholder);
      block.shown = false;
      observer.observe(block.placeholder);
    }
    for (const entry of entries) {
      const block = targets.get(entry.target);
      if (block && entry.isIntersecting) show(block);
    }
  }, { root: scroll, rootMargin: "1000px 0px" });

  function restore(): void {
    for (const block of blocks) show(block);
  }

  function schedule(): void {
    cancelAnimationFrame(frame);
    observer.disconnect();
    restore();
    observer.disconnect();
    blocks = [];
    targets = new WeakMap();
    frame = requestAnimationFrame(() => {
      frame = 0;
      // Small documents need no window. An individual giant paragraph stays
      // readable as ordinary HTML: window boundaries are complete blocks.
      if ((root.textContent?.length ?? 0) < 100_000) return;
      if (root.getBoundingClientRect().width === 0) return;
      for (const node of Array.from(root.children)) {
        if (!(node instanceof HTMLElement)) continue;
        // Keep offscreen links and fence copy/scroll controls in the native
        // tab order. Their focus must be able to scroll them into view.
        if (node.tagName !== "P" || node.matches("[tabindex], [contenteditable]") ||
            node.querySelector("a, button, input, select, textarea, [tabindex], [contenteditable], img, [data-math-style]")) continue;
        // Svelte's raw-HTML effect owns its first/last nodes as removal
        // boundaries. Keep both attached so a refresh can replace the range.
        if (node === root.firstChild || node === root.lastChild) continue;
        const placeholder = node.cloneNode(false) as HTMLElement;
        placeholder.removeAttribute("id");
        placeholder.setAttribute("aria-hidden", "true");
        placeholder.inert = true;
        placeholder.style.boxSizing = "border-box";
        placeholder.style.minHeight = "0";
        placeholder.style.overflow = "hidden";
        const block = { node, placeholder, shown: true };
        blocks.push(block);
        targets.set(node, block);
        targets.set(placeholder, block);
        observer.observe(node);
      }
    });
  }

  // Selecting and native Find need the complete text. Materialize before
  // Cmd/Ctrl+A/F/G's default action and keep it present for a drag selection.
  function onKey(event: KeyboardEvent): void {
    if (layer?.inert || scroll?.classList.contains("hidden")) return;
    if (!(event.metaKey || event.ctrlKey) || !["a", "f", "g"].includes(event.key.toLowerCase())) return;
    if (event.key.toLowerCase() === "a" && (!(event.target instanceof Node) || !scroll?.contains(event.target))) return;
    finding = event.key.toLowerCase() !== "a";
    selecting = !finding;
    restore();
  }
  function onSelection(): void {
    const selection = document.getSelection();
    const next = !layer?.inert && selection !== null && !selection.isCollapsed &&
      ((selection.anchorNode !== null && root.contains(selection.anchorNode)) ||
       (selection.focusNode !== null && root.contains(selection.focusNode)) || selection.containsNode(root, true));
    if (next === selecting) return;
    selecting = next;
    if (next) restore();
    else schedule();
  }
  function resume(): void {
    if (!selecting) {
      observer.disconnect();
      for (const block of blocks) observer.observe(block.shown ? block.node : block.placeholder);
    }
  }
  const activation = new MutationObserver(() => {
    if (layer?.inert) {
      finding = false;
      selecting = false;
      resume();
    }
  });
  if (layer) activation.observe(layer, { attributes: true, attributeFilter: ["inert"] });
  const resize = new ResizeObserver(([entry]) => {
    if (entry.contentRect.width === width) return;
    width = entry.contentRect.width;
    // Reading/source mode hides the scroller with display:none. Keep its
    // measured placeholders until it has real geometry again.
    if (width === 0) return;
    schedule();
  });
  resize.observe(root);
  function beforePrint(): void {
    printing = true;
    restore();
  }
  function afterPrint(): void {
    printing = false;
    schedule();
  }
  window.addEventListener("beforeprint", beforePrint);
  window.addEventListener("afterprint", afterPrint);
  document.addEventListener("keydown", onKey, true);
  document.addEventListener("selectionchange", onSelection);
  schedule();
  return {
    update: schedule,
    destroy() {
      cancelAnimationFrame(frame);
      resize.disconnect();
      activation.disconnect();
      observer.disconnect();
      window.removeEventListener("beforeprint", beforePrint);
      window.removeEventListener("afterprint", afterPrint);
      document.removeEventListener("keydown", onKey, true);
      document.removeEventListener("selectionchange", onSelection);
    },
  };
}
