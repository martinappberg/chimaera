import { writable } from "svelte/store";
import { secretContext, type SecretAttempt } from "./projectSecrets";

/** Window memory only: closing Settings preserves original receipt lookup,
 * while account replacement discards the old context. Never retain a value. */
type Memory = { context: string | null; attempts: SecretAttempt[] };
let current: Memory = { context: null, attempts: [] };
const store = writable<Memory>(current);
export const secretReconciliation = { subscribe: store.subscribe };
function publish(next: Memory): void { current = next; store.set(next); }
export function observeSecretContext(context: string): void {
  if (!secretContext(context)) return;
  if (current.context !== context) publish({ context, attempts: [] });
}
export function retainSecretAttempt(attempt: SecretAttempt): boolean {
  if (current.context !== attempt.context || current.attempts.length >= 24
    || current.attempts.some(row => row.workspace_id === attempt.workspace_id || row.operation_id === attempt.operation_id)) return false;
  // Copy only the enumerated redacted fields, even if a caller has extra keys.
  const { version, operation_id, workspace_id, expected_revision, expected_pending, context, action, names } = attempt;
  publish({ context, attempts: [...current.attempts, { version, operation_id, workspace_id, expected_revision, expected_pending, context, action, names: [...names] }] });
  return true;
}
export function settleSecretAttempt(context: string, operation: string): void {
  if (context !== current.context) return;
  publish({ context, attempts: current.attempts.filter(row => row.operation_id !== operation) });
}
