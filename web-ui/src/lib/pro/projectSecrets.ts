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
