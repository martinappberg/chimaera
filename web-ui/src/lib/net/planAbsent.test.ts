import { afterEach, expect, it, vi } from "vitest";
const probe = vi.hoisted(() => vi.fn());
vi.mock("../extensions/selected", () => ({ selectedApplication: null }));
vi.mock("./native", () => ({ isNativeShell: probe, proStatus: probe, onProChanged: probe }));
vi.mock("./api", () => ({ getHostLabel: probe }));
vi.mock("./base", () => ({ isAccountHome: probe, isBrowserGateway: probe, workbenchPath: probe }));
import { accountPlan, accountSignedOut, proOffered, paidPlan } from "./plan";
afterEach(() => vi.unstubAllGlobals());
it("default literal-null assembly has zero IPC/HEAD/listener probes for all shared projections", () => {
  const document = Object.assign(new EventTarget(), { visibilityState: "visible" });
  const listener = vi.spyOn(document, "addEventListener"); const fetcher = vi.fn();
  vi.stubGlobal("document", document); vi.stubGlobal("fetch", fetcher);
  const states: unknown[] = []; const stops = [accountPlan, accountSignedOut, proOffered, paidPlan].map(store => store.subscribe(value => states.push(value)));
  document.dispatchEvent(new Event("visibilitychange")); for (const stop of stops) stop();
  expect(states).toEqual(["unavailable", false, false, null]);
  expect(probe).not.toHaveBeenCalled(); expect(fetcher).not.toHaveBeenCalled(); expect(listener).not.toHaveBeenCalled();
});
