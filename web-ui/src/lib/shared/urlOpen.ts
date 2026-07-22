/**
 * One policy for every link chimaera renders — terminal output, chat prose,
 * markdown previews, the browser pane's own chrome.
 *
 * Two destinations, and the URL decides the default:
 *
 *  - **Proxyable** (loopback, or any host with an explicit port) is a live web
 *    app on the daemon's host, so it opens in a browser PANE through the
 *    reverse proxy. Opening it in the system browser would be wrong on a
 *    remote workspace: `localhost` there is the laptop, not the host that owns
 *    the work.
 *  - **Everything else** (github.com, docs) goes to the user's REAL browser.
 *    In the native app that must route through the shell: the window's
 *    navigation guard admits only the daemon origin and nothing receives a
 *    `target="_blank"` new-window request, so an external link was silently
 *    swallowed (found live). `window.open` is the plain-browser fallback.
 *
 * Either way the user can override per click via [`urlMenuEntries`] — the
 * right-click menu every link surface shares.
 */

import { openExternal } from "../net/native";
import type { ContextMenuEntry } from "./contextMenu.svelte";
import { writeClipboard } from "../net/native";

/** A proxyable target: what the browser pane needs to open it. */
export interface UrlTarget {
  host: string;
  port: number;
  /** path + query, "/" at minimum. */
  path: string;
}

function isLoopbackName(host: string): boolean {
  const h = host.toLowerCase();
  return h === "localhost" || h === "::1" || h === "[::1]" || /^127(\.\d{1,3}){3}$/.test(h);
}

/**
 * The proxy target a raw URL names, when it is one the daemon can serve.
 * Non-loopback hosts qualify only with an explicit port — that is what keeps
 * ordinary web links (no port) out of the pane and in the real browser.
 */
export function proxyableUrl(raw: string): UrlTarget | null {
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return null;
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return null;
  if (url.username !== "" || url.password !== "") return null;
  const loopback = isLoopbackName(url.hostname);
  if (!loopback && url.port === "") return null;
  const port =
    url.port !== "" ? Number.parseInt(url.port, 10) : url.protocol === "https:" ? 443 : 80;
  if (!(port > 0 && port <= 65535)) return null;
  return {
    host: url.hostname.replace(/^\[|\]$/g, ""),
    port,
    path: `${url.pathname}${url.search}` || "/",
  };
}

/** Only ever hand a web URL onward — an `href` is attacker-influenced (agents
 *  author chat prose and markdown), and `javascript:`/`file:`/app schemes must
 *  never reach an opener. The shell re-checks; this is the client-side wall. */
export function isWebUrl(raw: string): boolean {
  try {
    const u = new URL(raw);
    return u.protocol === "http:" || u.protocol === "https:";
  } catch {
    return false;
  }
}

// --- the pane opener (registered by App, which owns the layout) ---------------

type PaneOpener = (target: UrlTarget, newSplit: boolean) => void;
let paneOpener: PaneOpener | null = null;

/** App registers how a proxyable URL becomes a browser pane. Same
 *  module-handler pattern as the reference/upload inserters. */
export function setUrlPaneOpener(fn: PaneOpener): void {
  paneOpener = fn;
}

/** Open in the user's real browser: native shell first (the only route that
 *  works in the app), plain-browser `window.open` as the fallback. */
export function openInSystemBrowser(url: string): void {
  if (!isWebUrl(url)) return;
  void openExternal(url).then((handled) => {
    if (!handled) window.open(url, "_blank", "noopener,noreferrer");
  });
}

/** Open in a chimaera browser pane, when the URL is proxyable. Returns false
 *  when it isn't (or no opener is registered — e.g. the home screen). */
export function openInPane(url: string, newSplit = false): boolean {
  const target = proxyableUrl(url);
  if (target === null || paneOpener === null) return false;
  paneOpener(target, newSplit);
  return true;
}

/**
 * The default activation for a rendered link: a live local app opens in a
 * pane, anything else in the real browser. `newSplit` (Cmd/Ctrl) forces the
 * pane into a fresh split, matching what Cmd/Ctrl+click means on file links.
 */
export function activateUrl(url: string, newSplit = false): void {
  if (!isWebUrl(url)) return;
  if (openInPane(url, newSplit)) return;
  openInSystemBrowser(url);
}

/** The right-click menu every link surface shares. "Open in Chimaera" appears
 *  only when the URL is actually proxyable, so the menu never offers a pane
 *  that would just show "can't reach". */
export function urlMenuEntries(url: string): ContextMenuEntry[] {
  if (!isWebUrl(url)) return [];
  const entries: ContextMenuEntry[] = [];
  if (proxyableUrl(url) !== null && paneOpener !== null) {
    entries.push(
      { label: "Open in Chimaera", onSelect: () => openInPane(url) },
      { label: "Open Beside", onSelect: () => openInPane(url, true) },
    );
  }
  entries.push({ label: "Open in Browser", onSelect: () => openInSystemBrowser(url) });
  entries.push("separator");
  entries.push({ label: "Copy Link", onSelect: () => void copyUrl(url) });
  return entries;
}

async function copyUrl(url: string): Promise<void> {
  if (await writeClipboard(url)) return;
  try {
    await navigator.clipboard.writeText(url);
  } catch {
    // clipboard unavailable — quiet, same as the other copy affordances
  }
}

// --- dev-only self-checks -------------------------------------------------------
if (import.meta.env.DEV) {
  const ok = (cond: boolean, msg: string) => console.assert(cond, `urlOpen.ts self-check: ${msg}`);
  const t = (s: string) => {
    const p = proxyableUrl(s);
    return p === null ? "" : `${p.host}:${p.port}${p.path}`;
  };

  ok(t("http://localhost:8888/lab?token=a") === "localhost:8888/lab?token=a", "loopback + token");
  ok(t("http://127.0.0.1:8501") === "127.0.0.1:8501/", "bare loopback origin");
  ok(t("http://sh03-09n14:8888/tree") === "sh03-09n14:8888/tree", "host with explicit port");
  ok(t("https://github.com/foo/bar") === "", "ordinary web URLs are not proxyable");
  ok(t("http://user:pw@localhost:1/") === "", "userinfo URLs never qualify");
  ok(isWebUrl("https://x.dev") && !isWebUrl("javascript:alert(1)"), "only web schemes pass");
  ok(!isWebUrl("file:///etc/passwd"), "file: never passes");
}
