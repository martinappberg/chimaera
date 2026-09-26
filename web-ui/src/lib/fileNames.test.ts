import { readdirSync } from "node:fs";
import { join, relative } from "node:path";
import { describe, expect, it } from "vitest";

// macOS and Windows file systems ignore case, Linux (and so CI's ui job) does
// not. An import of `./x.svelte` meant for `x.svelte.ts` resolves to a sibling
// `X.svelte` component there, and the app bundle build fails only on those
// hosts. Catch any name that differs from another only by case, and the
// `x.svelte.ts` / `X.svelte` pair in particular, here on every platform.
const SRC = join(__dirname, "..");

function files(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    return entry.isDirectory() ? files(path) : [path];
  });
}

describe("source file names", () => {
  const all = files(SRC).map((path) => relative(SRC, path));

  it("never differ from another only by letter case", () => {
    const seen = new Map<string, string>();
    const clashes: string[] = [];
    for (const name of all) {
      const key = name.toLowerCase();
      const other = seen.get(key);
      if (other !== undefined) clashes.push(`${other} ~ ${name}`);
      seen.set(key, name);
    }
    expect(clashes).toEqual([]);
  });

  it("never make an `x.svelte` import ambiguous between a module and a component", () => {
    const lower = new Map(all.map((name) => [name.toLowerCase(), name]));
    const clashes = all
      .filter((name) => name.endsWith(".svelte.ts") || name.endsWith(".svelte.js"))
      .flatMap((module) => {
        const component = lower.get(module.slice(0, -3).toLowerCase());
        return component === undefined ? [] : [`${module} ~ ${component}`];
      });
    expect(clashes).toEqual([]);
  });
});
