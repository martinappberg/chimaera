import { isAccountHome } from "../net/base";
import { isNativeShell, proProjectSecretCommand, proProjectSecretOperation, proProjectSecretsCatalog } from "../net/native";
import { secretContext, secretPage, secretResult, type SecretCommand, type SecretPage, type SecretResult } from "./projectSecrets";

export type SecretFailureCode = "unsupported" | "invalid_request" | "state_changed" | "unavailable" | "operation_unavailable" | "limit_reached" | "sign_in_required" | "context_changed" | "unconfirmed" | "account_home_required";
const codes: SecretFailureCode[] = ["unsupported", "invalid_request", "state_changed", "unavailable", "operation_unavailable", "limit_reached", "sign_in_required", "context_changed", "unconfirmed", "account_home_required"];
export class SecretFailure extends Error {
  constructor(readonly code: SecretFailureCode) { super(`project_secrets_${code}`); }
}
export function secretFailure(reason: unknown, fallback: SecretFailureCode = "unconfirmed"): SecretFailure {
  if (reason instanceof SecretFailure) return reason;
  const code = typeof reason === "string" ? reason : reason instanceof Error ? reason.message : "";
  return new SecretFailure(codes.find(known => code === `project_secrets_${known}`) ?? fallback);
}
async function bounded(response: Response): Promise<unknown> {
  if (!response.body) throw new SecretFailure("unconfirmed");
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let length = 0;
  try {
    for (;;) {
      const next = await reader.read();
      if (next.done) break;
      length += next.value.byteLength;
      if (length > 1024 * 1024) throw new SecretFailure("unconfirmed");
      // One bounded response buffer follows below; chunk count also has a cap.
      if (chunks.length >= 4096) throw new SecretFailure("unconfirmed");
      chunks.push(next.value);
    }
    const bytes = new Uint8Array(length);
    let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
    return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
  } catch { throw new SecretFailure("unconfirmed"); }
  finally { await reader.cancel().catch(() => {}); reader.releaseLock(); }
}
async function browser(path: string, method: "GET" | "POST", context?: string, command?: SecretCommand, signal?: AbortSignal): Promise<unknown> {
  if (!isAccountHome()) throw new SecretFailure("account_home_required");
  const headers: Record<string, string> = {};
  if (context !== undefined) {
    if (!secretContext(context)) throw new SecretFailure("invalid_request");
    headers["X-Chimaera-Control-Context"] = context;
  }
  let body: string | undefined;
  if (command !== undefined) {
    body = JSON.stringify(command);
    if (new TextEncoder().encode(body).byteLength > 65536) throw new SecretFailure("invalid_request");
    headers["Content-Type"] = "application/json";
    headers["X-Chimaera-Browser"] = "1";
  }
  let response: Response;
  try {
    response = await fetch(path, { method, credentials: "same-origin", cache: "no-store", redirect: "error", headers, body,
      signal: signal ? AbortSignal.any([signal, AbortSignal.timeout(40_000)]) : AbortSignal.timeout(40_000) });
  } catch { throw new SecretFailure(method === "POST" ? "unconfirmed" : "unavailable"); }
  if (response.status === 401) throw new SecretFailure("sign_in_required");
  if (response.status === 404 && path.startsWith("/home/project-secrets") && !path.includes("/operations/")) throw new SecretFailure("unsupported");
  const value = await bounded(response);
  if (!response.ok) {
    if (typeof value === "object" && value !== null && !Array.isArray(value) && Object.keys(value).length === 2 && Object.hasOwn(value, "version") && Object.hasOwn(value, "error")) {
      const error = value as { version: unknown; error: unknown };
      if (error.version === 1 && codes.some(code => code === error.error)) throw new SecretFailure(error.error as SecretFailureCode);
    }
    throw new SecretFailure("unconfirmed");
  }
  return value;
}
export async function readSecretPage(after: string | null = null, signal?: AbortSignal): Promise<SecretPage> {
  if (after !== null && !/^[A-Za-z0-9_-]{1,128}$/.test(after)) throw new SecretFailure("invalid_request");
  let value: unknown;
  try { value = isNativeShell() ? await proProjectSecretsCatalog(after) : await browser(`/home/project-secrets${after === null ? "" : `?after=${encodeURIComponent(after)}`}`, "GET", undefined, undefined, signal); }
  catch (reason) { throw secretFailure(reason, "unsupported"); }
  signal?.throwIfAborted();
  if (!secretPage(value, after)) throw new SecretFailure("unsupported");
  return value;
}
export async function sendSecretCommand(context: string, command: SecretCommand): Promise<SecretResult> {
  let value: unknown;
  try { value = isNativeShell() ? await proProjectSecretCommand(context, command) : await browser("/home/project-secrets/commands", "POST", context, command); }
  catch (reason) { throw secretFailure(reason); }
  if (!secretResult(value) || value.context !== context) throw new SecretFailure("unconfirmed");
  return value;
}
export async function readSecretOperation(context: string, operation: string, signal?: AbortSignal): Promise<SecretResult> {
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(operation)) throw new SecretFailure("invalid_request");
  let value: unknown;
  try { value = isNativeShell() ? await proProjectSecretOperation(context, operation) : await browser(`/home/project-secrets/operations/${operation}`, "GET", context, undefined, signal); }
  catch (reason) { throw secretFailure(reason); }
  signal?.throwIfAborted();
  if (!secretResult(value) || value.context !== context || value.receipt.operation_id !== operation) throw new SecretFailure("unconfirmed");
  return value;
}
