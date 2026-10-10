import { afterEach, expect, it, vi } from "vitest";

// The default is decided once per window at module load, so each case loads a
// fresh copy under its own window location and viewport.
async function load(path: string, narrow: boolean) {
  vi.resetModules();
  vi.stubGlobal("location", new URL(`https://fixture.invalid${path}`));
  vi.stubGlobal("matchMedia", (query: string) => ({ matches: narrow && query.includes("max-width") }));
  return import("./viewerMode.svelte");
}

afterEach(() => {
  vi.unstubAllGlobals();
});

it("a narrow native window keeps terminal control", async () => {
  const mode = await load("/", true);
  expect(mode.isWatching("s-local")).toBe(false);
});

it("only a phone-width browser view of a project starts out watching", async () => {
  expect((await load("/workspace/w-one/", true)).isWatching("s-remote")).toBe(true);
  expect((await load("/workspace/w-one/", false)).isWatching("s-remote")).toBe(false);
});

it("an explicit choice wins over the default", async () => {
  const mode = await load("/workspace/w-one/", true);
  mode.setWatching("s-remote", false);
  expect(mode.isWatching("s-remote")).toBe(false);
});
