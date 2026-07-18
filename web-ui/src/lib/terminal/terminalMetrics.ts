import { getSetting } from "../settings/store.svelte";

export const BASE_FONT_SIZE = 13.5;
const PAD_X = 22;
const PAD_Y = 24;

export const FONT_FAMILY =
  '"JetBrains Mono", ui-monospace, "SF Mono", SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace';

/** The settings-resolved default font size (per-pane overrides layer on top). */
export function baseFontSize(): number {
  return getSetting("terminal.fontSize");
}

/** The settings-resolved font stack (a custom face falls back to bundled). */
export function fontFamily(): string {
  const custom = getSetting("terminal.fontFamily").trim();
  return custom === "" ? FONT_FAMILY : `"${custom.replaceAll('"', "")}", ${FONT_FAMILY}`;
}

let cellCache: { key: string; w: number; h: number } | null = null;

/** Measure one cell exactly like xterm without loading the xterm runtime. */
function cellDims(): { w: number; h: number } {
  const size = baseFontSize();
  const family = fontFamily();
  const lineHeight = getSetting("terminal.lineHeight");
  const key = `${size}|${lineHeight}|${family}`;
  if (cellCache !== null && cellCache.key === key) return cellCache;
  const probe = document.createElement("span");
  probe.textContent = "W";
  probe.style.cssText = `position:absolute;visibility:hidden;white-space:pre;font:400 ${size}px ${family};line-height:normal;`;
  document.body.appendChild(probe);
  const rect = probe.getBoundingClientRect();
  probe.remove();
  const dims = {
    key,
    w: rect.width > 1 ? rect.width : size * 0.6,
    h: rect.height > 1 ? rect.height * lineHeight : Math.round(size * 1.32 * lineHeight),
  };
  if (document.fonts.check(`400 ${size}px "JetBrains Mono"`)) cellCache = dims;
  return dims;
}

/** Estimate a session's initial grid without pulling xterm into App's entry. */
export function estimateSize(el: HTMLElement): { cols: number; rows: number } {
  const { w, h } = cellDims();
  const cols = Math.floor((el.clientWidth - PAD_X) / w);
  const rows = Math.floor((el.clientHeight - PAD_Y) / h);
  return {
    cols: Math.min(Math.max(cols, 20), 500),
    rows: Math.min(Math.max(rows, 5), 200),
  };
}
