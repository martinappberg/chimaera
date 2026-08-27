/**
 * Copy chrome for rendered markdown: one hover button pinned inside every
 * fenced block and blockquote, shared by the chat transcript and the markdown
 * file preview so both surfaces carry the identical affordance.
 *
 * APPEND-only by contract: both hosts render via `{@html}`, and Svelte tears
 * that content down by walking the live sibling chain between its tracked
 * first/last nodes — wrapping a top-level element would strand the walk and
 * leak DOM on re-render. The button is appended INSIDE the pre/blockquote.
 *
 * SVG-only content: the chat's streaming reveal wraps text nodes in spans, so
 * an icon-only button can never have a label word-wrapped or reveal-hidden.
 */

/** Exported for the markdown live preview's fence copy widget (a CodeMirror
 *  widget, not an `{@html}` host), so both surfaces share one icon. */
export const COPY_BUTTON_SVG =
  '<svg class="ic-copy" viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">' +
  '<rect x="6" y="6" width="7.5" height="7.5" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.5"/>' +
  '<path d="M4 10h-.5A1.5 1.5 0 0 1 2 8.5v-5A1.5 1.5 0 0 1 3.5 2h5A1.5 1.5 0 0 1 10 3.5V4" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>' +
  "</svg>" +
  '<svg class="ic-check" viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">' +
  '<path d="M3.5 8.5l3 3 6-6.5" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>' +
  "</svg>";

/** Blockquote buttons stay an unlabeled plain "copy" (the icon carries it);
 *  only code blocks name their payload. */
export function copyLabel(host: Element): string {
  return host.tagName === "BLOCKQUOTE" ? "copy" : "copy code";
}

/** Inject the copy button into every pre/blockquote under `root` that lacks
 *  one. Idempotent per host; safe to re-run after every `{@html}` flush. */
export function decorateCopyTargets(root: HTMLElement): void {
  for (const host of root.querySelectorAll("pre, blockquote")) {
    if (host.querySelector(":scope > button.md-copy") !== null) continue;
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "md-copy";
    btn.setAttribute("aria-label", copyLabel(host));
    btn.title = "copy";
    btn.innerHTML = COPY_BUTTON_SVG;
    host.appendChild(btn);
  }
}

/**
 * The text a copy button copies, resolved from the live DOM at click time
 * (never from an attribute — sanitized agent HTML may forge the button class,
 * but a forged button can only ever copy its own visible block, which is the
 * feature). Fences render as pre>code (the button is code's sibling); a bare
 * raw-HTML pre falls back to the whole pre; a blockquote copies its rendered
 * prose whole (the SVG-only button adds no text either way). innerText, not
 * textContent, so `hidden` markup can't smuggle invisible text into the
 * payload; the renderer ends every fence with a newline the author never
 * typed, hence the edge trims.
 */
export function copyPayload(btn: Element): string {
  const host = btn.closest("pre, blockquote");
  const src = host?.tagName === "PRE" ? (host.querySelector("code") ?? host) : host;
  return src instanceof HTMLElement
    ? src.innerText.replace(/^\n+/, "").replace(/\s+$/, "")
    : "";
}
