import { beforeEach, describe, expect, it, vi } from "vitest";
import { runHere, runInCloud } from "./placeHost";
const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("../net/api", () => ({ api: mocks.api }));

const reply = (status: number, body?: unknown) => new Response(body === undefined ? null : JSON.stringify(body), { status });

describe("the host indicator's two moves", () => {
  beforeEach(() => mocks.api.mockReset());

  it("posts once to the project's own route and reads 202 as started", async () => {
    mocks.api.mockResolvedValueOnce(reply(202, { returning: true })).mockResolvedValueOnce(reply(202, { moving: true }));
    await expect(runHere("w-one")).resolves.toEqual({ started: true });
    await expect(runInCloud("w-one")).resolves.toEqual({ started: true });
    expect(mocks.api.mock.calls.map(([path, init]) => [path, init.method])).toEqual([
      ["/pro/projects/w-one/here", "POST"], ["/pro/projects/w-one/cloud", "POST"],
    ]);
  });

  it("passes the daemon's own 409 codes through and nothing else", async () => {
    mocks.api.mockResolvedValueOnce(reply(409, { error: "not_elsewhere" }))
      .mockResolvedValueOnce(reply(409, { error: "cloud_time_used_up" }))
      .mockResolvedValueOnce(reply(409, { error: "Not A Code" }))
      .mockResolvedValueOnce(reply(404))
      .mockRejectedValueOnce(new Error("offline"));
    await expect(runHere("w-one")).resolves.toEqual({ started: false, error: "not_elsewhere" });
    await expect(runInCloud("w-one")).resolves.toEqual({ started: false, error: "cloud_time_used_up" });
    await expect(runInCloud("w-one")).resolves.toEqual({ started: false, error: "unavailable" });
    await expect(runHere("w-one")).resolves.toEqual({ started: false, error: "unavailable" });
    await expect(runHere("w-one")).resolves.toEqual({ started: false, error: "unavailable" });
  });

  it("never sends a malformed project id", async () => {
    await expect(runHere("../w")).resolves.toEqual({ started: false, error: "unavailable" });
    expect(mocks.api).not.toHaveBeenCalled();
  });
});
