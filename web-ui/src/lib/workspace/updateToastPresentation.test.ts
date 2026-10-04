import { expect, it, vi } from "vitest";
import { updateToastPresentation, type ToastPresentation } from "./updateToastPresentation";
import loaderSource from "./UpdateToastLoader.svelte?raw";
import appSource from "../../App.svelte?raw";

type ToastModule = typeof import("./UpdateToast.svelte");
const component = vi.fn() as unknown as ToastModule["default"];

it("starts only on presentation admission and coalesces retry while its import is held", async () => {
  let finish!: (module: ToastModule) => void;
  const load = vi.fn(() => new Promise<ToastModule>((resolve) => { finish = resolve; }));
  const publish = vi.fn();
  const original = updateToastPresentation(load, publish);
  original.retry(); original.retry();
  await Promise.resolve(); expect(load).toHaveBeenCalledTimes(1);
  finish({ default: component });
  await vi.waitFor(() => expect(publish).toHaveBeenLastCalledWith({ phase: "ready", component }));
  original.dispose();
});

it.each(["resolve", "reject"] as const)("retirement fences a late %s and all subsequent retry admission", async (outcome) => {
  let finish!: (module: ToastModule) => void, fail!: (error: Error) => void;
  const load = vi.fn(() => new Promise<ToastModule>((resolve, reject) => { finish = resolve; fail = reject; }));
  const states: ToastPresentation[] = [];
  const original = updateToastPresentation(load, (state) => { states.push(state); });
  await Promise.resolve(); original.dispose(); original.dispose();
  if (outcome === "resolve") finish({ default: component }); else fail(new Error("chunk unavailable"));
  await Promise.resolve(); await Promise.resolve(); await Promise.resolve();
  original.retry();
  expect(load).toHaveBeenCalledTimes(1); expect(states).toEqual([{ phase: "loading" }]);
});

it("failed import remains read-only until explicit retry, which invokes only the import", async () => {
  const load = vi.fn().mockRejectedValueOnce(new Error("unavailable")).mockResolvedValueOnce({ default: component });
  const publish = vi.fn();
  const original = updateToastPresentation(load, publish);
  await vi.waitFor(() => expect(publish).toHaveBeenLastCalledWith({ phase: "failed" }));
  expect(load).toHaveBeenCalledTimes(1);
  original.retry();
  await vi.waitFor(() => expect(publish).toHaveBeenLastCalledWith({ phase: "ready", component }));
  expect(load).toHaveBeenCalledTimes(2); original.dispose();
});

it("actual App retains its notice conditional and the loader passes the current reactive notice", () => {
  expect(appSource).toContain('updateNotice !== null && $assetTransition === null');
  expect(appSource).toContain('<UpdateToastLoader notice={updateNotice} />');
  expect(appSource).not.toContain('import UpdateToast from');
  expect(loaderSource).toContain('<Toast {notice} />');
  expect(loaderSource).toContain('return original.dispose;');
  expect(loaderSource).not.toMatch(/beginUpdate|checkForUpdates|updateLocalDaemon|connectHost/);
});
