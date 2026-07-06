import { api, ApiError } from "./api";

export interface Workspace {
  id: string;
  root: string;
  name: string;
}

export interface Session {
  id: string;
  name: string;
  cwd: string;
  cols: number;
  rows: number;
  created_at: number;
  alive: boolean;
  exit_status: number | null;
  title: string | null;
  workspace_id: string;
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
  if (res.status === 204) {
    return undefined as T;
  }
  return (await res.json()) as T;
}

export interface DirEntry {
  name: string;
  path: string;
}

export interface DirListing {
  path: string;
  parent: string | null;
  dirs: DirEntry[];
}

/** The daemon user's home directory. */
export async function fsHome(): Promise<string> {
  const body = await json<{ path: string }>(await api("/fs/home"));
  return body.path;
}

/**
 * List the subdirectories of `path`. The daemon expands a leading "~",
 * canonicalizes, and returns directories only; 400 with a message if the
 * path does not exist or is not a directory.
 */
export async function fsDirs(path: string, hidden = false): Promise<DirListing> {
  const q = new URLSearchParams({ path });
  if (hidden) q.set("hidden", "true");
  return json(await api(`/fs/dirs?${q.toString()}`));
}

export async function listWorkspaces(): Promise<Workspace[]> {
  return json(await api("/workspaces"));
}

export async function createWorkspace(root: string): Promise<Workspace> {
  return json(
    await api("/workspaces", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ root }),
    }),
  );
}

export async function listSessions(): Promise<Session[]> {
  return json(await api("/sessions"));
}

export async function createSession(
  workspaceId: string,
  name: string | null = null,
): Promise<Session> {
  return json(
    await api("/sessions", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ workspace_id: workspaceId, name }),
    }),
  );
}

export async function deleteSession(id: string): Promise<void> {
  await json<void>(await api(`/sessions/${id}`, { method: "DELETE" }));
}

/**
 * Poll GET /api/v1/sessions on an interval to refresh names/titles/alive.
 * Fires immediately, then every `intervalMs`. Returns a stop function.
 */
export function pollSessions(
  onResult: (sessions: Session[]) => void,
  onError: (e: unknown) => void,
  intervalMs = 5000,
): () => void {
  let stopped = false;
  const tick = async () => {
    try {
      const list = await listSessions();
      if (!stopped) onResult(list);
    } catch (e) {
      if (!stopped) onError(e);
    }
  };
  void tick();
  const id = setInterval(tick, intervalMs);
  return () => {
    stopped = true;
    clearInterval(id);
  };
}
