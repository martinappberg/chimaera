/**
 * Pure logic for `chimaera board show` cards in the chat transcript
 * (board plan §10.1): detecting the `shown … → <path>.board` signature
 * (a legacy `.board.json` path still matches) in completed tool output, the
 * transcript-level update-in-place
 * reduction (one card per board path, riding the latest re-show), the
 * boards/ promotion naming, and the chart-provenance parse the card's
 * "data" disclosure renders. No network here — ShownCard.svelte owns the
 * fetch/copy side; this module stays vitest-able.
 */

import { joinPath } from "../previews/files";

/** The structural slice of a chat tool block this module reads. */
export interface ShownToolLike {
  status: string;
  denied: boolean;
  content: { kind: string; text?: string } | null;
}

/** One board card to render. `revision` counts every shown-line occurrence
 *  for the path in view, so a same-`--id` re-show bumps it and the mounted
 *  card refetches without remounting (keyed on `path`). */
export interface ShownBoard {
  path: string;
  revision: number;
}

/** A ToolGroup's identity + rows, as ChatView's render list carries them. */
export interface ShownGroupInput<T extends ShownToolLike = ShownToolLike> {
  key: string;
  tools: readonly T[];
}

/** The stdout signature `chimaera board show` prints (plan §10.1). The CLI now
 *  emits the canonical `.board`; the optional `.json` still matches a legacy
 *  card. matchAll clones the regex, so the shared global literal is safe. */
const SHOWN_RE = /^shown .+ → (.+\.board(?:\.json)?)$/gm;

/** `board show` prints ABSOLUTE paths (so a board mounts regardless of where
 *  the agent's cwd sits versus the board's own workspace root — the two are
 *  different anchors, and a relative emit 404s the moment they diverge), which
 *  pass straight through. A bare/relative path is a legacy transcript or an
 *  older CLI: best-effort resolve it against the session cwd. */
function resolveShownPath(raw: string, cwd?: string): string {
  if (raw.startsWith("/")) return raw;
  if (cwd === undefined) return raw;
  return joinPath(cwd, raw.replace(/^\.\//, ""));
}

/**
 * The transcript-level reduction: every `shown` line in every group's
 * completed, non-denied output, deduped so each board path renders exactly
 * ONE card — in the LATEST group that showed it. That is what makes a
 * same-`--id` re-show in a later turn read as the card updating in place
 * (moving down to where the agent re-presented it) instead of a duplicate.
 */
export function collectShownByGroup<T extends ShownToolLike>(
  groups: readonly ShownGroupInput<T>[],
  cwd?: string,
): Map<string, ShownBoard[]> {
  const counts = new Map<string, number>();
  const lastGroup = new Map<string, string>();
  const perGroup: Array<[string, string[]]> = [];
  for (const g of groups) {
    const order: string[] = [];
    for (const t of g.tools) {
      if (t.status !== "completed" || t.denied) continue;
      if (t.content?.kind !== "output") continue;
      const text = t.content.text ?? "";
      for (const m of text.matchAll(SHOWN_RE)) {
        const p = resolveShownPath(m[1].trim(), cwd);
        counts.set(p, (counts.get(p) ?? 0) + 1);
        lastGroup.set(p, g.key);
        if (!order.includes(p)) order.push(p);
      }
    }
    if (order.length > 0) perGroup.push([g.key, order]);
  }
  const out = new Map<string, ShownBoard[]>();
  for (const [key, order] of perGroup) {
    const kept = order
      .filter((p) => lastGroup.get(p) === key)
      .map((path) => ({ path, revision: counts.get(path) ?? 1 }));
    if (kept.length > 0) out.set(key, kept);
  }
  return out;
}

/** The single-group view: what THIS run of tools showed (used for the
 *  collapsed summary's board count and the per-command reference row). */
export function collectShownBoards<T extends ShownToolLike>(
  tools: readonly T[],
  cwd?: string,
): ShownBoard[] {
  return collectShownByGroup([{ key: "g", tools }], cwd).get("g") ?? [];
}

/** A shown board lives under `<workspace>/.chimaera/board/shown/`. */
const SHOWN_DIR = "/.chimaera/board/shown/";

/** The workspace root a shown board belongs to: its path prefix when it sits
 *  in the shown/ pen (exact), else the session cwd. Null means "unknown" —
 *  the save affordance hides rather than guessing. */
export function workspaceRootFor(shownPath: string, cwd?: string): string | null {
  const i = shownPath.indexOf(SHOWN_DIR);
  if (i > 0) return shownPath.slice(0, i);
  return cwd ?? null;
}

/** Where an explicit "save to boards/" lands: `<workspace>/boards`. */
export function boardsDirFor(shownPath: string, cwd?: string): string | null {
  const root = workspaceRootFor(shownPath, cwd);
  return root === null ? null : joinPath(root, "boards");
}

/**
 * A collision-free basename for boards/: `name.board`, `name-2.board`, … (or
 * a legacy `name.board.json`). The suffix is split at the FIRST dot, so a
 * compound `.board.json` extension stays whole (the server's generic "name
 * copy" uniquifier would split at the last dot and the result would stop
 * opening as a board).
 */
export function uniqueBoardName(desired: string, existing: ReadonlySet<string>): string {
  if (!existing.has(desired)) return desired;
  const dot = desired.indexOf(".");
  const stem = dot > 0 ? desired.slice(0, dot) : desired;
  const ext = dot > 0 ? desired.slice(dot) : "";
  for (let n = 2; n < 10_000; n++) {
    const candidate = `${stem}-${n}${ext}`;
    if (!existing.has(candidate)) return candidate;
  }
  return desired;
}

/** What the card's "data" disclosure shows. `origin` is the schema's own
 *  label vocabulary; the rest are the optional provenance fields. */
export interface ChartProvenance {
  origin: string | null;
  source: string | null;
  inputs: string[];
  trace: string | null;
}

/** Mirrors `DataOrigin::label()` in chimaera-board's schema.rs — the wire
 *  values are kebab-case; an unknown value passes through verbatim. */
const ORIGIN_LABELS: Record<string, string> = {
  file: "from file",
  command: "from command",
  "stated-by-user": "stated by user",
  "derived-by-agent": "derived by agent",
};

/** Bounded: inputs shown, and trace characters kept (the schema clamps trace
 *  at 2 KiB in normalize(), but the file is agent-written — untrusted). */
const MAX_INPUTS = 16;
const MAX_TRACE = 4000;
const MAX_WALK_DEPTH = 6;

/**
 * Parse the first chart's `data` provenance out of a board file's JSON text
 * (the same board file the card already renders). Defensive throughout —
 * the file is agent-written; anything malformed is simply "no provenance".
 */
export function chartProvenance(boardJson: string): ChartProvenance | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(boardJson);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const pages = (parsed as { pages?: unknown }).pages;
  if (!Array.isArray(pages)) return null;
  for (const page of pages) {
    if (typeof page !== "object" || page === null) continue;
    const objects = (page as { objects?: unknown }).objects;
    const found = findChartData(objects, 0);
    if (found !== null) return found;
  }
  return null;
}

function findChartData(objects: unknown, depth: number): ChartProvenance | null {
  if (depth > MAX_WALK_DEPTH || !Array.isArray(objects)) return null;
  for (const obj of objects) {
    if (typeof obj !== "object" || obj === null) continue;
    const o = obj as { type?: unknown; data?: unknown; objects?: unknown };
    if (o.type === "chart" && typeof o.data === "object" && o.data !== null) {
      const d = o.data as {
        origin?: unknown;
        source?: unknown;
        inputs?: unknown;
        trace?: unknown;
      };
      const origin =
        typeof d.origin === "string" ? (ORIGIN_LABELS[d.origin] ?? d.origin) : null;
      const source = typeof d.source === "string" && d.source !== "" ? d.source : null;
      const inputs = Array.isArray(d.inputs)
        ? d.inputs
            .filter((p): p is string => typeof p === "string" && p !== "")
            .slice(0, MAX_INPUTS)
        : [];
      const trace =
        typeof d.trace === "string" && d.trace !== "" ? d.trace.slice(0, MAX_TRACE) : null;
      return { origin, source, inputs, trace };
    }
    if (o.type === "group") {
      const nested = findChartData(o.objects, depth + 1);
      if (nested !== null) return nested;
    }
  }
  return null;
}

/** The disclosure earns chrome only when there is something beyond the bare
 *  origin to inspect — origin alone was the chip the user called noise. */
export function hasProvenanceDetail(p: ChartProvenance | null): boolean {
  return p !== null && (p.source !== null || p.inputs.length > 0 || p.trace !== null);
}
