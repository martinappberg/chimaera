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

    const properties = new Map<string, string>();
    vi.stubGlobal("document", {
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
