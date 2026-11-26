/**
 * A line diff for the chat's edit rows and approval cards. Agents report an
 * edit as old text and new text — sometimes the changed lines only (claude
 * Edit), sometimes the whole file before and after (ACP `diff` content).
 * Drawn as two blocks, a one-line change to a long file reads as the file
 * twice; this finds the lines that actually changed.
 */

export interface DiffRow {
  /** `gap`: unchanged lines left out (`text` is how many). */
  kind: "ctx" | "del" | "add" | "gap";
  text: string;
}

/** Above this many cell comparisons the middle is shown as one removal and
 *  one addition: the table is quadratic, and a diff that large is skimmed. */
const MAX_CELLS = 250_000;

/** Split into lines without inventing a trailing empty one. */
function lines(text: string): string[] {
  if (text === "") return [];
  const out = text.split("\n");
  if (out[out.length - 1] === "") out.pop();
  return out;
}

/** The changed lines of `oldText` → `newText`, with `context` unchanged lines
 *  around each change and longer unchanged runs collapsed to a gap. */
export function diffLines(oldText: string, newText: string, context = 2): DiffRow[] {
  const a = lines(oldText);
  const b = lines(newText);
  let head = 0;
  while (head < a.length && head < b.length && a[head] === b[head]) head += 1;
  let tail = 0;
  while (tail < a.length - head && tail < b.length - head && a[a.length - 1 - tail] === b[b.length - 1 - tail]) {
    tail += 1;
  }
  const midA = a.slice(head, a.length - tail);
  const midB = b.slice(head, b.length - tail);

  const rows: DiffRow[] = a.slice(0, head).map((text) => ({ kind: "ctx", text }));
  rows.push(...middle(midA, midB));
  rows.push(...a.slice(a.length - tail).map((text): DiffRow => ({ kind: "ctx", text })));
  return collapse(rows, context);
}

/** Longest-common-subsequence rows of the differing middle. */
function middle(a: string[], b: string[]): DiffRow[] {
  const del = (text: string): DiffRow => ({ kind: "del", text });
  const add = (text: string): DiffRow => ({ kind: "add", text });
  if (a.length === 0 || b.length === 0 || a.length * b.length > MAX_CELLS) {
    return [...a.map(del), ...b.map(add)];
  }
  const width = b.length + 1;
  const table = new Uint32Array((a.length + 1) * width);
  for (let i = a.length - 1; i >= 0; i -= 1) {
    for (let j = b.length - 1; j >= 0; j -= 1) {
      table[i * width + j] =
        a[i] === b[j]
          ? table[(i + 1) * width + j + 1] + 1
          : Math.max(table[(i + 1) * width + j], table[i * width + j + 1]);
    }
  }
  const rows: DiffRow[] = [];
  let i = 0;
  let j = 0;
  while (i < a.length && j < b.length) {
    if (a[i] === b[j]) {
      rows.push({ kind: "ctx", text: a[i] });
      i += 1;
      j += 1;
    } else if (table[(i + 1) * width + j] >= table[i * width + j + 1]) {
      rows.push(del(a[i]));
      i += 1;
    } else {
      rows.push(add(b[j]));
      j += 1;
    }
  }
  rows.push(...a.slice(i).map(del), ...b.slice(j).map(add));
  return rows;
}

/** Keep `context` unchanged lines beside each change; fold the rest. */
function collapse(rows: DiffRow[], context: number): DiffRow[] {
  const keep = new Array<boolean>(rows.length).fill(false);
  rows.forEach((row, i) => {
    if (row.kind === "ctx") return;
    for (let k = Math.max(0, i - context); k <= Math.min(rows.length - 1, i + context); k += 1) keep[k] = true;
  });
  const out: DiffRow[] = [];
  let skipped = 0;
  rows.forEach((row, i) => {
    if (keep[i]) {
      if (skipped > 0) out.push({ kind: "gap", text: String(skipped) });
      skipped = 0;
      out.push(row);
    } else {
      skipped += 1;
    }
  });
  // Nothing changed (identical texts): say so with the gap alone.
  if (skipped > 0 && out.length > 0) out.push({ kind: "gap", text: String(skipped) });
  return out;
}

/** "+3 −1": the size of a change, for a row or card header. */
export function diffStat(rows: readonly DiffRow[]): { added: number; removed: number } {
  let added = 0;
  let removed = 0;
  for (const row of rows) {
    if (row.kind === "add") added += 1;
    else if (row.kind === "del") removed += 1;
  }
  return { added, removed };
}
