import { afterEach, expect, it, vi } from "vitest";
const bind = vi.hoisted(() => vi.fn());
vi.mock("../extensions/selected", () => ({ selectedApplication: { bindAccountBranding: bind } }));
vi.mock("./native", () => ({ isNativeShell: () => true }));
vi.mock("./api", () => ({ getHostLabel: () => "cluster" }));
vi.mock("./base", () => ({ isAccountHome: () => false, isBrowserGateway: () => false, workbenchPath: () => "/" }));
import { proTier } from "./plan";
afterEach(() => vi.unstubAllGlobals());
it("a native window on another host's daemon is free and never loads the extension", () => {
  vi.stubGlobal("document", new EventTarget());
  const tiers: unknown[] = [];
  proTier.subscribe(value => tiers.push(value))();
  expect(tiers).toEqual(["free"]);
  expect(bind).not.toHaveBeenCalled();
});
