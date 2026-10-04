/** Finite callbacks to the window's existing owners. No account transport is created here. */
import { isNativeShell, writeClipboard, navigateHome } from "../net/native";
import { isAccountHome, isBrowserGateway, gatewayWorkspace, gatewayPrefix, workbenchPath } from "../net/base";
import { paidPlan } from "../net/plan";
import { pageVisible } from "../shared/visibility";
import { matchAction, keyHint } from "../shared/keybindings";
import { cloudOnboarding } from "../pro/onboarding.svelte";
import { requestKeptReview } from "../pro/kept";
import { readSetupProfile, saveSetupProfile } from "../pro/profile";
import { legacyCloudRequest, readBrowserMirrorStatus } from "./accountDaemon";
import type { AccountHostScope } from "./accountPresentation";
export function accountHostScope(original: Pick<AccountHostScope, "current" | "signal" | "retire" | "modal" | "openWorkspace" | "originalIntent">): AccountHostScope {
  const check = () => { if (original.signal.aborted || !original.current()) throw new Error("Account presentation retired"); };
  return {
    current: original.current, signal: original.signal, retire: original.retire, modal: original.modal,
    openWorkspace: original.openWorkspace, originalIntent: original.originalIntent,
    environment: Object.freeze({ native: isNativeShell(), accountHome: isAccountHome(), gateway: isBrowserGateway(),
      workspace: gatewayWorkspace(), prefix: gatewayPrefix(), workbench: workbenchPath() }),
    pageVisible, paidPlan, writeClipboard, navigateHome,
    settingsShortcut: event => matchAction(event)?.id === "settings", settingsKeyHint: () => keyHint("settings"),
    requestKeptReview, legacyCloudRequest, readBrowserMirrorStatus,
    readSetupProfile, saveSetupProfile,
    cloudOnboarding: {
      get context() {
        check(); const value = cloudOnboarding.context;
        if (value === null) return null;
        const providers = [...value.providerIds]; Object.freeze(providers);
        return Object.freeze({ providerIds: providers,
          ...(value.workspaceId === undefined ? {} : { workspaceId: value.workspaceId }),
          ...(value.workspaceName === undefined ? {} : { workspaceName: value.workspaceName }) });
      },
      request: context => { check(); cloudOnboarding.request(context); },
      complete: () => {
        check(); if (cloudOnboarding.context !== original.originalIntent) throw new Error("Account intent retired");
        const intent = original.originalIntent; cloudOnboarding.clear();
        window.dispatchEvent(new CustomEvent("chimaera:providers-ready", { detail: intent }));
      },
    },
  };
}
