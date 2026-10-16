/**
 * The plan card's reading of the normalized `plan` level-set — the same
 * entries whether they came from claude's Task family or TodoWrite, codex
 * `update_plan`, or an ACP plan. Pure, so the card's states are pinned by
 * tests without a DOM.
 */
import type { PlanEntry } from "./store.svelte";

/** Task checkboxes stay visually distinct from scheduled-run clocks. */
export type StepMark = "done" | "current" | "todo" | "blocked";

/** Blocked is orthogonal to status: a blocked task is still `todo`. The
 *  server already filters `blockedBy` to blockers that are still open. */
export function stepMark(entry: PlanEntry): StepMark {
  if (entry.status === "done") return "done";
  if (entry.status === "in_progress") return "current";
  return entry.blockedBy.length > 0 ? "blocked" : "todo";
}

/** A step's line: the agent's present-continuous phrasing while it is the
 *  one in flight ("Running tests"), else its subject. */
export function stepText(entry: PlanEntry, running = true): string {
  const active = entry.activeForm?.trim();
  return running && entry.status === "in_progress" && active ? active : entry.content;
}

/** The second line, only when it adds something: agents often restate the
 *  subject as the description. */
export function stepDetail(entry: PlanEntry): string | null {
  const detail = entry.description?.trim();
  return detail && detail !== entry.content.trim() ? detail : null;
}

export interface PlanProgress {
  done: number;
  total: number;
  /** The first step in flight; null between steps or once all are done. */
  current: PlanEntry | null;
  allDone: boolean;
}

export function planProgress(entries: readonly PlanEntry[]): PlanProgress {
  const done = entries.filter((e) => e.status === "done").length;
  return {
    done,
    total: entries.length,
    current: entries.find((e) => e.status === "in_progress") ?? null,
    allDone: entries.length > 0 && done === entries.length,
  };
}

/** Idle plans are a compact record, even when the agent left steps open.
 *  Only an agent update can mark a step complete. */
export function planFolds(entries: readonly PlanEntry[], running: boolean): boolean {
  return !running && entries.length > 0;
}

/** Whether the pinned card shows the plan: one with open steps always (it
 *  stays through the idle gap for the next turn); a finished one only while
 *  the latest turn touched it — to be glanced at, folded, until the agent
 *  moves on (maintainer, 2026-10-15). */
export function planShows(entries: readonly PlanEntry[], touched: boolean): boolean {
  return entries.length > 0 && (touched || !planProgress(entries).allDone);
}

/** The folded line: the step in flight, else what is next, else the tally. */
export function planSummary(entries: readonly PlanEntry[], running = true): string {
  const p = planProgress(entries);
  if (p.allDone) return p.total === 1 ? "1 task done" : `${p.total} tasks done`;
  if (p.current !== null) return stepText(p.current, running);
  const next = entries.find((e) => e.status !== "done");
  return next !== undefined ? next.content : "";
}

/** Keyed-render identities for a whole plan: the agent's id when it gave
 *  one, else position (TodoWrite, codex and ACP plans re-send the whole list
 *  in order). Unique by construction — a repeated id (a re-sent TaskCreate,
 *  a journal from another build) gets the row's index appended, so a keyed
 *  `{#each}` can never throw on a duplicate. */
export function stepKeys(entries: readonly PlanEntry[]): string[] {
  const seen = new Set<string>();
  return entries.map((entry, index) => {
    let key = entry.id !== null ? `id:${entry.id}` : `ix:${index}`;
    while (seen.has(key)) key += `#${index}`;
    seen.add(key);
    return key;
  });
}
