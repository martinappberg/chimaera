/**
 * Client-side interaction model for BoardView: the identity/geometry parse,
 * corner-resize math, the actor-aware undo stack (board-plan §6.7), and the
 * external-edit attribution diff (§6.5).
 *
 * Pure data and plain classes only — the component owns every piece of
 * `$state`. Nothing here is layout truth: values are the file's own literal
 * numbers, and every mutation still routes through POST /board/edit.
 */

/** The design grid (mirrors the daemon's normalize()); client snaps match it
 *  exactly so an optimistic value never disagrees with the written file. */
export const GRID_PT = 8;
/** Client floor for a handle resize. Stricter than the daemon's 8 pt extent
 *  floor, so a handle can never collapse an object to a sliver. */
export const MIN_RESIZE_PT = 16;

export function snap8(v: number): number {
  return Math.round(v / GRID_PT) * GRID_PT;
}

// --- parsed identity/geometry (never layout truth) -------------------------

export interface ObjInfo {
  id: string;
  kind: string;
  at: [number, number] | null;
  size: [number, number] | null;
  /** Plain-text paragraphs for the kinds the /board/edit text op accepts
   *  (text/shape); null means "this kind carries no editable text". A rich
   *  (styled-run) paragraph is projected to its plain text — the same
   *  flattening the edit op applies by design, so what the editor seeds is
   *  exactly what an unchanged commit would write. */
  text: string[] | null;
}
export interface PageInfo {
  id: string;
  notes: string | null;
  objects: ObjInfo[];
}
export interface BoardInfo {
  title: string | null;
  canvas: [number, number];
  pages: PageInfo[];
}

/** Parse a board chunk for identity and geometry only (plus presenter notes). */
export function parseBoard(bytes: Uint8Array): BoardInfo | null {
  try {
    const raw = JSON.parse(new TextDecoder().decode(bytes)) as {
      title?: string;
      canvas?: { size?: [number, number] };
      pages?: {
        id?: string;
        notes?: string;
        objects?: {
          id?: string;
          type?: string;
          at?: [number, number];
          size?: [number, number];
          text?: unknown;
        }[];
      }[];
    };
    return {
      title: raw.title ?? null,
      canvas: raw.canvas?.size ?? [960, 540],
      pages: (raw.pages ?? []).map((p, i) => ({
        id: p.id ?? `page-${i + 1}`,
        notes: typeof p.notes === "string" ? p.notes : null,
        objects: (p.objects ?? []).map((o) => ({
          id: o.id ?? "",
          kind: o.type ?? "?",
          at: Array.isArray(o.at) ? [o.at[0], o.at[1]] : null,
          size: Array.isArray(o.size) ? [o.size[0], o.size[1]] : null,
          text: editableText(o.type, o.text),
        })),
      })),
    };
  } catch {
    return null;
  }
}

// --- in-place text editing (the /board/edit text op's client half) ----------

/**
 * Plain-text paragraph projection for the kinds the edit route's text op
 * accepts — `text` and `shape` only (the daemon bails for anything else, so
 * offering the editor elsewhere would just surface that error). A paragraph
 * is either a bare string or `{runs: [{t}, …]}`; runs concatenate to their
 * plain text. A text-bearing object with no `text` field is an empty editor,
 * not an ineligible one. Null = the kind carries no editable text.
 */
export function editableText(kind: string | undefined, raw: unknown): string[] | null {
  if (kind !== "text" && kind !== "shape") return null;
  if (!Array.isArray(raw)) return [];
  return raw.map((p) => {
    if (typeof p === "string") return p;
    const runs = (p as { runs?: unknown } | null)?.runs;
    if (Array.isArray(runs)) {
      return runs.map((r) => (typeof (r as { t?: unknown })?.t === "string" ? (r as { t: string }).t : "")).join("");
    }
    return "";
  });
}

/** Editor seed: paragraphs are newline-joined, matching the text op exactly. */
export function paragraphsToEditorText(paras: string[]): string {
  return paras.join("\n");
}

/**
 * The commit half of the round trip: newline-split back into plain
 * paragraphs (CR/CRLF normalized — a paste can carry them even though the
 * textarea itself never does). A fully empty editor means "no paragraphs",
 * not one empty paragraph, so clearing a shape's text writes `[]`.
 */
export function editorTextToParagraphs(text: string): string[] {
  const normalized = text.replace(/\r\n?/g, "\n");
  if (normalized === "") return [];
  return normalized.split("\n");
}

/** Exact paragraph equality — the no-change-commit-is-a-no-op gate. */
export function sameParagraphs(a: string[], b: string[]): boolean {
  return a.length === b.length && a.every((s, i) => s === b[i]);
}

/**
 * Approximate editor font-size in stage px from the object's own box: the
 * box height split across the seeded line count, at ~62% of the line slot
 * (a typical line-height's glyph share), clamped to a sane pt band. An
 * approximation on purpose — layout truth (real roles, wrap, rich runs)
 * stays server-side; this only keeps the overlay visually near the pixels
 * underneath. Never measures the DOM.
 */
export function editorFontPx(sizePt: [number, number], ptScale: number, lineCount: number): number {
  const linePt = sizePt[1] / Math.max(1, lineCount);
  const fontPt = Math.min(44, Math.max(9, linePt * 0.62));
  return Math.max(11, Math.round(fontPt * ptScale));
}

// --- selection-as-deixis (§6.4) ---------------------------------------------

/**
 * The compact context line injected into the chat composer:
 * `[board: figures/fig2.board.json › results › callout, arrow-1] `.
 * Trailing space, never a newline — same never-submits contract as every
 * reference composer (see shared/reference.ts).
 */
export function composeBoardContext(relPath: string, pageId: string, ids: string[]): string {
  return `[board: ${relPath} › ${pageId} › ${ids.join(", ")}] `;
}

/** Snapshot padding: small enough that the crop reads as the objects. */
export const SNAPSHOT_PAD_PT = 8;

/**
 * The union of the selected frames padded by `padPt`, clamped to the canvas —
 * the region cropped out of the server's page render for the deixis
 * attachment. Null when nothing carries a frame.
 */
export function snapshotRegion(
  frames: Frame[],
  canvas: [number, number],
  padPt = SNAPSHOT_PAD_PT,
): Frame | null {
  if (frames.length === 0) return null;
  let x0 = Infinity;
  let y0 = Infinity;
  let x1 = -Infinity;
  let y1 = -Infinity;
  for (const f of frames) {
    x0 = Math.min(x0, f.at[0]);
    y0 = Math.min(y0, f.at[1]);
    x1 = Math.max(x1, f.at[0] + f.size[0]);
    y1 = Math.max(y1, f.at[1] + f.size[1]);
  }
  const left = Math.max(0, x0 - padPt);
  const top = Math.max(0, y0 - padPt);
  const right = Math.min(canvas[0], x1 + padPt);
  const bottom = Math.min(canvas[1], y1 + padPt);
  if (right <= left || bottom <= top) return null;
  return { at: [left, top], size: [right - left, bottom - top] };
}

export interface Frame {
  at: [number, number];
  size: [number, number];
}

/** Float-tolerant tuple equality (values are 8 pt multiples in practice). */
export function samePair(a: [number, number], b: [number, number]): boolean {
  return Math.abs(a[0] - b[0]) < 1e-6 && Math.abs(a[1] - b[1]) < 1e-6;
}

/** id → frame for the objects that have full geometry. */
export function pageFrames(objects: ObjInfo[]): Map<string, Frame> {
  const m = new Map<string, Frame>();
  for (const o of objects) {
    if (o.at !== null && o.size !== null) m.set(o.id, { at: o.at, size: o.size });
  }
  return m;
}

/** Whole-board frames, for undo staleness checks across page navigation. */
export function boardFrames(board: BoardInfo): Map<string, Frame> {
  const m = new Map<string, Frame>();
  for (const p of board.pages) {
    for (const [id, f] of pageFrames(p.objects)) m.set(id, f);
  }
  return m;
}

// --- corner resize ---------------------------------------------------------

export type Corner = "nw" | "ne" | "sw" | "se";
export const CORNERS: Corner[] = ["nw", "ne", "sw", "se"];

/**
 * Frame after dragging `corner` by (dx,dy) pt with the opposite corner
 * anchored. Each axis floors at MIN_RESIZE_PT by pinning the moving edge, so
 * the anchor never shifts.
 */
export function resizeFrame(
  corner: Corner,
  origAt: [number, number],
  origSize: [number, number],
  dx: number,
  dy: number,
): Frame {
  let [x, y] = origAt;
  let [w, h] = origSize;
  if (corner === "ne" || corner === "se") {
    w = Math.max(MIN_RESIZE_PT, origSize[0] + dx);
  } else {
    w = Math.max(MIN_RESIZE_PT, origSize[0] - dx);
    x = origAt[0] + origSize[0] - w;
  }
  if (corner === "sw" || corner === "se") {
    h = Math.max(MIN_RESIZE_PT, origSize[1] + dy);
  } else {
    h = Math.max(MIN_RESIZE_PT, origSize[1] - dy);
    y = origAt[1] + origSize[1] - h;
  }
  return { at: [x, y], size: [w, h] };
}

// --- external-edit attribution (§6.5) --------------------------------------

/** The exact post-state this pane last committed for an object. Used to tell
 *  "our own write landing" from "an agent's write" by value, because the
 *  fileStore publishes chunk and mtime in separate microtasks (external edits
 *  refresh geometry *before* the mtime token moves, own writes the reverse),
 *  so a token check alone misattributes at geometry-change time. */
export interface ExpectedChange {
  at?: [number, number];
  size?: [number, number];
}

/**
 * Diff two frame maps and attribute the changes. Frames that match a pending
 * own-committed value are consumed from `expected` and not returned; every
 * other changed or added frame is an external (agent) edit to flash. Removed
 * objects are deliberately ignored (nothing to outline).
 */
export function attributeDiff(
  baseline: Map<string, Frame>,
  next: Map<string, Frame>,
  expected: Map<string, ExpectedChange>,
): { id: string; frame: Frame }[] {
  const external: { id: string; frame: Frame }[] = [];
  for (const [id, frame] of next) {
    const prev = baseline.get(id);
    if (prev !== undefined && samePair(prev.at, frame.at) && samePair(prev.size, frame.size)) {
      continue;
    }
    const exp = expected.get(id);
    const ownAt = exp?.at === undefined ? prev !== undefined && samePair(prev.at, frame.at) : samePair(exp.at, frame.at);
    const ownSize =
      exp?.size === undefined
        ? prev !== undefined && samePair(prev.size, frame.size)
        : samePair(exp.size, frame.size);
    if (exp !== undefined) {
      // Consumed on match; also dropped on mismatch — every refresh reads the
      // live file, so a mismatch means our write was superseded and its value
      // can only return via a fresh commit (which re-arms `expected`).
      expected.delete(id);
      if (ownAt && ownSize) continue;
    }
    external.push({ id, frame });
  }
  return external;
}

// --- actor-aware undo (§6.7) -----------------------------------------------

/** One field of one gesture, recorded as prior/new values so overlap with a
 *  later external write is detectable at undo time. */
export interface FieldChange {
  field: "at" | "size";
  from: [number, number];
  to: [number, number];
}

/** One user gesture on one object. A corner resize that moves the anchor
 *  carries both fields; a plain drag or inspector edit carries one. */
export interface Gesture {
  object: string;
  fields: FieldChange[];
}

export type UndoResult =
  | { kind: "apply"; object: string; verb: string; change: { at?: [number, number]; size?: [number, number] } }
  | { kind: "stale"; object: string }
  | { kind: "empty" };

function verbOf(g: Gesture): string {
  return g.fields.some((f) => f.field === "size") ? "resize" : "move";
}

/**
 * The pane's own gesture history. THE ACTOR RULE: only this pane's gestures
 * ever enter the stack, and a gesture is invertible only while the file still
 * holds the exact value the gesture wrote — if an agent has since touched the
 * object, the entry (and everything older for that object) is dropped rather
 * than reverting the agent's work with a stale absolute value.
 */
export class UndoStack {
  private past: Gesture[] = [];
  private future: Gesture[] = [];

  push(g: Gesture): void {
    this.past.push(g);
    this.future = [];
  }

  /** Undo the newest still-valid gesture against the current file geometry. */
  undo(current: Map<string, Frame>): UndoResult {
    return this.take(this.past, this.future, current, "to", "from");
  }

  /** Redo the most recently undone gesture, with the same staleness check. */
  redo(current: Map<string, Frame>): UndoResult {
    return this.take(this.future, this.past, current, "from", "to");
  }

  /**
   * Pop gestures off `src` until one is fresh (each recorded `check` value
   * still matches the file); stale ones are dropped along with every other
   * `src` entry for the same object. A fresh gesture moves to `dst` and
   * returns the values from its `apply` side.
   */
  private take(
    src: Gesture[],
    dst: Gesture[],
    current: Map<string, Frame>,
    check: "to" | "from",
    apply: "to" | "from",
  ): UndoResult {
    let dropped: string | null = null;
    for (;;) {
      const g = src.pop();
      if (g === undefined) {
        return dropped === null ? { kind: "empty" } : { kind: "stale", object: dropped };
      }
      const cur = current.get(g.object);
      const fresh =
        cur !== undefined && g.fields.every((f) => samePair(cur[f.field], f[check]));
      if (!fresh) {
        dropped = g.object;
        for (let i = src.length - 1; i >= 0; i--) {
          if (src[i].object === g.object) src.splice(i, 1);
        }
        continue;
      }
      dst.push(g);
      const change: { at?: [number, number]; size?: [number, number] } = {};
      for (const f of g.fields) change[f.field] = f[apply];
      return { kind: "apply", object: g.object, verb: verbOf(g), change };
    }
  }
}
