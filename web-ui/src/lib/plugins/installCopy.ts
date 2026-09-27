/**
 * The words around installing a plugin, shared by the card's **Install
 * x.y.z** button and the attach sheet's install step (one flow, one
 * wording). Pure; `installCopy.test.ts`.
 */
import type { PluginChange, WorkspacePlugin } from "./store";

/** A one-line outcome; `title` carries what the line shortens. */
export interface Outcome {
  text: string;
  title?: string;
}

/** The version an Install button installs: the one chimaera pins. */
export function pinnedVersion(p: WorkspacePlugin): string {
  return p.pinned_version ?? p.version;
}

/** The Install button's tooltip for a first-party plugin not installed yet. */
export function installTitle(p: WorkspacePlugin): string {
  const v = pinnedVersion(p);
  const from = p.repo !== null ? `github.com/${p.repo}/releases/v${v}` : `its v${v} release`;
  return `downloads plugin.wasm and plugin.toml from ${from} into ~/.chimaera/plugins on this host, verifies both checksums, runs sandboxed inside chimaera, and does nothing until switched on`;
}

function verifiedSha(c: PluginChange): Outcome {
  const sha = c.sha256?.["plugin.wasm"];
  return sha
    ? { text: ` · plugin.wasm sha256 ${sha.slice(0, 16)}… verified`, title: `plugin.wasm sha256 ${sha}` }
    : { text: "" };
}

/** "installed Agent notes 0.1.0 · plugin.wasm sha256 1a2b…… verified". */
export function installedOutcome(c: PluginChange, fallbackName: string): Outcome {
  const raw = (c.plugin as { name?: unknown } | null | undefined)?.name;
  const name = typeof raw === "string" && raw !== "" ? raw : fallbackName;
  const sha = verifiedSha(c);
  return { text: `installed ${name}${c.version ? ` ${c.version}` : ""}${sha.text}`, title: sha.title };
}

/** "updated to 0.1.1 · plugin.wasm sha256 …… verified". */
export function updatedOutcome(c: PluginChange): Outcome {
  const sha = verifiedSha(c);
  return { text: `updated to ${c.version ?? ""}${sha.text}`, title: sha.title };
}
