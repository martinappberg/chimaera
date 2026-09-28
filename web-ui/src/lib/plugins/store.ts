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
 *   POST /plugins/{pid}/install                    install a first-party plugin at the version chimaera pins
 *   POST /plugins/{pid}/update | rollback | check   Update · Use previous · Check for updates
 *   DELETE /plugins/{pid}                          Remove (the installed copy, every version)
 *   GET  /plugins/{pid}/details                    everything a card shows, before install too
 *   POST /plugins/preview {github}                 a repository's plugin, described, nothing written
 *
 * The user-facing surface is called Extensions; the wire, the routes and this
 * module keep the name "plugins".
 *
 * Only the active workspace's plugin status is held reactively (the rail's
 * knowledge row and the dashboard's attach line read it); agent-plugins and
 * skills are fetched on demand by the views that show them — the daemon
 * asks the agents' own CLIs, which is bounded but not free.
 */
import { derived, get, writable, type Readable } from "svelte/store";

import { api, ApiError } from "../net/api";
import { refreshKnowledge } from "../workspace/knowledge";

export type AgentId = "claude" | "codex";

export interface PluginRequirement {
  agent: string;
  id: string;
  marketplace: string;
}

/** An installed copy on this host, or a first-party plugin chimaera pins
 *  (its `plugins.lock` entry) with nothing installed yet. */
export type PluginSource = "installed" | "available";

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
  /** A few plain sentences from the plugin's author (null when it has none,
   *  and for an available entry, which has no manifest yet). */
  description: string | null;
  /** The plugin's own page (the card's name links to it); an http(s) URL
   *  or null. */
  homepage: string | null;
  adds: { ui: string[]; agents: string[] };
  provides: { knowledge?: string | null; mcp_tools: string[]; views: string[] };
  setup: { prompt: string } | null;
  detect: string[];
  on: boolean;
  detected: boolean;
  active: boolean;
  /** Agent plugins it can't work without, per agent. */
  requires: PluginRequirement[];
  /** Agent plugins that make it more useful for the agents that run them —
   *  never needed for it to work. */
  recommends: PluginRequirement[];
  /** The author's one sentence about what the required / recommended
   *  agent-side plugin is for (the card's "Agent-side plugin" box). */
  requires_summary: string | null;
  recommends_summary: string | null;
  /** The installed copy's version; for an available entry, the version
   *  chimaera pins (`""` from daemons that predate versions). */
  version: string;
  /** The plugin API (WIT) version it targets (`""` for an available entry). */
  api: string;
  source: PluginSource;
  /** A copy is installed on this host (only installed copies can be on). */
  installed: boolean;
  /** Listed in chimaera's `plugins.lock` and updating from the repository
   *  it names there. */
  first_party: boolean;
  /** Its files match its release's SHA256SUMS (and, at the pinned version
   *  of a first-party plugin, the checksums chimaera pins). */
  verified: boolean;
  /** The loaded copy's `plugin.wasm` sha256 (installed copies only). */
  sha256_wasm: string | null;
  /** The GitHub `owner/repo` it installs and updates from, when it names one. */
  repo: string | null;
  /** First-party only: the version chimaera pins in its `plugins.lock`. */
  pinned_version: string | null;
  /** A local install: the directory it was copied from. */
  local_path: string | null;
  /** The installed copy's version directory. */
  path: string | null;
  /** The installed copy's previous version (Use previous). */
  previous: string | null;
  update: PluginUpdate | null;
  /** Why it can't run on this daemon, or isn't answering here. */
  fault: string | null;
}

/** A plugin as its release describes it before install (`/details` of an
 *  available entry, `/preview`) — or, for an installed one, its entry. */
export interface PluginDetails extends WorkspacePlugin {
  /** The release's page (null for an installed entry). */
  release_url: string | null;
  /** What Install downloads, when the releases API said. */
  download: { wasm_bytes: number } | null;
}

/** What an install, update, Use previous or Remove did. */
export interface PluginChange {
  id: string;
  version?: string;
  previous?: string | null;
  /** The checksums the daemon verified the download against. */
  sha256?: { "plugin.wasm"?: string; "plugin.toml"?: string };
  /** The plugin's catalog entry now (after a Remove: the available entry
   *  for a first-party id, else null — nothing is left under that id). */
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

/** The message of a refusal that carried no `{error}` body. */
const generic = (status: number): string => `request failed with status ${status}`;

async function json<T>(res: Response): Promise<T> {
  if (!res.ok) {
    let message = generic(res.status);
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

/** Defensive: an older daemon omits the newer fields (and says `"embedded"`
 *  for what is now simply installed), and an available entry sends
 *  `api: null`. */
function normalizePlugin(raw: WorkspacePlugin): WorkspacePlugin {
  const adds = (raw.adds ?? {}) as Partial<WorkspacePlugin["adds"]>;
  const update = raw.update as Partial<PluginUpdate> | null | undefined;
  const provides = (raw.provides ?? {}) as Partial<WorkspacePlugin["provides"]>;
  const source: PluginSource = raw.source === "available" ? "available" : "installed";
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
    recommends: arr<PluginRequirement>(raw.recommends),
    description: str(raw.description),
    homepage: str(raw.homepage),
    requires_summary: str(raw.requires_summary),
    recommends_summary: str(raw.recommends_summary),
    on: raw.on === true,
    detected: raw.detected === true,
    active: raw.active === true,
    setup: raw.setup ?? null,
    version: typeof raw.version === "string" ? raw.version : "",
    api: typeof raw.api === "string" ? raw.api : "",
    source,
    installed: typeof raw.installed === "boolean" ? raw.installed : source !== "available",
    first_party: raw.first_party === true,
    verified: raw.verified === true,
    sha256_wasm: str(raw.sha256_wasm),
    repo: str(raw.repo),
    pinned_version: str(raw.pinned_version),
    local_path: str(raw.local_path),
    path: str(raw.path),
    previous: str(raw.previous),
    update:
      update && typeof update.version === "string"
        ? { version: update.version, url: str(update.url) ?? "", checked_ms: Number(update.checked_ms ?? 0) }
        : null,
    fault: str(raw.fault),
  };
}

function normalizeDetails(raw: PluginDetails): PluginDetails {
  const d = raw.download as Partial<{ wasm_bytes: number }> | null | undefined;
  const bytes = d !== null && d !== undefined ? Number(d.wasm_bytes) : NaN;
  return {
    ...normalizePlugin(raw),
    release_url: str(raw.release_url),
    download: Number.isFinite(bytes) && bytes > 0 ? { wasm_bytes: bytes } : null,
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

export async function fetchAgentPlugins(workspaceId: string, refresh = false): Promise<AgentPlugins> {
  const body = await json<AgentPlugins>(await api(`${ws(workspaceId)}/agent-plugins${refresh ? "?refresh=true" : ""}`));
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

/** Keep the handoff from the card or setup sheet after its terminal opens.
 *  One pending continuation, scoped to the workspace that requested it. */
export const agentInstallContinuation = writable<{
  workspaceId: string; pluginId: string; agent: AgentId; agentPluginId: string;
} | null>(null);

export async function installPlugin(
  workspaceId: string,
  pluginId: string,
  agent: AgentId,
  agentPluginId: string,
): Promise<{ session_id: string }> {
  const result = await json<{ session_id: string }>(
    await api(`${ws(workspaceId)}/plugins/${encodeURIComponent(pluginId)}/install`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ agent, agent_plugin_id: agentPluginId }),
    }),
  );
  agentInstallContinuation.set({ workspaceId, pluginId, agent, agentPluginId });
  return result;
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

/** A route this daemon doesn't have — a bare 404 with no `{error}` body —
 *  as opposed to a refusal the daemon explains ("unknown plugin"). */
export function isMissingRoute(e: unknown): boolean {
  return e instanceof ApiError && e.status === 404 && e.message === generic(404);
}

/** Install a first-party plugin at the version chimaera pins in its
 *  `plugins.lock` (downloaded from its release, checked against both the
 *  release's SHA256SUMS and the pinned checksums). */
export async function installPinnedRelease(pid: string): Promise<PluginChange> {
  return json(await api(`${plugin(pid)}/install`, { method: "POST" }));
}

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

/** Check for updates: ask the installed copy's release source for a newer version. */
export async function checkWorkbenchPlugin(pid: string): Promise<{ id: string; update: PluginUpdate | null }> {
  return json(await api(`${plugin(pid)}/check`, { method: "POST" }));
}

/** Everything a card shows about `pid` — for a plugin not installed yet,
 *  what its pinned release says (the daemon fetches it once and keeps it). */
export async function fetchPluginDetails(pid: string): Promise<PluginDetails> {
  return normalizeDetails(await json<PluginDetails>(await api(`${plugin(pid)}/details`)));
}

/** A repository's plugin as its latest release describes it (`owner/repo` or
 *  its github.com URL, passed through as typed); nothing is installed. */
export async function previewPlugin(github: string): Promise<PluginDetails> {
  return normalizeDetails(
    await json<PluginDetails>(
      await api("/plugins/preview", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ github }),
      }),
    ),
  );
}

// ---- reactive store (active workspace only) ---------------------------------

const agentPluginsRevisionStore = writable(0);
/** Invalidates mounted reports, including after a socket reconnect. Only a
 *  visible view fetches; a hidden one catches up when shown. */
export const agentPluginsRevision: Readable<number> = agentPluginsRevisionStore;
export function onAgentPluginsChanged(): void {
  agentPluginsRevisionStore.update((revision) => revision + 1);
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

const checkedStore = writable<ReadonlyMap<string, number>>(new Map());
/** When this page last asked each plugin's release source (Check for
 *  updates), by plugin id — the card's quiet "checked just now" line. Only
 *  the user's own checks: the daemon's daily check says nothing here. */
export const checkedAt: Readable<ReadonlyMap<string, number>> = checkedStore;

// ---- available cards opened in place (this page's session only) -------------

const expandedStore = writable<ReadonlySet<string>>(new Set());
/** The available cards the user opened, by plugin id — kept while this page
 *  lives (a tab switch or a re-render keeps them open), never persisted. */
export const expandedPlugins: Readable<ReadonlySet<string>> = expandedStore;

/** A card's fetched details: in flight, answered, or the daemon's refusal. */
export type DetailsState =
  | { state: "loading" }
  | { state: "ok"; plugin: PluginDetails }
  | { state: "error"; message: string; missingRoute: boolean };

const detailsStore = writable<ReadonlyMap<string, DetailsState>>(new Map());
/** Details by `detailsKey` (id + the version the card shows). */
export const pluginDetails: Readable<ReadonlyMap<string, DetailsState>> = detailsStore;

export const detailsKey = (pid: string, version: string): string => `${pid}@${version}`;

/** Open or close an available card; opening fetches its details once (an
 *  earlier refusal is asked again). */
export function toggleExpanded(pid: string, version: string): void {
  let opened = false;
  expandedStore.update((set) => {
    const next = new Set(set);
    opened = !next.delete(pid);
    if (opened) next.add(pid);
    return next;
  });
  if (opened) void loadPluginDetails(pid, version);
}

async function loadPluginDetails(pid: string, version: string): Promise<void> {
  const key = detailsKey(pid, version);
  const known = get(detailsStore).get(key);
  if (known !== undefined && known.state !== "error") return;
  const put = (s: DetailsState) => detailsStore.update((m) => new Map(m).set(key, s));
  put({ state: "loading" });
  try {
    put({ state: "ok", plugin: await fetchPluginDetails(pid) });
  } catch (e) {
    put({ state: "error", message: e instanceof Error ? e.message : String(e), missingRoute: isMissingRoute(e) });
  }
}

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

/** One installed-plugin change (Update, Use previous, Remove, Check for updates), then
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
    const res = await call(pid);
    if (kind === "check") checkedStore.update((m) => new Map(m).set(pid, Date.now()));
    return res;
  } finally {
    if (currentWs !== null) await refresh(currentWs);
    if (kind !== "check") refreshKnowledge();
  }
}

/** Install a plugin from its repository's latest release (`owner/repo` or its
 *  github.com URL — the daemon normalizes both), then re-sync like any other
 *  change: a switch left on under that id comes back active, and a 409 for a
 *  version already installed (say, by the CLI) still brings its card up. */
export async function installWorkbenchPlugin(github: string, version?: string): Promise<PluginChange> {
  try {
    return await installFromRelease(github, version);
  } finally {
    if (currentWs !== null) await refresh(currentWs);
    refreshKnowledge();
  }
}

/** Install a first-party plugin at the version chimaera pins, then re-sync
 *  like an install from a repository (a switch left on under that id comes
 *  back active; Knowledge may gain a provider). */
export async function installFirstPartyPlugin(pid: string): Promise<PluginChange> {
  try {
    return await installPinnedRelease(pid);
  } finally {
    if (currentWs !== null) await refresh(currentWs);
    refreshKnowledge();
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
