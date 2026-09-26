import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("../net/api", () => ({
  api: mocks.api,
  ApiError: class extends Error {},
}));

import { fsRawTicket, lastRawTicketUrl, rawTicketUrl } from "./files";

/** The daemon: one ticket per (path, version), like its TicketStore. */
const versions = new Map<string, number>();
function answer(path: string): Response {
  return new Response(JSON.stringify({ ticket: `t-${path.replace(/\W/g, "")}-v${versions.get(path) ?? 1}` }));
}

describe("rawTicketUrl", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
    versions.clear();
    mocks.api.mockReset();
    mocks.api.mockImplementation(async (_url: string, init: RequestInit) =>
      answer((JSON.parse(String(init.body)) as { path: string }).path),
    );
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("shares one ask between concurrent callers", async () => {
    const [a, b] = await Promise.all([rawTicketUrl("/w/a.png"), rawTicketUrl("/w/a.png")]);
    expect(a).toBe("/raw/t-wapng-v1");
    expect(b).toBe(a);
    expect(mocks.api).toHaveBeenCalledTimes(1);
  });

  it("reuses an answer for a render's burst, then asks again", async () => {
    const first = await rawTicketUrl("/w/b.png");
    vi.advanceTimersByTime(1_000);
    expect(await rawTicketUrl("/w/b.png")).toBe(first);
    expect(mocks.api).toHaveBeenCalledTimes(1);
    // Past the burst: the daemon is asked again, and for an unchanged file
    // answers the same ticket (the src stays, the cached bytes show).
    vi.advanceTimersByTime(5_000);
    expect(await rawTicketUrl("/w/b.png")).toBe(first);
    expect(mocks.api).toHaveBeenCalledTimes(2);
  });

  it("never holds an overwritten file's old ticket past the burst", async () => {
    const before = await rawTicketUrl("/w/c.png");
    versions.set("/w/c.png", 2); // overwritten on disk
    vi.advanceTimersByTime(3_000);
    // A synchronous re-render shows the last answer at once…
    expect(lastRawTicketUrl("/w/c.png")).toBe(before);
    // …and the next ask brings the new version's URL.
    const after = await rawTicketUrl("/w/c.png");
    expect(after).toBe("/raw/t-wcpng-v2");
    expect(lastRawTicketUrl("/w/c.png")).toBe(after);
  });

  it("forgets an answer once its ticket may have expired", async () => {
    expect(lastRawTicketUrl("/w/d.png")).toBeNull();
    await rawTicketUrl("/w/d.png");
    vi.advanceTimersByTime(9 * 60 * 1000);
    expect(lastRawTicketUrl("/w/d.png")).toBeNull();
  });

  it("does not keep a failed ask", async () => {
    mocks.api.mockRejectedValueOnce(new Error("offline"));
    await expect(rawTicketUrl("/w/e.png")).rejects.toThrow("offline");
    expect(lastRawTicketUrl("/w/e.png")).toBeNull();
    expect(await rawTicketUrl("/w/e.png")).toBe("/raw/t-wepng-v1");
  });
});

describe("fsRawTicket", () => {
  beforeEach(() => {
    mocks.api.mockReset();
  });

  it("answers the canonical name the ticket is bound to", async () => {
    // Asked through a symlink, the daemon names the target.
    mocks.api.mockResolvedValueOnce(new Response(JSON.stringify({ ticket: "t-9", name: "report.html" })));
    expect(await fsRawTicket("/w/latest.html")).toEqual({ url: "/raw/t-9", name: "report.html" });
    mocks.api.mockResolvedValueOnce(new Response(JSON.stringify({ ticket: "t-8", name: ".summary.html" })));
    expect(await fsRawTicket("/w/.summary.html")).toEqual({ url: "/raw/t-8", name: ".summary.html" });
    // A daemon that doesn't say leaves the caller to its own name.
    mocks.api.mockResolvedValueOnce(new Response(JSON.stringify({ ticket: "t-7" })));
    expect(await fsRawTicket("/w/a.html")).toEqual({ url: "/raw/t-7", name: null });
  });
});
