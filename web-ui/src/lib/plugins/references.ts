/**
 * Plugins' `references/1` as id-reference sources (shared/references.ts),
 * the registry's second kind of source after the Knowledge snapshot
 * (docs/plugin-platform-plan.md §4): each active plugin that publishes ids
 * here registers one source — its id shapes, and its ids resolved to
 * targets that preview and open their span, or open one of its views. Read
 * again when the workspace or its plugins change and on a `references/1`
 * surface frame; a plugin switched off takes its chips with it (the surface
 * route answers only for active plugins). Imported once by the app.
 */
import { registerReferenceSource, type RefShape, type RefSource, type RefSpan, type RefTarget } from "../shared/references";
import { openPath } from "../shared/openPath";
import { fetchSurface, onPlatformFrame, openPluginView, type SurfaceItem } from "./platform";
import { workspacePlugins } from "./store";

/** One id a plugin answers for. */
export interface PluginRef {
  id: string;
  key: string;
  kind: string;
  title: string;
  span?: RefSpan;
  view?: string;
}

/** Everything one plugin published here, merged across its keys. */
export interface PluginRefs {
  plugin: string;
  shapes: RefShape[];
  byId: Map<string, PluginRef[]>;
}

const text = (v: unknown): v is string => typeof v === "string" && v !== "";

function spanOf(v: unknown): RefSpan | undefined {
  const s = v as Partial<RefSpan> | null | undefined;
  if (s === null || typeof s !== "object" || !text(s.path) || typeof s.line !== "number" || s.line < 1) return undefined;
  return { path: s.path, line: s.line, end_line: typeof s.end_line === "number" ? s.end_line : s.line };
}

/** The surface's items as one entry per plugin. Rows the daemon would have
 *  refused are dropped, and so is an id with nowhere to go (no span, no
 *  view): a chip that opens nothing is worse than text. */
export function collectReferences(items: readonly SurfaceItem[]): Map<string, PluginRefs> {
  const out = new Map<string, PluginRefs>();
  for (const it of items) {
    const data = it.data as { shapes?: unknown; ids?: unknown } | null;
    if (data === null || typeof data !== "object") continue;
    let refs = out.get(it.plugin);
    if (refs === undefined) {
      refs = { plugin: it.plugin, shapes: [], byId: new Map() };
      out.set(it.plugin, refs);
    }
    for (const s of Array.isArray(data.shapes) ? data.shapes : []) {
      const shape = s as Partial<RefShape>;
      if (!text(shape.kind) || !text(shape.pattern)) continue;
      if (!refs.shapes.some((x) => x.kind === shape.kind && x.pattern === shape.pattern)) {
        refs.shapes.push({ kind: shape.kind, pattern: shape.pattern });
      }
    }
    for (const r of Array.isArray(data.ids) ? data.ids : []) {
      const o = r as Partial<PluginRef> & { span?: unknown };
      if (!text(o.id) || !text(o.key) || !text(o.kind) || !text(o.title)) continue;
      const span = spanOf(o.span);
      const view = text(o.view) ? o.view : undefined;
      if (span === undefined && view === undefined) continue;
      const ref: PluginRef = { id: o.id, key: o.key, kind: o.kind, title: o.title, ...(span ? { span } : {}), ...(view ? { view } : {}) };
      const list = refs.byId.get(ref.id);
      if (list === undefined) refs.byId.set(ref.id, [ref]);
      else if (!list.some((x) => x.key === ref.key)) list.push(ref);
    }
  }
  for (const [pid, refs] of out) if (refs.shapes.length === 0 || refs.byId.size === 0) out.delete(pid);
  return out;
}

/** A plugin's ids as a registry source. `name` is the plugin's own name
 *  (the preview's note says whose id it is). */
export function pluginSource(refs: PluginRefs, name: string, root: string | null): RefSource {
  const target = (r: PluginRef): RefTarget => ({
    key: `${refs.plugin}:${r.key}`,
    kind: r.kind,
    title: r.title,
    ...(r.span !== undefined ? { span: r.span, base: root } : {}),
    note: [r.id, r.kind, name].filter((x, i, all) => all.indexOf(x) === i).join(" · "),
    open: (from) => {
      if (r.span !== undefined) {
        const path = r.span.path.startsWith("/") ? r.span.path : root !== null ? `${root}/${r.span.path}` : null;
        if (path === null) return;
        const endLine = r.span.end_line > r.span.line ? r.span.end_line : undefined;
        openPath(path, "file", {
          split: from.newSplit,
          reveal: { line: r.span.line, ...(endLine !== undefined ? { endLine } : {}) },
          ...(from.paneId !== null ? { fromPane: from.paneId } : {}),
        });
      } else if (r.view !== undefined) {
        openPluginView(refs.plugin, r.view);
      }
    },
  });
  return {
    id: `plugin:${refs.plugin}`,
    shapes: refs.shapes,
    root,
    lookup: (id, kind) => {
      const all = refs.byId.get(id) ?? [];
      // The id's own kind when it has that one; any of its kinds otherwise.
      const same = all.filter((r) => r.kind === kind);
      return (same.length > 0 ? same : all).map(target);
    },
  };
}

// ---- wiring: the active workspace's publishers, kept registered ---------------

const registered = new Map<string, () => void>();
let ws: string | null = null;
let root: string | null = null;
let names = new Map<string, string>();
let activeSig = "";
let generation = 0;

function clearAll(): void {
  for (const un of registered.values()) un();
  registered.clear();
}

async function refresh(): Promise<void> {
  const mine = ++generation;
  const w = ws;
  if (w === null) {
    clearAll();
    return;
  }
  let items: SurfaceItem[];
  try {
    items = await fetchSurface(w, "references/1");
  } catch {
    // A daemon without the surface, or a blip: nothing of the plugins'
    // links (the Knowledge source is separate).
    items = [];
  }
  if (mine !== generation) return;
  const refs = collectReferences(items);
  for (const [pid, un] of registered) {
    if (!refs.has(pid)) {
      un();
      registered.delete(pid);
    }
  }
  // Registering replaces a source in place: one registry update each.
  for (const [pid, r] of refs) registered.set(pid, registerReferenceSource(pluginSource(r, names.get(pid) ?? pid, root)));
}

workspacePlugins.subscribe((p) => {
  const nextWs = p?.workspace_id ?? null;
  const nextRoot = p?.root ?? null;
  const active = (p?.plugins ?? []).filter((x) => x.active);
  const sig = active.map((x) => `${x.id}@${x.version}`).join(",");
  names = new Map(active.map((x) => [x.id, x.name]));
  if (nextWs !== ws || nextRoot !== root || sig !== activeSig) {
    ws = nextWs;
    root = nextRoot;
    activeSig = sig;
    void refresh();
  }
});

onPlatformFrame((f) => {
  if (f.type === "surface" && f.surface === "references/1" && f.workspace === ws) void refresh();
});
