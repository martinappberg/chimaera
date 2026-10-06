/**
 * The row a cycle chord lands on (next agent, next terminal): the one after
 * `current` in `ids` — one sidebar group, in the order the rail lists it —
 * wrapping at the end. From outside the group (a file tab, the other group,
 * nothing focused) it enters at the first row. Null only for an empty group.
 */
export function nextInGroup(ids: readonly string[], current: string | null): string | null {
  if (ids.length === 0) return null;
  const at = current === null ? -1 : ids.indexOf(current);
  return ids[(at + 1) % ids.length];
}
