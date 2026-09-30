import { describe, expect, it } from "vitest";

// Settings → Chimaera Pro never makes the user think about a machine: the
// cloud is "your cloud", available when needed, not a computer that sleeps
// and wakes (docs/features/pro.md, Cloud readiness). The only sentences that
// may mention waking a sleeping project are the chat and terminal lines
// attached to the user's own action (`net/api.ts` `ASLEEP_NOTE`, the chat
// header), which live outside these files.
const sources = Object.fromEntries(Object.entries(import.meta.glob<string>([
  "/src/lib/pro/**/*.{svelte,ts}",
  "/src/lib/settings/CloudSetup.svelte",
  "/src/lib/settings/ProSettings.svelte",
  "/src/lib/settings/MirrorSettings.svelte",
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

describe("Chimaera Pro vocabulary", () => {
  it("scans the Pro and cloud settings sources", () => {
    expect(Object.keys(sources)).toEqual(expect.arrayContaining([
      "/src/lib/settings/CloudSetup.svelte", "/src/lib/pro/ProviderConnections.svelte", "/src/lib/pro/presentation.ts", "/src/lib/pro/providers.ts",
    ]));
  });
  it("never calls the cloud a machine", () => {
    expect(hits(/cloud[\s-]+machines?/gi)).toEqual([]);
    expect(hits(/\bmachines?\b/gi).filter(hit => !SSH_HOSTS.some(phrase => hit.includes(phrase)))).toEqual([]);
  });
  it("never narrates the cloud sleeping or waking on its own", () => {
    expect(hits(/waking up|is asleep|wakes it|going to sleep|starting up|still starting/gi)).toEqual([]);
  });
});
