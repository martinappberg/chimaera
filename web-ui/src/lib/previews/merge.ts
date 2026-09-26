/**
 * Line-wise three-way merge for an editor buffer whose file changed on disk
 * under it (usually an agent editing the same document). Pure over strings
 * so it is unit-tested headless; the buffer store applies the result.
 *
 * Lines are compared WITH their terminator ("a\n", and a last line that may
 * lack one), so adding or removing a final newline is an ordinary line change
 * and every result re-joins by plain concatenation.
 *
 * node-diff3's LCS scans its candidate list linearly per equal-line pair, so
 * repeated lines (blank lines in prose, `}` in code) make it quadratic or
 * worse over a whole file. Both entry points therefore anchor first — lines
 * that occur exactly once in every version, kept in the same order in all of
 * them (patience diff's sync points) — and run node-diff3 only on the short
 * segments between anchors. A segment still too repetitive to diff within
 * PAIR_BUDGET degrades to a conflict (merge) or one coarse change (diff):
 * never wrong, only less clever.
 */

import { diff3Merge, diffPatch } from "node-diff3";

/** Equal-line pairs a segment may present to the LCS before we give up. */
const PAIR_BUDGET = 250_000;

/** Split into lines that keep their "\n" (the last may have none). */
export function lineTokens(text: string): string[] {
  return text.length === 0 ? [] : text.split(/(?<=\n)/);
}

export type MergeResult =
  | { clean: true; text: string }
  | { clean: false; conflicts: number };

function same(x: readonly string[], y: readonly string[]): boolean {
  return x.length === y.length && x.every((t, i) => t === y[i]);
}

/** Line → its index when it occurs once, -1 when repeated. */
function uniqueIndex(lines: readonly string[]): Map<string, number> {
  const m = new Map<string, number>();
  lines.forEach((t, i) => m.set(t, m.has(t) ? -1 : i));
  return m;
}

/** Longest subsequence of `items` strictly increasing in `key` (O(n log n)). */
function longestIncreasing(items: number[][], key: number): number[][] {
  const tails: number[] = [];
  const prev: number[] = new Array<number>(items.length);
  for (let i = 0; i < items.length; i++) {
    const k = items[i][key];
    let lo = 0;
    let hi = tails.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (items[tails[mid]][key] < k) lo = mid + 1;
      else hi = mid;
    }
    prev[i] = lo > 0 ? tails[lo - 1] : -1;
    tails[lo] = i;
  }
  const out: number[][] = [];
  for (let i = tails.length > 0 ? tails[tails.length - 1] : -1; i >= 0; i = prev[i]) out.push(items[i]);
  return out.reverse();
}

/**
 * Sync points across `seqs`: per anchor, the index of one line in each
 * sequence, where the line occurs exactly once in every sequence and the
 * anchors increase in all of them.
 */
function anchors(seqs: readonly (readonly string[])[]): number[][] {
  const idx = seqs.map(uniqueIndex);
  let items: number[][] = [];
  seqs[0].forEach((t, i) => {
    if (idx[0].get(t) !== i) return;
    const at = [i];
    for (let s = 1; s < seqs.length; s++) {
      const j = idx[s].get(t);
      if (j === undefined || j < 0) return;
      at.push(j);
    }
    items.push(at);
  });
  // Items are already increasing in sequence 0; each pass keeps the earlier
  // orders (a subsequence of an increasing run is increasing).
  for (let s = 1; s < seqs.length; s++) items = longestIncreasing(items, s);
  return items;
}

/** The LCS work a diff of `x` against `y` would face. */
function equalPairs(x: readonly string[], y: readonly string[]): number {
  const counts = new Map<string, number>();
  for (const t of x) counts.set(t, (counts.get(t) ?? 0) + 1);
  let pairs = 0;
  for (const t of y) pairs += counts.get(t) ?? 0;
  return pairs;
}

/** Lengths of the head and tail every list shares (never overlapping). */
function sharedEnds(lists: readonly (readonly string[])[]): [number, number] {
  const min = Math.min(...lists.map((l) => l.length));
  const first = lists[0];
  let head = 0;
  while (head < min && lists.every((l) => l[head] === first[head])) head++;
  let tail = 0;
  while (
    tail < min - head &&
    lists.every((l) => l[l.length - 1 - tail] === first[first.length - 1 - tail])
  ) {
    tail++;
  }
  return [head, tail];
}

/** Three-way merge of one anchor-free segment; null = conflict. */
function mergeSegment(o: string[], a: string[], b: string[]): string[] | null {
  if (same(a, b) || same(o, b)) return a;
  if (same(o, a)) return b;
  const [head, tail] = sharedEnds([o, a, b]);
  const cut = (l: string[]) => l.slice(head, l.length - tail);
  const [mo, ma, mb] = [cut(o), cut(a), cut(b)];
  if (equalPairs(mo, ma) + equalPairs(mo, mb) > PAIR_BUDGET) return null;
  const out: string[] = a.slice(0, head);
  for (const r of diff3Merge(ma, mo, mb, { excludeFalseConflicts: true })) {
    if (r.conflict !== undefined) return null;
    if (r.ok !== undefined) out.push(...r.ok);
  }
  out.push(...a.slice(a.length - tail));
  return out;
}

/**
 * Merge `mine` and `disk`, both derived from `base`. Changes on one side only
 * are taken; identical changes on both sides are taken once; overlapping or
 * adjacent differing changes are a conflict (the usual diff3 rule — an edit
 * that touches the line next to another edit is not provably independent).
 */
export function merge3(base: string, mine: string, disk: string): MergeResult {
  if (mine === disk || disk === base) return { clean: true, text: mine };
  if (mine === base) return { clean: true, text: disk };
  const o = lineTokens(base);
  const a = lineTokens(mine);
  const b = lineTokens(disk);
  const out: string[] = [];
  let conflicts = 0;
  let po = 0;
  let pa = 0;
  let pb = 0;
  for (const [io, ia, ib] of [...anchors([o, a, b]), [o.length, a.length, b.length]]) {
    const seg = mergeSegment(o.slice(po, io), a.slice(pa, ia), b.slice(pb, ib));
    if (seg === null) conflicts++;
    else out.push(...seg);
    if (io < o.length) out.push(o[io]);
    po = io + 1;
    pa = ia + 1;
    pb = ib + 1;
  }
  return conflicts > 0 ? { clean: false, conflicts } : { clean: true, text: out.join("") };
}

/** A replacement in `from`/`to` offsets of the OLD text. */
export interface TextChange {
  from: number;
  to: number;
  insert: string;
}

/**
 * The line-granular edits that turn `before` into `after`, as non-overlapping
 * changes in ascending order over `before` — one CodeMirror transaction, so
 * the cursor and selection map through untouched lines instead of jumping to
 * the start of a whole-document replace.
 */
export function lineChanges(before: string, after: string): TextChange[] {
  if (before === after) return [];
  const a = lineTokens(before);
  const b = lineTokens(after);
  const offsets: number[] = [0];
  for (const t of a) offsets.push(offsets[offsets.length - 1] + t.length);
  const changes: TextChange[] = [];
  const replace = (fromLine: number, toLine: number, lines: readonly string[]) =>
    changes.push({ from: offsets[fromLine], to: offsets[toLine], insert: lines.join("") });
  let pa = 0;
  let pb = 0;
  for (const [ia, ib] of [...anchors([a, b]), [a.length, b.length]]) {
    const seg = [a.slice(pa, ia), b.slice(pb, ib)];
    if (!same(seg[0], seg[1])) {
      const [head, tail] = sharedEnds(seg);
      const x = seg[0].slice(head, seg[0].length - tail);
      const y = seg[1].slice(head, seg[1].length - tail);
      const at = pa + head;
      if (equalPairs(x, y) > PAIR_BUDGET) {
        replace(at, at + x.length, y);
      } else {
        for (const p of diffPatch(x, y)) {
          replace(at + p.buffer1.offset, at + p.buffer1.offset + p.buffer1.length, p.buffer2.chunk);
        }
      }
    }
    pa = ia + 1;
    pb = ib + 1;
  }
  return changes;
}

/** Apply `changes` (ascending, non-overlapping, over `text`) — the inverse
 *  check the tests use; the editor applies them through a transaction. */
export function applyChanges(text: string, changes: readonly TextChange[]): string {
  let out = "";
  let at = 0;
  for (const c of changes) {
    out += text.slice(at, c.from) + c.insert;
    at = c.to;
  }
  return out + text.slice(at);
}
