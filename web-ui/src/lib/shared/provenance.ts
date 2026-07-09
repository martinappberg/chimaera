/**
 * Copy provenance: a copied snippet remembers where it came from.
 *
 * When the user copies while a tracked selection is live (file views and
 * terminals both publish into `activeSelection`), the selection's source is
 * snapshotted alongside its text. Pasting that exact text into an AGENT
 * session later appends a visible ` [from @path#L3-L9] ` suffix after the
 * paste — the pasted bytes themselves are never modified (plain paste stays
 * plain, shells are never touched), the tag is reviewable and deletable in
 * the composer, and nothing ever auto-submits.
 *
 * The register holds ONE entry (the last copy), expires quickly, and only
 * matches snippets long enough to be unambiguous — "ls" never grows a tag.
 */

import { get } from "svelte/store";
import { activeSelection, type SelectionSource } from "./reference";

/** How long a copied snippet keeps its provenance. */
const TTL_MS = 5 * 60 * 1000;
/** Snippets shorter than this are too ambiguous to tag. */
const MIN_LENGTH = 24;

interface CopyRecord {
  source: SelectionSource;
  /** Normalized text at copy time (the match key). */
  key: string;
  at: number;
}

let record: CopyRecord | null = null;

/** Line endings and outer whitespace never break a match. */
function normalize(text: string): string {
  return text.replace(/\r\n/g, "\n").trim();
}

/**
 * Snapshot the live selection as the copy's provenance. Wire to the window
 * `copy` event; a copy with no tracked selection clears the register (the
 * clipboard moved on).
 */
export function rememberCopy(): void {
  const sel = get(activeSelection);
  if (sel === null || normalize(sel.text).length < MIN_LENGTH) {
    record = null;
    return;
  }
  record = { source: sel, key: normalize(sel.text), at: Date.now() };
}

/**
 * The source of `pastedText`, when it is the snippet last copied from a
 * tracked view (normalized match, fresh, long enough). Null otherwise.
 */
export function provenanceFor(pastedText: string): SelectionSource | null {
  if (record === null) return null;
  if (Date.now() - record.at > TTL_MS) {
    record = null;
    return null;
  }
  return normalize(pastedText) === record.key ? record.source : null;
}

/** Test/HMR hook. */
export function clearProvenance(): void {
  record = null;
}
