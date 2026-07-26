/**
 * The reactive settings store: a sparse user map (exactly what settings.json
 * holds) over the schema defaults. Components read through `getSetting()`
 * inside reactive contexts; plain-TS consumers (termPool) subscribe with
 * `onSettingsChange`. Writes go local-first, then a debounced PUT persists
 * the sparse map on the daemon, which broadcasts it back to every window
 * over /ws/events (`applyRemote`).
 *
 * Appearance (theme, accent, interface typography, and the editor font) is
 * applied to the document HERE, synchronously with every change, so CSS
 * variables are already correct when subscribers (e.g. the terminal theme
 * rebuild) read them.
 */

import { api } from "../net/api";
import {
  cacheAppearanceBootstrap,
  type AppearanceBootstrap,
} from "../net/native";
import {
  defaultValue,
  sanitize,
  settingDef,
  type SettingId,
  type SettingsMap,
} from "./schema";
import { defaultThemeFor, themeById, type ThemeDef } from "./themes";

const PUT_DEBOUNCE_MS = 400;
/** Keep in lockstep with the pre-module bootstrap in web-ui/index.html. */
const APPEARANCE_BOOTSTRAP_KEY = "chimaera.appearanceBootstrap.v1";

function isAppearanceBootstrap(
  value: Partial<AppearanceBootstrap> | null | undefined,
): value is AppearanceBootstrap {
  return (
    (value?.mode === "light" || value?.mode === "dark") &&
    typeof value.themeId === "string" &&
    typeof value.background === "string" &&
    (value.accent === null || typeof value.accent === "string")
  );
}

/** The last daemon-confirmed appearance carried by the native shell or saved
 *  on this origin. It bridges the short gap before `/settings` answers; the
 *  confirmed response always replaces it. */
function readAppearanceBootstrap(): AppearanceBootstrap | null {
  const carried = (
    globalThis as typeof globalThis & {
      __CHIMAERA_APPEARANCE_BOOTSTRAP__?: Partial<AppearanceBootstrap> | null;
    }
  ).__CHIMAERA_APPEARANCE_BOOTSTRAP__;
  if (isAppearanceBootstrap(carried)) return carried;
  try {
    const value = JSON.parse(localStorage.getItem(APPEARANCE_BOOTSTRAP_KEY) ?? "null") as
      | Partial<AppearanceBootstrap>
      | null;
    if (isAppearanceBootstrap(value)) return value;
  } catch {
    // Storage can be unavailable or from an older malformed build.
  }
  return null;
}

const appearanceBootstrap =
  typeof localStorage === "undefined" ? null : readAppearanceBootstrap();

/** Sparse user map, exactly as stored in settings.json (unknown keys kept). */
let user = $state<Record<string, unknown>>({});
let loaded = $state(false);
/** Whether a daemon-confirmed map has actually made it past the local-edit
 *  guard. `loaded` also becomes true after a failed GET so the rest of the UI
 *  can proceed with defaults; it therefore cannot identify the first
 *  authoritative appearance frame. */
let authoritativeSettingsApplied = false;

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
  const firstAuthoritativeMap = !authoritativeSettingsApplied;
  authoritativeSettingsApplied = true;
  if (JSON.stringify(map) === JSON.stringify(user)) {
    // The first confirmed empty/default map still needs to replace a stale
    // bootstrap cached by an older daemon setting.
    if (firstAuthoritativeMap) applyAppearance();
    return;
  }
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

// --- appearance: theme + typography applied at the document root -----------

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
  if (!loaded && dirtySince === null && appearanceBootstrap !== null) {
    return appearanceBootstrap.mode;
  }
  const pref = getSetting("appearance.theme");
  if (pref === "system") return (systemDark?.matches ?? false) ? "dark" : "light";
  return pref;
}

// $state.raw: swapped wholesale on theme change, so Svelte consumers (the
// accent swatch) track it while plain-TS consumers (termPool) just read it.
const initialThemeDef = defaultThemeFor("light");
let activeThemeDef = $state.raw<ThemeDef>(initialThemeDef);
let appliedAppearance: AppearanceBootstrap = {
  mode: "light",
  themeId: initialThemeDef.id,
  background: initialThemeDef.tokens["--bg"],
  accent: null,
};
let lastNativeAppearance = "";

/** The full theme currently applied (termPool reads its ANSI palette). */
export function activeTheme(): ThemeDef {
  return activeThemeDef;
}

/** Snapshot carried in native re-home URLs before the next origin paints. */
export function appearanceBootstrapForNavigation(): AppearanceBootstrap {
  return { ...appliedAppearance };
}

function applyAppearance(): void {
  const root = document.documentElement;
  const mode = resolvedTheme();
  const bootstrapping = !loaded && dirtySince === null && appearanceBootstrap !== null;
  const id = bootstrapping
    ? appearanceBootstrap.themeId
    : getSetting(mode === "dark" ? "appearance.darkTheme" : "appearance.lightTheme");
  const theme = themeById(id) ?? defaultThemeFor(mode);
  activeThemeDef = theme;
  // data-theme keeps carrying the MODE (color-scheme + the app.css fallback
  // blocks); the palette itself lands inline, every theme treated alike.
  root.dataset.theme = mode;
  // The pre-module bootstrap sets this inline to prevent a light native
  // scrollbar flash. Keep that higher-specificity declaration synchronized
  // once daemon settings (or a live user change) resolve a different mode.
  root.style.colorScheme = mode;
  for (const [name, value] of Object.entries(theme.tokens)) {
    root.style.setProperty(name, value);
  }
  const accent = bootstrapping
    ? (appearanceBootstrap.accent ?? "")
    : getSetting("appearance.accentColor");
  appliedAppearance = {
    mode,
    themeId: theme.id,
    background: theme.tokens["--bg"],
    accent: accent === "" ? null : accent,
  };
  // Always replace the bootstrap value. An empty user setting means the
  // selected theme's accent, not "leave whatever inline custom accent the
  // bootstrap installed". Themes live inline too, so removing the property
  // would incorrectly fall back to app.css instead of the selected palette.
  root.style.setProperty("--accent", accent === "" ? theme.tokens["--accent"] : accent);
  // The HTML head reads this before the JavaScript module graph on the next
  // navigation/window creation. Do not overwrite a real saved choice during
  // the module-load defaults pass; wait for a daemon-confirmed map or a local
  // edit.
  if (loaded || dirtySince !== null) {
    const serialized = JSON.stringify(appliedAppearance);
    try {
      localStorage.setItem(APPEARANCE_BOOTSTRAP_KEY, serialized);
    } catch {
      // Private/restricted storage: the system fallback in index.html holds.
    }
    if (serialized !== lastNativeAppearance) {
      lastNativeAppearance = serialized;
      void cacheAppearanceBootstrap(appliedAppearance).catch(() => {
        // A browser has no shell, and a shell write failure must not disturb
        // the live theme; this cache is only the next document's first paint.
      });
    }
  }

  // One application-wide interface scale. Components consume only the four
  // --text-* tokens; updating them here makes a settings edit apply to every
  // mounted surface (including lazy views) without a reload. The deliberately
  // small two-pixel spread keeps labels subordinate without returning to the
  // 9–11px chrome that prompted this setting.
  const ui = getSetting("appearance.interfaceFontSize");
  root.style.setProperty("--text-xs", `${Math.max(9, ui - 2)}px`);
  root.style.setProperty("--text-sm", `${Math.max(10, ui - 1)}px`);
  root.style.setProperty("--text-md", `${ui}px`);
  root.style.setProperty("--text-lg", `${ui + 2}px`);

  const uiFont = getSetting("appearance.interfaceFontFamily").trim();
  if (uiFont === "") root.style.removeProperty("--ui-font-custom");
  else root.style.setProperty("--ui-font-custom", uiFont);

  const editorFont = getSetting("editor.fontFamily").trim();
  if (editorFont === "") root.style.removeProperty("--editor-font-custom");
  else root.style.setProperty("--editor-font-custom", editorFont);
}

// Apply defaults immediately on module load so the first paint has a theme
// even before the daemon answers.
if (typeof document !== "undefined") applyAppearance();
