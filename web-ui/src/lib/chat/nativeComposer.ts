import type { UiRecord } from "./nativeUi";

export interface NativeComposer {
  nativeRead(): { text: string; cursor: number };
  nativeFill(text: string, mode: string, decorations?: unknown): boolean;
  nativeSuggest(text: string): boolean;
}
export interface NativeComposerHost { composer?: NativeComposer; canEdit: boolean; focused: boolean }

export interface NativeComposerKey { key: string; ctrl?: true; shift?: true; meta?: true }
export function composerKey(event: { key: string; ctrlKey: boolean; shiftKey: boolean; metaKey: boolean; altKey: boolean; isComposing?: boolean; keyCode?: number }): NativeComposerKey | undefined {
  if (event.isComposing || event.keyCode === 229 || ["Dead", "Process", "Unidentified", "Shift", "Control", "Meta", "Alt"].includes(event.key)) return undefined;
  const names: Record<string, string> = { " ": "space", ArrowUp: "up", ArrowDown: "down", ArrowLeft: "left", ArrowRight: "right", Enter: "return", Tab: "tab", Backspace: "backspace", Delete: "delete", PageUp: "pageup", PageDown: "pagedown", Home: "home", End: "end", Escape: "escape" };
  const key = names[event.key] ?? event.key;
  if (!key.length || key.length > 32) return undefined;
  return { key, ...(event.ctrlKey ? { ctrl: true as const } : {}), ...(event.shiftKey ? { shift: true as const } : {}), ...(event.metaKey || event.altKey ? { meta: true as const } : {}) };
}

/** Paste, IME and folded edits have no single key in the native protocol. */
export function composerInputKey(key: NativeComposerKey | undefined, event: { inputType?: string; isComposing?: boolean }, coalesced: boolean): NativeComposerKey | undefined {
  if (coalesced || event.isComposing || !event.inputType || !/^(insertText|insertLineBreak|delete(Content|Word|SoftLine|HardLine)(Backward|Forward))$/.test(event.inputType)) return undefined;
  return key;
}

/** Fill runs describe the inserted text, while the textarea paints the whole draft. */
export function fillComposerDraft(draft: string, text: string, mode: string, selectionStart: number, selectionEnd: number, decorations?: unknown): { text: string; cursor: number; decorations: UiRecord[] } | null {
  if (!["replace", "append", "insert"].includes(mode) || text.length > 64_000) return null;
  const clamp = (position: number) => Number.isFinite(position) ? Math.max(0, Math.min(draft.length, Math.trunc(position))) : draft.length;
  const start = mode === "append" ? draft.length : mode === "insert" ? clamp(selectionStart) : 0;
  const end = mode === "append" ? draft.length : mode === "insert" ? Math.max(start, clamp(selectionEnd)) : draft.length;
  const next = draft.slice(0, start) + text + draft.slice(end);
  if (next.length > 64_000) return null;
  const runs = Array.isArray(decorations) ? decorations.slice(0, 128).flatMap((value) => {
    if (!value || typeof value !== "object" || !Number.isInteger(value.start) || !Number.isInteger(value.end)) return [];
    const from = Math.max(0, Math.min(text.length, value.start));
    const to = Math.max(0, Math.min(text.length, value.end));
    return from < to ? [{ ...value, start: start + from, end: start + to }] : [];
  }) : [];
  return { text: next, cursor: start + text.length, decorations: runs };
}

/** Range merging is paint-only, bounded, and never changes the user's text. */
export function decorationRuns(text: string, decorations: unknown): { text: string; props: UiRecord }[] {
  if (!Array.isArray(decorations) || !decorations.length) return [{ text, props: {} }];
  const boundaries = new Set([0, text.length]);
  const graphemes = [...new Intl.Segmenter(undefined, { granularity: "grapheme" }).segment(text)].map((part) => part.index);
  graphemes.push(text.length);
  const snap = (position: number) => { let low = 0, high = graphemes.length; while (low + 1 < high) { const mid = (low + high) >> 1; if (graphemes[mid] <= position) low = mid; else high = mid; } return graphemes[low]; };
  const runs = decorations.slice(0, 128).flatMap((value) => {
    if (!value || typeof value !== "object" || !Number.isInteger(value.start) || !Number.isInteger(value.end)) return [];
    const start = snap(Math.max(0, Math.min(text.length, value.start)));
    const end = snap(Math.max(0, Math.min(text.length, value.end)));
    if (start >= end) return [];
    boundaries.add(start); boundaries.add(end);
    return [{ start, end, props: value as UiRecord }];
  });
  const points = [...boundaries].sort((a, b) => a - b);
  return points.slice(0, -1).map((start, index) => {
    const props: UiRecord = {};
    for (const run of runs) if (run.start <= start && run.end > start) {
      for (const key of ["color", "backgroundColor", "dimColor", "bold", "italic", "underline", "strikethrough"]) if (run.props[key] !== undefined) props[key] = run.props[key];
    }
    return { text: text.slice(start, points[index + 1]), props };
  });
}
