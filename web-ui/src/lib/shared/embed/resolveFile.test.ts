import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("../../net/api", () => ({
  api: mocks.api,
  ApiError: class extends Error {},
}));

import { pathTarget, resolveFile } from "./embed";

/** What the daemon's `target_path` does to a target before resolving it:
 *  cut at `#`/`?`, trim, unwrap `<…>`, then decode `%XX`. */
function daemonReads(target: string): string {
  let t = target.trim();
  if (t.startsWith("<") && t.endsWith(">")) t = t.slice(1, -1);
  t = t.split("#")[0].split("?")[0];
  return decodeURIComponent(t);
}

describe("pathTarget", () => {
  it("escapes everything the daemon would read as URL syntax", () => {
    expect(pathTarget("/scratch/run#2/plot.png")).toBe("/scratch/run%232/plot.png");
    expect(pathTarget("/data/50%/x.png")).toBe("/data/50%25/x.png");
    expect(pathTarget("/data/50%25/x.png")).toBe("/data/50%2525/x.png");
    expect(pathTarget("/q/why?.png")).toBe("/q/why%3F.png");
    expect(pathTarget("/a/<b>.png")).toBe("/a/%3Cb%3E.png");
    expect(pathTarget("/a/trailing ")).toBe("/a/trailing%20");
    expect(pathTarget("//server/share/x.png")).toBe("/%2Fserver/share/x.png");
    expect(pathTarget("/plain/figs/plot.png")).toBe("/plain/figs/plot.png");
  });

  it("round-trips through the daemon's reading", () => {
    for (const path of [
      "/scratch/run#2/plot.png",
      "/data/50%/x.png",
      "/data/50%25/x.png",
      "/q/why?.png#frag",
      "/a/ spaced /tab\there.png ",
      "/ünï/cødé ✓.png",
    ]) {
      expect(daemonReads(pathTarget(path))).toBe(path);
    }
  });
});

describe("resolveFile", () => {
  beforeEach(() => {
    mocks.api.mockReset();
  });

  it("sends escaped targets and hands each caller its own answer", async () => {
    const sent: string[] = [];
    mocks.api.mockImplementation(async (_url: string, init: RequestInit) => {
      const body = JSON.parse(String(init.body)) as { base: string; targets: string[] };
      expect(body.base).toBe("/");
      sent.push(...body.targets);
      const results: Record<string, unknown> = {};
      for (const t of body.targets) {
        results[t] = { path: daemonReads(t), kind: "file", size: 1, version: "v", mtime_ms: 1, mime: "image/png" };
      }
      return new Response(JSON.stringify({ results }));
    });
    const [a, b, c] = await Promise.all([
      resolveFile("/scratch/run#2/plot.png"),
      resolveFile("/data/50%25/x.png"),
      resolveFile("/plain/y.png"),
    ]);
    expect(mocks.api).toHaveBeenCalledTimes(1);
    expect(sent).toEqual(["/scratch/run%232/plot.png", "/data/50%2525/x.png", "/plain/y.png"]);
    expect(a).toMatchObject({ path: "/scratch/run#2/plot.png" });
    expect(b).toMatchObject({ path: "/data/50%25/x.png" });
    expect(c).toMatchObject({ path: "/plain/y.png" });
  });
});
