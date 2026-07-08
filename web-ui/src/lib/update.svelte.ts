/**
 * Update awareness, merged into one offer.
 *
 * Three signals arrive on their own schedules — the daemon's release check
 * (`update` frame on /ws/events, any window), the shell's signed-app check
 * (`app-update` event, native only), and daemon/app build skew (health
 * build vs shell build, native only) — and this store reduces them to at
 * most ONE offer for the toast, by how much a click can actually fix here:
 * the full app+daemon chain beats a daemon-only restart beats a "new
 * release exists" notice (browser windows, where applying is the CLI's or
 * the app's job).
 *
 * Snooze/skip live in localStorage: windows on the same daemon share an
 * origin, so dismissing once quiets every window there.
 */

import { isNativeShell } from "./native";

export interface UpdateStatus {
  current: string;
  build: string | null;
  available: boolean;
  latest: { version: string; url: string; published_at?: string | null } | null;
}

export type UpdateOffer =
  /** Native: a newer signed app exists — one click runs app + daemon. */
  | { kind: "app"; version: string; url: string | null }
  /** Native, local window: this daemon is older than the app build. */
  | { kind: "daemon-local" }
  /** Native, remote window: the host's daemon is older than the app build. */
  | { kind: "daemon-remote"; alias: string }
  /** Browser: a newer release exists; applying happens elsewhere. */
  | { kind: "release"; version: string; url: string | null };

const SKIP_KEY = "chimaera.update.skip";
const SNOOZE_KEY = "chimaera.update.snooze";
/** "Later" quiets the toast for ~20h — under a day, so it returns tomorrow. */
const SNOOZE_MS = 20 * 60 * 60 * 1000;

/** Raw signals; each arrives from its own listener. */
export const updateState = $state({
  /** Daemon's own release knowledge (ws frame / GET /update). */
  daemon: null as UpdateStatus | null,
  /** Newer signed app version (native `app-update` event). */
  appVersion: null as string | null,
  /** This window's daemon build differs from the app build (native). */
  buildSkew: false,
  /** Bumped on snooze/skip so the derived offer re-evaluates. */
  dismissedAt: 0,
});

function skipped(): string {
  try {
    return localStorage.getItem(SKIP_KEY) ?? "";
  } catch {
    return "";
  }
}

function snoozedUntil(): number {
  try {
    return Number(localStorage.getItem(SNOOZE_KEY) ?? "0");
  } catch {
    return 0;
  }
}

/** Quiet this offer until tomorrow (all windows on this origin). */
export function snoozeUpdate(): void {
  try {
    localStorage.setItem(SNOOZE_KEY, String(Date.now() + SNOOZE_MS));
  } catch {
    // Private-mode storage failures just mean the toast returns sooner.
  }
  updateState.dismissedAt = Date.now();
}

/** Never offer `version` again (a "skip this version" click). */
export function skipUpdateVersion(version: string): void {
  try {
    localStorage.setItem(SKIP_KEY, version);
  } catch {
    // Same as snooze: failure to persist only means re-offering.
  }
  updateState.dismissedAt = Date.now();
}

/**
 * The one offer worth showing right now, or null. `hostAlias` is the ssh
 * alias this window reaches its daemon through (null = the local daemon).
 */
export function currentOffer(hostAlias: string | null): UpdateOffer | null {
  // Touch the dismissal marker so runes re-derive after snooze/skip.
  void updateState.dismissedAt;
  if (Date.now() < snoozedUntil()) return null;

  const native = isNativeShell();
  if (native && updateState.appVersion !== null) {
    if (updateState.appVersion === skipped()) return null;
    return {
      kind: "app",
      version: updateState.appVersion,
      url: updateState.daemon?.latest?.url ?? null,
    };
  }
  if (native && updateState.buildSkew) {
    return hostAlias === null
      ? { kind: "daemon-local" }
      : { kind: "daemon-remote", alias: hostAlias };
  }
  if (!native && updateState.daemon?.available && updateState.daemon.latest !== null) {
    const latest = updateState.daemon.latest;
    if (latest.version === skipped()) return null;
    return { kind: "release", version: latest.version, url: latest.url };
  }
  return null;
}
