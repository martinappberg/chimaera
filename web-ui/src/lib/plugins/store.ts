/**
 * Client for the daemon's plugin seam (design §6) and the small reactive
 * store the workbench needs from it:
 *   GET  /workspaces/{id}/plugins                per-workspace status (on / detected / active)
 *   PUT  /workspaces/{id}/plugins/{pid}          {on}
 *   GET  /workspaces/{id}/agent-plugins          what each agent CLI reports (installed, hooks)
 *   GET  /workspaces/{id}/skills                 every skill each agent can use here
 *   POST /workspaces/{id}/plugins/{pid}/install  {agent} → a visible terminal session
 *   POST /workspaces/{id}/plugins/{pid}/setup    {agent} → a chat session with the setup prompt
 *   POST /workspaces/{id}/plugins/{pid}/trust-hooks {hooks:[{key,hash}]}
 *   POST /plugins/install {github, version?}      install a plugin from its GitHub release
 *   POST /plugins/{pid}/update | rollback | check   update · Use previous · Check now
 *   DELETE /plugins/{pid}                          Remove (the installed copy, every version)
 *
 * Only the active workspace's plugin status is held reactively (the rail's
 * knowledge row and the dashboard's attach line read it); agent-plugins and
 * skills are fetched on demand by the views that show them — the daemon
 * asks the agents' own CLIs, which is bounded but not free.
 */
import { derived, writable, type Readable } from "svelte/store";

import { api, ApiError, health } from "../net/api";
import { refreshKnowledge } from "../workspace/knowledge";

export type AgentId = "claude" | "codex";

export interface PluginRequirement {
  agent: string;
  id: string;
  marketplace: string;
}

/** Where the running copy of a plugin came from. */
export type PluginSource = "embedded" | "installed";

/** A newer, compatible release a check found (the card's Update chip). */
export interface PluginUpdate {
  version: string;
  /** The release's page. */
  url: string;
  checked_ms: number;
}

/** One catalog entry with its status in the active workspace. */
export interface WorkspacePlugin {
  id: string;
  name: string;
  summary: string;
  homepage?: string | null;
  adds: { ui: string[]; agents: string[] };
  provides: { knowledge?: string | null; mcp_tools: string[]; views: string[] };
  setup: { prompt: string } | null;
  detect: string[];
  on: boolean;
  detected: boolean;
  active: boolean;
  requires: PluginRequirement[];
  /** The running copy's own version (`""` from daemons that predate it). */
  version: string;
  /** The plugin API (WIT) version it targets. */
  api: string;
  source: PluginSource;
  /** The installed copy's version directory, when there is an installed copy. */
  path: string | null;
  /** Set when a copy ships with chimaera (the embedded one's version). */
  embedded_version: string | null;
  /** Set when there is an installed copy (its current version). */
  installed_version: string | null;
  /** The installed copy's previous version (Use previous). */
  previous: string | null;
  /** The installed copy is older than the one that ships with chimaera. */
  stale: boolean;
  update: PluginUpdate | null;
  /** Why it can't run on this daemon, or isn't answering here. */
  fault: string | null;
}

/** What an install, update, Use previous or Remove did. */
export interface PluginChange {
  id: string;
  version?: string;
  previous?: string | null;
  /** The checksums the daemon verified the download against. */
  sha256?: { "plugin.wasm"?: string; "plugin.toml"?: string };
  /** The plugin's catalog entry now (null: removed, nothing ships under that id). */
  plugin?: unknown;
}

export interface WorkspacePlugins {
  schema: number;
  workspace_id: string;
  root: string;
  plugins: WorkspacePlugin[];
}

export interface AgentPlugin {
  id: string;
  version?: string;
  scope?: string;
  enabled: boolean;
  skills_n?: number;
  hooks_n?: number;
  always_on_tokens?: number;
}

export type HookTrust = "untrusted" | "trusted" | "modified" | "managed";

export interface AgentHook {
  key: string;
  event: string;
  /** Absent = the hook fires always (codex reports no matcher). */
  matcher?: string | null;
  command?: string | null;
  plugin_id?: string | null;
  trust: HookTrust;
  hash: string;
}

export interface AgentPluginsEntry {
  agent: AgentId | string;
  available: boolean;
  version?: string;
  error?: string;
  plugins: AgentPlugin[];
  /** Codex only: its hook registry with trust state. */
  hooks?: AgentHook[];
}

export interface AgentPlugins {
  schema: number;
  host: string;
  agents: AgentPluginsEntry[];
}

export type SkillSource = "project" | "plugin" | "user" | "builtin" | "system";
export type SkillState = "available" | "off" | "absent";

export interface SkillAgentState {
  state: SkillState;
  reason?: string;
  invoke?: string;
}

export interface Skill {
  name: string;
  description: string;
  source: SkillSource | string;
  plugin?: string;
  paths: { claude?: string; codex?: string };
  agents: { claude: SkillAgentState; codex: SkillAgentState };
}

export interface SkillsReport {
  schema: number;
  host: string;
  agents: {
    claude: { available: boolean; version?: string; live: boolean };
    codex: { available: boolean; version?: string };
  };
  skills: Skill[];
  errors: { agent: string; path?: string; message: string }[];
}

async function json<T>(res: Response): Promise<T> {
  if (!res.ok) {
    let message = `request failed with status ${res.status}`;
    try {
      const body = (await res.json()) as { error?: string };
      if (body.error) message = body.error;
    } catch {
      // non-JSON error body; keep the generic message
    }
    throw new ApiError(res.status, message);
  }
  return (await res.json()) as T;
}

const ws = (id: string): string => `/workspaces/${encodeURIComponent(id)}`;

function arr<T>(v: unknown): T[] {
  return Array.isArray(v) ? (v as T[]) : [];
}

function str(v: unknown): string | null {
  return typeof v === "string" && v !== "" ? v : null;
}

function normalizePlugin(raw: WorkspacePlugin): WorkspacePlugin {
  const adds = (raw.adds ?? {}) as Partial<WorkspacePlugin["adds"]>;
  const update = raw.update as Partial<PluginUpdate> | null | undefined;
  const provides = (raw.provides ?? {}) as Partial<WorkspacePlugin["provides"]>;
  return {
    ...raw,
    adds: { ui: arr<string>(adds.ui), agents: arr<string>(adds.agents) },
    provides: {
      knowledge: provides.knowledge ?? null,
      mcp_tools: arr<string>(provides.mcp_tools),
      views: arr<string>(provides.views),
    },
    detect: arr<string>(raw.detect),
    requires: arr<PluginRequirement>(raw.requires),
    on: raw.on === true,
    detected: raw.detected === true,
    active: raw.active === true,
    setup: raw.setup ?? null,
    version: typeof raw.version === "string" ? raw.version : "",
    api: typeof raw.api === "string" ? raw.api : "",
    source: raw.source === "installed" ? "installed" : "embedded",
    path: str(raw.path),
    embedded_version: str(raw.embedded_version),
    installed_version: str(raw.installed_version),
    previous: str(raw.previous),
    stale: raw.stale === true,
    update:
      update && typeof update.version === "string"
        ? { version: update.version, url: str(update.url) ?? "", checked_ms: Number(update.checked_ms ?? 0) }
        : null,
    fault: str(raw.fault),
  };
}

export async function fetchWorkspacePlugins(workspaceId: string): Promise<WorkspacePlugins> {
  const body = await json<WorkspacePlugins>(await api(`${ws(workspaceId)}/plugins`));
  return { ...body, plugins: arr<WorkspacePlugin>(body.plugins).map(normalizePlugin) };
}

export async function putWorkspacePlugin(
  workspaceId: string,
  pluginId: string,
  on: boolean,
): Promise<{ workspace_id: string; plugins_on: string[] }> {
  return json(
    await api(`${ws(workspaceId)}/plugins/${encodeURIComponent(pluginId)}`, {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ on }),
    }),
  );
}

export async function fetchAgentPlugins(workspaceId: string): Promise<AgentPlugins> {
  const body = await json<AgentPlugins>(await api(`${ws(workspaceId)}/agent-plugins`));
  return {
    schema: body.schema ?? 1,
    host: typeof body.host === "string" ? body.host : "",
    agents: arr<AgentPluginsEntry>(body.agents).map((a) => ({
      ...a,
      available: a.available === true,
      plugins: arr<AgentPlugin>(a.plugins),
      hooks: a.hooks === undefined ? undefined : arr<AgentHook>(a.hooks),
    })),
  };
}

export async function fetchSkills(workspaceId: string): Promise<SkillsReport> {
  const body = await json<SkillsReport>(await api(`${ws(workspaceId)}/skills`));
  const agents = (body.agents ?? {}) as Partial<SkillsReport["agents"]>;
  return {
    schema: body.schema ?? 1,
    host: typeof body.host === "string" ? body.host : "",
    agents: {
      claude: { available: false, live: false, ...(agents.claude ?? {}) },
      codex: { available: false, ...(agents.codex ?? {}) },
    },
    skills: arr<Skill>(body.skills).map((s) => ({
      ...s,
      paths: s.paths ?? {},
      agents: {
        claude: s.agents?.claude ?? { state: "absent" },
        codex: s.agents?.codex ?? { state: "absent" },
      },
    })),
    errors: arr<SkillsReport["errors"][number]>(body.errors),
  };
}

export async function installPlugin(
  workspaceId: string,
  pluginId: string,
  agent: AgentId,
): Promise<{ session_id: string }> {
  return json(
    await api(`${ws(workspaceId)}/plugins/${encodeURIComponent(pluginId)}/install`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ agent }),
    }),
  );
}

export async function setupPlugin(
  workspaceId: string,
  pluginId: string,
  agent: AgentId,
): Promise<{ session_id: string }> {
  return json(
    await api(`${ws(workspaceId)}/plugins/${encodeURIComponent(pluginId)}/setup`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ agent }),
    }),
  );
}

export async function trustHooks(
  workspaceId: string,
  pluginId: string,
  hooks: { key: string; hash: string }[],
): Promise<{ trusted: string[]; skipped: { key: string; reason: string }[] }> {
  return json(
    await api(`${ws(workspaceId)}/plugins/${encodeURIComponent(pluginId)}/trust-hooks`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ hooks }),
    }),
  );
}

const plugin = (pid: string): string => `/plugins/${encodeURIComponent(pid)}`;

/** Install a plugin from its GitHub release (`owner/repo`, the latest or `version`). */
export async function installFromRelease(github: string, version?: string): Promise<PluginChange> {
  return json(
    await api("/plugins/install", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ github, version: version ?? null }),
    }),
  );
}

/** Update an installed plugin to its latest compatible release. */
export async function updateWorkbenchPlugin(pid: string): Promise<PluginChange> {
  return json(await api(`${plugin(pid)}/update`, { method: "POST" }));
}

/** Use previous: swap the installed copy back to its previous version. */
export async function rollbackWorkbenchPlugin(pid: string): Promise<PluginChange> {
  return json(await api(`${plugin(pid)}/rollback`, { method: "POST" }));
}

/** Remove the installed copy (every version of it). */
export async function removeWorkbenchPlugin(pid: string): Promise<PluginChange> {
  return json(await api(plugin(pid), { method: "DELETE" }));
}

/** Check now: ask the installed copy's release source for a newer version. */
export async function checkWorkbenchPlugin(pid: string): Promise<{ id: string; update: PluginUpdate | null }> {
  return json(await api(`${plugin(pid)}/check`, { method: "POST" }));
}

// ---- reactive store (active workspace only) ---------------------------------

const daemonVersionStore = writable<string | null>(null);
/** The daemon's own version (`/health`): the "ships with chimaera <version>" chip. */
export const daemonVersion: Readable<string | null> = daemonVersionStore;
let versionAsked = false;

async function ensureDaemonVersion(): Promise<void> {
  if (versionAsked) return;
  versionAsked = true;
  try {
    daemonVersionStore.set((await health()).version);
  } catch {
    versionAsked = false;
  }
}

const pluginsStore = writable<WorkspacePlugins | null>(null);
/** The active workspace's plugin status (`null` = not loaded / unavailable). */
export const workspacePlugins: Readable<WorkspacePlugins | null> = pluginsStore;

const availableStore = writable<boolean | null>(null);
/** false = the daemon has no plugin routes (predates the feature). */
export const pluginsAvailable: Readable<boolean | null> = availableStore;

/** The structured knowledge provider is active here (mycelium on + detected):
 *  the rail's knowledge row and Knowledge's source chip key on this. */
export const knowledgeProviderActive: Readable<boolean> = derived(pluginsStore, (p) =>
  p !== null && p.plugins.some((x) => x.active && typeof x.provides.knowledge === "string"),
);

/** The mycelium plugin's status here, for the attach affordances. */
export const myceliumPlugin: Readable<WorkspacePlugin | null> = derived(
  pluginsStore,
  (p) => p?.plugins.find((x) => x.id === "mycelium") ?? null,
);

let currentWs: string | null = null;
let refreshSeq = 0;

/** Point the store at a workspace (or `null`) and fetch its plugin status. */
export async function activatePluginsWorkspace(wsId: string | null): Promise<void> {
  if (wsId === currentWs) return;
  currentWs = wsId;
  pluginsStore.set(null);
  availableStore.set(null);
  if (wsId !== null) await refresh(wsId);
}

async function refresh(wsId: string): Promise<void> {
  const seq = ++refreshSeq;
  void ensureDaemonVersion();
  try {
    const p = await fetchWorkspacePlugins(wsId);
    if (currentWs !== wsId || seq !== refreshSeq) return;
    pluginsStore.set(p);
    availableStore.set(true);
  } catch (e) {
    if (currentWs !== wsId || seq !== refreshSeq) return;
    if (e instanceof ApiError && e.status === 404) availableStore.set(false);
  }
}

/** Re-pull the active workspace's status (after a PUT, a setup, the sheet
 *  closing, a view regaining focus — footprints appear without a push). */
export function refreshWorkspacePlugins(): void {
  if (currentWs !== null) void refresh(currentWs);
}

/** One installed-plugin change (Update, Use previous, Remove, Check now), then
 *  the active workspace's cards re-synced — a new version can add or drop a
 *  knowledge provider, so Knowledge refreshes too. */
export async function changeWorkbenchPlugin(
  kind: "update" | "rollback" | "remove" | "check",
  pid: string,
): Promise<PluginChange | { id: string; update: PluginUpdate | null }> {
  const call = {
    update: updateWorkbenchPlugin,
    rollback: rollbackWorkbenchPlugin,
    remove: removeWorkbenchPlugin,
    check: checkWorkbenchPlugin,
  }[kind];
  try {
    return await call(pid);
  } finally {
    if (currentWs !== null) await refresh(currentWs);
    if (kind !== "check") refreshKnowledge();
  }
}

/** Switch a plugin on/off in the active workspace and re-sync. */
export async function setWorkspacePluginOn(pluginId: string, on: boolean): Promise<void> {
  if (currentWs === null) return;
  await putWorkspacePlugin(currentWs, pluginId, on);
  await refresh(currentWs);
  // A knowledge provider switched on/off changes what Knowledge (and the
  // dashboard's Where things stand) holds — don't wait for a Timeline nudge.
  refreshKnowledge();
}

// ---- the attach sheet request ------------------------------------------------

/** Non-null while the "Use mycelium for Knowledge" sheet should be open; App
 *  hosts the one modal instance so every surface (Knowledge's empty card,
 *  the dashboard line, the plugin card, the dock) opens the same sheet. */
export const attachRequest = writable<{ pluginId: string } | null>(null);

export function openAttachSheet(pluginId = "mycelium"): void {
  attachRequest.set({ pluginId });
}

export function closeAttachSheet(): void {
  attachRequest.set(null);
}
