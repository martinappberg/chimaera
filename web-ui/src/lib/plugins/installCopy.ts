/**
 * The words around installing a plugin, shared by the card's **Install
 * x.y.z** button and the attach sheet's install step (one flow, one
 * wording). Pure; `installCopy.test.ts`.
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
