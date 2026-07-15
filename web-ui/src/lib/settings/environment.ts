/**
 * Client half of the environment-prelude store (`/api/v1/environment`): the
 * per-daemon map of prelude scripts run once at session start. The wire shape
 * is the WHOLE map — GET returns it, PUT replaces it — so a writer must
 * fetch-merge-put to keep other workspaces' entries intact. The daemon
 * persists it as `env-profiles.json` (not settings.json); the Environment
 * settings panel is the editor.
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
 * PUT /api/v1/environment — replace the whole map (204). Merge your edit into
 * a fresh GET first. 413 when a scope's text exceeds the daemon's size cap
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
