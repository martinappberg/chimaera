/** Redacted account controls. None of these status types contain a value. */
export type SecretPending = { operation_id: string; base_revision: number; names: string[] };
export type SecretProject = { workspace_id: string; revision: number; applied_names: string[]; pending: SecretPending | null; state: "ready" | "applying" | "unavailable" };
export type SecretPolicy = { max_name_bytes: 128; max_value_bytes: 8192; max_names: 32; reserved_names: string[]; reserved_prefixes: string[] };
export type SecretCatalog = { version: 1; project_secrets: 1; name_policy: SecretPolicy; projects: SecretProject[]; next: string | null };
export type SecretReceipt = { version: 1; operation_id: string; workspace_id: string; base_revision: number; result_revision: number | null; names: string[]; outcome: "queued" | "applying" | "applied" | "canceled" };
export type SecretPage = { version: 1; context: string; catalog: SecretCatalog };
export type SecretResult = { version: 1; context: string; receipt: SecretReceipt };
type Decision = { version: 1; operation_id: string; workspace_id: string; expected_revision: number; expected_pending: string | null };
export type SecretCommand = Decision & ({ action: "set"; name: string; value: string } | { action: "remove"; name: string } | { action: "apply" | "cancel" });
/** Retained only in memory after a send; includes no submitted value. */
export type SecretAttempt = Decision & { context: string; action: SecretCommand["action"]; names: string[] };

const uuid = (value: unknown): value is string => typeof value === "string" && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(value);
const id = (value: unknown): value is string => typeof value === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(value);
const name = (value: unknown): value is string => typeof value === "string" && /^[A-Z_][A-Z0-9_]{0,127}$/.test(value);
const revision = (value: unknown): value is number => typeof value === "number" && Number.isSafeInteger(value) && value > 0;
export const secretContext = (value: unknown): value is string => typeof value === "string" && /^[0-9a-f]{64}$/.test(value);
function object(value: unknown, keys: string[]): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value) && Object.keys(value).length === keys.length && keys.every(key => Object.hasOwn(value, key));
}
function names(value: unknown, max = 32): value is string[] {
  return Array.isArray(value) && value.length <= max && value.every(name) && value.every((item, index) => index === 0 || value[index - 1] < item);
}
export function secretPolicy(value: unknown): value is SecretPolicy {
  return object(value, ["max_name_bytes", "max_value_bytes", "max_names", "reserved_names", "reserved_prefixes"])
    && value.max_name_bytes === 128 && value.max_value_bytes === 8192 && value.max_names === 32
    && names(value.reserved_names, 128) && names(value.reserved_prefixes, 128);
}
export function secretNameAllowed(policy: SecretPolicy, candidate: string): boolean {
  return secretPolicy(policy) && name(candidate) && !policy.reserved_names.includes(candidate) && !policy.reserved_prefixes.some(prefix => candidate.startsWith(prefix));
}
export function secretValueAllowed(value: string): boolean {
  if (value.length === 0 || value.length > 8192 || value.includes("\0")) return false;
  // JSON/Rust accepts scalar Unicode, while TextEncoder replaces isolated
  // surrogates. Refuse them before clearing a value for a doomed command.
  for (let index = 0; index < value.length; index++) {
    const unit = value.charCodeAt(index);
    if (unit >= 0xd800 && unit <= 0xdbff) {
      const next = value.charCodeAt(++index);
      if (!(next >= 0xdc00 && next <= 0xdfff)) return false;
    } else if (unit >= 0xdc00 && unit <= 0xdfff) return false;
  }
  return new TextEncoder().encode(value).byteLength <= 8192;
}
function project(value: unknown): value is SecretProject {
  if (!object(value, ["workspace_id", "revision", "applied_names", "pending", "state"]) || !id(value.workspace_id) || !revision(value.revision) || !names(value.applied_names) || !["ready", "applying", "unavailable"].some(state => state === value.state)) return false;
  if (value.pending === null) return true;
  return object(value.pending, ["operation_id", "base_revision", "names"]) && uuid(value.pending.operation_id) && value.pending.base_revision === value.revision
    && names(value.pending.names) && value.pending.names.length > 0 && new Set([...value.applied_names, ...value.pending.names]).size <= 32;
}
export function secretPage(value: unknown, after: string | null = null): value is SecretPage {
  if (!object(value, ["version", "context", "catalog"]) || value.version !== 1 || !secretContext(value.context)
    || !object(value.catalog, ["version", "project_secrets", "name_policy", "projects", "next"])) return false;
  const page = value.catalog;
  if (page.version !== 1 || page.project_secrets !== 1 || !secretPolicy(page.name_policy) || !Array.isArray(page.projects) || page.projects.length > 64 || !page.projects.every(project)) return false;
  const rows = page.projects;
  return rows.every((row, index) => (after === null || row.workspace_id > after) && (index === 0 || rows[index - 1].workspace_id < row.workspace_id))
    && (page.next === null || id(page.next) && rows.length > 0 && page.next === rows[rows.length - 1].workspace_id);
}
export function secretResult(value: unknown): value is SecretResult {
  if (!object(value, ["version", "context", "receipt"]) || value.version !== 1 || !secretContext(value.context)
    || !object(value.receipt, ["version", "operation_id", "workspace_id", "base_revision", "result_revision", "names", "outcome"])) return false;
  const receipt = value.receipt;
  return receipt.version === 1 && uuid(receipt.operation_id) && id(receipt.workspace_id) && revision(receipt.base_revision) && names(receipt.names) && receipt.names.length > 0
    && ((receipt.outcome === "queued" || receipt.outcome === "canceled") && receipt.result_revision === null
      || (receipt.outcome === "applying" || receipt.outcome === "applied") && revision(receipt.result_revision) && receipt.result_revision > receipt.base_revision);
}
/** Build the exact displayed decision and a value-free reconciliation record. */
export function secretAttempt(command: SecretCommand, context: string, row: SecretProject, policy: SecretPolicy): SecretAttempt | null {
  if (!secretContext(context) || !uuid(command.operation_id) || command.version !== 1 || !project(row) || row.state !== "ready"
    || command.workspace_id !== row.workspace_id || command.expected_revision !== row.revision || command.expected_pending !== (row.pending?.operation_id ?? null)
    || command.operation_id === command.expected_pending || !secretPolicy(policy)) return null;
  let affected: string[];
  if (command.action === "set") {
    if (!secretNameAllowed(policy, command.name) || !secretValueAllowed(command.value)) return null;
    affected = [...new Set([...(row.pending?.names ?? []), command.name])].sort();
    if (new Set([...row.applied_names, ...affected]).size > 32) return null;
  } else if (command.action === "remove") {
    if (!row.applied_names.includes(command.name)) return null;
    affected = [command.name];
  } else {
    if (row.pending === null) return null;
    affected = [...row.pending.names];
  }
  return { version: 1, operation_id: command.operation_id, workspace_id: command.workspace_id, expected_revision: command.expected_revision, expected_pending: command.expected_pending, context, action: command.action, names: affected };
}
export function confirmsSecretAttempt(result: SecretResult, attempt: SecretAttempt): boolean {
  if (!secretResult(result) || result.context !== attempt.context) return false;
  const receipt = result.receipt;
  const outcome = attempt.action === "cancel" ? receipt.outcome === "canceled" : attempt.action === "set" ? receipt.outcome !== "canceled" : receipt.outcome === "applying" || receipt.outcome === "applied";
  return outcome && receipt.operation_id === attempt.operation_id && receipt.workspace_id === attempt.workspace_id && receipt.base_revision === attempt.expected_revision
    && receipt.names.length === attempt.names.length && receipt.names.every((name, index) => name === attempt.names[index]);
}
