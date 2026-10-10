import { afterEach, expect, it, vi } from "vitest";
import type { SurfaceMount } from "./application";
import keptViewSource from "./KeptApplicationView.svelte?raw";
import accountViewSource from "./AccountApplicationView.svelte?raw";

afterEach(() => {
  vi.doUnmock("virtual:chimaera-application-entry");
  vi.doUnmock("./installed");
  vi.resetModules();
});

it("literal-null selection never constructs a facade or resolves a package", async () => {
  vi.resetModules();
  const construct = vi.fn();
  vi.doMock("./installed", () => ({ installedExtension: construct }));
  vi.doMock("virtual:chimaera-application-entry", () => ({ loadApplicationEntry: null }));
  const { selectedApplication } = await import("./selected");
  expect(selectedApplication).toBeNull(); expect(construct).not.toHaveBeenCalled();
});

it("kept and account callers use one selected facade and one module across original mounts", async () => {
  vi.resetModules();
  const mount = vi.fn(async () => ({ update() {}, dispose() {} }));
  const load = vi.fn(async () => ({ default: { version: 1, id: "chimaera-pro", mount } }));
  const actual = await vi.importActual<typeof import("./installed")>("./installed");
  const construct = vi.fn(actual.installedExtension);
  vi.doMock("./installed", () => ({ installedExtension: construct }));
  vi.doMock("virtual:chimaera-application-entry", () => ({ loadApplicationEntry: load }));
  const first = (await import("./selected")).selectedApplication!;
  const second = (await import("./selected")).selectedApplication!;
  expect(first).toBe(second); expect(construct).toHaveBeenCalledTimes(1);
  const scope = (signal: AbortSignal) => ({ signal }) as SurfaceMount;
  await Promise.all([
    first.mount("kept-review", {} as HTMLElement, scope(new AbortController().signal)),
    second.mount("account", {} as HTMLElement, scope(new AbortController().signal)),
  ]);
  expect(load).toHaveBeenCalledTimes(1); expect(mount).toHaveBeenCalledTimes(2);
  // Prevent either actual caller from reintroducing its own module-level facade.
  for (const source of [keptViewSource, accountViewSource]) {
    expect(source).toContain('from "./selected"');
    expect(source).not.toContain("installedExtension(");
  }
});
