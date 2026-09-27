/**
 * Update awareness, merged into one offer — and one answer when asked.
 *
 * Three signals arrive on their own schedules — the daemon's release check
 * (`update` frame on /ws/events, any window), the shell's signed-app check
 * (`app-update` event + `app_update_status`, native only), and daemon/app
 * build skew (health build vs shell build, native only) — and this store
 * reduces them to at most ONE offer for the toast, by how much a click can
 * actually fix here: the full app+daemon chain beats a daemon-only restart
 * beats a "new release exists" notice (browser windows, where applying is
 * the CLI's or the app's job).
 *
 * An explicit check ("Check for Updates…", the home screen's version stamp)
 * always gets an answer, "you're up to date" and "couldn't check" included,
 * and it outranks snooze/skip: the user asked. Every source reports a failed
 * check as a failure — never as "no update".
 *
 * Snooze/skip live in localStorage: windows on the same daemon share an
 * origin, so dismissing once quiets every window there.
 */

import { api } from "../net/api";
import { appUpdateStatus, isNativeShell, type AppUpdateStatus } from "../net/native";

/** The daemon's release knowledge (GET /api/v1/update, the `update` frame). */
export interface UpdateStatus {
  current: string;
  build: string | null;
  /** A dev build: release checks don't apply (it is never "outdated"). */
  dev: boolean;
  /** One word for "is there an update?", decided by the daemon. */
  state: "unchecked" | "current" | "available" | "failed";
  available: boolean;
  latest: { version: string; url: string; published_at?: string | null } | null;
  /** Last attempt / last success, unix seconds. */
  checked_at: number | null;
  succeeded_at: number | null;
  /** Why the last attempt failed, in plain words; null after a success. */
  error: string | null;
  /** The periodic cadence ("checks every 6 hours"). */
  interval_secs: number;
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

/** The answer to an explicit check when there is nothing to offer. */
export type UpdateAnswer =
  | { kind: "checking" }
  /** Nothing to install. `pending`: a newer release the daemon has seen that
   *  the app's signed channel doesn't offer yet (so "newest" would be false). */
  | { kind: "current"; version: string; pending?: string | null }
  | { kind: "dev" }
  | { kind: "failed"; error: string };

export type UpdateNotice = UpdateOffer | UpdateAnswer;

const SKIP_KEY = "chimaera.update.skip";
const SNOOZE_KEY = "chimaera.update.snooze";
/** "Later" quiets the toast for ~20h — under a day, so it returns tomorrow. */
const SNOOZE_MS = 20 * 60 * 60 * 1000;

const STATES = new Set(["unchecked", "current", "available", "failed"]);

/** Raw signals; each arrives from its own listener. */
export const updateState = $state({
  /** Daemon's own release knowledge (ws frame / GET /update). */
  daemon: null as UpdateStatus | null,
  /** Newer signed app version (native `app-update` event / status). */
  appVersion: null as string | null,
  /** The shell's whole signed-update answer (native; null in a browser). */
  app: null as AppUpdateStatus | null,
  /** This window's daemon build differs from the app build (native). */
  buildSkew: false,
  /** The ssh scope this window reaches its daemon through (null = local);
   *  set once by App, read by the Settings panel's update actions. */
  scope: null as string | null,
  /** Bumped on snooze/skip so the derived offer re-evaluates. */
  dismissedAt: 0,
  /**
   * An explicit check from this window: null when nobody asked, "checking"
   * while it runs, then "answered" until the answer is dismissed.
   */
  asked: null as null | "checking" | "answered",
  /** The daemon could not be asked at all during the last explicit check. */
  askError: null as string | null,
});

function num(v: unknown): number | null {
  return typeof v === "number" && Number.isFinite(v) ? v : null;
}

function str(v: unknown): string | null {
  return typeof v === "string" ? v : null;
}

/** Validate a daemon status payload (HTTP body or ws frame). */
export function parseUpdateStatus(raw: unknown): UpdateStatus | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as Record<string, unknown>;
  if (typeof r.available !== "boolean") return null;
  const latestRaw = r.latest as Record<string, unknown> | null | undefined;
  const latest =
    typeof latestRaw === "object" &&
    latestRaw !== null &&
    typeof latestRaw.version === "string" &&
    typeof latestRaw.url === "string"
      ? {
          version: latestRaw.version,
          url: latestRaw.url,
          published_at: str(latestRaw.published_at),
        }
      : null;
  const state = typeof r.state === "string" && STATES.has(r.state) ? r.state : null;
  return {
    current: str(r.current) ?? "",
    build: str(r.build),
    dev: r.dev === true,
    state: (state ?? (r.available ? "available" : latest !== null ? "current" : "unchecked")) as UpdateStatus["state"],
    available: r.available,
    latest,
    checked_at: num(r.checked_at),
    succeeded_at: num(r.succeeded_at),
    error: str(r.error),
    interval_secs: num(r.interval_secs) ?? 6 * 60 * 60,
  };
}

/** Record the shell's signed-update answer (native). */
export function applyAppStatus(status: AppUpdateStatus): void {
  updateState.app = status;
  // A failed re-check keeps the last known offer (the release still exists).
  if (status.available !== null) updateState.appVersion = status.available;
  else if (status.error === null) updateState.appVersion = null;
}

/** The daemon bounds its own fetch (curl, 10s) and may wait out one check
 *  already running; past this, the tunnel is the problem, not GitHub. */
const DAEMON_ASK_MS = 30_000;

async function fetchDaemonStatus(refresh: boolean): Promise<UpdateStatus | null> {
  const res = await api(`/update${refresh ? "?refresh=true" : ""}`, {
    signal: AbortSignal.timeout(DAEMON_ASK_MS),
  });
  if (!res.ok) throw new Error(`the daemon answered ${res.status}`);
  return parseUpdateStatus(await res.json());
}

function failureText(e: unknown): string {
  if (e instanceof Error && e.name === "TimeoutError") return "the daemon did not answer in time";
  return e instanceof Error ? e.message : String(e);
}

let inFlight: Promise<void> | null = null;

/**
 * Ask every source now: the daemon's release check and, in the app, the
 * signed-update endpoint. `announce` makes the toast answer (the menu item,
 * the version stamp); the Settings panel answers inline instead. Concurrent
 * calls share one round.
 */
export function checkForUpdates(announce: boolean): Promise<void> {
  if (announce) {
    updateState.asked = "checking";
    updateState.askError = null;
  }
  if (inFlight !== null) return inFlight;
  const answer = (): void => {
    if (updateState.asked === "checking") updateState.asked = "answered";
  };
  const round = (async () => {
    const native = isNativeShell();
    const daemon = fetchDaemonStatus(true).then(
      (status) => {
        if (status !== null) updateState.daemon = status;
        updateState.askError = null;
      },
      (e: unknown) => {
        updateState.askError = failureText(e);
      },
    );
    const app = native
      ? appUpdateStatus(true).then(
          (status) => {
            if (status !== null) applyAppStatus(status);
          },
          (e: unknown) => {
            // Keep what the shell knew, marked as a failed re-check.
            const known = updateState.app;
            if (known !== null) applyAppStatus({ ...known, error: failureText(e) });
          },
        )
      : Promise.resolve();
    // In the app the signed channel decides the answer: don't hold it for
    // the daemon's release check (a slow curl on an air-gapped host).
    await (native ? app : daemon);
    answer();
    await Promise.all([daemon, app]);
    // An announcing call that joined after the early answer.
    answer();
  })();
  inFlight = round.finally(() => {
    inFlight = null;
  });
  return inFlight;
}

/** Put the explicit check's answer away (its close button, its timeout). */
export function dismissAnswer(): void {
  updateState.asked = null;
}

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
  updateState.asked = null;
  updateState.dismissedAt = Date.now();
}

/** Never offer `version` again (a "skip this version" click). */
export function skipUpdateVersion(version: string): void {
  try {
    localStorage.setItem(SKIP_KEY, version);
  } catch {
    // Same as snooze: failure to persist only means re-offering.
  }
  updateState.asked = null;
  updateState.dismissedAt = Date.now();
}

/**
 * The one offer worth making right now, or null. `hostAlias` is the ssh
 * alias this window reaches its daemon through (null = the local daemon).
 * `asked` bypasses snooze/skip: an explicit check shows what it found.
 */
export function currentOffer(hostAlias: string | null, asked = false): UpdateOffer | null {
  // Touch the dismissal marker so runes re-derive after snooze/skip.
  void updateState.dismissedAt;
  if (!asked && Date.now() < snoozedUntil()) return null;

  const native = isNativeShell();
  if (native && updateState.appVersion !== null) {
    if (!asked && updateState.appVersion === skipped()) return null;
    // Release notes only when the daemon's release IS this version.
    const latest = updateState.daemon?.latest;
    return {
      kind: "app",
      version: updateState.appVersion,
      url: latest?.version === updateState.appVersion ? latest.url : null,
    };
  }
  if (native && updateState.buildSkew) {
    return hostAlias === null
      ? { kind: "daemon-local" }
      : { kind: "daemon-remote", alias: hostAlias };
  }
  if (!native && updateState.daemon?.available && updateState.daemon.latest !== null) {
    const latest = updateState.daemon.latest;
    if (!asked && latest.version === skipped()) return null;
    return { kind: "release", version: latest.version, url: latest.url };
  }
  return null;
}

/**
 * What an explicit check found when there is nothing to offer. The source
 * that decides is the one that can actually update this window: the app's
 * signed channel in the native shell, the daemon's release check in a
 * browser.
 */
function currentAnswer(): UpdateAnswer {
  if (isNativeShell()) {
    const app = updateState.app;
    if (app === null) return { kind: "failed", error: "the app did not answer" };
    if (app.dev) return { kind: "dev" };
    if (app.error !== null) return { kind: "failed", error: app.error };
    const daemon = updateState.daemon;
    const pending =
      daemon !== null &&
      !daemon.dev &&
      daemon.state === "available" &&
      daemon.latest !== null &&
      daemon.latest.version !== app.current
        ? daemon.latest.version
        : null;
    return { kind: "current", version: app.current, pending };
  }
  if (updateState.askError !== null) return { kind: "failed", error: updateState.askError };
  const daemon = updateState.daemon;
  if (daemon === null) return { kind: "failed", error: "the daemon did not answer" };
  if (daemon.dev) return { kind: "dev" };
  if (daemon.state === "failed" || daemon.state === "unchecked") {
    return { kind: "failed", error: daemon.error ?? "the check did not finish" };
  }
  return { kind: "current", version: daemon.current };
}

/** The toast's content right now: an offer, an explicit check's answer, or null. */
export function currentNotice(hostAlias: string | null): UpdateNotice | null {
  const asked = updateState.asked;
  if (asked === "checking") return { kind: "checking" };
  const offer = currentOffer(hostAlias, asked === "answered");
  if (offer !== null) return offer;
  return asked === "answered" ? currentAnswer() : null;
}
