/**
 * The app's theme as a page Chimaera shows can wear it: a few of the
 * window's own tokens under stable names, which the page API
 * (`chimaera-core` `page_api.js`) sets on the page's root element as
 * `--ch-<name>` variables. Read off the window's computed style, so a
 * custom theme, the accent setting and light/dark all come through as they
 * are drawn; encoded once (base64url JSON) for the page's URL and for the
 * framed page's `theme` message.
 *
 * The names are a public contract for agents' pages (the chat host frame
 * names them): add a name freely, never rename or drop one.
 */

import { readable, type Readable } from "svelte/store";

/** Page name → the window's token it reads. */
export const PAGE_THEME_TOKENS: ReadonlyArray<readonly [name: string, token: string]> = [
  ["bg", "--bg"],
  ["fg", "--fg"],
  ["muted", "--muted"],
  ["accent", "--accent"],
  ["accent-text", "--accent-text"],
  ["edge", "--edge"],
  ["surface", "--rail-bg"],
  ["hover", "--hover-tint"],
  ["focus", "--focus-ring"],
  ["err", "--err"],
  ["warn", "--warn"],
  ["font", "--ui-font"],
  ["mono", "--mono"],
  ["text-sm", "--text-sm"],
  ["text-md", "--text-md"],
  ["text-lg", "--text-lg"],
  ["radius", "--radius-card"],
];

export interface PageTheme {
  m: "light" | "dark";
  v: Record<string, string>;
}

/** The theme as drawn now. A token the window doesn't define is left out. */
export function readPageTheme(root: HTMLElement = document.documentElement): PageTheme {
  const style = getComputedStyle(root);
  const v: Record<string, string> = {};
  for (const [name, token] of PAGE_THEME_TOKENS) {
    const value = style.getPropertyValue(token).trim();
    if (value !== "" && value.length <= 200) v[name] = value;
  }
  return { m: root.dataset.theme === "dark" ? "dark" : "light", v };
}

/** base64url (no padding) of the theme's JSON, as the page API decodes it. */
export function encodePageTheme(theme: PageTheme): string {
  const bytes = new TextEncoder().encode(JSON.stringify(theme));
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/**
 * The encoded theme, renewed when the window's root changes theme (the
 * appearance settings write its `data-theme` and inline tokens). Equal
 * encodings don't notify, so a root style change that leaves these tokens
 * alone reloads nothing.
 */
export const pageTheme: Readable<string> = readable("", (set) => {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  let queued = false;
  const read = (): void => {
    queued = false;
    set(encodePageTheme(readPageTheme(root)));
  };
  read();
  const observer = new MutationObserver(() => {
    if (queued) return;
    queued = true;
    queueMicrotask(read);
  });
  observer.observe(root, { attributes: true, attributeFilter: ["style", "data-theme", "class"] });
  return () => observer.disconnect();
});
