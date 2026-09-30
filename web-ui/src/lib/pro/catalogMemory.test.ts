import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { CloudProviderStatus } from "../net/native";
const mocks = vi.hoisted(() => ({ native: vi.fn(() => false) }));
vi.mock("../net/native", () => ({ isNativeShell: mocks.native }));
import { forgetCatalogs, recallCatalog, rememberCatalog } from "./catalogMemory";

const row = (id: string, state: CloudProviderStatus["state"], category: CloudProviderStatus["category"] = "agent"): CloudProviderStatus => ({ id, label: id, state, category, installed: true, reason: "private detail", checked_at: 1, methods: ["device_code"] });
function storage(): Storage {
  const values = new Map<string, string>();
  return {
    get length() { return values.size; },
    key: (index: number) => [...values.keys()][index] ?? null,
    getItem: (name: string) => values.get(name) ?? null,
    setItem: (name: string, value: string) => void values.set(name, value),
    removeItem: (name: string) => void values.delete(name),
    clear: () => values.clear(),
  };
}
function at(path: string): void { vi.stubGlobal("location", new URL(`https://account.test${path}`)); }

beforeEach(() => { mocks.native.mockReturnValue(false); vi.stubGlobal("localStorage", storage()); at("/app/worker-1/"); });
afterEach(() => vi.unstubAllGlobals());

describe("a browser view's remembered catalog", () => {
  it("keeps only what rendering needs, per cloud address", () => {
    rememberCatalog([row("claude", "signed_in"), row("github", "missing", "repository")]);
    const stored = JSON.parse(localStorage.getItem("chimaera.pro.catalog:/app/worker-1") ?? "null");
    expect(stored).toEqual([
      { id: "claude", label: "claude", category: "agent", state: "signed_in", methods: ["device_code"] },
      { id: "github", label: "github", category: "repository", state: "missing", methods: ["device_code"] },
    ]);
    expect(recallCatalog()?.map(entry => [entry.id, entry.state])).toEqual([["claude", "signed_in"], ["github", "missing"]]);
    // Another cloud's address (another account's) has none.
    at("/app/worker-2/");
    expect(recallCatalog()).toBeNull();
    at("/workspace/ws-1/");
    expect(recallCatalog()).toBeNull();
  });
  it("forgets every cloud's rows on sign-out and leaves other storage alone", () => {
    rememberCatalog([row("claude", "signed_in")]);
    at("/workspace/ws-1/");
    rememberCatalog([row("codex", "needs_sign_in")]);
    localStorage.setItem("other", "kept");
    forgetCatalogs();
    expect(recallCatalog()).toBeNull();
    at("/app/worker-1/");
    expect(recallCatalog()).toBeNull();
    expect(localStorage.getItem("other")).toBe("kept");
  });
  it("stays out of the native app and off pages without a cloud address", () => {
    mocks.native.mockReturnValue(true);
    rememberCatalog([row("claude", "signed_in")]);
    expect(localStorage.length).toBe(0);
    expect(recallCatalog()).toBeNull();
    mocks.native.mockReturnValue(false);
    at("/");
    rememberCatalog([row("claude", "signed_in")]);
    expect(localStorage.length).toBe(0);
  });
  it("works without storage and ignores what it cannot read", () => {
    localStorage.setItem("chimaera.pro.catalog:/app/worker-1", "{not json");
    expect(recallCatalog()).toBeNull();
    vi.stubGlobal("localStorage", { get length(): number { throw new Error("denied"); }, getItem: () => { throw new Error("denied"); }, setItem: () => { throw new Error("denied"); }, removeItem: () => { throw new Error("denied"); }, key: () => null });
    expect(recallCatalog()).toBeNull();
    expect(() => rememberCatalog([row("claude", "signed_in")])).not.toThrow();
    expect(() => forgetCatalogs()).not.toThrow();
  });
});
