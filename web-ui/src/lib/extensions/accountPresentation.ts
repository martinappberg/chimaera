/** Finite first-party account presentation port. No request, bearer, Client,
 * module URL or native command selector crosses this interface. */
import type * as Native from "../net/native";
import type { CloudSetupRequest, CloudSetupInfo, CloudProviderConnection, CloudProviderStatus } from "../net/native";

import type { SecretPage, SecretResult, SecretCommand, SecretAttempt } from "../pro/projectSecrets";


import type { CloudOnboardingContext } from "../pro/onboarding.svelte";
import type { ProposalDecision, StoredProfile } from "../pro/profile";
import type { Workspace } from "../workspace/sessions";
import type { Observable } from "./application";
export type { ProStatus, CloudProjectOpen, ProHost, ProDevice, ProAuthScreenHint, CloudProject, CloudProviderConnection, CloudProviderStatus, CloudProviderState, CloudSetupInfo, CloudSetupRequest, CloudProvisioningStatus, MirrorStatus, MirrorWorkspace, MirrorProfile, ProBillingAttempt, ProPlanPrice, RememberedProvider } from "../net/native";
/** `leave` (additive): where the work went when a computer's app last quit,
 *  in the account's closed states and reasons. */
export interface HomeProject { workspace_id: string; name: string | null; href: string; available: boolean; leave?: { state: string; reason: string | null } | null }
/** Where a Home project is right now, from its passive placement read (never
 *  a wake): running in the cloud, idle there, on a computer, or nowhere. */
export type HomePlace = "cloud" | "cloud_idle" | "computer" | null;
export interface HomeProjects { projects: HomeProject[]; pending: boolean }
export interface BrowserAccountStatus extends Native.ProStatus { account_lifetime: string | null }
export type SecretFailureCode = "unsupported" | "invalid_request" | "state_changed" | "unavailable" | "operation_unavailable" | "limit_reached" | "sign_in_required" | "context_changed" | "unconfirmed" | "account_home_required";

export type { CloudOnboardingContext } from "../pro/onboarding.svelte";
export type { Workspace } from "../workspace/sessions";
export interface ProviderPresentationPort {
  readonly personal: boolean;
  readonly personalRequired: boolean;
  revalidate(): Promise<void>;
  pending(): CloudProviderConnection | null;
  catalog(signal?: AbortSignal): Promise<CloudSetupInfo>;
  request(request: CloudSetupRequest, signal?: AbortSignal): Promise<CloudSetupInfo>;
  action(request: CloudSetupRequest, wanted: () => boolean): Promise<CloudSetupInfo | null>;
  clear(): void;
}
export interface AccountPresentationServices {
  version: 1;
  pageVisible: Observable<boolean>;
  paidPlan: Observable<"pro" | "max" | null>;
  isNativeShell(): boolean;
  isBrowserGateway(): boolean;
  BILLING_PATH: "/account/billing";
  onProChanged(send: () => void): Promise<() => void>;
  proStatus(): Promise<Native.ProStatus>;
  proSignIn(screenHint?: Native.ProAuthScreenHint): Promise<void>;
  proCancelSignIn(): Promise<void>;
  proSignOut(): Promise<void>;
  proSignOutEverywhere(): Promise<void>;
  proHosts(): Promise<Native.ProHost[]>;
  proSetHostKept(alias: string, kept: boolean): Promise<void>;
  proDevices(): Promise<Native.ProDevice[]>;
  proRevokeDevice(deviceId: string): Promise<void>;
  proBillingCheckout(plan: "pro" | "max", interval: "month" | "year"): Promise<void>;
  proBillingPortal(target?: { plan: "pro" | "max"; interval: "month" | "year" }): Promise<void>;
  proCancelBilling(attemptId?: number): Promise<void>;
  proRefreshAccount(): Promise<void>;
  proMirrorStatus(): Promise<Native.MirrorStatus>;
  proSetNeverMirror(workspaceId: string, neverMirror: boolean): Promise<void>;
  proCloudStatus(): Promise<Native.CloudProvisioningStatus>;
  proCloudProjects(): Promise<Native.CloudProject[]>;
  proOpenCloudProject(workspaceId: string): Promise<Native.CloudProjectOpen | null>;
  writeClipboard: typeof Native.writeClipboard;
  navigateHome: typeof Native.navigateHome;
  fetchHomeProjects(signal?: AbortSignal): Promise<HomeProjects>;
  /** Each listed project's place, read passively per project (bounded). */
  fetchHomePlaces(projects: readonly HomeProject[], signal?: AbortSignal): Promise<Map<string, HomePlace>>;
  fetchBrowserAccount(signal?: AbortSignal): Promise<BrowserAccountStatus>;
  signOutBrowser(): Promise<void>;
  forgetCatalogs(): void;
  recallCatalog(): CloudProviderStatus[] | null;
  rememberCatalog(providers: CloudProviderStatus[]): void;
  providerTransport(): ProviderPresentationPort;
  cloudRequest(request: Native.CloudSetupRequest, signal?: AbortSignal): Promise<Native.CloudSetupInfo>;
  readBrowserMirrorStatus(signal?: AbortSignal): Promise<Native.MirrorStatus | null>;
  openBrowserWorkspace(workspaceId: string): void;
  readSecretPage(after?: string | null, signal?: AbortSignal): Promise<SecretPage>;
  readSecretOperation(context: string, operation: string, signal?: AbortSignal): Promise<SecretResult>;
  sendSecretCommand(context: string, command: SecretCommand): Promise<SecretResult>;
  secretFailure(reason: unknown, fallback?: SecretFailureCode): Error & { readonly code: SecretFailureCode };
  observeSecretContext(context: string): void;
  retainSecretAttempt(attempt: SecretAttempt): boolean;
  secretReconciliation: Observable<{ context: string | null; attempts: SecretAttempt[] }>;
  settleSecretAttempt(context: string, operation: string): void;
  settleProposal(workspaceId: string, shown: string, decision: ProposalDecision): Promise<"saved" | "changed">;
  requestKeptReview(workspaceId: string): void;
  cloudOnboarding: {
    readonly context: CloudOnboardingContext | null;
    request(context: CloudOnboardingContext): void;
    complete(): void;
  };
  openWorkspace(workspace: Workspace): void;
  settingsShortcut(event: KeyboardEvent): boolean;
  settingsKeyHint(): string;
  modal(node: HTMLElement): { destroy(): void };
  current(): boolean;
  dispose(): void;
}

/** Captured host services only; no generic request/invoke/URL/token capability. */
export interface SetupProfileTransport {
  readSetupProfile(workspaceId: string, lifetime?: string): Promise<Response>;
  saveSetupProfile(workspaceId: string, profile: StoredProfile, revision: string, lifetime?: string): Promise<Response>;
}
export interface AccountHostScope extends SetupProfileTransport {
  current(): boolean;
  signal: AbortSignal;
  retire(preserveIntent: boolean): void;
  modal(node: HTMLElement): { destroy(): void };
  openWorkspace(workspace: Workspace): void;
  originalIntent: CloudOnboardingContext | null;
  environment: Readonly<{ native: boolean; accountHome: boolean; gateway: boolean; workspace: string | null; prefix: string; workbench: string }>;
  pageVisible: Observable<boolean>;
  paidPlan: Observable<"pro" | "max" | null>;
  writeClipboard(text: string): Promise<boolean>;
  navigateHome(alias: string | null, workspaceId?: string | null): Promise<void>;
  settingsShortcut(event: KeyboardEvent): boolean;
  settingsKeyHint(): string;
  requestKeptReview(workspaceId: string): void;
  cloudOnboarding: AccountPresentationServices["cloudOnboarding"];
  legacyCloudRequest(request: CloudSetupRequest, signal?: AbortSignal, expectedLifetime?: string): Promise<CloudSetupInfo>;
  readBrowserMirrorStatus(signal: AbortSignal | undefined, lifetime: string): Promise<Native.MirrorStatus | null>;
}
export interface AccountBranding {
  plan: "loading" | "unknown" | "unavailable" | "free" | "pro" | "max";
  available: boolean;
  signedOut?: boolean;
}
export interface AccountBrandingScope {
  environment: Readonly<{ native: boolean; gateway: boolean; local: boolean; workbench: string }>;
  signal: AbortSignal;
}

/** One original shared-branding subscriber; no request or account action port. */
export interface AccountBrandingState {
  plan: AccountBranding["plan"];
  offered: boolean | null;
  signedOut: boolean;
}
export interface AccountBrandingSubscription extends AccountBrandingScope {
  /** Original first gateway lookup deadline, including lazy admission; minted by the facade. */
  startupDeadline?: number;
  publish(state: AccountBrandingState): void;
}
