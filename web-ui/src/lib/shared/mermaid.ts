/**
 * Mermaid diagrams, for every surface that draws them (markdown fences, `.mmd`
 * files). The library is large, so it loads on first use as its own chunk.
 *
 * Diagram source is untrusted (agents write it): mermaid runs with
 * `securityLevel: "strict"` (labels sanitized, no click handlers, no HTML
 * labels), and its SVG is sanitized once more before it reaches the DOM.
 * `render` is serialized: mermaid keeps global state while laying out, and two
 * interleaved renders corrupt each other.
 */

import DOMPurify from "dompurify";

/** Larger sources are refused: layout is synchronous and can pin the tab. */
export const MERMAID_MAX_SOURCE = 50_000;

type Mermaid = (typeof import("mermaid"))["default"];

let loading: Promise<Mermaid> | null = null;
let configuredFor: "light" | "dark" | null = null;
let queue: Promise<unknown> = Promise.resolve();
let seq = 0;

function load(): Promise<Mermaid> {
  loading ??= import("mermaid").then((m) => m.default);
  return loading;
}

export class MermaidError extends Error {}

/**
 * Render `source` to sanitized SVG markup for the given theme. Rejects with a
 * `MermaidError` carrying mermaid's parse message on bad input.
 */
export function renderMermaid(source: string, theme: "light" | "dark"): Promise<string> {
  const run = async (): Promise<string> => {
    if (source.length > MERMAID_MAX_SOURCE) {
      throw new MermaidError(`diagram too large (${source.length} characters)`);
    }
    const mermaid = await load();
    if (configuredFor !== theme) {
      mermaid.initialize({
        startOnLoad: false,
        securityLevel: "strict",
        theme: theme === "dark" ? "dark" : "default",
        fontFamily: "inherit",
        maxTextSize: MERMAID_MAX_SOURCE,
        // Keys an in-diagram `%%{init: …}%%` directive may NOT override: the
        // security level, and every route to author-supplied CSS.
        secure: [
          "secure",
          "securityLevel",
          "startOnLoad",
          "maxTextSize",
          "maxEdges",
          "themeCSS",
          "themeVariables",
          "fontFamily",
        ],
      });
      configuredFor = theme;
    }
    seq += 1;
    let svg: string;
    try {
      ({ svg } = await mermaid.render(`chimaera-mermaid-${seq}`, source));
    } catch (e) {
      // mermaid leaves its measuring node behind on a parse error.
      document.getElementById(`dchimaera-mermaid-${seq}`)?.remove();
      throw new MermaidError(e instanceof Error ? e.message : String(e));
    }
    return DOMPurify.sanitize(svg, {
      USE_PROFILES: { svg: true, svgFilters: true },
      // Diagram styling lives in the SVG's own <style>; mermaid scopes it to
      // the diagram id, so it cannot restyle the workbench.
      ADD_TAGS: ["style", "foreignObject"],
    });
  };
  const next = queue.then(run, run);
  queue = next.catch(() => undefined);
  return next;
}
