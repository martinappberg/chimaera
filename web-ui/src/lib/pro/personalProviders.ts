import origins from "../../../../crates/chimaera-core/src/cloud-providers.json";
export type PersonalProvider = "claude" | "codex" | "github";
export interface ProviderMode { version: 1; context: string; mode: "legacy" | "personal" }
export interface ProviderRegistration { version: 1; account_id: string; holder_id: string; process_boot: string; registration_generation: number; worker_credential_digest: string }
export interface ProviderCatalog { providers_control: ProviderRegistration; connections: { provider: PersonalProvider; state: "disconnected" | "connected" | "needs_sign_in" | "recovery_needed"; generation: number; revision: number }[] }
export interface ProviderPage { version: 1; context: string; catalog: ProviderCatalog }
export interface ProviderOriginal { context: string; operation_id: string; provider: PersonalProvider; operation: "connect" | "disconnect"; expected_connection_generation: number; registration: ProviderRegistration; attempt_id: string | null }
export interface ProviderCommand { version: 1; operation_id: string; provider: PersonalProvider; expected_connection_generation: number; command: { type: "connect" } | { type: "disconnect"; acknowledge_cloud_work: true } | { type: "cancel"; attempt_id: string } | { type: "submit"; attempt_id: string; submission_nonce: string; code: string } }
export interface ProviderAttempt { id: string; provider_id: PersonalProvider; operation: "connect" | "disconnect"; phase: "preparing" | "waiting" | "verifying" | "connected" | "disconnected" | "failed" | "canceled" | "expired"; expires_at: number; action: { type: "device_code"; verification_url: string; user_code: string } | { type: "browser"; url: string; input: "authorization_code" } | null; error_code: string | null; control_version: 1; connection_generation: number; credential_revision: number; registration_generation: number }
export interface ProviderResult { version: 1; context: string; operation_id: string; attempt: ProviderAttempt }
export const providerIds: PersonalProvider[] = ["claude", "codex", "github"];
export const providerContext = (value: unknown): value is string => typeof value === "string" && /^[a-f0-9]{64}$/.test(value);
export const providerUuid = (value: unknown): value is string => typeof value === "string" && /^[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}$/.test(value);
const counter = (v: unknown): v is number => typeof v === "number" && Number.isSafeInteger(v) && v >= 0;
const stableId = (v: unknown): v is string => typeof v === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(v);
function object(v: unknown): v is Record<string, unknown> { return typeof v === "object" && v !== null && !Array.isArray(v); }
function keys(v: Record<string, unknown>, expected: string[]): boolean { return Object.keys(v).length === expected.length && expected.every(key => Object.hasOwn(v, key)); }
export function providerMode(v: unknown): v is ProviderMode { return object(v) && keys(v, ["version", "context", "mode"]) && v.version === 1 && providerContext(v.context) && (v.mode === "personal" || v.mode === "legacy"); }
export function providerRegistration(v: unknown): v is ProviderRegistration { return object(v) && keys(v, ["version", "account_id", "holder_id", "process_boot", "registration_generation", "worker_credential_digest"]) && v.version === 1 && stableId(v.account_id) && stableId(v.holder_id) && providerUuid(v.process_boot) && counter(v.registration_generation) && v.registration_generation > 0 && providerContext(v.worker_credential_digest); }
export function providerPage(v: unknown): v is ProviderPage {
  if (!object(v) || !keys(v, ["version", "context", "catalog"]) || v.version !== 1 || !providerContext(v.context) || !object(v.catalog) || !keys(v.catalog, ["providers_control", "connections"]) || !providerRegistration(v.catalog.providers_control) || !Array.isArray(v.catalog.connections) || v.catalog.connections.length !== 3) return false;
  const seen = new Set();
  return v.catalog.connections.every(row => {
    if (!object(row) || !keys(row, ["provider", "state", "generation", "revision"]) || !providerIds.includes(row.provider as PersonalProvider) || seen.has(row.provider) || !["disconnected", "connected", "needs_sign_in", "recovery_needed"].includes(String(row.state)) || !counter(row.generation) || !counter(row.revision) || row.state === "connected" && row.revision === 0 || row.state === "disconnected" && row.revision !== 0) return false;
    seen.add(row.provider); return true;
  });
}
export function personalLoginUrl(provider: PersonalProvider, value: string): boolean {
  if (value.length > 4096) return false;
  try {
    const url = new URL(value);
    const row = origins.providers.find(row => row.id === provider);
    return url.protocol === "https:" && !url.username && !url.password && !url.hash && (row?.auth_origins ?? []).includes(url.origin);
  } catch { return false; }
}
export function providerResult(v: unknown, original: ProviderOriginal, operation: string): v is ProviderResult {
  if (!object(v) || !keys(v, ["version", "context", "operation_id", "attempt"]) || v.version !== 1 || v.context !== original.context || v.operation_id !== operation || !object(v.attempt)) return false;
  const a = v.attempt;
  if (!keys(a, ["id", "provider_id", "operation", "phase", "expires_at", "action", "error_code", "control_version", "connection_generation", "credential_revision", "registration_generation"]) || !providerUuid(a.id) || original.attempt_id !== null && original.attempt_id !== a.id || a.provider_id !== original.provider || a.operation !== original.operation || a.control_version !== 1 || a.registration_generation !== original.registration.registration_generation || !counter(a.expires_at) || !counter(a.connection_generation) || !counter(a.credential_revision) || !["preparing", "waiting", "verifying", "connected", "disconnected", "failed", "canceled", "expired"].includes(String(a.phase))) return false;
  const committed = a.phase === "connected" || a.phase === "disconnected";
  if (a.connection_generation !== original.expected_connection_generation + (committed ? 1 : 0) || a.phase === "connected" && (original.operation !== "connect" || a.credential_revision === 0) || a.phase === "disconnected" && (original.operation !== "disconnect" || a.credential_revision !== 0)) return false;
  if (a.error_code !== null && !["canceled", "expired", "unsupported_login", "control_unavailable", "cleanup_failed", "account_changed", "provider_changed", "sign_in_failed", "publication_failed", "invalid_authorization_code", "verification_failed"].includes(String(a.error_code))) return false;
  if (a.action === null) return true;
  if (a.phase !== "waiting" || !object(a.action)) return false;
  const action = a.action;
  if (action.type === "browser") return keys(action, ["type", "url", "input"]) && original.provider === "claude" && action.input === "authorization_code" && typeof action.url === "string" && personalLoginUrl(original.provider, action.url);
  return original.provider !== "claude" && action.type === "device_code" && keys(action, ["type", "verification_url", "user_code"]) && typeof action.verification_url === "string" && personalLoginUrl(original.provider, action.verification_url) && typeof action.user_code === "string" && /^[A-Za-z0-9-]{4,32}$/.test(action.user_code);
}
