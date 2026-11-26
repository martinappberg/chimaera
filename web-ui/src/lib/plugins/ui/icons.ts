/**
 * The icons a plugin can name: a `ui/1` `icon` node, a button's or a
 * segmented option's `icon`, and a `[[views]]` entry's `icon` (the glyph of
 * its tab). Semantic names, Tabler strokes on a 24-unit box; a name outside
 * the set draws `UNKNOWN_ICON` in a screen and the Extensions glyph on a tab.
 * The set is part of the plugin interface: add names, never rename one.
 */
export const PLUGIN_ICONS: Record<string, string[]> = {
  check: ["M5 12l5 5l10 -10"],
  x: ["M18 6l-12 12", "M6 6l12 12"],
  alert: ["M12 9v4", "M12 16v.01", "M10.363 3.591l-8.106 13.534a1.914 1.914 0 0 0 1.636 2.871h16.214a1.914 1.914 0 0 0 1.636 -2.87l-8.106 -13.536a1.914 1.914 0 0 0 -3.274 0"],
  info: ["M12 9h.01", "M11 12h1v4h1", "M3 12a9 9 0 1 0 18 0a9 9 0 0 0 -18 0"],
  clock: ["M3 12a9 9 0 1 0 18 0a9 9 0 0 0 -18 0", "M12 7v5l3 3"],
  file: ["M14 3v4a1 1 0 0 0 1 1h4", "M17 21h-10a2 2 0 0 1 -2 -2v-14a2 2 0 0 1 2 -2h7l5 5v11a2 2 0 0 1 -2 2"],
  folder: ["M5 4h4l3 3h7a2 2 0 0 1 2 2v8a2 2 0 0 1 -2 2h-14a2 2 0 0 1 -2 -2v-11a2 2 0 0 1 2 -2"],
  play: ["M7 4v16l13 -8z"],
  stop: ["M5 5h14v14h-14z"],
  refresh: ["M20 11a8.1 8.1 0 0 0 -15.5 -2m-.5 -4v4h4", "M4 13a8.1 8.1 0 0 0 15.5 2m.5 4v-4h-4"],
  download: ["M4 17v2a2 2 0 0 0 2 2h12a2 2 0 0 0 2 -2v-2", "M7 11l5 5l5 -5", "M12 4l0 12"],
  external: ["M12 6h-6a2 2 0 0 0 -2 2v10a2 2 0 0 0 2 2h10a2 2 0 0 0 2 -2v-6", "M11 13l9 -9", "M15 4h5v5"],
  grid: ["M4 4h6v6h-6z", "M14 4h6v6h-6z", "M4 14h6v6h-6z", "M14 14h6v6h-6z"],
  list: ["M9 6l11 0", "M9 12l11 0", "M9 18l11 0", "M5 6l0 .01", "M5 12l0 .01", "M5 18l0 .01"],
  book: ["M3 19a9 9 0 0 1 9 0a9 9 0 0 1 9 0", "M3 6a9 9 0 0 1 9 0a9 9 0 0 1 9 0", "M3 6l0 13", "M12 6l0 13", "M21 6l0 13"],
  bolt: ["M13 3l0 7l6 0l-8 11l0 -7l-6 0l8 -11"],
  settings: ["M10.325 4.317c.426 -1.756 2.924 -1.756 3.35 0a1.724 1.724 0 0 0 2.573 1.066c1.543 -.94 3.31 .826 2.37 2.37a1.724 1.724 0 0 0 1.065 2.572c1.756 .426 1.756 2.924 0 3.35a1.724 1.724 0 0 0 -1.066 2.573c.94 1.543 -.826 3.31 -2.37 2.37a1.724 1.724 0 0 0 -2.572 1.065c-.426 1.756 -2.924 1.756 -3.35 0a1.724 1.724 0 0 0 -2.573 -1.066c-1.543 .94 -3.31 -.826 -2.37 -2.37a1.724 1.724 0 0 0 -1.065 -2.572c-1.756 -.426 -1.756 -2.924 0 -3.35a1.724 1.724 0 0 0 1.066 -2.573c-.94 -1.543 .826 -3.31 2.37 -2.37c1 .608 2.296 .07 2.572 -1.065", "M9 12a3 3 0 1 0 6 0a3 3 0 0 0 -6 0"],
  search: ["M10 10m-7 0a7 7 0 1 0 14 0a7 7 0 1 0 -14 0", "M21 21l-6 -6"],
  slides: ["M3 4l18 0", "M4 4v10a2 2 0 0 0 2 2h12a2 2 0 0 0 2 -2v-10", "M12 16l0 4", "M9 20l6 0", "M8 12l3 -3l2 2l3 -3"],
};

/** A dot: what a screen draws for a name it does not know. */
export const UNKNOWN_ICON = ["M12 12m-2 0a2 2 0 1 0 4 0a2 2 0 1 0 -4 0"];

/** The paths of a named icon, or null when the set has no such name. */
export function pluginIcon(name: unknown): string[] | null {
  // Not `Object.hasOwn`: the app's web view may predate it.
  return typeof name === "string" && Object.prototype.hasOwnProperty.call(PLUGIN_ICONS, name) ? PLUGIN_ICONS[name] : null;
}
