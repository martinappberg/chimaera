import { marked } from "marked";
import DOMPurify from "dompurify";

/**
 * Client-side Markdown → sanitized HTML for the LIVE split edit|preview pane.
 *
 * The authoritative render stays server-side (comrak GFM, ammonia) and drives
 * the plain Preview mode; this is only the as-you-type mirror, so a small
 * divergence from comrak on exotic constructs is acceptable — the saved file
 * re-renders through the server the moment you leave the split. Sanitize
 * regardless (a Markdown file can carry raw HTML): forbid `<style>`/`style` so a
 * document can never restyle the surrounding workbench, matching the policy in
 * `chat/Markdown.svelte`. GFM is on by default in marked; `breaks` stays off to
 * track standard Markdown (and comrak) rather than chat's soft-break behavior.
 */
export function renderMarkdown(text: string): string {
  const html = marked.parse(text, { async: false }) as string;
  return DOMPurify.sanitize(html, { FORBID_TAGS: ["style"], FORBID_ATTR: ["style"] });
}
