import { afterEach, expect, it, vi } from "vitest";
import type { AccountBrandingSubscription } from "../extensions/accountPresentation";
const binding = vi.hoisted(() => ({ scope: null as AccountBrandingSubscription | null, stop: vi.fn() }));
vi.mock("../extensions/selected", () => ({ selectedApplication: {
  bindAccountBranding: async (scope: AccountBrandingSubscription) => { binding.scope = scope; return binding.stop; },
} }));
vi.mock("./native", () => ({ isNativeShell: () => true }));
vi.mock("./api", () => ({ getHostLabel: () => "local" }));
vi.mock("./base", () => ({ isAccountHome: () => false, isBrowserGateway: () => false, workbenchPath: () => "/" }));
import { accountPlan, accountSignedOut, paidPlan, proOffered } from "./plan";
afterEach(() => vi.unstubAllGlobals());
it("shares one selected publication lifecycle and rejects delivery after its last subscriber", async () => {
  vi.stubGlobal("document", new EventTarget());
  const stopped = new Promise<void>((resolve) => { binding.stop.mockImplementationOnce(resolve); });
  const states: unknown[][] = [[], [], [], []];
  const stops = [accountPlan, accountSignedOut, proOffered, paidPlan].map((store, index) => store.subscribe(value => states[index].push(value)));
  const scope = binding.scope!;
  scope.publish({ plan: "pro", offered: true, signedOut: false });
  expect(states).toEqual([["loading", "pro"], [false], [null, true], [null, "pro"]]);
  stops[0](); expect(scope.signal.aborted).toBe(false);
  for (const stop of stops.slice(1)) stop();
  expect(scope.signal.aborted).toBe(true);
  const before = JSON.stringify(states); scope.publish({ plan: "max", offered: true, signedOut: false });
  expect(JSON.stringify(states)).toBe(before);
  // Observe the actual late-registration disposer, not an assumed microtask count.
  await stopped;
  expect(binding.stop).toHaveBeenCalledTimes(1);
  scope.publish({ plan: "max", offered: true, signedOut: false });
  expect(JSON.stringify(states)).toBe(before);
});
