import { derived, readable } from "svelte/store";
import { getHostLabel } from "./api";
import { asyncDisposer } from "../shared/asyncDisposer";
import { isAccountHome, isBrowserGateway, workbenchPath } from "./base";
import { isNativeShell } from "./native";
import { selectedApplication } from "../extensions/selected";

export type PaidPlan = "pro" | "max" | null;

/** `unavailable` = this window can never offer Pro (a build without an account
 * endpoint, or an ordinary browser); it is static for the process. */
export type AccountPlan = "loading" | "unknown" | "unavailable" | "free" | "pro" | "max";

interface AccountState {
  plan: AccountPlan;
  /** Whether Pro is offered at all: null until the first answer. */
  offered: boolean | null;
  /** This computer's account answered that it is signed out. False while
   *  unknown, and always in a browser view (its account gateway serves only
   *  a signed-in account). */
  signedOut: boolean;
}

/** Account branding only; transport availability never implies entitlement.
 * Each window owns one subscription lifecycle and keeps no account data on disk.
 */
const account = readable<AccountState>({ plan: "loading", offered: null, signedOut: false }, (setState) => {
  const selected = selectedApplication;
  if (selected === null) { setState({ plan: "unavailable", offered: false, signedOut: false }); return; }
  if (typeof document === "undefined") return;
  setState({ plan: "loading", offered: null, signedOut: false });
  const controller = new AbortController();
  const native = isNativeShell();
  const publish = (state: AccountState): void => {
    if (!controller.signal.aborted) setState({ plan: state.plan, offered: state.offered, signedOut: state.signedOut });
  };
  const gateway = !native && (isBrowserGateway() || isAccountHome());
  if (gateway) setState({ plan: "loading", offered: true, signedOut: false });
  const dispose = asyncDisposer(selected.bindAccountBranding!({ signal: controller.signal, publish,
    environment: { native, gateway,
      local: getHostLabel() === "local", workbench: workbenchPath() },
  }).catch(() => { publish({ plan: "unknown", offered: gateway ? true : null, signedOut: false }); return () => {}; }));
  return () => { controller.abort(); dispose(); };
});

export const accountPlan = derived(account, (state): AccountPlan => state.plan);

/** This computer's account is signed out: a conversation the cloud still
 *  holds stays there until the person signs in again. */
export const accountSignedOut = derived(account, (state): boolean => state.signedOut);

/** Gates every Pro entry point (Home, Settings). `null` while the first answer
 * is pending, so an endpoint-less build never flashes Pro chrome; once known it
 * holds through later refreshes. */
export const proOffered = derived(account, (state): boolean | null => state.offered);

/** Existing badge consumers share the account read; unknown is never a paid badge. */
export const paidPlan = derived(accountPlan, (plan): PaidPlan =>
  plan === "pro" || plan === "max" ? plan : null,
);
