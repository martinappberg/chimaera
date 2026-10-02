/** Provider catalogs use picker aliases, resolved ids, and context suffixes. */
export interface ModelChoice {
  id: string;
  label: string;
  resolved?: string | null;
  description?: string | null;
}

export function isRealModel(value: unknown): value is string {
  return typeof value === "string" && value.trim().length > 0 && !value.trim().startsWith("<");
}

export function modelChoice<T extends ModelChoice>(choices: T[], target: string | null): T | undefined {
  if (!isRealModel(target)) return undefined;
  const picked = choices.find((m) => m.id === target);
  if (picked) return picked;
  const named = choices.filter((m) => m.id !== "default");
  const ordered = [...named, ...choices.filter((m) => m.id === "default")];
  const exact = ordered.find((m) => m.resolved === target);
  if (exact) return exact;
  const bare = (s: string) => s.replace(/\[[^\]]*\]$/, "");
  return ordered.find((m) => bare(m.resolved ?? m.id) === bare(target));
}
