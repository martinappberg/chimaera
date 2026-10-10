import { describe, expect, it, vi } from "vitest";
import { installedExtension } from "./installed";
import type { AccountBrandingSubscription } from "./accountPresentation";
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

it("fences original branding publication and disposes a late subscription without adopting it", async () => {
  let resolve!: (stop: () => void) => void;
  let captured!: AccountBrandingSubscription;
  const stop = vi.fn(), publish = vi.fn();
  const binder = vi.fn((value: AccountBrandingSubscription) => {
    captured = value;
    return new Promise<() => void>((finish) => { resolve = finish; });
  });
  const facade = installedExtension(async () => ({ default: { version: 1, id: "chimaera-pro", mount: vi.fn(), bindAccountBranding: binder } }))!;
  const owner = new AbortController();
  const pending = facade.bindAccountBranding!({ signal: owner.signal, publish,
    environment: { native: true, gateway: false, local: true, workbench: "/" },
  });
  // Wait for the actual original module/binding admission before retiring it.
  await vi.waitFor(() => expect(binder).toHaveBeenCalledTimes(1));
  captured.publish({ plan: "pro", offered: true, signedOut: false });
  expect(publish).toHaveBeenCalledTimes(1);
  owner.abort();
  captured.publish({ plan: "max", offered: true, signedOut: false });
  resolve(stop);
  await expect(pending).rejects.toThrow("ended");
  expect(stop).toHaveBeenCalledTimes(1);
  expect(publish).toHaveBeenCalledTimes(1);
});

it("gateway module admission consumes the original ten seconds and never starts a late lookup", async () => {
  vi.useFakeTimers();
  let finish!: (value: { default: unknown }) => void;
  const bind = vi.fn(() => () => {});
  const facade = installedExtension(() => new Promise<{ default: unknown }>((resolve) => { finish = resolve; }))!;
  const scope = { signal: new AbortController().signal, publish: vi.fn(), environment: { native: false, gateway: true, local: true, workbench: "/" } };
  const pending = facade.bindAccountBranding!(scope);
  const rejected = expect(pending).rejects.toThrow("ended");
  try {
    await vi.advanceTimersByTimeAsync(10_000);
    await rejected;
    finish({ default: { version: 1, id: "chimaera-pro", mount: vi.fn(), bindAccountBranding: bind } });
    await Promise.resolve(); await Promise.resolve();
    expect(bind).not.toHaveBeenCalled(); expect(scope.publish).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
  } finally { vi.useRealTimers(); }
});


it("an already expired original gateway deadline starts no package load", async () => {
  vi.useFakeTimers();
  const clock = vi.spyOn(performance, "now").mockReturnValue(10_001);
  const load = vi.fn();
  const facade = installedExtension(load)!;
  const parent = new AbortController();
  try {
    await expect(facade.bindAccountBranding!({ signal: parent.signal, startupDeadline: 10_000,
      publish: vi.fn(), environment: { native: false, gateway: true, local: true, workbench: "/" },
    })).rejects.toThrow("ended");
    expect(load).not.toHaveBeenCalled(); expect(vi.getTimerCount()).toBe(0);
  } finally { parent.abort(); clock.mockRestore(); vi.useRealTimers(); }
});
