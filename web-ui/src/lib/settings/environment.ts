/**
 * Client half of the environment-prelude store (`/api/v1/environment`): the
 * per-daemon map of prelude scripts run once at session start. GET returns the
 * whole map; PATCH changes only the scopes it names, read-modify-written
 * one at a time behind the store's disk gate (not its mutex, which is never
 * held across I/O), so two windows editing different scopes never undo each
 * other (PUT, which replaces the whole map, stays for other callers).
 * The daemon persists it as `env-profiles.json` (not settings.json); the
 * Environment settings panel is the editor.
 */

import { api, ApiError } from "../net/api";

/**
 * One scope's prelude. An object rather than a bare string so named profiles
 * can extend the shape later without a wire break — do not flatten.
 */
export interface EnvironmentEntry {
  text: string;
}

/** The whole prelude map. A scope is absent when never set. */
export interface EnvironmentMap {
  host?: EnvironmentEntry;
  workspaces?: Record<string, EnvironmentEntry>;
}

async function errorFrom(res: Response): Promise<ApiError> {
  let message = `request failed with status ${res.status}`;
  try {
    const body = (await res.json()) as { error?: string };
    if (body.error) message = body.error;
  } catch {
    // non-JSON error body; keep the generic message
  }
  return new ApiError(res.status, message);
}

function parseEntry(raw: unknown): EnvironmentEntry | null {
  if (typeof raw !== "object" || raw === null) return null;
  const text = (raw as Record<string, unknown>).text;
  return typeof text === "string" ? { text } : null;
}

/** GET /api/v1/environment — the current prelude map. */
export async function getEnvironment(): Promise<EnvironmentMap> {
  const res = await api("/environment");
  if (!res.ok) throw await errorFrom(res);
  const body = (await res.json()) as unknown;
  const out: EnvironmentMap = {};
  if (typeof body !== "object" || body === null) return out;
  const b = body as Record<string, unknown>;
  const host = parseEntry(b.host);
  if (host !== null) out.host = host;
  if (typeof b.workspaces === "object" && b.workspaces !== null) {
    const ws: Record<string, EnvironmentEntry> = {};
    for (const [id, raw] of Object.entries(b.workspaces)) {
      const entry = parseEntry(raw);
      if (entry !== null) ws[id] = entry;
    }
    if (Object.keys(ws).length > 0) out.workspaces = ws;
  }
  return out;
}

/**
 * PUT /api/v1/environment — replace the whole map (204). Any scope this
 * writer did not read fresh is overwritten; an editor should PATCH instead. 413 when a scope's text exceeds the daemon's size cap
 * (32 KB per scope, 256 KB for the map).
 */
export async function putEnvironment(map: EnvironmentMap): Promise<void> {
  const res = await api("/environment", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(map),
  });
  if (!res.ok) throw await errorFrom(res);
}

/**
 * A scoped update: a present scope replaces that entry, `null` deletes it,
 * an absent scope is untouched.
 */
export interface EnvironmentPatch {
  host?: EnvironmentEntry | null;
  workspaces?: Record<string, EnvironmentEntry | null>;
}

/** PATCH /api/v1/environment — apply `patch` (204); errors as for PUT. */
export async function patchEnvironment(patch: EnvironmentPatch): Promise<void> {
  const res = await api("/environment", {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(patch),
  });
  if (!res.ok) throw await errorFrom(res);
}

/** The panel's two editors (or their saved baselines). */
export interface PreludeTexts {
  host: string;
  workspace: string;
}

/** What one save sends, and the editor text each sent scope carried. */
export interface EnvironmentSavePlan {
  patch: EnvironmentPatch;
  sent: Partial<PreludeTexts>;
}

/** An empty editor deletes its scope; `{text: ""}` is never persisted. */
function entryFor(text: string): EnvironmentEntry | null {
  return text.trim() === "" ? null : { text };
}

/**
 * The PATCH for the scopes whose editor differs from its baseline, or null
 * when nothing changed. Unchanged scopes stay out of the body, so a value
 * another window saved meanwhile survives. `wsId` null: no workspace scope.
 */
export function planEnvironmentSave(
  editors: PreludeTexts,
  saved: PreludeTexts,
  wsId: string | null,
): EnvironmentSavePlan | null {
  const plan: EnvironmentSavePlan = { patch: {}, sent: {} };
  if (editors.host !== saved.host) {
    plan.patch.host = entryFor(editors.host);
    plan.sent.host = editors.host;
  }
  if (wsId !== null && editors.workspace !== saved.workspace) {
    plan.patch.workspaces = { [wsId]: entryFor(editors.workspace) };
    plan.sent.workspace = editors.workspace;
  }
  return plan.sent.host === undefined && plan.sent.workspace === undefined ? null : plan;
}

/**
 * Baselines after `plan` landed: each sent scope's baseline is the text that
 * was SENT, not the editor's text now — whatever was typed while the save was
 * out stays dirty for the next one.
 */
export function baselinesAfterSave(saved: PreludeTexts, plan: EnvironmentSavePlan): PreludeTexts {
  return { ...saved, ...plan.sent };
}
