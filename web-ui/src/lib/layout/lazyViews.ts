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
  settings: () => import("../settings/SettingsView.svelte"),
};

const pending = new Map<PaneViewKind, Promise<Component<any>>>();

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
