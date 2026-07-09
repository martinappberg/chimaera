/**
 * The reactive settings store: a sparse user map (exactly what settings.json
 * holds) over the schema defaults. Components read through `getSetting()`
 * inside reactive contexts; plain-TS consumers (termPool) subscribe with
 * `onSettingsChange`. Writes go local-first, then a debounced PUT persists
 * the sparse map on the daemon, which broadcasts it back to every window
 * over /ws/events (`applyRemote`).
 *
 * Appearance (theme + accent) is applied to the document HERE, synchronously
 * with every change, so CSS variables are already correct when subscribers
 * (e.g. the terminal theme rebuild) read them.
 */

import { api } from "../net/api";
import {
  defaultValue,
  sanitize,
  settingDef,
  type SettingId,
  type SettingsMap,
} from "./schema";
import { defaultThemeFor, themeById, type ThemeDef } from "./themes";

const PUT_DEBOUNCE_MS = 400;

/** Sparse user map, exactly as stored in settings.json (unknown keys kept). */
let user = $state<Record<string, unknown>>({});
let loaded = $state(false);

const listeners = new Set<() => void>();
let putTimer: ReturnType<typeof setTimeout> | null = null;
/** True from the first local edit until its PUT lands: remote echoes of
 *  older state must not revert what the user is typing. */
let dirtySince: number | null = null;

/** The effective value of a setting: sanitized user value, else the default. */
export function getSetting<K extends SettingId>(id: K): SettingsMap[K] {
  const def = settingDef(id);
  if (def === undefined) return defaultValue(id);
  const raw = user[id];
  if (raw === undefined) return defaultValue(id);
  const clean = sanitize(def, raw);
  return (clean ?? defaultValue(id)) as SettingsMap[K];
}

/** True when the user explicitly set `id` (a "modified" row in the UI). */
export function isModified(id: string): boolean {
  return user[id] !== undefined;
}

/** True once the initial GET /settings resolved (or failed — defaults hold). */
export function settingsLoaded(): boolean {
  return loaded;
}

/** The raw sparse map (for the JSON editor). */
export function rawUserSettings(): Record<string, unknown> {
  return user;
}

/** Set a value; storing the default removes the key (VS Code semantics). */
export function setSetting<K extends SettingId>(id: K, value: SettingsMap[K]): void {
  const def = settingDef(id);
  if (def === undefined) return;
  const clean = sanitize(def, value);
  const next = { ...user };
  if (clean === null || JSON.stringify(clean) === JSON.stringify(def.default)) {
    delete next[id];
  } else {
    next[id] = clean;
  }
  applyLocal(next);
}

/** Clear a single setting back to its default. */
export function resetSetting(id: string): void {
  if (user[id] === undefined) return;
  const next = { ...user };
  delete next[id];
  applyLocal(next);
}

/** Replace the whole map (the JSON editor's save). */
export function replaceSettings(map: Record<string, unknown>): void {
  applyLocal({ ...map });
}

function applyLocal(next: Record<string, unknown>): void {
  user = next;
  dirtySince = Date.now();
  applyAppearance();
  notify();
  if (putTimer !== null) clearTimeout(putTimer);
  putTimer = setTimeout(() => {
    putTimer = null;
    void flushSettings();
  }, PUT_DEBOUNCE_MS);
}

/** Push the pending write now (also called on pagehide). */
export async function flushSettings(): Promise<void> {
  if (dirtySince === null) return;
  if (putTimer !== null) {
    clearTimeout(putTimer);
    putTimer = null;
  }
  const body = JSON.stringify(user);
  try {
    const res = await api("/settings", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body,
      keepalive: true,
    });
    // Only clear dirty when what we sent is still what we have — a keystroke
    // mid-flight keeps the guard up for the next flush.
    if (res.ok && JSON.stringify(user) === body) dirtySince = null;
  } catch {
    // daemon unreachable; the next change (or reconnect echo) retries
  }
}

/** A settings frame from /ws/events (including the echo of our own PUT). */
export function applyRemoteSettings(map: Record<string, unknown>): void {
  loaded = true;
  // Never clobber unsent local edits with an older broadcast.
  if (dirtySince !== null) return;
  if (JSON.stringify(map) === JSON.stringify(user)) return;
  user = map;
  applyAppearance();
  notify();
}

/** Initial load; the events socket keeps it fresh afterwards. */
export async function loadSettings(): Promise<void> {
  try {
    const res = await api("/settings");
    if (res.ok) {
      const body = (await res.json()) as { settings?: Record<string, unknown> };
      applyRemoteSettings(body.settings ?? {});
      return;
    }
  } catch {
    // unreachable daemon: defaults hold; the events socket will deliver
  } finally {
    loaded = true;
  }
}

/**
 * Imperative change notification for non-reactive consumers (termPool).
 * Fired after every applied change, local or remote, with appearance
 * already applied to the document.
 */
export function onSettingsChange(cb: () => void): () => void {
  listeners.add(cb);
  return () => listeners.delete(cb);
}

function notify(): void {
  for (const cb of listeners) cb();
}

// --- appearance: theme + accent applied at the document root ---------------

const systemDark =
  typeof matchMedia !== "undefined" ? matchMedia("(prefers-color-scheme: dark)") : null;

systemDark?.addEventListener("change", () => {
  if (getSetting("appearance.theme") === "system") {
    applyAppearance();
    notify();
  }
});

/** The mode actually in effect right now ("light" | "dark"). */
export function resolvedTheme(): "light" | "dark" {
  const pref = getSetting("appearance.theme");
  if (pref === "system") return (systemDark?.matches ?? false) ? "dark" : "light";
  return pref;
}

// $state.raw: swapped wholesale on theme change, so Svelte consumers (the
// accent swatch) track it while plain-TS consumers (termPool) just read it.
let activeThemeDef = $state.raw<ThemeDef>(defaultThemeFor("light"));

/** The full theme currently applied (termPool reads its ANSI palette). */
export function activeTheme(): ThemeDef {
  return activeThemeDef;
}

function applyAppearance(): void {
  const root = document.documentElement;
  const mode = resolvedTheme();
  const id = getSetting(mode === "dark" ? "appearance.darkTheme" : "appearance.lightTheme");
  const theme = themeById(id) ?? defaultThemeFor(mode);
  activeThemeDef = theme;
  // data-theme keeps carrying the MODE (color-scheme + the app.css fallback
  // blocks); the palette itself lands inline, every theme treated alike.
  root.dataset.theme = mode;
  for (const [name, value] of Object.entries(theme.tokens)) {
    root.style.setProperty(name, value);
  }
  const accent = getSetting("appearance.accentColor");
  if (accent !== "") root.style.setProperty("--accent", accent);
}

// Apply defaults immediately on module load so the first paint has a theme
// even before the daemon answers.
if (typeof document !== "undefined") applyAppearance();
