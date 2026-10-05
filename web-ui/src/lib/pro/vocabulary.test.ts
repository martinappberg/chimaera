import { describe, expect, it } from "vitest";
import { ASLEEP_NOTE, plainError, runningElsewhere } from "../net/api";
import { PlacementError, pauseLabel, placementLabel, projectWhereLabel, refusalWords } from "../net/placement";
import { refusalText, statusText } from "../terminal/refusals.svelte";
import { parseOwnershipHints } from "../workspace/placementHints";
import { projectCopyError } from "./projectCopy";

// Pro surfaces name places, never the machinery that moves work between
// them (docs/features/pro.md, "Words"): the cloud is "the cloud" or "your
// cloud", a computer is "this computer" or its name, and an action in
// progress reads as that action's progress. The only sentences allowed to
// say a machine are about the user's own SSH hosts.
const BANNED = /\b(asleep|sleeping|waking|wake[sn]?|owner|lease[sd]?|checkpoints?|epochs?|baton|worker|keeper|mirror(?:s|ed|ing)?|take over|takeover|moving execution|execution|index snapshots?|copy-only)\b/i;

const sources = Object.fromEntries(Object.entries(import.meta.glob<string>([
  "/src/lib/pro/**/*.{svelte,ts}",
  "/src/lib/extensions/**/*.{svelte,ts}",
  "/src/lib/workspace/placementHints.ts",
  "/src/lib/net/placement.ts",
  "/src/lib/terminal/refusals.svelte.ts",
], { query: "?raw", import: "default", eager: true })).filter(([file]) => !file.endsWith(".test.ts")));

// Cluster logins the user added on Home (their own SSH hosts, which Home
// lists as "Remote machines"), never the cloud.
const SSH_HOSTS = ["Connected machines", "Add remote machines on Home", "No machines to show yet"];

/** What the user can read: without comments, styles or path data. */
function copy(src: string): string {
  return src
    .replace(/<style[\s\S]*?<\/style>/g, "")
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/<!--[\s\S]*?-->/g, "")
    .replace(/(^|\s)\/\/.*$/gm, "$1")
    .replace(/\sd="[^"]*"/g, "");
}

function hits(pattern: RegExp): string[] {
  const found: string[] = [];
  for (const [file, raw] of Object.entries(sources)) {
    for (const line of copy(raw).split("\n")) {
      for (const match of line.matchAll(pattern)) {
        const at = match.index ?? 0;
        found.push(`${file}: …${line.slice(Math.max(0, at - 60), at + 60).trim()}…`);
      }
    }
  }
  return found;
}

/** Sentences in a source: quoted literals with a space (wire codes and
 *  identifiers have none) and Svelte markup text. */
function sentences(file: string, raw: string): string[] {
  const text = copy(raw);
  // Whole literals scanned left to right, so a match never spans code.
  const quoted = [...text.matchAll(/"((?:[^"\\\n]|\\.)*)"|'((?:[^'\\\n]|\\.)*)'|`((?:[^`\\\n]|\\.)*)`/g)].map(match => match[1] ?? match[2] ?? match[3]);
  const markup = file.endsWith(".svelte") ? [...text.matchAll(/>([^<>{}\n]*[A-Za-z][^<>{}\n]*)</g)].map(match => match[1]) : [];
  return [...quoted, ...markup].filter(value => value.trim().includes(" "));
}

describe("Chimaera Pro vocabulary", () => {
  it("scans the public Pro copy sources", () => {
    expect(Object.keys(sources)).toEqual(expect.arrayContaining([
      "/src/lib/pro/presentation.ts", "/src/lib/pro/providers.ts", "/src/lib/net/placement.ts",
      "/src/lib/terminal/refusals.svelte.ts", "/src/lib/extensions/PlaceSlot.svelte",
    ]));
  });
  it("never calls the cloud a machine", () => {
    expect(hits(/cloud[\s-]+machines?/gi)).toEqual([]);
    expect(hits(/\bmachines?\b/gi).filter(hit => !SSH_HOSTS.some(phrase => hit.includes(phrase)))
      // Wire codes and identifiers, not words anyone reads.
      .filter(hit => !/on_other_machine|machine_id/.test(hit))).toEqual([]);
  });
  it("never narrates the cloud sleeping or waking on its own", () => {
    expect(hits(/waking up|is asleep|wakes it|going to sleep|starting up|still starting/gi)).toEqual([]);
  });
  it("names places, never the machinery, in every sentence of these sources", () => {
    const found = Object.entries(sources).flatMap(([file, raw]) => sentences(file, raw)
      .filter(sentence => BANNED.test(sentence))
      .map(sentence => `${file}: ${sentence}`));
    expect(found).toEqual([]);
  });
  it("names places in the shared project, chat and terminal states", () => {
    const said: string[] = [ASLEEP_NOTE, new PlacementError(503).message, projectWhereLabel(null),
      projectWhereLabel({ where: "cloud", asleep: true }), projectWhereLabel({ where: "computer", asleep: false })];
    for (const code of ["project_unavailable", "remote_unavailable", "workspace_scope_changed", "workspace_unavailable", "worker_asleep", "on_other_machine", "workspace_owned_elsewhere", "read_only"]) {
      for (const where of ["cloud", "computer", "other", null] as const) said.push(plainError(code, where));
    }
    for (const where of ["cloud", "computer", "other", null] as const) said.push(runningElsewhere(where));
    for (const remote of ["worker-a", "device-b", "opaque"]) {
      for (const owner of ["asleep", "waking", "bringing", null] as const) said.push(placementLabel({ remote }, false, { owner }) ?? "");
    }
    for (const reason of ["watching", "busy", "reconnecting", "waking", "bringing", "still_working", "elsewhere", null]) said.push(refusalText(reason, null));
    for (const status of ["asleep", "waking", "bringing", "bringing-computer"] as const) said.push(statusText(status), statusText(status, true));
    for (const pause of [null, { type: "moved", to: "cloud" }, { type: "moved", to: "computer" }, { type: "moved", to: "other" }, { type: "moved", to: "elsewhere" },
      { type: "paused", reason: "needs_provider", provider: "claude" }, { type: "paused", reason: "restarting", provider: null },
      { type: "paused", reason: "stays_on_computer", provider: null }, { type: "paused", reason: "elsewhere", provider: null }] as const) {
      for (const signedOut of [false, true]) {
        const label = pauseLabel(pause as Parameters<typeof pauseLabel>[0], { signedOut });
        said.push(label.status, label.detail ?? "");
      }
    }
    for (const state of ["remote", "hydrating", "transferring"]) {
      said.push(...parseOwnershipHints({ configured: true, workspaces: [{ workspace_id: "w", ownership: { state } }] }).hints.values());
    }
    // A relay's own machinery words never reach the chat for these reasons.
    for (const reason of ["waking", "bringing", "reconnecting"]) said.push(refusalWords("Still waking the cloud machine. That was not sent.", reason));
    for (const code of ["project_busy", "project_copy_update_required", "project_checkpoint_pending", "project_unavailable", "anything"]) said.push(projectCopyError(code));
    expect(said.filter(sentence => BANNED.test(sentence))).toEqual([]);
  });
});
