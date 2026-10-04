import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { readSetupProfile, saveSetupProfile } from "./profile";
const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("../net/api", () => ({ api: mocks.api }));
beforeEach(() => mocks.api.mockReset());
afterEach(() => vi.restoreAllMocks());
it("uses only the fixed profile route, exact original lifetime and read/save deadlines", async () => {
  const timeout = vi.spyOn(AbortSignal, "timeout");
  const lifetime = "a".repeat(64);
  const profile = { pending_setup_command: null, future_field: { kept: true } };
  const read = Response.json(profile, { headers: { etag: '\"original\"' } });
  const saved = new Response(null, { status: 204 });
  mocks.api.mockResolvedValueOnce(read).mockResolvedValueOnce(saved);
  await expect(readSetupProfile("workspace/a?b", lifetime)).resolves.toBe(read);
  await expect(saveSetupProfile("workspace/a?b", profile, '\"original\"', lifetime)).resolves.toBe(saved);
  expect(timeout.mock.calls.map(([ms]) => ms)).toEqual([15_000, 35_000]);
  const [[readPath, get], [savePath, put]] = mocks.api.mock.calls;
  expect(readPath).toBe("/pro/profile?workspace_id=workspace%2Fa%3Fb");
  expect(savePath).toBe(readPath); expect(get.cache).toBe("no-store"); expect(get.method).toBeUndefined();
  expect(get.headers).toEqual({ "X-Chimaera-Account-Lifetime": lifetime });
  expect(put.method).toBe("PUT"); expect(put.headers).toEqual({ "Content-Type": "application/json", "If-Match": '\"original\"', "X-Chimaera-Account-Lifetime": lifetime });
  expect(JSON.parse(put.body)).toEqual(profile);
});
it("malformed captured lifetime causes zero API sends, without omission fallback", () => {
  for (const lifetime of ["", "A".repeat(64), "a".repeat(63)]) {
    expect(() => readSetupProfile("workspace-a", lifetime)).toThrow("account_changed");
    expect(() => saveSetupProfile("workspace-a", {}, '\"original\"', lifetime)).toThrow("account_changed");
  }
  expect(mocks.api).not.toHaveBeenCalled();
});
it("legacy fixed calls omit the optional lifetime and never retry a refusal", async () => {
  const refusal = new Response(null, { status: 409 }); mocks.api.mockResolvedValue(refusal);
  await expect(readSetupProfile("workspace-a")).resolves.toBe(refusal);
  await expect(saveSetupProfile("workspace-a", {}, '\"original\"')).resolves.toBe(refusal);
  expect(mocks.api).toHaveBeenCalledTimes(2);
  expect(mocks.api.mock.calls[0][1].headers).toEqual({});
  expect(mocks.api.mock.calls[1][1].headers).toEqual({ "Content-Type": "application/json", "If-Match": '\"original\"' });
});
