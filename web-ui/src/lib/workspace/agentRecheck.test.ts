import { afterEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import * as net from "../net/api";
import { agentRechecks, recheckAgent } from "./launcher";

afterEach(() => vi.restoreAllMocks());

describe("retrying a check the login shell couldn't finish", () => {
  it("joins a retry already in flight, and every surface sees it running", async () => {
    let answer: (r: Response) => void = () => {};
    const api = vi.spyOn(net, "api").mockImplementation((path) =>
      String(path).endsWith("/detect")
        ? new Promise<Response>((resolve) => (answer = resolve))
        : Promise.resolve(Response.json([])),
    );
    const first = recheckAgent("claude");
    const second = recheckAgent("claude");
    expect(second).toBe(first);
    expect(get(agentRechecks).has("claude")).toBe(true);
    answer(Response.json({ installed: false, unchecked: "Your login shell (/bin/zsh) took longer than 30 seconds to start." }));
    expect(await first).toEqual({
      installed: false, path: null, version: null,
      unchecked: "Your login shell (/bin/zsh) took longer than 30 seconds to start.",
    });
    expect(api.mock.calls.filter(([path]) => String(path).endsWith("/detect"))).toHaveLength(1);
    expect(get(agentRechecks).has("claude")).toBe(false);
  });
});
