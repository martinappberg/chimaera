/**
 * Proxyable URLs in terminals — the browser pane's front door.
 *
 * Web URLs are deliberately NOT linkified in chimaera terminals (that
 * decision stands). This provider underlines exactly the URLs the daemon's
 * reverse proxy can serve — a local web app's address, the thing Jupyter,
 * marimo, and Streamlit print at startup:
 *
 *   - loopback hosts (localhost, 127.x, ::1), any port
 *   - any other host WITH an explicit port (a login/compute node's hostname)
 *
 * `https://github.com/...` never underlines. Detection is a pure regex over
 * rendered lines — no daemon validation round-trips (nothing to validate:
 * clicking a dead URL lands on the pane's honest "can't reach" state).
 * Click opens the browser pane, preserving path + query (Jupyter's ?token=
 * auth rides along); Cmd/Ctrl+click opens the pane in a fresh split.
 */

import type { ILink, ILinkProvider, Terminal } from "@xterm/xterm";
import { groupText } from "./links";
import { proxyableUrl, type UrlTarget } from "../shared/urlOpen";

export type { UrlTarget };

export interface UrlLinkHost {
  open(sessionId: string, target: UrlTarget, newSplit: boolean): void;
  /** Right-click on a detected URL: the shared Chimaera/Browser/Copy menu. */
  menu(event: MouseEvent, url: string): void;
}

const URL_RE = /https?:\/\/[^\s"'`<>\\^]+/g;
/** Trailing sentence punctuation never belongs to a printed URL. */
const TRAIL_RE = /[),.;:!?'"\]]+$/;

/** One URL candidate in a scanned line. */
export interface UrlCandidate {
  raw: string;
  start: number;
  length: number;
  target: UrlTarget;
}

/** Scan `text` for proxyable URLs (indexes into that string). */
export function extractUrls(text: string): UrlCandidate[] {
  const out: UrlCandidate[] = [];
  URL_RE.lastIndex = 0;
  for (let m = URL_RE.exec(text); m !== null; m = URL_RE.exec(text)) {
    const raw = m[0].replace(TRAIL_RE, "");
    if (raw.length < 10) continue;
    const target = proxyableUrl(raw);
    if (target === null) continue;
    out.push({ raw, start: m.index, length: raw.length, target });
  }
  return out;
}

class UrlLinkProvider implements ILinkProvider {
  constructor(
    private readonly term: Terminal,
    private readonly sessionId: string,
    private readonly host: UrlLinkHost,
  ) {}

  provideLinks(bufferLineNumber: number, callback: (links: ILink[] | undefined) => void): void {
    const grp = groupText(this.term, bufferLineNumber - 1);
    if (grp === null) {
      callback(undefined);
      return;
    }
    const links: ILink[] = [];
    for (const c of extractUrls(grp.g.text)) {
      const endIdx = c.start + c.length - 1;
      if (endIdx >= grp.g.cellOf.length) continue;
      links.push({
        text: grp.g.text.slice(c.start, c.start + c.length),
        range: {
          start: { x: grp.g.cellOf[c.start] + 1, y: grp.g.rowOf[c.start] + 1 },
          end: { x: grp.g.cellOf[endIdx] + 1, y: grp.g.rowOf[endIdx] + 1 },
        },
        activate: (event: MouseEvent) => {
          this.host.open(this.sessionId, c.target, event.metaKey || event.ctrlKey);
        },
        // xterm exposes no contextmenu hook on a link, but right-clicking one
        // means the pointer is already over it — so remember what's hovered
        // and let the element-level listener below use it.
        hover: () => {
          hovered = c.raw;
        },
        leave: () => {
          if (hovered === c.raw) hovered = null;
        },
      });
    }
    callback(links.length > 0 ? links : undefined);
  }
}

/** The URL under the pointer, tracked via the provider's hover/leave. */
let hovered: string | null = null;

/** Wire proxyable-URL links into a pooled terminal. Returns dispose. */
export function registerUrlLinks(term: Terminal, sessionId: string, host: UrlLinkHost): () => void {
  const provider = term.registerLinkProvider(new UrlLinkProvider(term, sessionId, host));
  const onContextMenu = (e: MouseEvent) => {
    const url = hovered;
    if (url === null) return; // not on a link: leave the terminal's own menu
    host.menu(e, url);
  };
  term.element?.addEventListener("contextmenu", onContextMenu);
  return () => {
    term.element?.removeEventListener("contextmenu", onContextMenu);
    provider.dispose();
  };
}

// --- dev-only self-checks -------------------------------------------------------
if (import.meta.env.DEV) {
  const ok = (cond: boolean, msg: string) => console.assert(cond, `urlLinks.ts self-check: ${msg}`);
  const targets = (s: string) => extractUrls(s).map((c) => `${c.target.host}:${c.target.port}${c.target.path}`);

  ok(
    targets("    http://localhost:8888/lab?token=abc123").join() ===
      "localhost:8888/lab?token=abc123",
    "jupyter's startup URL links with its token",
  );
  ok(
    targets("Local URL: http://127.0.0.1:8501").join() === "127.0.0.1:8501/",
    "streamlit's bare-origin URL links",
  );
  ok(
    targets("running on http://sh03-09n14:8888/tree.").join() === "sh03-09n14:8888/tree",
    "compute-node hostnames link when a port is explicit (trailing dot trimmed)",
  );
  ok(targets("see https://github.com/foo/bar").length === 0, "ordinary web URLs stay plain");
  ok(targets("docs at https://example.com.").length === 0, "no port, not loopback → no link");
  ok(targets("ftp://localhost:21/x http://user:pw@localhost:1/").length === 0, "non-http and userinfo URLs never link");
  ok(
    targets("(see http://localhost:4173/report.html)").join() === "localhost:4173/report.html",
    "wrapping punctuation is trimmed",
  );
}
