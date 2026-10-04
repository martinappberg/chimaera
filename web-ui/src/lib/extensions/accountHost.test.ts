import { beforeEach, expect, it, vi } from "vitest";
const mocks = vi.hoisted(() => ({ intent: { providerIds: ["claude"], workspaceId: "project-a" }, clear: vi.fn(), request: vi.fn(), daemon: vi.fn(), profileRead: vi.fn(), profileSave: vi.fn() }));
vi.mock("../net/native", () => ({ isNativeShell: () => true, writeClipboard: vi.fn(), navigateHome: vi.fn() }));
vi.mock("../net/base", () => ({ isAccountHome: () => false, isBrowserGateway: () => false, gatewayWorkspace: () => null, gatewayPrefix: () => "", workbenchPath: () => "/" }));
vi.mock("../net/plan", () => ({ paidPlan: { subscribe: vi.fn() } }));
vi.mock("../shared/visibility", () => ({ pageVisible: { subscribe: vi.fn() } }));
vi.mock("../shared/keybindings", () => ({ matchAction: () => null, keyHint: () => "" }));
vi.mock("../pro/onboarding.svelte", () => ({ cloudOnboarding: { get context() { return mocks.intent; }, clear: mocks.clear, request: mocks.request } }));
vi.mock("./accountDaemon", () => ({ legacyCloudRequest: mocks.daemon, readBrowserMirrorStatus: vi.fn() }));
vi.mock("../pro/profile", () => ({ readSetupProfile: mocks.profileRead, saveSetupProfile: mocks.profileSave }));
vi.mock("../pro/kept", () => ({ requestKeptReview: vi.fn() }));
import { accountHostScope } from "./accountHost";
beforeEach(() => vi.clearAllMocks());
it("keeps original host projections/callbacks and exact onboarding object without account probes", () => {
  const abort = new AbortController(); const open = vi.fn(); const event = vi.fn(); vi.stubGlobal("window", { dispatchEvent: event });
  const scope = accountHostScope({ current: () => true, signal: abort.signal, retire: vi.fn(), modal: () => ({ destroy() {} }), openWorkspace: open, originalIntent: mocks.intent });
  expect(scope.environment).toEqual({ native: true, accountHome: false, gateway: false, workspace: null, prefix: "", workbench: "/" });
  expect(scope.cloudOnboarding.context).not.toBe(mocks.intent); expect(scope.cloudOnboarding.context?.providerIds).not.toBe(mocks.intent.providerIds);
  scope.cloudOnboarding.complete(); expect(mocks.clear).toHaveBeenCalledTimes(1); expect(event).toHaveBeenCalledTimes(1);
  abort.abort(); expect(() => scope.cloudOnboarding.complete()).toThrow("retired"); expect(mocks.clear).toHaveBeenCalledTimes(1);
  vi.unstubAllGlobals();
});
it("a same-valued successor intent cannot complete the original draft", () => {
  const original = { ...mocks.intent, providerIds: [...mocks.intent.providerIds] };
  const scope = accountHostScope({ current: () => true, signal: new AbortController().signal, retire: vi.fn(), modal: () => ({ destroy() {} }), openWorkspace: vi.fn(), originalIntent: original });
  expect(() => scope.cloudOnboarding.complete()).toThrow("intent retired"); expect(mocks.clear).not.toHaveBeenCalled();
});
