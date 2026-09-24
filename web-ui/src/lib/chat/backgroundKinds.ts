import type { BackgroundTask } from "./store.svelte";

/** What a background lane IS, in words. The wire's lane names are canonical
 *  but opaque — `local_bash` covers both a backgrounded command and a Monitor
 *  watch — so the tray, the waiting line and the tray label share one
 *  vocabulary. */
export type BackgroundKind = "monitor" | "command" | "agent" | "workflow" | "task";

export function backgroundKind(t: BackgroundTask): BackgroundKind {
  if (t.monitor) return "monitor";
  switch (t.taskType) {
    case "local_bash":
      return "command";
    case "local_agent":
    case "remote_agent":
      return "agent";
    case "local_workflow":
      return "workflow";
    default:
      return "task";
  }
}

const ORDER: BackgroundKind[] = ["agent", "command", "monitor", "workflow", "task"];

/** "1 agent · 2 commands" — counts per kind, in a stable order. */
export function countKinds(tasks: BackgroundTask[], skip: BackgroundKind[] = []): string {
  const counts = new Map<BackgroundKind, number>();
  for (const t of tasks) {
    const k = backgroundKind(t);
    if (!skip.includes(k)) counts.set(k, (counts.get(k) ?? 0) + 1);
  }
  return ORDER.filter((k) => counts.has(k))
    .map((k) => {
      const n = counts.get(k) ?? 0;
      return `${n} ${k}${n === 1 ? "" : "s"}`;
    })
    .join(" · ");
}
