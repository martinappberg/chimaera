/** Watching never takes PTY input or grid ownership. Remember explicit choices
 * for this window only; opening a phone view must not reflow the desktop. */
const defaultWatching = typeof matchMedia === "function" && matchMedia("(max-width: 700px)").matches;
const choices = $state<Record<string, boolean>>({});
const order: string[] = [];
export function isWatching(id: string): boolean { return choices[id] ?? defaultWatching; }
export function setWatching(id: string, value: boolean): void {
  if (!(id in choices)) order.push(id);
  choices[id] = value;
  while (order.length > 256) delete choices[order.shift()!];
}
