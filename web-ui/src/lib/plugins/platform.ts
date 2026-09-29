/**
 * The client half of the plugin platform (docs/plugin-platform-plan.md
 * §3–§9): a plugin's 0.2 tables on the wire, the pure rules that pick what
 * a plugin draws where (the file kinds it claims, its file menu items, its
 * panels, cards and status chips), and the fetchers for its screens,
 * actions, queries, data surfaces, output folder and settings.
 *
 * Screens are data (`ui/1`); `ui/UiNode.svelte` draws them and
 * `ui/PluginScreen.svelte` hosts one. `view` and `surface` frames from
 * `/ws/events` reach listeners through `onPlatformFrame`, so a screen
 * re-renders when its plugin says it changed.
 */
import { api, ApiError } from "../net/api";
import type { WorkspacePlugin } from "./store";

export type Slot = "tab" | "panel" | "file" | "status" | "card";

export interface ViewDecl {
  id: string;
  title: string;
  slot: Slot;
  icon: string | null;
}

export interface FileKindDecl {
  match: string[];
  view: string;
  label: string;
}

export interface FileActionDecl {
  match: string[];
  label: string;
  action: string;
  icon: string | null;
}

export type SettingType = "bool" | "enum" | "string" | "number" | "path";

export interface SettingDecl {
  key: string;
  type: SettingType;
  default: unknown;
  label: string;
  description: string | null;
  scope: "host" | "workspace";
  options: string[];
  min: number | null;
  max: number | null;
}

/** A side program the plugin can download (`[[tools]]`, §8). */
export interface ToolDecl {
  id: string;
  name: string;
  version: string;
  programs: string[];
  home: string | null;
  /** What Install downloads on this host; null: no build for it. */
  download: { host: string; size: number | null } | null;
}

export interface PlatformTables {
  views: ViewDecl[];
  files: FileKindDecl[];
  actions: FileActionDecl[];
  settings: SettingDecl[];
  /** The programs it may run (`[[programs]]`, §6). */
  programs: string[];
  tools: ToolDecl[];
}

export const EMPTY_PLATFORM: PlatformTables = { views: [], files: [], actions: [], settings: [], programs: [], tools: [] };

const SLOTS: readonly Slot[] = ["tab", "panel", "file", "status", "card"];
const TYPES: readonly SettingType[] = ["bool", "enum", "string", "number", "path"];

function list(v: unknown): unknown[] {
  return Array.isArray(v) ? v : [];
}
function text(v: unknown): string | null {
  return typeof v === "string" && v.length > 0 ? v : null;
}
function strings(v: unknown): string[] {
  return list(v).filter((x): x is string => typeof x === "string");
}
function num(v: unknown): number | null {
  return typeof v === "number" && Number.isFinite(v) ? v : null;
}

/** The `platform` key of a plugin's wire entry; empty from an older daemon
 *  or a 0.1 plugin. Malformed rows are dropped, never guessed at. */
export function normalizePlatform(raw: unknown): PlatformTables {
  const r = (raw ?? {}) as Record<string, unknown>;
  const views: ViewDecl[] = [];
  for (const v of list(r.views)) {
    const o = v as Record<string, unknown>;
    const id = text(o?.id);
    const title = text(o?.title);
    const slot = o?.slot as Slot;
    if (id !== null && title !== null && SLOTS.includes(slot)) views.push({ id, title, slot, icon: text(o.icon) });
  }
  const files: FileKindDecl[] = [];
  for (const f of list(r.files)) {
    const o = f as Record<string, unknown>;
    const view = text(o?.view);
    const label = text(o?.label);
    const match = strings(o?.match);
    if (view !== null && label !== null && match.length > 0) files.push({ match, view, label });
  }
  const actions: FileActionDecl[] = [];
  for (const a of list(r.actions)) {
    const o = a as Record<string, unknown>;
    const action = text(o?.action);
    const label = text(o?.label);
    const match = strings(o?.match);
    if (action !== null && label !== null && match.length > 0) {
      actions.push({ match, label, action, icon: text(o.icon) });
    }
  }
  const settings: SettingDecl[] = [];
  for (const s of list(r.settings)) {
    const o = s as Record<string, unknown>;
    const key = text(o?.key);
    const label = text(o?.label);
    const type = o?.type as SettingType;
    if (key === null || label === null || !TYPES.includes(type)) continue;
    settings.push({
      key,
      type,
      default: o.default ?? null,
      label,
      description: text(o.description),
      scope: o.scope === "host" ? "host" : "workspace",
      options: strings(o.options),
      min: num(o.min),
      max: num(o.max),
    });
  }
  const tools: ToolDecl[] = [];
  for (const t of list(r.tools)) {
    const o = t as Record<string, unknown>;
    const id = text(o?.id);
    const name = text(o?.name);
    const version = text(o?.version);
    if (id === null || name === null || version === null) continue;
    const d = o.download as Record<string, unknown> | null | undefined;
    const host = text(d?.host);
    tools.push({
      id,
      name,
      version,
      programs: strings(o.programs),
      home: text(o.home),
      download: host !== null ? { host, size: num(d?.size) } : null,
    });
  }
  return { views, files, actions, settings, programs: strings(r.programs), tools };
}

// --- matching (the daemon's `platform::matches`, mirrored) -----------------

/** `*` and `?` within one path component. */
function glob(pat: string, name: string): boolean {
  let p = 0;
  let t = 0;
  let star = -1;
  let mark = 0;
  while (t < name.length) {
    if (p < pat.length && (pat[p] === "?" || pat[p] === name[t])) {
      p++;
      t++;
    } else if (p < pat.length && pat[p] === "*") {
      star = p++;
      mark = t;
    } else if (star >= 0) {
      p = star + 1;
      t = ++mark;
    } else {
      return false;
    }
  }
  while (p < pat.length && pat[p] === "*") p++;
  return p === pat.length;
}

function components(pat: string[], parts: string[]): boolean {
  if (pat.length === 0) return parts.length === 0;
  const [first, ...rest] = pat;
  if (first === "**") {
    for (let skip = 0; skip <= parts.length; skip++) if (components(rest, parts.slice(skip))) return true;
    return false;
  }
  return parts.length > 0 && glob(first, parts[0]) && components(rest, parts.slice(1));
}

/** Whether workspace-relative `path` matches `pattern`: a pattern without
 *  a `/` matches the file name anywhere (`*.tex`); `**` spans folders. */
export function matchesPattern(pattern: string, path: string): boolean {
  if (!pattern.includes("/")) return glob(pattern, path.slice(path.lastIndexOf("/") + 1));
  return components(pattern.split("/"), path.split("/"));
}

/** `path` relative to `root` (null when outside it). */
export function workspaceRelative(root: string | null, path: string): string | null {
  if (root === null) return null;
  const r = root.endsWith("/") ? root.slice(0, -1) : root;
  return path.startsWith(`${r}/`) ? path.slice(r.length + 1) : null;
}

/** An active plugin that opens `rel` in one of its views. */
export interface FileClaim {
  plugin: WorkspacePlugin;
  kind: FileKindDecl;
  view: ViewDecl;
}

/** Every active plugin that claims `rel` (`[[files]]`), in the daemon's
 *  order; the first is the default, the rest are the Open with choices. */
export function claimsFor(plugins: readonly WorkspacePlugin[], rel: string): FileClaim[] {
  const out: FileClaim[] = [];
  for (const p of plugins) {
    if (!p.active) continue;
    const kind = p.platform.files.find((f) => f.match.some((m) => matchesPattern(m, rel)));
    const view = kind && p.platform.views.find((v) => v.id === kind.view && v.slot === "file");
    if (kind && view) out.push({ plugin: p, kind, view });
  }
  return out;
}

/** The file menu items active plugins add for `rel` (`[[actions]]`). */
export function actionsFor(
  plugins: readonly WorkspacePlugin[],
  rel: string,
): { plugin: WorkspacePlugin; action: FileActionDecl }[] {
  const out: { plugin: WorkspacePlugin; action: FileActionDecl }[] = [];
  for (const p of plugins) {
    if (!p.active) continue;
    for (const action of p.platform.actions) {
      if (action.match.some((m) => matchesPattern(m, rel))) out.push({ plugin: p, action });
    }
  }
  return out;
}

/** An active plugin's views of one slot, as (plugin, view) pairs. */
export function viewsIn(
  plugins: readonly WorkspacePlugin[],
  slot: Slot,
): { plugin: WorkspacePlugin; view: ViewDecl }[] {
  const out: { plugin: WorkspacePlugin; view: ViewDecl }[] = [];
  for (const p of plugins) {
    if (!p.active) continue;
    for (const view of p.platform.views) if (view.slot === slot) out.push({ plugin: p, view });
  }
  return out;
}

// --- `open with`, remembered per workspace ---------------------------------

const OPEN_WITH_KEY = "chimaera.plugins.openWith";

/** What the user picked for files of this kind here: a plugin id, `"text"`,
 *  or null (the default: the first claimant). Per-viewer convenience only. */
export function openWith(wsId: string, label: string): string | null {
  try {
    const all = JSON.parse(localStorage.getItem(OPEN_WITH_KEY) ?? "{}") as Record<string, string>;
    return all[`${wsId}\u0000${label}`] ?? null;
  } catch {
    return null;
  }
}

export function rememberOpenWith(wsId: string, label: string, choice: string | null): void {
  try {
    const all = JSON.parse(localStorage.getItem(OPEN_WITH_KEY) ?? "{}") as Record<string, string>;
    const key = `${wsId}\u0000${label}`;
    if (choice === null) delete all[key];
    else all[key] = choice;
    localStorage.setItem(OPEN_WITH_KEY, JSON.stringify(all));
  } catch {
    // Private windows and blocked storage: the choice lasts this page.
  }
}

// --- the wire ---------------------------------------------------------------

async function body<T>(res: Response): Promise<T> {
  if (!res.ok) {
    let message = `request failed with status ${res.status}`;
    try {
      const b = (await res.json()) as { error?: string };
      if (b.error) message = b.error;
    } catch {
      // keep the generic message
    }
    throw new ApiError(res.status, message);
  }
  return (await res.json()) as T;
}

const base = (ws: string, pid: string): string =>
  `/workspaces/${encodeURIComponent(ws)}/plugins/${encodeURIComponent(pid)}`;

export interface UiTree {
  ui: string;
  root: UiNodeData;
}

/** A screen node: `{type, …props, children?}`; see `ui/UiNode.svelte`. */
export type UiNodeData = { type: string; [prop: string]: unknown };

/** A rendered view, or why it can't be drawn (the plugin failed, or sent a
 *  tree the daemon's check refused — `problems` name what to fix). */
export type RenderResult =
  | { ok: true; title: string; slot: Slot; tree: UiTree }
  | { ok: false; error: string; problems: string[] };

type RenderWire = { title?: string; slot?: Slot; tree?: UiTree | null; error?: string; problems?: string[] };

function renderResult(b: RenderWire, fallbackTitle = ""): RenderResult {
  if (b.tree && typeof b.tree === "object") {
    return { ok: true, title: b.title ?? fallbackTitle, slot: b.slot ?? "tab", tree: b.tree };
  }
  return { ok: false, error: b.error ?? "this screen could not be drawn", problems: strings(b.problems) };
}

export async function fetchView(
  ws: string,
  pid: string,
  view: string,
  opts: { file?: string; width?: "narrow" | "wide" } = {},
): Promise<RenderResult> {
  const q = new URLSearchParams();
  if (opts.file !== undefined) q.set("file", opts.file);
  if (opts.width !== undefined) q.set("width", opts.width);
  const qs = q.toString();
  const b = await body<RenderWire>(
    await api(`${base(ws, pid)}/views/${encodeURIComponent(view)}${qs ? `?${qs}` : ""}`),
  );
  return renderResult(b);
}

/** A node's action: the view's new tree, `null` (keep what it shows), or
 *  why it failed. */
export async function postViewAction(
  ws: string,
  pid: string,
  view: string,
  action: string,
  payload: unknown,
  extra: { form?: Record<string, unknown>; value?: unknown } = {},
): Promise<RenderResult | null> {
  const b = await body<RenderWire>(
    await api(`${base(ws, pid)}/views/${encodeURIComponent(view)}/actions`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ action, payload: payload ?? null, ...extra }),
    }),
  );
  if (b.tree === null && b.error === undefined) return null;
  return renderResult(b);
}

export interface FileActionAnswer {
  message: string | null;
  open: { view: string; file?: string } | null;
}

export async function postFileAction(ws: string, pid: string, action: string, file: string): Promise<FileActionAnswer> {
  const b = await body<{ message?: string | null; open?: { view: string; file?: string } | null }>(
    await api(`${base(ws, pid)}/file-actions/${encodeURIComponent(action)}`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ file }),
    }),
  );
  return { message: b.message ?? null, open: b.open ?? null };
}

export async function fetchQuery(ws: string, pid: string, name: string, args: unknown): Promise<unknown> {
  const q = new URLSearchParams({ args: JSON.stringify(args ?? {}) });
  const b = await body<{ data: unknown }>(await api(`${base(ws, pid)}/query/${encodeURIComponent(name)}?${q}`));
  return b.data;
}

export interface SurfaceItem {
  plugin: string;
  key: string;
  data: unknown;
}

/** Every active plugin's published data of one surface here (`file`
 *  narrows diagnostics to a file and outputs to a source). */
export async function fetchSurface(ws: string, surface: string, file?: string): Promise<SurfaceItem[]> {
  const q = file !== undefined ? `?file=${encodeURIComponent(file)}` : "";
  const b = await body<{ items?: SurfaceItem[] }>(
    await api(`/workspaces/${encodeURIComponent(ws)}/surfaces/${surface}${q}`),
  );
  return Array.isArray(b.items) ? b.items : [];
}

export interface Diagnostic {
  file: string;
  severity: "error" | "warning" | "info" | "hint";
  line: number;
  column?: number;
  end_line?: number;
  end_column?: number;
  message: string;
  context?: string;
  source?: string;
  plugin: string;
}

/** The `diagnostics/1` items for `file` (every file when omitted), errors
 *  first, then by line. */
export async function fetchDiagnostics(ws: string, file?: string): Promise<Diagnostic[]> {
  const items = await fetchSurface(ws, "diagnostics/1", file);
  const out: Diagnostic[] = [];
  for (const it of items) {
    const data = it.data as { items?: unknown };
    for (const d of list(data?.items)) {
      const o = d as Partial<Diagnostic>;
      if (typeof o.file !== "string" || typeof o.line !== "number" || typeof o.message !== "string") continue;
      out.push({ ...(o as Diagnostic), plugin: it.plugin });
    }
  }
  const rank = { error: 0, warning: 1, info: 2, hint: 3 } as const;
  return out.sort((a, b) => rank[a.severity] - rank[b.severity] || a.file.localeCompare(b.file) || a.line - b.line);
}

const outputRoots = new Map<string, Promise<string>>();

/** The plugin's output folder here, absolute (cached for the page). */
export function outputRoot(ws: string, pid: string): Promise<string> {
  const key = `${ws}\u0000${pid}`;
  let p = outputRoots.get(key);
  if (p === undefined) {
    p = (async () => (await body<{ root: string }>(await api(`${base(ws, pid)}/output`))).root)();
    p.catch(() => outputRoots.delete(key));
    outputRoots.set(key, p);
  }
  return p;
}

/** `ref` (a workspace path, or `output:<path>`) as an absolute path. */
export async function resolvePlace(ws: string, wsRoot: string | null, pid: string, ref: string): Promise<string> {
  if (ref.startsWith("output:")) {
    const root = await outputRoot(ws, pid);
    return `${root}/${ref.slice("output:".length).replace(/^\/+/, "")}`;
  }
  if (ref.startsWith("/")) return ref;
  const r = wsRoot ?? "";
  return `${r.endsWith("/") ? r.slice(0, -1) : r}/${ref}`;
}

export async function saveOutput(
  ws: string,
  pid: string,
  from: string,
  to: string,
  replace = false,
): Promise<{ path: string; bytes: number }> {
  return body(
    await api(`${base(ws, pid)}/output/save`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ from, to, replace }),
    }),
  );
}

export interface OutputUse {
  bytes: number;
  quota: number;
}

export async function fetchOutputUse(pid: string): Promise<OutputUse> {
  return body(await api(`/plugins/${encodeURIComponent(pid)}/output`));
}

export async function clearOutput(pid: string): Promise<OutputUse> {
  return body(await api(`/plugins/${encodeURIComponent(pid)}/output`, { method: "DELETE" }));
}

/** A tool on this host (`GET /plugins/{pid}/tools`): the card's Tools
 *  section. `current`: the installed version is the declared one. */
export interface ToolState {
  tool: string;
  name: string;
  version: string;
  programs: string[];
  installing: boolean;
  installed: { version: string; bytes: number; installed_ms: number } | null;
  current: boolean;
  download: { host: string; size: number | null } | null;
}

function toolState(raw: unknown): ToolState | null {
  const o = (raw ?? {}) as Record<string, unknown>;
  const tool = text(o.tool);
  const name = text(o.name);
  const version = text(o.version);
  if (tool === null || name === null || version === null) return null;
  const i = o.installed as Record<string, unknown> | null | undefined;
  const iv = text(i?.version);
  const d = o.download as Record<string, unknown> | null | undefined;
  const host = text(d?.host);
  return {
    tool,
    name,
    version,
    programs: strings(o.programs),
    installing: o.installing === true,
    installed: iv !== null ? { version: iv, bytes: num(i?.bytes) ?? 0, installed_ms: num(i?.installed_ms) ?? 0 } : null,
    current: o.current === true,
    download: host !== null ? { host, size: num(d?.size) } : null,
  };
}

const toolsBase = (pid: string): string => `/plugins/${encodeURIComponent(pid)}/tools`;

export async function fetchTools(pid: string): Promise<ToolState[]> {
  const b = await body<{ tools?: unknown[] }>(await api(toolsBase(pid)));
  return list(b.tools)
    .map(toolState)
    .filter((t): t is ToolState => t !== null);
}

/** Install or update: download, check, unpack, set up. Answers once it is
 *  in place (a large download takes a while); the error is the daemon's. */
export async function installTool(pid: string, tool: string): Promise<void> {
  await body<unknown>(await api(`${toolsBase(pid)}/${encodeURIComponent(tool)}/install`, { method: "POST" }));
}

export async function removeTool(pid: string, tool: string): Promise<void> {
  await body<unknown>(await api(`${toolsBase(pid)}/${encodeURIComponent(tool)}`, { method: "DELETE" }));
}

/** A tool's one line before install: "TeX Live 2026.09 · 152 MB from
 *  github.com", or that this host has no build of it. */
export function downloadWords(t: ToolDecl): string {
  if (t.download === null) return `${t.name} ${t.version} · no build for this computer`;
  const size = t.download.size !== null ? ` · ${sizeWords(t.download.size)}` : "";
  return `${t.name} ${t.version}${size} from ${t.download.host}`;
}

export interface SettingValue extends SettingDecl {
  value: unknown;
  /** The user set it (else it is the default). */
  set: boolean;
}

export async function fetchPluginSettings(pid: string, ws: string | null): Promise<SettingValue[]> {
  const q = ws !== null ? `?workspace=${encodeURIComponent(ws)}` : "";
  const b = await body<{ settings?: unknown[] }>(await api(`/plugins/${encodeURIComponent(pid)}/settings${q}`));
  const decls = normalizePlatform({ settings: b.settings }).settings;
  return decls.map((d, i) => {
    const raw = (b.settings?.[i] ?? {}) as { value?: unknown; set?: unknown };
    return { ...d, value: raw.value ?? d.default, set: raw.set === true };
  });
}

/** Set one (`null` resets it to the default). */
export async function putPluginSetting(pid: string, key: string, value: unknown, ws: string | null): Promise<SettingValue[]> {
  const res = await api(`/plugins/${encodeURIComponent(pid)}/settings`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ key, value, workspace: ws }),
  });
  await body<unknown>(res);
  return fetchPluginSettings(pid, ws);
}

// --- frames -----------------------------------------------------------------

/** A platform frame from `/ws/events`: a view to render again, a surface
 *  to fetch again, a plugin's own `emit`, or a job that changed state
 *  (queued, running, done: `GET /workspaces/{id}/jobs/{job}`'s shape). Already scoped to the window's
 *  workspace by the daemon. */
export interface PlatformFrame {
  type: "view" | "surface" | "plugin" | "job";
  plugin: string;
  workspace: string;
  view?: string;
  surface?: string;
  key?: string;
  [k: string]: unknown;
}

const listeners = new Set<(f: PlatformFrame) => void>();

export function onPlatformFrame(fn: (f: PlatformFrame) => void): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

/** Called by the events socket for each platform frame. */
export function platformFrame(f: PlatformFrame): void {
  for (const fn of listeners) {
    try {
      fn(f);
    } catch {
      // one listener's failure never stops the others
    }
  }
}

/** Bytes in words (the card's "Uses 12 MB"). */
export function sizeWords(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB"];
  let n = bytes / 1024;
  let u = 0;
  while (n >= 1024 && u < units.length - 1) {
    n /= 1024;
    u++;
  }
  return `${n < 10 ? n.toFixed(1) : Math.round(n)} ${units[u]}`;
}

// --- opening a plugin's tab view --------------------------------------------

type ViewOpener = (plugin: string, view: string) => void;
let viewOpener: ViewOpener | null = null;

/** App-level wiring (the layout owns tabs); null unregisters. */
export function setViewOpener(fn: ViewOpener | null): void {
  viewOpener = fn;
}

/** Open (or focus) a plugin's tab view. False when nothing can open it. */
export function openPluginView(plugin: string, view: string): boolean {
  if (viewOpener === null) return false;
  viewOpener(plugin, view);
  return true;
}

/** A plugin tab's name: its view's title, else the view id. */
export function pluginViewTitle(plugins: readonly WorkspacePlugin[] | undefined, plugin: string, view: string): string {
  const p = plugins?.find((x) => x.id === plugin);
  return p?.platform.views.find((v) => v.id === view)?.title ?? view;
}
