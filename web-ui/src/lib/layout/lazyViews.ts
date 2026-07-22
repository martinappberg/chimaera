import type { Component } from "svelte";
import { preloadTerminalRuntime } from "../terminal/termPool";

export type PaneViewKind =
  | "terminal"
  | "chat"
  | "file"
  | "finder"
  | "diff"
  | "git"
  | "changes"
  | "dashboard"
  | "browser"
  | "settings";

type PaneViewModule = { default: Component<any> };

// Keep imports explicit so Vite produces one feature chunk per workbench
// surface. The promise cache is shared by every pane: simultaneous split panes
// request a chunk once, while each Pane keeps its own mounted component state.
const loaders: Record<PaneViewKind, () => Promise<PaneViewModule>> = {
  terminal: async () => {
    const [view] = await Promise.all([
      import("../terminal/Terminal.svelte"),
      preloadTerminalRuntime(),
    ]);
    return view;
  },
  chat: () => import("../chat/ChatView.svelte"),
  file: () => import("../previews/FileView.svelte"),
  finder: () => import("../previews/FinderView.svelte"),
  diff: () => import("../previews/DiffView.svelte"),
  git: () => import("../workspace/GitView.svelte"),
  changes: () => import("../workspace/SessionChangesView.svelte"),
  dashboard: () => import("../dashboard/DashboardView.svelte"),
  browser: () => import("../browser/BrowserView.svelte"),
  settings: () => import("../settings/SettingsView.svelte"),
};

const pending = new Map<PaneViewKind, Promise<Component<any>>>();

const viewChunkPrefixes: Record<PaneViewKind, string> = {
  terminal: "Terminal-",
  chat: "ChatView-",
  file: "FileView-",
  finder: "FinderView-",
  diff: "DiffView-",
  git: "GitView-",
  changes: "SessionChangesView-",
  dashboard: "DashboardView-",
  settings: "SettingsView-",
};

let retrySequence = 0;

/** Extract only same-origin, immutable build assets from a loader error. Error
 *  messages are browser-specific, so ignore everything except explicit URLs
 *  under Vite's production asset namespace. */
export function failedAssetUrls(
  error: unknown,
  extension: "css" | "js",
  base = typeof location === "undefined" ? "http://localhost/" : location.href,
): URL[] {
  const raw = error instanceof Error ? `${error.message}\n${error.stack ?? ""}` : String(error);
  const matches = raw.match(/(?:https?:\/\/|\/)[^\s)'"<>]+\.(?:css|m?js)(?:\?[^\s)'"<>]*)?/g) ?? [];
  const origin = new URL(base).origin;
  const seen = new Set<string>();
  const urls: URL[] = [];
  for (const match of matches) {
    let url: URL;
    try {
      url = new URL(match, base);
    } catch {
      continue;
    }
    const wanted = extension === "css" ? url.pathname.endsWith(".css") : /\.m?js$/.test(url.pathname);
    if (url.origin !== origin || !url.pathname.startsWith("/assets/") || !wanted) continue;
    url.hash = "";
    if (seen.has(url.href)) continue;
    seen.add(url.href);
    urls.push(url);
  }
  return urls;
}

function freshUrl(url: URL): string {
  const fresh = new URL(url);
  fresh.searchParams.set("chimaera-retry", String(++retrySequence));
  return fresh.href;
}

/** Vite caches every attempted preload URL, including a stylesheet whose
 *  request failed while a tunnel was down. Load those styles under a fresh
 *  query before calling the normal view loader again. */
async function recoverFailedStyles(error: unknown): Promise<void> {
  if (typeof document === "undefined") return;
  await Promise.all(
    failedAssetUrls(error, "css").map(
      (url) =>
        new Promise<void>((resolve, reject) => {
          const link = document.createElement("link");
          link.rel = "stylesheet";
          link.crossOrigin = "";
          link.href = freshUrl(url);
          const nonce = document.querySelector<HTMLMetaElement>('meta[property="csp-nonce"]')
            ?.nonce;
          if (nonce) link.setAttribute("nonce", nonce);
          link.addEventListener("load", () => resolve(), { once: true });
          link.addEventListener(
            "error",
            () => reject(new Error(`Unable to retry stylesheet ${url.pathname}`)),
            { once: true },
          );
          document.head.appendChild(link);
        }),
    ),
  );
}

export function loadPaneView(kind: PaneViewKind): Promise<Component<any>> {
  let request = pending.get(kind);
  if (request === undefined) {
    request = loaders[kind]().then((module) => module.default);
    pending.set(kind, request);
    // A chunk can fail while a tunnel reconnects or when a daemon handoff
    // replaces the immutable asset set. Cache successes, not failures, so a
    // later request never inherits an already-rejected shared promise; Pane
    // still asks for a full reload because browsers can memoize failed module
    // imports by URL.
    void request.catch(() => {
      if (pending.get(kind) === request) pending.delete(kind);
    });
  }
  return request;
}

/** Retry a failed view without reloading the workbench. The usual case is a
 *  failed CSS preload: recover it, then let Vite run the original import. If
 *  the component module itself was memoized as failed by the browser, import
 *  that exact same-origin hashed chunk under a fresh query instead. */
export async function retryPaneView(kind: PaneViewKind, error: unknown): Promise<Component<any>> {
  await recoverFailedStyles(error);
  try {
    return await loadPaneView(kind);
  } catch (retryError) {
    const prefix = viewChunkPrefixes[kind];
    const componentUrl = failedAssetUrls(retryError, "js").find((url) =>
      url.pathname.split("/").at(-1)?.startsWith(prefix),
    );
    if (componentUrl === undefined) throw retryError;
    const module = (await import(/* @vite-ignore */ freshUrl(componentUrl))) as PaneViewModule;
    const component = module.default;
    pending.set(kind, Promise.resolve(component));
    return component;
  }
}
