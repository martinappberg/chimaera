import { afterEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { assetTransition } from "./assetTransition";
import { claimWindowReload, reloadWindow } from "./windowReload";

describe("native Reload Window hook", () => {
  afterEach(() => {
    assetTransition.set(null);
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("goes through App's gate once App claims it, never a plain reload", () => {
    const reload = vi.fn();
    vi.stubGlobal("location", { reload });
    const release = claimWindowReload();
    reloadWindow();
    expect(get(assetTransition)).toMatchObject({ reason: "manual", requested: true });
    expect(reload).not.toHaveBeenCalled();
    release();
  });

  it("reloads plainly before App mounts, ignoring key repeats within a second", () => {
    const reload = vi.fn();
    vi.stubGlobal("location", { reload });
    const now = vi.spyOn(performance, "now");
    now.mockReturnValue(10_000);
    reloadWindow();
    now.mockReturnValue(10_500);
    reloadWindow();
    expect(reload).toHaveBeenCalledTimes(1);
    now.mockReturnValue(11_200);
    reloadWindow();
    expect(reload).toHaveBeenCalledTimes(2);
    expect(get(assetTransition)).toBeNull();
  });
});
