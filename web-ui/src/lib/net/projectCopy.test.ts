import { afterEach, describe, expect, it, vi } from "vitest";
import { proOpenCloudProject, proTakeOverProject } from "./native";
afterEach(() => vi.unstubAllGlobals());
describe("explicit native project actions", () => {
  it("never invokes legacy Open when an older shell lacks copy capability", async () => {
    const invoke = vi.fn().mockRejectedValue("Command pro_copy_project not found");
    vi.stubGlobal("window", { __TAURI__: { core: { invoke } } });
    await expect(proOpenCloudProject("w-one")).rejects.toThrow("project_copy_update_required");
    expect(invoke).toHaveBeenCalledExactlyOnceWith("pro_copy_project", { workspaceId: "w-one" });
  });
  it("keeps execution transfer behind its separate observed-epoch invocation", async () => {
    const invoke = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("window", { __TAURI__: { core: { invoke } } });
    await proTakeOverProject("w-one", 4);
    expect(invoke).toHaveBeenCalledExactlyOnceWith("pro_take_over_project", { workspaceId: "w-one", expectedEpoch: 4 });
  });
});
