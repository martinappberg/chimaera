/**
 * The groups the Extensions page sorts plugins into, in the order it shows
 * them, with the page's word for each. The ids are the daemon's
 * (`CATEGORIES` in `crates/chimaera-server/src/plugins/mod.rs`): a plugin's
 * `category` on the wire is one of them or null, and anything else (a
 * daemon that knows a group this page doesn't) is listed under Other.
 * Pure; `categories.test.ts`.
 */
export const CATEGORIES: readonly { id: string; label: string }[] = [
  { id: "writing", label: "Writing" },
  { id: "presentations", label: "Presentations" },
  { id: "data", label: "Data" },
  { id: "biology", label: "Biology" },
  { id: "knowledge", label: "Knowledge" },
  { id: "agents", label: "Agent tools" },
];

export const OTHER = { id: "other", label: "Other" } as const;

export interface PluginGroup<T> {
  id: string;
  label: string;
  plugins: T[];
}

/**
 * `plugins` under their categories: the groups in the page's order, Other
 * last, empty ones left out, each keeping the order it was given. Nothing
 * categorised (a daemon that predates categories) is one unnamed group, so
 * the page draws the plain list it always did.
 */
export function groupByCategory<T extends { category?: string | null }>(plugins: readonly T[]): PluginGroup<T>[] {
  const known = new Map<string, T[]>(CATEGORIES.map((c) => [c.id, []]));
  const other: T[] = [];
  for (const p of plugins) (known.get(p.category ?? "") ?? other).push(p);
  const groups: PluginGroup<T>[] = CATEGORIES.map((c) => ({ ...c, plugins: known.get(c.id) ?? [] })).filter((g) => g.plugins.length > 0);
  if (groups.length === 0) return other.length === 0 ? [] : [{ id: OTHER.id, label: "", plugins: other }];
  if (other.length > 0) groups.push({ ...OTHER, plugins: other });
  return groups;
}
