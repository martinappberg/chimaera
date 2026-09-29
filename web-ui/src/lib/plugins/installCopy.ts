/**
 * The words on a plugin card, shared by the card and the attach sheet's
 * install step (one flow, one wording): the Install button's version and
 * tooltip, the state beside the switch, the "Here" line, the outcome lines,
 * the quiet "checked 2 hours ago", and when Reinstall applies. Pure;
 * `installCopy.test.ts`.
 */
import type { ActivityEntry, PluginChange, TrustAsk, WorkspacePlugin } from "./store";

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

/** A download's size as a person reads it: "≈ 130 KB", "≈ 1.2 MB" — with
 *  no-break spaces, so a narrow card never wraps the "≈" away from it. */
export function approxSize(bytes: number): string {
  const nb = "\u00a0";
  if (bytes < 1000 * 1000) return `≈${nb}${Math.max(1, Math.round(bytes / 1000))}${nb}KB`;
  const mb = bytes / (1000 * 1000);
  return `≈${nb}${mb < 10 ? mb.toFixed(1).replace(/\.0$/, "") : Math.round(mb)}${nb}MB`;
}

/** The line under a card opened before install: where Install downloads
 *  from (and how much, when the daemon knows), and that it stays off. */
export function installLine(repo: string | null, wasmBytes: number | null): string {
  const from = repo !== null ? `github.com/${repo}` : "its release";
  const size = wasmBytes !== null ? ` (${approxSize(wasmBytes)})` : "";
  return `Installing downloads it from ${from}${size} into this host's ~/.chimaera/plugins. It does nothing until you switch it on in a workspace.`;
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
  if (p.hold?.kind === "untrusted") return "waiting for your trust";
  if (p.hold !== null) return "off on this host";
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

/** The two letters on the card's tile, from the plugin's name (core names no
 *  plugin): the first two words' initials ("Agent notes" → "an"), else the
 *  name's first two letters ("Mycelium" → "my"). */
export function tileLetters(name: string): string {
  const words = name.toLowerCase().split(/[^\p{L}\p{N}]+/u).filter((w) => w !== "");
  if (words.length >= 2) return words[0][0] + words[1][0];
  return (words[0] ?? "").slice(0, 2);
}

/** Where an installed copy came from, as the trust prompt says it. */
export function sourceWords(p: WorkspacePlugin): string {
  if (p.local_path !== null) return `a local build in ${p.local_path}`;
  if (p.repo !== null) return `github.com/${p.repo}`;
  return "an unknown source";
}

/** The trust prompt for an installed build waiting for trust (its card's
 *  Trust): what the daemon would ask, from the card's own fields. */
export function trustAskFor(p: WorkspacePlugin): TrustAsk {
  return {
    id: p.id,
    name: p.name,
    version: p.version,
    source: sourceWords(p),
    tier: p.tier,
    caps: p.caps,
    can: p.can,
    grown: null,
    from_version: null,
    confirm: p.tier === "privileged" ? p.name : null,
  };
}

/** The card's callout for a plugin that can't run on this host: why, in
 *  plain words, and the one thing the user can do about it (if any). */
export function holdWords(p: WorkspacePlugin): { text: string; action: "trust" | "allow" | null } | null {
  const h = p.hold;
  if (h === null) return null;
  const who = `${p.name} ${p.version}`.trim();
  switch (h.kind) {
    case "untrusted":
      return {
        text: `${p.name} waits for your trust: nothing it adds works until you trust what it can do.`,
        action: "trust",
      };
    case "blocked":
      return h.level === "soft"
        ? { text: `Chimaera turned ${who} off: ${h.reason}. You can switch it back on anyway.`, action: "allow" }
        : { text: `Chimaera blocked ${who}: ${h.reason}. Update or remove it.`, action: null };
    case "policy":
      return { text: `Off on this host: ${h.reason}.`, action: null };
  }
}

/** The trust prompt's title and confirm label, by what it asks for. */
export function trustWords(ask: TrustAsk, mode: "install" | "update" | "trust"): { title: string; lead: string; confirm: string } {
  const who = `${ask.name} ${ask.version}`.trim();
  const grown = ask.grown !== null && ask.grown.length > 0;
  if (mode === "update" && grown) {
    return {
      title: `Allow ${ask.name} to do more?`,
      lead:
        ask.from_version !== null
          ? `${who} would also do things ${ask.from_version} doesn't. ${ask.from_version} keeps running until you decide.`
          : `${who} would also do things the version you have doesn't. It keeps running until you decide.`,
      confirm: "Allow update",
    };
  }
  return {
    title: `Trust ${ask.name}?`,
    lead: `${who} from ${ask.source}. The Chimaera maintainers haven't verified it, so it runs only if you trust what it can do.`,
    confirm: mode === "install" ? "Trust and install" : mode === "update" ? "Trust and update" : "Trust it",
  };
}

const str = (v: unknown): string => (typeof v === "string" ? v : "");

/** One line of the activity log, in words. */
export function activityWords(e: ActivityEntry): string {
  const v = str(e.version);
  switch (e.kind) {
    case "install":
      return `Installed ${v}${str(e.source) !== "" ? ` from ${str(e.source)}` : ""}`;
    case "update":
      return `Updated to ${v}${str(e.from) !== "" ? ` (was ${str(e.from)})` : ""}`;
    case "rollback":
      return `Went back to ${v}${str(e.from) !== "" ? ` from ${str(e.from)}` : ""}`;
    case "remove":
      return "Removed from this host";
    case "trust":
      return e.how === "subset"
        ? `Trusted ${v}: it asked for nothing new`
        : e.how === "grandfathered"
          ? "Trusted as already installed"
          : `You trusted what ${v} can do`;
    case "untrust":
      return "You withdrew your trust";
    case "blocked":
      return `Chimaera blocked ${v}`;
    case "allow-block":
      return `You switched ${v} back on despite a block${str(e.reason) !== "" ? ` (${str(e.reason)})` : ""}`;
    case "skip":
      return `You skipped ${v}`;
    case "job": {
      const args = Array.isArray(e.args) ? e.args.filter((a): a is string => typeof a === "string") : [];
      const how =
        e.cancelled === true
          ? "stopped"
          : e.timed_out === true
            ? "ran out of time"
            : typeof e.exit === "number"
              ? `exit ${e.exit}`
              : "ended by a signal";
      const secs = typeof e.duration_ms === "number" ? `, ${(e.duration_ms / 1000).toFixed(1)} s` : "";
      return `Ran ${[str(e.program), ...args].join(" ")} (${how}${secs})`;
    }
    case "tool-install":
      return `Downloaded ${str(e.tool)} ${v}${str(e.url) !== "" ? ` from ${str(e.url)}` : ""}`;
    case "tool-install-failed":
      return `Couldn't install ${str(e.tool)} ${v}${str(e.error) !== "" ? `: ${str(e.error)}` : ""}`;
    case "tool-remove":
      return `Removed its ${str(e.tool)}`;
    default:
      return e.kind;
  }
}

/** When an activity entry happened: "29 Sep 2026, 14:03". */
export function activityTime(ts: number): string {
  return new Date(ts).toLocaleString(undefined, {
    day: "numeric",
    month: "short",
    year: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}
