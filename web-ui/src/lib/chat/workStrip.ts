export type WorkPart = "goal" | "plan" | "agents" | "background" | "schedule";

/** One inventory for both the folding threshold and the visible buttons. */
export function workParts(present: Record<WorkPart, boolean>): WorkPart[] {
  const order: WorkPart[] = ["goal", "plan", "agents", "background", "schedule"];
  return order.filter((part) => present[part]);
}

/** Keep the compact layout through a brief gap as background work settles. */
export function foldWork(parts: WorkPart[], wasFolded: boolean): boolean {
  if (parts.length >= 3) return true;
  if (parts.length <= 1) return false;
  return wasFolded;
}
