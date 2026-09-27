/**
 * The words on a plugin card, shared by the card and the attach sheet's
 * install step (one flow, one wording): the Install button's version and
 * tooltip, the state beside the switch, the "Here" line, the outcome lines,
 * the quiet "checked 2 hours ago", and when Reinstall applies. Pure;
 * `installCopy.test.ts`.
 */
import type { PluginChange, WorkspacePlugin } from "./store";

/** A one-line outcome. No checksums in it: the daemon verifies every
 *  download, and only a failure is news (the refusal, or the card's fault). */
export interface Outcome {
  text: string;
}

/** The version an Install button installs: the one chimaera pins. */
export function pinnedVersion(p: WorkspacePlugin): string {
  return p.pinned_version ?? p.version;
}

/** The Install button's tooltip for a first-party plugin not installed yet. */
export function installTitle(p: WorkspacePlugin): string {
  const from = p.repo !== null ? `github.com/${p.repo}` : "its release";
  return `Downloads it from ${from} into this host's ~/.chimaera/plugins. It does nothing until you switch it on in a workspace.`;
}

/** "installed Agent notes 0.1.1". */
export function installedOutcome(c: PluginChange, fallbackName: string): Outcome {
  const raw = (c.plugin as { name?: unknown } | null | undefined)?.name;
  const name = typeof raw === "string" && raw !== "" ? raw : fallbackName;
  return { text: `installed ${name}${c.version ? ` ${c.version}` : ""}` };
}

/** "updated to 0.1.1". */
export function updatedOutcome(c: PluginChange): Outcome {
  return { text: `updated to ${c.version ?? ""}` };
}

/** The words beside the switch: what the switch means in THIS workspace. */
export function stateWords(p: WorkspacePlugin): string {
  if (p.active) return "active here";
  if (p.on) return "on · not set up here yet";
  return "off";
}

/** The folder or file a plugin's footprint names, as a person would say
 *  it: `.living/INDEX.md` → `.living/`, `MYCELIUM.md` stays. */
export function footprint(p: WorkspacePlugin): string | null {
  const first = p.detect[0];
  if (first === undefined) return null;
  const slash = first.indexOf("/");
  return slash > 0 ? first.slice(0, slash + 1) : first;
}

const plural = (n: number, one: string): string => `${n} ${one}${n === 1 ? "" : "s"}`;

/** The card's "Here" line: what the plugin found in this workspace, only
 *  when that means something — `kind` says which way the line reads (the
 *  card adds "Set it up" to `setup`). A plugin with no footprint (it is
 *  always present) has none. */
export function hereLine(
  p: WorkspacePlugin,
  counts: { findings: number; decisions: number } | null,
): { text: string; kind: "using" | "found" | "setup" } | null {
  const where = footprint(p);
  if (where === null) return null;
  if (p.active) {
    const tail = counts !== null ? ` · ${plural(counts.findings, "finding")} · ${plural(counts.decisions, "decision")}` : "";
    return { text: `using ${where} in this workspace${tail}`, kind: "using" };
  }
  if (p.detected) return { text: `found ${where} in this workspace — switch it on to use it`, kind: "found" };
  if (p.on && p.setup !== null) return { text: "not set up in this workspace yet", kind: "setup" };
  return null;
}

/** "checked just now" · "checked 5 minutes ago" · "checked 2 hours ago". */
export function checkedWords(checkedMs: number, nowMs: number): string {
  const s = Math.max(0, Math.floor((nowMs - checkedMs) / 1000));
  if (s < 60) return "checked just now";
  const m = Math.floor(s / 60);
  if (m < 60) return `checked ${plural(m, "minute")} ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `checked ${plural(h, "hour")} ago`;
  return `checked ${plural(Math.floor(h / 24), "day")} ago`;
}

/** Reinstall mends a copy whose files stopped matching its release: it has
 *  a fault, isn't verified, and came from a repository (a local build is
 *  re-added from its directory instead). */
export function canReinstall(p: WorkspacePlugin): boolean {
  return p.installed && p.fault !== null && !p.verified && p.repo !== null && p.local_path === null;
}

/** The plugin's repository page (the menu's "Open on GitHub"). */
export function repoUrl(p: WorkspacePlugin): string | null {
  return p.repo !== null ? `https://github.com/${p.repo}` : null;
}

/** The two letters (or TeX) on the card's tile. */
export function tileLetters(id: string): string {
  if (id === "mycelium") return "my";
  if (id === "agent-notes") return "an";
  if (id === "latex") return "TeX";
  return id.replace(/[^a-z0-9]/g, "").slice(0, 2);
}
