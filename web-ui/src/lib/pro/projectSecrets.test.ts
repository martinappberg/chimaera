import { describe, expect, it } from "vitest";
import { confirmsSecretAttempt, secretAttempt, secretNameAllowed, secretPage, secretPolicy, secretResult, secretValueAllowed, type SecretCommand, type SecretPage, type SecretResult } from "./projectSecrets";

const context = "a".repeat(64);
const operation = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const pending = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
const page = (): SecretPage => ({ version: 1, context, catalog: { version: 1, project_secrets: 1, name_policy: { max_name_bytes: 128, max_value_bytes: 8192, max_names: 32, reserved_names: ["HOME"], reserved_prefixes: ["CODEX_"] }, projects: [{ workspace_id: "project-a", revision: 3, applied_names: ["EXISTING"], pending: { operation_id: pending, base_revision: 3, names: ["QUEUED"] }, state: "ready" }], next: null } });
const command = (): SecretCommand => ({ version: 1, operation_id: operation, workspace_id: "project-a", expected_revision: 3, expected_pending: pending, action: "set", name: "NEW", value: "synthetic-only" });
const result = (): SecretResult => ({ version: 1, context, receipt: { version: 1, operation_id: operation, workspace_id: "project-a", base_revision: 3, result_revision: null, names: ["NEW", "QUEUED"], outcome: "queued" } });

describe("selected project secret decisions", () => {
  it("requires a closed positive page and the exact current restrictions", () => {
    expect(secretPage(page())).toBe(true);
    expect(secretPage({ ...page(), context: "account-name" })).toBe(false);
    expect(secretPage({ ...page(), token: "synthetic" })).toBe(false);
    const missing = page() as unknown as { catalog: Record<string, unknown> };
    delete missing.catalog.next;
    expect(secretPage(missing)).toBe(false);
    const unknown = page();
    (unknown.catalog.projects[0] as unknown as Record<string, unknown>).state = "unknown";
    expect(secretPage(unknown)).toBe(false);
    const policy = page().catalog.name_policy;
    expect(secretPolicy({ ...policy, max_value_bytes: 10000 })).toBe(false);
    expect(secretNameAllowed(policy, "HOME")).toBe(false);
    expect(secretNameAllowed(policy, "CODEX_TOKEN")).toBe(false);
    expect(secretNameAllowed(policy, "SELECTED_TOKEN")).toBe(true);
  });
  it("bounds UTF-8 bytes rather than displayed characters", () => {
    expect(secretValueAllowed("ä".repeat(4096))).toBe(true);
    expect(secretValueAllowed("ä".repeat(4097))).toBe(false);
    expect(secretValueAllowed("value\0suffix")).toBe(false);
    expect(secretValueAllowed("")).toBe(false);
    expect(secretValueAllowed("\ud800")).toBe(false);
    expect(secretValueAllowed("\udc00")).toBe(false);
    expect(secretValueAllowed("😀".repeat(2048))).toBe(true);
  });
  it("refuses stale batches and keeps only a redacted original decision", () => {
    const current = page();
    const attempt = secretAttempt(command(), context, current.catalog.projects[0], current.catalog.name_policy);
    expect(attempt?.names).toEqual(["NEW", "QUEUED"]);
    expect(attempt).not.toHaveProperty("value");
    expect(JSON.stringify(attempt)).not.toContain("synthetic-only");
    expect(secretAttempt({ ...command(), expected_pending: null }, context, current.catalog.projects[0], current.catalog.name_policy)).toBeNull();
    expect(secretAttempt({ ...command(), expected_revision: 4 }, context, current.catalog.projects[0], current.catalog.name_policy)).toBeNull();
    current.catalog.projects[0].state = "applying";
    expect(secretAttempt(command(), context, current.catalog.projects[0], current.catalog.name_policy)).toBeNull();
  });
  it("correlates original context, operation, workspace, revision and whole batch", () => {
    const current = page();
    const attempt = secretAttempt(command(), context, current.catalog.projects[0], current.catalog.name_policy)!;
    expect(confirmsSecretAttempt(result(), attempt)).toBe(true);
    expect(confirmsSecretAttempt({ ...result(), context: "b".repeat(64) }, attempt)).toBe(false);
    for (const changed of [{ operation_id: pending }, { workspace_id: "project-b" }, { base_revision: 2 }, { names: ["NEW"] }]) {
      expect(confirmsSecretAttempt({ ...result(), receipt: { ...result().receipt, ...changed } }, attempt)).toBe(false);
    }
    expect(confirmsSecretAttempt({ ...result(), receipt: { ...result().receipt, outcome: "canceled" } }, attempt)).toBe(false);
  });
  it("shows applying separately and never accepts it without a newer floor", () => {
    expect(secretResult({ ...result(), receipt: { ...result().receipt, outcome: "applying", result_revision: 4 } })).toBe(true);
    expect(secretResult({ ...result(), receipt: { ...result().receipt, outcome: "applied", result_revision: 3 } })).toBe(false);
    expect(secretResult({ ...result(), receipt: { ...result().receipt, outcome: "queued", result_revision: 4 } })).toBe(false);
    expect(secretResult({ ...result(), receipt: { ...result().receipt, value: "never-status" } })).toBe(false);
  });
  it("requires exact pending state for Apply and Cancel, while Remove names applied access", () => {
    const current = page();
    const row = current.catalog.projects[0], policy = current.catalog.name_policy;
    const base = { version: 1 as const, operation_id: operation, workspace_id: row.workspace_id, expected_revision: row.revision, expected_pending: pending };
    expect(secretAttempt({ ...base, action: "apply" }, context, row, policy)?.names).toEqual(["QUEUED"]);
    expect(secretAttempt({ ...base, action: "cancel" }, context, row, policy)?.names).toEqual(["QUEUED"]);
    expect(secretAttempt({ ...base, action: "remove", name: "EXISTING" }, context, row, policy)?.names).toEqual(["EXISTING"]);
    expect(secretAttempt({ ...base, action: "remove", name: "QUEUED" }, context, row, policy)).toBeNull();
    row.pending = null;
    expect(secretAttempt({ ...base, expected_pending: null, action: "apply" }, context, row, policy)).toBeNull();
  });
  it("requires ordered progress and rejects cursor or capability drift", () => {
    expect(secretPage(page(), "project-0")).toBe(true);
    expect(secretPage(page(), "project-a")).toBe(false);
    const bad = page();
    bad.catalog.next = "project-z";
    expect(secretPage(bad)).toBe(false);
    bad.catalog.next = "project-a";
    bad.catalog.projects = [];
    expect(secretPage(bad)).toBe(false);
  });
});
