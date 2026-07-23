/**
 * Client-side interaction model for BoardView: the identity/geometry parse,
 * corner-resize math, the actor-aware undo stack (board-plan §6.7), and the
 * external-edit attribution diff (§6.5).
 *
 * Pure data and plain classes only — the component owns every piece of
 * `$state`. Nothing here is layout truth: values are the file's own literal
 * numbers, and every mutation still routes through POST /board/edit.
 */

import type { BoardJournalEvent } from "./files";

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
  /** The object's own parsed JSON, verbatim — the inspector's config
   *  projection ([`chartConfig`]) and the expected-fingerprint math for
   *  `set` commits read from it. Still never layout truth. */
  raw: unknown;
  /** Configuration fingerprint ([`configSig`]) — what the §6.5 attribution
   *  diff compares so an external restyle (which moves no frame) still
   *  flashes. */
  sig: string;
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
          raw: o,
          sig: configSig(o),
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

// --- object configuration (the /board/edit set op's client half) -------------

/** Deterministic sorted-key serialization, so two JSON values compare equal
 *  iff they are structurally equal regardless of key order. */
function stableStringify(v: unknown): string {
  if (Array.isArray(v)) return `[${v.map(stableStringify).join(",")}]`;
  if (typeof v === "object" && v !== null) {
    const rec = v as Record<string, unknown>;
    const keys = Object.keys(rec).sort();
    return `{${keys.map((k) => `${JSON.stringify(k)}:${stableStringify(rec[k])}`).join(",")}}`;
  }
  return JSON.stringify(v);
}

/**
 * An object's configuration fingerprint: everything but `at`/`size` (geometry
 * attribution is value-checked separately) and `text` (text edits predate the
 * config diff and deliberately stay flash-free — the inline editor commits
 * no fingerprint expectation). Sorted keys, so the client's own applied-set
 * prediction and a reparse of the daemon's canonical bytes agree.
 */
export function configSig(raw: unknown): string {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return stableStringify(raw);
  const rest: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(raw as Record<string, unknown>)) {
    if (k === "at" || k === "size" || k === "text") continue;
    rest[k] = v;
  }
  return stableStringify(rest);
}

/**
 * Client mirror of the daemon's dot-path `set` application, used ONLY to
 * predict the post-write fingerprint for own-edit attribution (the daemon
 * remains the sole authority on bytes). Same semantics: numeric segments
 * index arrays in bounds, missing intermediate keys materialize as objects,
 * null removes the field; paths applied in sorted order like the daemon's
 * BTreeMap. A path that cannot be applied is skipped — the daemon rejects
 * that request with nothing written, so there is no fingerprint to predict.
 */
export function applyFieldSet(raw: unknown, set: Record<string, unknown>): unknown {
  const clone = JSON.parse(JSON.stringify(raw)) as unknown;
  for (const path of Object.keys(set).sort()) {
    const value = set[path];
    const segs = path.split(".");
    if (segs.some((s) => s === "")) continue;
    let cur: unknown = clone;
    for (let i = 0; i < segs.length; i++) {
      const seg = segs[i];
      const last = i === segs.length - 1;
      if (/^\d+$/.test(seg)) {
        if (!Array.isArray(cur)) break;
        const idx = Number(seg);
        if (idx >= cur.length) break;
        if (last) cur[idx] = value;
        else cur = cur[idx];
      } else {
        if (typeof cur !== "object" || cur === null || Array.isArray(cur)) break;
        const rec = cur as Record<string, unknown>;
        if (last) {
          if (value === null) delete rec[seg];
          else rec[seg] = value;
        } else {
          if (typeof rec[seg] !== "object" || rec[seg] === null) rec[seg] = {};
          cur = rec[seg];
        }
      }
    }
  }
  return clone;
}

/**
 * The `sort` values chart.rs's `category_order` actually accepts, labeled by
 * what each does: `x`/`-x` hit its label-sort branch (the literal `"x"` key,
 * either orientation), any other key — canonically `y` — sorts by the summed
 * magnitude, `-` descending. The literal token stays visible (canonical
 * vocabulary); absent (`""`) is data order.
 */
export const SORT_OPTIONS: { value: string; label: string }[] = [
  { value: "", label: "data order" },
  { value: "x", label: "x · label A→Z" },
  { value: "-x", label: "-x · label Z→A" },
  { value: "y", label: "y · value low→high" },
  { value: "-y", label: "-y · value high→low" },
];

/** The interchangeable single-mark kinds. `box`/`rect` are different
 *  geometries and an interval bar (x2/y2) states a span — none swap. */
export const MARK_SWAP_KINDS = ["bar", "line", "point"] as const;

/** The chart inspector's projection of one chart object's config. */
export interface ChartConfig {
  /** Per-channel field + current axis label (`title`); null = the channel is
   *  absent, so there is nothing to label (a channel needs `field`). */
  x: { field: string; title: string } | null;
  y: { field: string; title: string } | null;
  /** The channel whose `sort` orders the categories — chart.rs's orient rule
   *  (x quantitative/temporal × y nominal/ordinal reads horizontal, category
   *  on y; otherwise category on x). Null = sort does not apply (missing
   *  channels or a continuous category axis). */
  sortChannel: "x" | "y" | null;
  /** Current `sort` on that channel; "" = data order. */
  sort: string;
  markCount: number;
  /** The single mark's kind when the chart has exactly one; null otherwise. */
  markKind: string | null;
  /** Single mark of an interchangeable kind and not an interval. */
  markSwappable: boolean;
  /** The single mark's stated color token (`fill` ?? `stroke`, the same
   *  precedence series_color resolves); "" = the theme's default. */
  markColor: string;
}

/** Project a chart object's raw JSON for the inspector; null for non-charts.
 *  Reads the file's own literal values — never derived layout. */
export function chartConfig(o: ObjInfo): ChartConfig | null {
  if (o.kind !== "chart" || typeof o.raw !== "object" || o.raw === null) return null;
  const raw = o.raw as Record<string, unknown>;
  const channel = (v: unknown): Record<string, unknown> | null =>
    typeof v === "object" && v !== null && typeof (v as { field?: unknown }).field === "string"
      ? (v as Record<string, unknown>)
      : null;
  const x = channel(raw.x);
  const y = channel(raw.y);
  // Undeclared channel types default exactly as chart.rs's build does.
  const xKind = typeof x?.type === "string" ? (x.type as string) : "nominal";
  const yKind = typeof y?.type === "string" ? (y.type as string) : "quantitative";
  const horizontal =
    (xKind === "quantitative" || xKind === "temporal") &&
    (yKind === "nominal" || yKind === "ordinal");
  const cat = horizontal ? y : x;
  const catKind = horizontal ? yKind : xKind;
  const sortChannel =
    x !== null && y !== null && (catKind === "nominal" || catKind === "ordinal")
      ? horizontal
        ? "y"
        : "x"
      : null;
  const marks = Array.isArray(raw.marks) ? raw.marks : [];
  const mark =
    marks.length === 1 && typeof marks[0] === "object" && marks[0] !== null
      ? (marks[0] as Record<string, unknown>)
      : null;
  const markKind = typeof mark?.mark === "string" ? (mark.mark as string) : null;
  const fields = mark?.fields;
  const interval =
    typeof fields === "object" &&
    fields !== null &&
    ("x2" in (fields as Record<string, unknown>) || "y2" in (fields as Record<string, unknown>));
  const color =
    typeof mark?.fill === "string"
      ? (mark.fill as string)
      : typeof mark?.stroke === "string"
        ? (mark.stroke as string)
        : "";
  const proj = (ch: Record<string, unknown> | null): { field: string; title: string } | null =>
    ch === null
      ? null
      : { field: ch.field as string, title: typeof ch.title === "string" ? ch.title : "" };
  return {
    x: proj(x),
    y: proj(y),
    sortChannel,
    sort: typeof cat?.sort === "string" ? (cat.sort as string) : "",
    markCount: marks.length,
    markKind,
    markSwappable:
      markKind !== null && !interval && (MARK_SWAP_KINDS as readonly string[]).includes(markKind),
    markColor: color,
  };
}

// --- composite children (the render's childFrames map) ----------------------

/**
 * One derived child from /board/render's `childFrames`: the stable derived id
 * (`<composite>/<part>` — the same id the journal and describe speak) and its
 * laid-out `[x, y, w, h]` frame in page points. Layout truth stays
 * server-side: these rects are the engine's own expansion, so child
 * hit-testing agrees with the pixels by construction.
 */
export interface ChildFrame {
  id: string;
  frame: [number, number, number, number];
}

/** Topmost child under the point — expansion order is z-order, so walk
 *  backwards, exactly like the stage's own object hit-test. */
export function hitChild(children: ChildFrame[], pt: [number, number]): ChildFrame | null {
  for (let i = children.length - 1; i >= 0; i--) {
    const [x, y, w, h] = children[i].frame;
    if (pt[0] >= x && pt[0] <= x + w && pt[1] >= y && pt[1] <= y + h) return children[i];
  }
  return null;
}

/** A ChildFrame's rect as the pane's `Frame` shape. */
export function childFrameRect(c: ChildFrame): Frame {
  return { at: [c.frame[0], c.frame[1]], size: [c.frame[2], c.frame[3]] };
}

/**
 * Resolve a derived child id to its node INDEX in the parent diagram's own
 * `nodes` array — the anchor a `set` edit needs (`nodes.<i>.at` /
 * `nodes.<i>.label`). Resolved from the child id at commit time, never cached:
 * the id is stable, the index is not. First declaration wins on a duplicate,
 * mirroring the engine's expansion (later duplicates are never emitted). Null
 * = not a diagram node child (a lane box/label, another composite kind, a
 * foreign id).
 */
export function diagramNodeIndex(parent: ObjInfo, childId: string): number | null {
  if (parent.kind !== "diagram") return null;
  const prefix = `${parent.id}/`;
  if (!childId.startsWith(prefix)) return null;
  const nodeId = childId.slice(prefix.length);
  const raw = parent.raw as { nodes?: unknown } | null;
  const nodes = Array.isArray(raw?.nodes) ? raw.nodes : [];
  for (let i = 0; i < nodes.length; i++) {
    const n = nodes[i] as { id?: unknown } | null;
    if (typeof n?.id === "string" && n.id === nodeId) return i;
  }
  return null;
}

/** The node's stored label at `index` — the overlay editor's seed. Null when
 *  the index does not name a labeled node (the file changed under us). */
export function diagramNodeLabel(parent: ObjInfo, index: number): string | null {
  const raw = parent.raw as { nodes?: unknown } | null;
  const nodes = Array.isArray(raw?.nodes) ? raw.nodes : [];
  const n = nodes[index] as { label?: unknown } | null | undefined;
  return typeof n?.label === "string" ? n.label : null;
}

/**
 * The overlay editor's commit projection for a node label: one string, CR
 * forms normalized and newlines collapsed to spaces — a diagram node lays
 * out as a single measured line, so a stored newline would be invisible
 * intent.
 */
export function editorTextToNodeLabel(text: string): string {
  return text.replace(/\r\n?/g, "\n").replace(/\n+/g, " ");
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

// --- comment pins (§6.4: journal-only, never the board file) -----------------

/** One unresolved comment pin, reduced from the journal. */
export interface PinInfo {
  pin: string;
  seq: number;
  actor: string;
  page: string;
  /** Bound object id, or null for a point pin. */
  object: string | null;
  /** Stored canvas point — where a point pin sits, and the fallback anchor
   *  for an object-bound pin whose object has since been removed. */
  at: [number, number] | null;
  text: string;
}

/**
 * Reduce the journal (oldest first) to its unresolved pins: a `comment`
 * opens a pin, a `comment-resolved` closes it. Order-aware on purpose — a
 * resolve only clears the incarnations before it, so a re-used pin id after
 * its resolution is a fresh, open pin (the same rule the journal's own
 * compaction applies). Returned in seq order, which is also the overlay's
 * numbering order.
 */
export function unresolvedPins(events: BoardJournalEvent[]): PinInfo[] {
  const open = new Map<string, PinInfo>();
  for (const ev of events) {
    if (typeof ev.pin !== "string") continue;
    if (ev.event === "comment") {
      open.set(ev.pin, {
        pin: ev.pin,
        seq: ev.seq,
        actor: ev.actor,
        page: typeof ev.page === "string" ? ev.page : "",
        object: typeof ev.object === "string" ? ev.object : null,
        at:
          Array.isArray(ev.at) && ev.at.length === 2
            ? [Number(ev.at[0]), Number(ev.at[1])]
            : null,
        text: typeof ev.text === "string" ? ev.text : "",
      });
    } else if (ev.event === "comment-resolved") {
      open.delete(ev.pin);
    }
  }
  return [...open.values()].sort((a, b) => a.seq - b.seq);
}

/**
 * The next pin id to mint: `c<n>` past the highest `c<digits>` id anywhere in
 * the journal — resolved pins included, so a fresh pin never collides with a
 * still-visible resolve marker for an older incarnation.
 */
export function nextPinId(events: BoardJournalEvent[]): string {
  let max = 0;
  for (const ev of events) {
    if (ev.event !== "comment" && ev.event !== "comment-resolved") continue;
    if (typeof ev.pin !== "string") continue;
    const m = /^c(\d+)$/.exec(ev.pin);
    if (m !== null) max = Math.max(max, Number(m[1]));
  }
  return `c${max + 1}`;
}

/**
 * Where a pin's dot anchors, in board points. An object-bound pin rides its
 * object's top-right frame corner — the file's own literal geometry, so the
 * dot tracks moves/resizes for free; if the object has since been removed it
 * falls back to the stored point. A point pin sits at its stored point. Null
 * (nothing to anchor to) means the dot is not drawn.
 */
export function pinAnchor(pin: PinInfo, objects: ObjInfo[]): [number, number] | null {
  if (pin.object !== null) {
    const o = objects.find((x) => x.id === pin.object);
    if (o !== undefined && o.at !== null && o.size !== null) {
      return [o.at[0] + o.size[0], o.at[1]];
    }
  }
  return pin.at;
}

/** Float-tolerant tuple equality (values are 8 pt multiples in practice). */
export function samePair(a: [number, number], b: [number, number]): boolean {
  return Math.abs(a[0] - b[0]) < 1e-6 && Math.abs(a[1] - b[1]) < 1e-6;
}

/** A frame plus the object's config fingerprint — what the attribution diff
 *  snapshots per object. Objects without full geometry are absent (an
 *  external restyle of one has no frame to outline, like a removed object). */
export interface ObjSnap extends Frame {
  sig: string;
}

/** id → frame + config fingerprint for the objects that have full geometry. */
export function pageFrames(objects: ObjInfo[]): Map<string, ObjSnap> {
  const m = new Map<string, ObjSnap>();
  for (const o of objects) {
    if (o.at !== null && o.size !== null) m.set(o.id, { at: o.at, size: o.size, sig: o.sig });
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
 *  so a token check alone misattributes at geometry-change time. Geometry is
 *  compared by value; a `set` commit's config lands as a predicted
 *  fingerprint ([`configSig`] over [`applyFieldSet`]). */
export interface ExpectedChange {
  at?: [number, number];
  size?: [number, number];
  sig?: string;
}

/**
 * Diff two snapshot maps and attribute the changes — frame moves AND config
 * restyles (fingerprint). Changes that match a pending own-committed value
 * are consumed from `expected` and not returned; every other changed or
 * added snapshot is an external (agent) edit to flash at its current frame.
 * Removed objects are deliberately ignored (nothing to outline).
 */
export function attributeDiff(
  baseline: Map<string, ObjSnap>,
  next: Map<string, ObjSnap>,
  expected: Map<string, ExpectedChange>,
): { id: string; frame: Frame }[] {
  const external: { id: string; frame: Frame }[] = [];
  for (const [id, snap] of next) {
    const prev = baseline.get(id);
    const frameSame =
      prev !== undefined && samePair(prev.at, snap.at) && samePair(prev.size, snap.size);
    const sigSame = prev !== undefined && prev.sig === snap.sig;
    if (frameSame && sigSame) continue;
    const exp = expected.get(id);
    const ownAt =
      exp?.at === undefined
        ? prev !== undefined && samePair(prev.at, snap.at)
        : samePair(exp.at, snap.at);
    const ownSize =
      exp?.size === undefined
        ? prev !== undefined && samePair(prev.size, snap.size)
        : samePair(exp.size, snap.size);
    const ownSig = exp?.sig === undefined ? sigSame : exp.sig === snap.sig;
    if (exp !== undefined) {
      // Consumed on match; also dropped on mismatch — every refresh reads the
      // live file, so a mismatch means our write was superseded and its value
      // can only return via a fresh commit (which re-arms `expected`).
      expected.delete(id);
      if (ownAt && ownSize && ownSig) continue;
    }
    external.push({ id, frame: { at: snap.at, size: snap.size } });
  }
  return external;
}

// --- export preflight (§11) -------------------------------------------------

/** Fidelity order for the preflight census — best first, matching the plan's
 *  tier table. Unknown tiers (a future daemon) sort last, never dropped. */
export const EXPORT_TIERS = ["native", "grouped", "vector", "raster"] as const;

/**
 * The preflight census line — `"1 native · 3 grouped"` — from the per-object
 * fates a pptx export declares. Tiers appear best-first, only when present;
 * an unknown tier from a newer daemon is appended verbatim rather than
 * hidden (the preflight's honesty is the feature).
 */
export function fateCensus(fates: { tier: string }[]): string {
  const counts = new Map<string, number>();
  for (const f of fates) counts.set(f.tier, (counts.get(f.tier) ?? 0) + 1);
  const parts: string[] = [];
  for (const tier of EXPORT_TIERS) {
    const n = counts.get(tier);
    if (n !== undefined) {
      parts.push(`${n} ${tier}`);
      counts.delete(tier);
    }
  }
  for (const [tier, n] of counts) parts.push(`${n} ${tier}`);
  return parts.join(" · ");
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
