import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { observeSecretContext, retainSecretAttempt, secretReconciliation, settleSecretAttempt } from "./projectSecretsMemory";
import type { SecretAttempt } from "./projectSecrets";
function attempt(index: number, context: string): SecretAttempt { return { version: 1, operation_id: `${index.toString(16).padStart(8, "0")}-aaaa-aaaa-aaaa-aaaaaaaaaaaa`, workspace_id: `project-${index}`, expected_revision: 1, expected_pending: null, action: "set", context, names: ["SERVICE_TOKEN"] }; }
describe("value-free receipt memory", () => {
  it("retains originals across settings closure but not account replacement", () => {
    const context = "1".repeat(64);
    observeSecretContext(context);
    expect(retainSecretAttempt({ ...attempt(1, context), value: "must-not-retain" } as SecretAttempt)).toBe(true);
    expect(JSON.stringify(get(secretReconciliation))).not.toContain("must-not-retain");
    observeSecretContext(context);
    expect(get(secretReconciliation).attempts).toHaveLength(1);
    observeSecretContext("2".repeat(64));
    expect(get(secretReconciliation).attempts).toHaveLength(0);
    expect(retainSecretAttempt(attempt(1, context))).toBe(false);
  });
  it("never evicts an ambiguous original to admit another request", () => {
    const context = "3".repeat(64);
    observeSecretContext(context);
    for (let index = 0; index < 24; index++) expect(retainSecretAttempt(attempt(index, context))).toBe(true);
    expect(retainSecretAttempt(attempt(24, context))).toBe(false);
    expect(retainSecretAttempt(attempt(0, context))).toBe(false);
    settleSecretAttempt("4".repeat(64), attempt(0, context).operation_id);
    expect(get(secretReconciliation).attempts).toHaveLength(24);
    settleSecretAttempt(context, attempt(0, context).operation_id);
    expect(retainSecretAttempt(attempt(24, context))).toBe(true);
  });
});
