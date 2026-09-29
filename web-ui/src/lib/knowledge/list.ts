/**
 * The browse list's model: which entries a section shows, how they group
 * (findings by topic, to-dos by status, the rest by date), and the filter
 * chips that fit what is actually there. Pure; unit-tested in list.test.ts.
 */
import type { Todo } from "../workspace/knowledge";
import { byDateDesc, type Entry, type EntryKind } from "./entries";
import { isRetired, TODO_GROUP_LABEL, todoGroups } from "./overview";

export type Section = "findings" | "decisions" | "learnings" | "conventions" | "todos" | "sessions";

export const SECTION_KIND: Record<Section, EntryKind> = {
  findings: "finding",
  decisions: "decision",
  learnings: "learning",
  conventions: "convention",
  todos: "todo",
  sessions: "session",
};

export interface ListFilter {
  id: string;
  label: string;
  count: number;
  test: (e: Entry) => boolean;
}

export interface ListGroup {
  key: string;
  title: string;
  /** Mono title (a topic slug). */
  mono: boolean;
  subtitle: string;
  entries: Entry[];
  /** Starts folded (done to-dos). */
  folded: boolean;
}

/** The first word of a status as written ("supported", "established"). */
export function statusHead(stated: string): string {
  return stated.trim().toLowerCase().match(/^[a-z][a-z-]*/)?.[0] ?? "";
}

function numericId(id: string): number {
  const m = id.match(/(\d+)/);
  return m !== null ? Number(m[1]) : -1;
}

/** Newest first, then the higher id (F-231 above F-228 on one day). */
function newestFirst(entries: readonly Entry[]): Entry[] {
  return byDateDesc(entries).sort((a, b) => {
    if (a.date.slice(0, 10) !== b.date.slice(0, 10)) return 0;
    return numericId(b.id) - numericId(a.id);
  });
}

function chip(id: string, label: string, entries: readonly Entry[], test: (e: Entry) => boolean): ListFilter {
  return { id, label, count: entries.filter(test).length, test };
}

/**
 * The chips above a section's list, built from what its entries hold (a
 * chip that would match nothing isn't offered). "All" first, always.
 */
export function filtersFor(section: Section, entries: readonly Entry[], weekStart: string): ListFilter[] {
  const all: ListFilter = { id: "all", label: "All", count: entries.length, test: () => true };
  const out: ListFilter[] = [all];
  if (section === "findings" || section === "decisions") {
    out.push(chip("week", "This week", entries, (e) => e.date.slice(0, 10) >= weekStart));
    if (section === "findings") {
      out.push(chip("amended", "Corrected", entries, (e) => e.amends.length > 0 || e.state !== null));
      const heads = new Map<string, number>();
      for (const e of entries) {
        const h = statusHead(e.stated);
        if (h !== "") heads.set(h, (heads.get(h) ?? 0) + 1);
      }
      for (const [h] of [...heads.entries()].sort((a, b) => b[1] - a[1]).slice(0, 4)) {
        out.push(chip(`status:${h}`, h, entries, (e) => statusHead(e.stated) === h));
      }
    } else {
      out.push(chip("force", "In force", entries, (e) => !isRetired(e)));
      out.push(chip("retired", "Superseded", entries, (e) => isRetired(e)));
    }
  } else if (section === "learnings") {
    const cats = new Map<string, number>();
    for (const e of entries) {
      const c = e.learning?.category ?? "";
      if (c !== "" && c !== "other") cats.set(c, (cats.get(c) ?? 0) + 1);
    }
    for (const [c] of [...cats.entries()].sort((a, b) => b[1] - a[1]).slice(0, 5)) {
      out.push(chip(`cat:${c}`, c, entries, (e) => e.learning?.category === c));
    }
  }
  return out.filter((f) => f.id === "all" || (f.count > 0 && f.count < entries.length));
}

/** The groups a section's (already filtered) entries show in. */
export function listGroups(section: Section, entries: readonly Entry[]): ListGroup[] {
  if (section === "findings") {
    const byTopic = new Map<string, Entry[]>();
    for (const e of entries) {
      const list = byTopic.get(e.topic);
      if (list === undefined) byTopic.set(e.topic, [e]);
      else list.push(e);
    }
    const groups = [...byTopic.entries()].map(([topic, list]) => {
      const sorted = newestFirst(list);
      return {
        key: `topic:${topic}`,
        title: topic,
        mono: true,
        subtitle: list[0]?.topicOf?.description ?? "",
        entries: sorted,
        folded: false,
        newest: sorted.find((e) => e.date !== "")?.date ?? "",
      };
    });
    // The topic with the newest entry first: where the work is now.
    return groups
      .sort((a, b) => (a.newest === b.newest ? a.title.localeCompare(b.title) : b.newest.localeCompare(a.newest)))
      .map(({ newest: _n, ...g }) => g);
  }
  if (section === "todos") {
    return todoGroups(entries).map(({ group, items }) => ({
      key: `todo:${group}`,
      title: TODO_GROUP_LABEL[group],
      mono: false,
      subtitle: "",
      entries: items,
      folded: group === "done",
    }));
  }
  return [{ key: section, title: "", mono: false, subtitle: "", entries: newestFirst(entries), folded: false }];
}

/** What a to-do row clamps under its title: the rest of the item. */
export function todoRest(t: Todo): string {
  if (t.item === t.title) return "";
  // The title is the item's first sentence (markdown stripped): the rest
  // is what follows that sentence in the item as written.
  const first = t.item.match(/^[\s\S]*?[.!?](?=\s|$)/);
  const rest = first !== null ? t.item.slice(first[0].length) : "";
  return rest.replace(/^[\s.:;—–-]+/, "").trim();
}
