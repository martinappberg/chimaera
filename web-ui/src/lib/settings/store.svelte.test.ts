import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  api: vi.fn(),
  cacheAppearanceBootstrap: vi.fn(),
}));

vi.mock("../net/api", () => ({ api: mocks.api }));
vi.mock("../net/native", () => ({
  cacheAppearanceBootstrap: mocks.cacheAppearanceBootstrap,
}));

const BOOTSTRAP_KEY = "chimaera.appearanceBootstrap.v1";

/** The slice of the DOM the store's appearance pass touches. */
function stubDocument(): void {
  const properties = new Map<string, string>();
  vi.stubGlobal("document", {
    visibilityState: "visible",
    documentElement: {
      dataset: {} as Record<string, string>,
      style: {
        colorScheme: "",
        setProperty: (name: string, value: string) => properties.set(name, value),
        removeProperty: (name: string) => properties.delete(name),
        getPropertyValue: (name: string) => properties.get(name) ?? "",
      },
    },
  });
  vi.stubGlobal("matchMedia", () => ({
    matches: false,
    addEventListener: vi.fn(),
  }));
}

describe("settings appearance bootstrap", () => {
  beforeEach(() => {
    vi.resetModules();
    mocks.api.mockReset();
    mocks.cacheAppearanceBootstrap.mockReset();
    mocks.cacheAppearanceBootstrap.mockResolvedValue(undefined);
    delete (
      globalThis as typeof globalThis & {
        __CHIMAERA_APPEARANCE_BOOTSTRAP__?: unknown;
      }
    ).__CHIMAERA_APPEARANCE_BOOTSTRAP__;

    const storage = new Map<string, string>();
    storage.set(
      BOOTSTRAP_KEY,
      JSON.stringify({
        mode: "dark",
        themeId: "chimaera-dark",
        background: "#17171c",
        accent: "#ff00ff",
      }),
    );
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => storage.set(key, value),
      removeItem: (key: string) => storage.delete(key),
    });

    stubDocument();
  });

  afterEach(() => {
    delete (
      globalThis as typeof globalThis & {
        __CHIMAERA_APPEARANCE_BOOTSTRAP__?: unknown;
      }
    ).__CHIMAERA_APPEARANCE_BOOTSTRAP__;
    vi.unstubAllGlobals();
  });

  it("prefers a shell-carried snapshot over storage from a different origin visit", async () => {
    (
      globalThis as typeof globalThis & {
        __CHIMAERA_APPEARANCE_BOOTSTRAP__?: unknown;
      }
    ).__CHIMAERA_APPEARANCE_BOOTSTRAP__ = {
      mode: "light",
      themeId: "chimaera-light",
      background: "#fbfbfc",
      accent: null,
    };

    await import("./store.svelte");

    expect(document.documentElement.dataset.theme).toBe("light");
    expect(document.documentElement.style.colorScheme).toBe("light");
    expect(document.documentElement.style.getPropertyValue("--bg")).toBe("#fbfbfc");
  });

  it("replaces a stale bootstrap on the first authoritative frame after GET failure", async () => {
    mocks.api.mockRejectedValue(new Error("daemon unavailable"));
    const store = await import("./store.svelte");

    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(document.documentElement.style.colorScheme).toBe("dark");
    expect(document.documentElement.style.getPropertyValue("--accent")).toBe("#ff00ff");
    await store.loadSettings();
    expect(store.settingsLoaded()).toBe(true);
    expect(document.documentElement.dataset.theme).toBe("dark");

    store.applyRemoteSettings({});

    expect(document.documentElement.dataset.theme).toBe("light");
    expect(document.documentElement.style.colorScheme).toBe("light");
    expect(document.documentElement.style.getPropertyValue("--bg")).toBe("#fbfbfc");
    expect(document.documentElement.style.getPropertyValue("--accent")).toBe("#2e9e6b");
    expect(mocks.cacheAppearanceBootstrap).toHaveBeenLastCalledWith({
      mode: "light",
      themeId: "chimaera-light",
      background: "#fbfbfc",
      accent: null,
    });
  });
});

describe("settings persistence", () => {
  const putBodies = () =>
    mocks.api.mock.calls
      .filter(([, init]) => (init as RequestInit | undefined)?.method === "PUT")
      .map(([, init]) => (init as RequestInit).body);

  beforeEach(() => {
    vi.resetModules();
    vi.useFakeTimers();
    mocks.api.mockReset();
    mocks.cacheAppearanceBootstrap.mockReset();
    mocks.cacheAppearanceBootstrap.mockResolvedValue(undefined);
    vi.stubGlobal("localStorage", {
      getItem: () => null,
      setItem: vi.fn(),
      removeItem: vi.fn(),
    });
    stubDocument();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("retries an offline edit when the reconnect settings frame arrives", async () => {
    const store = await import("./store.svelte");
    mocks.api.mockRejectedValueOnce(new TypeError("offline"));
    store.setSetting("terminal.fontSize", 17);
    await vi.advanceTimersByTimeAsync(400);
    expect(store.settingsSaveState()).toBe("failed");

    mocks.api.mockResolvedValue(new Response(null, { status: 204 }));
    // The frame carries the daemon's older map; it must not win.
    store.applyRemoteSettings({});
    await vi.advanceTimersByTimeAsync(0);

    expect(putBodies()).toEqual(['{"terminal.fontSize":17}', '{"terminal.fontSize":17}']);
    expect(store.getSetting("terminal.fontSize")).toBe(17);
    expect(store.settingsSaveState()).toBe("idle");
  });

  it("ignores a remote frame while a newer local edit is unsaved, then accepts frames", async () => {
    const store = await import("./store.svelte");
    let answer: (res: Response) => void = () => {};
    mocks.api.mockReturnValueOnce(new Promise<Response>((resolve) => (answer = resolve)));
    store.setSetting("terminal.fontSize", 17);
    await vi.advanceTimersByTimeAsync(400);
    store.applyRemoteSettings({ "terminal.fontSize": 12 });
    expect(store.getSetting("terminal.fontSize")).toBe(17);

    answer(new Response(null, { status: 204 }));
    await vi.advanceTimersByTimeAsync(0);
    store.applyRemoteSettings({ "terminal.fontSize": 12 });
    expect(store.getSetting("terminal.fontSize")).toBe(12);
  });

  it("lets the next frame replace a map the daemon refused", async () => {
    const store = await import("./store.svelte");
    mocks.api.mockResolvedValueOnce(
      new Response(JSON.stringify({ error: "settings exceed 262144 bytes" }), { status: 413 }),
    );
    store.setSetting("terminal.fontSize", 17);
    await vi.advanceTimersByTimeAsync(400);
    expect(store.settingsSaveState()).toBe("rejected");
    expect(store.settingsSaveRejection()).toBe("settings exceed 262144 bytes");

    store.applyRemoteSettings({ "terminal.fontSize": 12 });
    expect(store.getSetting("terminal.fontSize")).toBe(12);
    expect(putBodies()).toHaveLength(1);
  });
});
