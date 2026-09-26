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
 *
 * Only the active workspace's plugin status is held reactively (the rail's
 * knowledge row and the dashboard's attach line read it); agent-plugins and
 * skills are fetched on demand by the views that show them — the daemon
 * asks the agents' own CLIs, which is bounded but not free.
 */
import { derived, writable, type Readable } from "svelte/store";

import { api, ApiError } from "../net/api";

export type AgentId = "claude" | "codex";

export interface PluginRequirement {
  agent: string;
  id: string;
  marketplace: string;
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

function normalizePlugin(raw: WorkspacePlugin): WorkspacePlugin {
  const adds = (raw.adds ?? {}) as Partial<WorkspacePlugin["adds"]>;
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

// ---- reactive store (active workspace only) ---------------------------------

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

/** Switch a plugin on/off in the active workspace and re-sync. */
export async function setWorkspacePluginOn(pluginId: string, on: boolean): Promise<void> {
  if (currentWs === null) return;
  await putWorkspacePlugin(currentWs, pluginId, on);
  await refresh(currentWs);
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
