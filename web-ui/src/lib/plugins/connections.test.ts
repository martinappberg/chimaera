import { beforeEach, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("../net/api", () => ({ api: mocks.api, ApiError: class extends Error {
  constructor(public status: number, message: string) { super(message); }
} }));
import { connectionAuthAction, loginConnection } from "./connections";

beforeEach(() => { mocks.api.mockReset(); });

it("sends JSON for sign-in and callbacks, as the daemon's extractor requires", async () => {
  mocks.api.mockImplementation(async (_path: string, init: RequestInit) => {
    if (new Headers(init.headers).get("Content-Type") !== "application/json") {
      return new Response("Expected application/json", { status: 415 });
    }
    const body = JSON.parse(String(init.body));
    return body.url ? new Response(null, { status: 204 }) : Response.json({ id: "auth-one", state: "starting" });
  });
  expect((await loginConnection("ws-one", "claude", "claude.ai Docs")).id).toBe("auth-one");
  await expect(connectionAuthAction("ws-one", "auth-one", "callback", "http://localhost:1234/cb?code=one")).resolves.toBeUndefined();
});

it("keeps daemon refusals actionable", async () => {
  mocks.api.mockResolvedValue(Response.json({ error: "This connection is already being signed in." }, { status: 409 }));
  await expect(loginConnection("ws-one", "codex", "docs")).rejects.toMatchObject({ status: 409, message: "This connection is already being signed in." });
});
