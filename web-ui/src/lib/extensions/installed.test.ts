import { describe, expect, it, vi } from "vitest";
import { installedExtension } from "./installed";
import type { SurfaceMount } from "./application";
const scope = (signal: AbortSignal) => ({ signal }) as SurfaceMount;
describe("selected optional entry", () => {
  it("absence does not resolve a package or create work", () => { expect(installedExtension(null)).toBeNull(); });
  it("shares one original module across panes and refuses late retired mount", async () => {
    let finish!: (value: { default: unknown }) => void;
    const mount = vi.fn(async () => ({ update() {}, dispose() {} }));
    const load = vi.fn(() => new Promise<{ default: unknown }>((resolve) => { finish = resolve; }));
    const facade = installedExtension(load)!;
    const a = new AbortController(), c = new AbortController();
    const first = facade.mount("kept-review", {} as HTMLElement, scope(a.signal));
    const second = facade.mount("kept-review", {} as HTMLElement, scope(c.signal));
    await Promise.resolve(); a.abort();
    finish({ default: { version: 1, id: "chimaera-pro", mount } });
    await expect(first).rejects.toThrow("retired"); await second;
    expect(load).toHaveBeenCalledTimes(1); expect(mount).toHaveBeenCalledTimes(1);
  });
  it("does not automatically retry a failed selection or accept a wrong version", async () => {
    const load = vi.fn().mockResolvedValueOnce({ default: { version: 2, id: "chimaera-pro", mount: vi.fn() } })
      .mockResolvedValueOnce({ default: { version: 1, id: "chimaera-pro", mount: async () => ({ update() {}, dispose() {} }) } });
    const facade = installedExtension(load)!;
    await expect(facade.mount("kept-review", {} as HTMLElement, scope(new AbortController().signal))).rejects.toThrow();
    expect(load).toHaveBeenCalledTimes(1);
    await facade.mount("kept-review", {} as HTMLElement, scope(new AbortController().signal));
    expect(load).toHaveBeenCalledTimes(2);
  });
});
