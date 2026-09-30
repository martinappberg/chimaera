import type { ProStatus } from "../net/native";

/** The account's plan, billing, usage and devices page. Only Settings →
 *  Chimaera Pro links to it; nothing billing-related shows before that. */
export const BILLING_PATH = "/account/billing";

/** One project on the web's Home (`GET /home/projects`). */
export interface HomeProject {
  workspace_id: string;
  /** Null when its owner could not be asked for it just now. */
  name: string | null;
  /** Where it opens: `/workspace/{id}/` (it follows its owner) or, for a
   *  project only a cloud machine has, `/app/{host}/#ws={id}`. */
  href: string;
  /** False while its owner can't be reached; it still opens. */
  available: boolean;
}

export interface HomeProjects {
  projects: HomeProject[];
  /** Part of the list couldn't be read just now (the page asks again). */
  pending: boolean;
}

const ID = "[A-Za-z0-9_-]{1,128}";
const WORKSPACE_ID = new RegExp(`^${ID}$`);
const HREF = new RegExp(`^/(?:workspace/${ID}/|app/${ID}/#ws=${ID})$`);

/** Only the two same-origin project addresses the account builds; anything
 *  else a response might carry is never followed. */
export function projectHref(href: unknown): string | null {
  return typeof href === "string" && HREF.test(href) ? href : null;
}

/** The list as Home shows it: well-formed rows only, each id once, by name
 *  (unnamed last) so a refresh never reorders it. Null when unreadable. */
export function readHomeProjects(value: unknown): HomeProjects | null {
  if (typeof value !== "object" || value === null) return null;
  const { projects, pending } = value as { projects?: unknown; pending?: unknown };
  if (!Array.isArray(projects)) return null;
  const rows: HomeProject[] = [];
  const seen = new Set<string>();
  for (const entry of projects.slice(0, 256)) {
    if (typeof entry !== "object" || entry === null) continue;
    const row = entry as Record<string, unknown>;
    const id = row.workspace_id;
    const href = projectHref(row.href);
    if (typeof id !== "string" || !WORKSPACE_ID.test(id) || href === null || seen.has(id)) continue;
    seen.add(id);
    const name = typeof row.name === "string" && row.name.trim() !== "" ? row.name.trim() : null;
    rows.push({ workspace_id: id, name, href, available: row.available === true });
  }
  rows.sort((a, b) => {
    if (a.name === null || b.name === null) return a.name === b.name ? a.workspace_id.localeCompare(b.workspace_id) : a.name === null ? 1 : -1;
    return a.name.localeCompare(b.name, undefined, { sensitivity: "base" }) || a.workspace_id.localeCompare(b.workspace_id);
  });
  return { projects: rows, pending: pending === true };
}

function usagePair(value: unknown): { cloud_hours: number; storage_bytes: number } | null {
  if (typeof value !== "object" || value === null) return null;
  const { cloud_hours, storage_bytes } = value as Record<string, unknown>;
  return typeof cloud_hours === "number" && typeof storage_bytes === "number" ? { cloud_hours, storage_bytes } : null;
}

/** Settings → Chimaera Pro's account (`GET /home/account`), shaped as the
 *  desktop's status so the same readers (`status.ts`) and `AccountUsage`
 *  apply. The account serves only a signed-in browser, so it is signed in.
 *  Null when unreadable. */
export function readBrowserAccount(value: unknown): ProStatus | null {
  if (typeof value !== "object" || value === null) return null;
  const row = value as Record<string, unknown>;
  const plan = row.plan === "pro" || row.plan === "max" || row.plan === "none" ? row.plan : null;
  if (plan === null) return null;
  return {
    available: true,
    signed_in: true,
    error: null,
    email: typeof row.email === "string" ? row.email : null,
    plan,
    payment_due: row.payment_due === true,
    returning_until: typeof row.returning_until === "string" ? row.returning_until : null,
    limits: usagePair(row.limits),
    usage: usagePair(row.usage),
    hours_exhausted: row.hours_exhausted === true,
  };
}

/** A same-origin read with the browser's own session cookie. A session that
 *  ended (401) goes back to `/`, which is then the sign-in page. */
async function read(path: string, signal?: AbortSignal): Promise<unknown> {
  const timeout = AbortSignal.timeout(20_000);
  const response = await fetch(path, {
    credentials: "same-origin",
    cache: "no-store",
    redirect: "error",
    signal: signal ? AbortSignal.any([signal, timeout]) : timeout,
  });
  if (response.status === 401) {
    location.replace("/");
    throw new Error("signed_out");
  }
  if (!response.ok) throw new Error("unavailable");
  return response.json();
}

export async function fetchHomeProjects(signal?: AbortSignal): Promise<HomeProjects> {
  const list = readHomeProjects(await read("/home/projects", signal));
  if (list === null) throw new Error("unavailable");
  return list;
}

export async function fetchBrowserAccount(signal?: AbortSignal): Promise<ProStatus> {
  const account = readBrowserAccount(await read("/home/account", signal));
  if (account === null) throw new Error("unavailable");
  return account;
}

/** Signs this browser out (other devices stay signed in). */
export async function signOutBrowser(): Promise<void> {
  const response = await fetch("/home/sign-out", {
    method: "POST",
    credentials: "same-origin",
    headers: { "X-Chimaera-Browser": "1" },
    redirect: "error",
    signal: AbortSignal.timeout(20_000),
  });
  if (!response.ok && response.status !== 401) throw new Error("sign_out_failed");
}
