import { gatewayWorkspace, isAccountHome } from "../net/base";
import { isNativeShell, proPersonalProviderCatalog, proPersonalProviderCommand, proPersonalProviderMode, proPersonalProviderOpen, proPersonalProviderOperation, type CloudProviderConnection, type CloudProviderStatus, type CloudSetupInfo, type CloudSetupRequest } from "../net/native";
import { cloudAction, cloudRequest, peekCatalog } from "./cloudTransport";
import { personalLoginUrl, providerIds, providerMode, providerPage, providerResult, type ProviderCommand, type ProviderMode, type ProviderOriginal, type ProviderPage, type ProviderResult, type PersonalProvider } from "./personalProviders";
import names from "../../../../crates/chimaera-core/src/cloud-providers.json";
const errors = ["unsupported", "invalid_request", "state_changed", "unavailable", "operation_unavailable", "limit_reached", "sign_in_required", "context_changed", "unconfirmed"];
function failure(value: unknown, fallback = "unconfirmed"): Error {
  const text = value instanceof Error ? value.message : typeof value === "string" ? value : "";
  return new Error(`providers_${errors.find(code => text === `providers_${code}`) ?? fallback}`);
}
async function bounded(response: Response): Promise<unknown> {
  if (!response.body) throw failure(null);
  const reader = response.body.getReader(); const bytes = new Uint8Array(65536); let length = 0;
  try {
    for (;;) { const next = await reader.read(); if (next.done) break; if (next.value.byteLength > bytes.length - length) throw failure(null); bytes.set(next.value, length); length += next.value.byteLength; }
    return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes.subarray(0, length)));
  } catch { throw failure(null); }
  finally { await reader.cancel().catch(() => {}); reader.releaseLock(); }
}
async function browser(path: string, original?: ProviderOriginal, command?: ProviderCommand, signal?: AbortSignal): Promise<unknown> {
  if (!isAccountHome() && gatewayWorkspace() === null) throw failure(null, "unsupported");
  const headers: Record<string, string> = {};
  if (original) headers["X-Chimaera-Control-Context"] = original.context;
  let body: string | undefined;
  if (command) {
    body = JSON.stringify({ original, command });
    if (new TextEncoder().encode(body).byteLength > 8192) throw failure(null, "invalid_request");
    headers["Content-Type"] = "application/json"; headers["X-Chimaera-Browser"] = "1";
  } else if (original) {
    const identity = JSON.stringify(original);
    if (new TextEncoder().encode(identity).byteLength > 4096) throw failure(null, "invalid_request");
    headers["X-Chimaera-Provider-Original"] = identity;
  }
  let response: Response;
  try { response = await fetch(path, { method: command ? "POST" : "GET", credentials: "same-origin", cache: "no-store", redirect: "error", headers, body, signal: signal ? AbortSignal.any([signal, AbortSignal.timeout(40_000)]) : AbortSignal.timeout(40_000) }); }
  catch { throw failure(null, command ? "unconfirmed" : "unavailable"); }
  if (response.status === 401) throw failure(null, "sign_in_required");
  if (response.status === 404 && !path.includes("/operations/")) throw failure(null, "unsupported");
  const value = await bounded(response);
  if (!response.ok) {
    if (typeof value === "object" && value !== null && Object.keys(value).length === 2 && "version" in value && value.version === 1 && "error" in value && errors.includes(String(value.error))) throw failure(`providers_${value.error}`);
    throw failure(null);
  }
  return value;
}
function row(page: ProviderPage): CloudProviderStatus[] {
  return names.providers.map(name => {
    const connection = page.catalog.connections.find(row => row.provider === name.id)!;
    return { id: name.id, label: name.label, category: name.category as "agent" | "repository", installed: true, state: connection.state === "connected" ? "signed_in" : connection.state === "recovery_needed" ? "unknown" : "needs_sign_in", reason: connection.state === "recovery_needed" ? "control_unavailable" : null, checked_at: null, methods: ["personal_control"], disconnect_supported: true };
  });
}
/** A panel owns one selected transport and one original attempt. Failed Personal
 * reads never select Legacy, and ambiguous effects never create a new attempt. */
export class ProviderTransport {
  private mode: ProviderMode | null = null;
  private selection: Promise<void> | null = null;
  private revalidation: Promise<void> | null = null;
  private page: ProviderPage | null = null;
  private original: ProviderOriginal | null = null;
  private attempt: ProviderResult["attempt"] | null = null;
  private retired = false;
  private originalDeadline = 0;
  get personal(): boolean { return this.mode?.mode === "personal"; }
  get personalRequired(): boolean { return this.personal || this.mode === null && (isNativeShell() || isAccountHome() || gatewayWorkspace() !== null); }
  clear(): void { this.retired = true; this.page = null; this.original = null; this.attempt = null; }
  private async select(): Promise<void> {
    if (this.retired) throw failure(null, "context_changed");
    if (this.mode) return;
    if (!isNativeShell() && !isAccountHome() && gatewayWorkspace() === null) { this.mode = { version: 1, context: "", mode: "legacy" }; return; }
    if (this.selection) return this.selection;
    this.selection = (async () => {
      let value: unknown;
      try { value = isNativeShell() ? await proPersonalProviderMode() : await browser("/home/providers/mode"); }
      catch (cause) { throw this.rejected(cause, "unavailable"); }
      if (!providerMode(value)) throw failure(null, "unsupported");
      if (gatewayWorkspace() !== null && value.mode !== "personal") throw failure(null, "unsupported");
      if (this.retired) throw failure(null, "context_changed");
      this.mode = value;
    })();
    try { await this.selection; } finally { this.selection = null; }
  }
  async revalidate(): Promise<void> {
    if (!this.mode || this.mode.context === "") return;
    if (this.revalidation) return this.revalidation;
    this.revalidation = (async () => {
      let value: unknown;
      try { value = isNativeShell() ? await proPersonalProviderMode() : await browser("/home/providers/mode"); }
      catch (cause) { throw this.rejected(cause, "unavailable"); }
      if (!providerMode(value)) throw failure(null, "unsupported");
      this.check(value.context);
      if (this.personal && value.mode !== "personal") throw failure(null, "unsupported");
      if (this.mode?.mode === "legacy" && value.mode === "personal") { this.clear(); throw failure(null, "context_changed"); }
    })();
    try { await this.revalidation; } finally { this.revalidation = null; }
  }
  private rejected(cause: unknown, fallback = "unconfirmed"): Error {
    const error = failure(cause, fallback);
    if (["providers_context_changed", "providers_sign_in_required"].includes(error.message)) this.clear();
    return error;
  }
  private check(context: string): void {
    if (this.retired || this.mode?.context !== context) { this.clear(); throw failure(null, "context_changed"); }
  }
  pending(): CloudProviderConnection | null {
    if (!this.original) return null;
    if (this.attempt) return { ...this.attempt, id: this.original.operation_id };
    return { id: this.original.operation_id, provider_id: this.original.provider, operation: this.original.operation, phase: "preparing", expires_at: this.originalDeadline, action: null, error_code: null };
  }
  async catalog(signal?: AbortSignal): Promise<CloudSetupInfo> {
    await this.select();
    if (!this.personal) return peekCatalog(signal);
    let value: unknown;
    try { value = isNativeShell() ? await proPersonalProviderCatalog() : await browser("/home/providers", undefined, undefined, signal); }
    catch (cause) { throw this.rejected(cause); }
    signal?.throwIfAborted();
    if (!providerPage(value)) throw failure(null, "unsupported");
    this.check(value.context); this.page = value;
    return { available: true, providers: row(value), connection: this.pending(), handoffs: [] };
  }
  private async status(signal?: AbortSignal): Promise<ProviderResult> {
    const original = this.original;
    if (!original) throw failure(null, "operation_unavailable");
    let value: unknown;
    try { value = isNativeShell() ? await proPersonalProviderOperation(original) : await browser(`/home/providers/operations/${original.operation_id}`, original, undefined, signal); }
    catch (cause) { throw this.rejected(cause); }
    signal?.throwIfAborted(); this.check(original.context);
    if (!providerResult(value, original, original.operation_id)) throw failure(null);
    this.original = { ...original, attempt_id: value.attempt.id }; this.attempt = value.attempt;
    return value;
  }
  private async send(command: ProviderCommand): Promise<CloudSetupInfo> {
    const original = this.original;
    if (!original) throw failure(null, "operation_unavailable");
    let value: unknown;
    try { value = isNativeShell() ? await proPersonalProviderCommand(original, command) : await browser("/home/providers/commands", original, command); }
    catch (cause) { throw this.rejected(cause); }
    this.check(original.context);
    if (!providerResult(value, original, command.operation_id)) throw failure(null);
    this.original = { ...original, attempt_id: value.attempt.id }; this.attempt = value.attempt;
    return { available: true, connection: this.pending() };
  }
  async action(request: CloudSetupRequest, wanted: () => boolean): Promise<CloudSetupInfo | null> {
    await this.select();
    if (!this.personal) return cloudAction(request, wanted);
    return this.request(request);
  }
  async request(request: CloudSetupRequest, signal?: AbortSignal): Promise<CloudSetupInfo> {
    // A synchronous blank popup is permitted only after this panel positively
    // selected Personal; its destination stays blank until fresh status proves it.
    const popup = request.operation === "open_provider_browser" && this.personal && !isNativeShell() ? window.open("about:blank", "_blank") : null;
    if (popup) popup.opener = null;
    try {
      await this.select();
      if (!this.personal) return cloudRequest(request, signal);
      switch (request.operation) {
        case "providers": return this.catalog(signal);
        case "provider_connection": if (request.connection_id !== this.original?.operation_id) throw failure(null, "state_changed"); await this.status(signal); return { available: true, connection: this.pending() };
        case "provider_connect": case "provider_disconnect": {
          if (this.original && this.pending() && ["preparing", "waiting", "verifying"].includes(this.pending()!.phase)) throw failure(null, "state_changed");
          if (request.operation === "provider_disconnect" && request.acknowledge_cloud_work !== true) throw failure(null, "invalid_request");
          await this.catalog(signal); const page = this.page!;
          if (!providerIds.includes(request.provider_id as PersonalProvider)) throw failure(null, "invalid_request");
          const provider = request.provider_id as PersonalProvider;
          const connection = page.catalog.connections.find(row => row.provider === provider)!;
          const operation = request.operation === "provider_connect" ? "connect" : "disconnect";
          const operation_id = crypto.randomUUID();
          this.originalDeadline = Math.floor(Date.now() / 1000) + 900;
          this.original = { context: page.context, operation_id, provider, operation, expected_connection_generation: connection.generation, registration: page.catalog.providers_control, attempt_id: null }; this.attempt = null;
          return this.send({ version: 1, operation_id, provider, expected_connection_generation: connection.generation, command: operation === "connect" ? { type: "connect" } : { type: "disconnect", acknowledge_cloud_work: true } });
        }
        case "provider_submit": case "provider_cancel": {
          const status = await this.status(signal); const original = this.original!;
          if (!["preparing", "waiting", "verifying"].includes(status.attempt.phase) || request.connection_id !== original.operation_id) throw failure(null, "state_changed");
          const operation_id = crypto.randomUUID();
          const command: ProviderCommand["command"] = request.operation === "provider_cancel" ? { type: "cancel", attempt_id: status.attempt.id } : { type: "submit", attempt_id: status.attempt.id, submission_nonce: crypto.randomUUID(), code: "code" in request ? request.code : "" };
          return this.send({ version: 1, operation_id, provider: original.provider, expected_connection_generation: original.expected_connection_generation, command });
        }
        case "open_provider_browser": {
          const original = this.original; if (!original || request.connection_id !== original.operation_id) throw failure(null, "state_changed");
          if (isNativeShell()) { await proPersonalProviderOpen(original); this.check(original.context); return {}; }
          if (!popup) throw failure(null, "unavailable");
          const { attempt } = await this.status(signal); const action = attempt.action;
          const url = action?.type === "browser" ? action.url : action?.type === "device_code" ? action.verification_url : null;
          if (!url || !personalLoginUrl(original.provider, url)) throw failure(null, "state_changed");
          popup.location.replace(url); return {};
        }
        default: throw failure(null, "unsupported");
      }
    } catch (cause) { popup?.close(); throw cause; }
  }
}
