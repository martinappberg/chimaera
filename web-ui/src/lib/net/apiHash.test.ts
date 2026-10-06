import { afterEach, describe, expect, it, vi } from "vitest";

// api.ts reads the `#…` bootstrap once at module load, so each case sets the
// URL first and imports a fresh copy of the module.
const g = globalThis as Record<string, unknown>;
const originalLocation = g.location;
const originalHistory = g.history;

async function boot(hash: string) {
  g.location = new URL(`http://127.0.0.1:4100/${hash}`);
  const replaced: string[] = [];
  g.history = { replaceState: (_s: unknown, _t: string, url: string) => replaced.push(url) };
  sessionStorage.clear();
  vi.resetModules();
  const api = await import("./api");
  return { api, replaced };
}

afterEach(() => {
  g.location = originalLocation;
  g.history = originalHistory;
  sessionStorage.clear();
});

describe("job window hash", () => {
  it("carries the cluster workspace id with the job", async () => {
    const { api, replaced } = await boot("#token=t&host=hpc&job=4242&node=n042&cws=w-0000abcd");
    expect(api.getJobContext()).toEqual({ jobId: "4242", node: "n042", cws: "w-0000abcd" });
    expect(replaced).toHaveLength(1);
  });

  it("leaves cws null on a window from an older build", async () => {
    const { api } = await boot("#token=t&host=hpc&job=7");
    expect(api.getJobContext()).toEqual({ jobId: "7", node: null, cws: null });
  });

  it("ignores a malformed cws", async () => {
    const { api } = await boot("#token=t&host=hpc&job=7&cws=..%2Fx");
    expect(api.getJobContext()?.cws).toBeNull();
  });
});

describe("open request", () => {
  it("is handed out once and never persisted", async () => {
    const { api, replaced } = await boot(
      "#token=t&ws=abc&open=%2Fhome%2Fu%2F.chimaera%2Fpeek%2Fhpc%2Fresults%2Fplot.png",
    );
    expect(api.takeOpenRequest()).toBe("/home/u/.chimaera/peek/hpc/results/plot.png");
    expect(api.takeOpenRequest()).toBeNull();
    // Only the workbench's own keys reach sessionStorage; a reload can't reopen.
    expect(sessionStorage.getItem("chimaera.ws")).toBe("abc");
    expect(replaced).toHaveLength(1);
  });

  it("drops a relative path", async () => {
    const { api } = await boot("#token=t&open=notes.md");
    expect(api.takeOpenRequest()).toBeNull();
  });
});

describe("nested in a browser-pane proxy frame", () => {
  const originalWindow = g.window;
  afterEach(() => {
    g.window = originalWindow;
  });

  it("neither consumes the fragment nor overwrites the host's token", async () => {
    g.window = { self: {}, top: {} };
    g.location = new URL("http://127.0.0.1:4100/proxy/p-abc/#token=other");
    const replaced: string[] = [];
    g.history = { replaceState: (_s: unknown, _t: string, url: string) => replaced.push(url) };
    sessionStorage.clear();
    sessionStorage.setItem("chimaera:token", "host");
    vi.resetModules();
    const api = await import("./api");
    expect(api.isNestedInProxy()).toBe(true);
    expect(api.getToken()).toBeNull();
    expect(sessionStorage.getItem("chimaera:token")).toBe("host");
    expect(replaced).toHaveLength(0);
  });
});
