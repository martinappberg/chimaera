/**
 * The effort control reads faster → smarter, left to right. Agents list their
 * levels in their own order (Grok: `xhigh` first), so levels every agent
 * means the same thing by are placed by rank. A list with a level outside
 * that vocabulary keeps the agent's order: never guess a ladder. Labels are
 * never changed.
 */
const RANK = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];

export function effortScale(levels: readonly string[]): string[] {
  if (levels.some((level) => !RANK.includes(level))) return [...levels];
  return [...levels].sort((a, b) => RANK.indexOf(a) - RANK.indexOf(b));
}
