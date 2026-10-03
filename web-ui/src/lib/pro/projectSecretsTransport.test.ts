import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { readSecretOperation, readSecretPage, sendSecretCommand } from "./projectSecretsTransport";
import type { SecretCommand } from "./projectSecrets";
const mocks = vi.hoisted(() => ({ native: vi.fn(() => false), home: vi.fn(() => true), catalog: vi.fn(), command: vi.fn(), operation: vi.fn() }));
vi.mock("../net/base", () => ({ isAccountHome: mocks.home }));
vi.mock("../net/native", () => ({ isNativeShell: mocks.native, proProjectSecretsCatalog: mocks.catalog, proProjectSecretCommand: mocks.command, proProjectSecretOperation: mocks.operation }));
const context = "a".repeat(64), operation = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const command: SecretCommand = { version: 1, action: "set", operation_id: operation, workspace_id: "project-a", expected_revision: 1, expected_pending: null, name: "SERVICE_TOKEN", value: "synthetic-only-value" };
function page() { return { version: 1, context, catalog: { version: 1, project_secrets: 1, name_policy: { max_name_bytes: 128, max_value_bytes: 8192, max_names: 32, reserved_names: ["HOME"], reserved_prefixes: ["CODEX_"] }, projects: [], next: null } }; }
function result() { return { version: 1, context, receipt: { version: 1, operation_id: operation, workspace_id: "project-a", base_revision: 1, result_revision: null, names: ["SERVICE_TOKEN"], outcome: "queued" } }; }
let fetcher: ReturnType<typeof vi.fn>;
beforeEach(() => {
  mocks.native.mockReturnValue(false); mocks.home.mockReturnValue(true);
  mocks.catalog.mockReset(); mocks.command.mockReset(); mocks.operation.mockReset();
  fetcher = vi.fn(); vi.stubGlobal("fetch", fetcher);
});
afterEach(() => vi.unstubAllGlobals());
describe("personal selected-secret adapter", () => {
  it("reads only account Home, with no daemon/project or native fallback", async () => {
    mocks.home.mockReturnValue(false);
    await expect(readSecretPage()).rejects.toThrow("project_secrets_account_home_required");
    expect(fetcher).not.toHaveBeenCalled();
    mocks.native.mockReturnValue(true);
    mocks.catalog.mockRejectedValue("old shell missing command");
    await expect(readSecretPage()).rejects.toThrow("project_secrets_unsupported");
    expect(fetcher).not.toHaveBeenCalled();
  });
  it("preserves the canonical cursor and closed contextual page", async () => {
    fetcher.mockResolvedValue(Response.json(page()));
    await expect(readSecretPage("project-0")).resolves.toEqual(page());
    expect(fetcher.mock.calls[0][0]).toBe("/home/project-secrets?after=project-0");
    expect(fetcher.mock.calls[0][1]).toMatchObject({ method: "GET", credentials: "same-origin", cache: "no-store", redirect: "error", headers: {} });
    fetcher.mockResolvedValue(Response.json({ ...page(), credential: "untrusted" }));
    await expect(readSecretPage()).rejects.toThrow("project_secrets_unsupported");
  });
  it("sends a value once and reconciles only the original operation passively", async () => {
    fetcher.mockRejectedValueOnce(new Error("raw upstream diagnostic"));
    await expect(sendSecretCommand(context, command)).rejects.toThrow("project_secrets_unconfirmed");
    expect(fetcher).toHaveBeenCalledTimes(1);
    expect(fetcher.mock.calls[0]).toEqual(["/home/project-secrets/commands", expect.objectContaining({ method: "POST", body: JSON.stringify(command), headers: { "Content-Type": "application/json", "X-Chimaera-Browser": "1", "X-Chimaera-Control-Context": context } })]);
    fetcher.mockResolvedValueOnce(Response.json(result()));
    await expect(readSecretOperation(context, operation)).resolves.toEqual(result());
    expect(fetcher.mock.calls[1]).toEqual([`/home/project-secrets/operations/${operation}`, expect.objectContaining({ method: "GET", body: undefined, headers: { "X-Chimaera-Control-Context": context } })]);
    expect(fetcher.mock.calls.filter(([, init]) => init.method === "POST")).toHaveLength(1);
  });
  it("never retries a 401 command and never echoes unknown response errors", async () => {
    fetcher.mockResolvedValueOnce(Response.json({ raw: "synthetic-provider-content" }, { status: 401 }));
    await expect(sendSecretCommand(context, command)).rejects.toThrow("project_secrets_sign_in_required");
    expect(fetcher).toHaveBeenCalledTimes(1);
    fetcher.mockResolvedValueOnce(Response.json({ version: 1, error: "synthetic-provider-content" }, { status: 502 }));
    await expect(sendSecretCommand(context, command)).rejects.toThrow("project_secrets_unconfirmed");
    fetcher.mockResolvedValueOnce(Response.json({ version: 1, error: "context_changed" }, { status: 409 }));
    await expect(sendSecretCommand(context, command)).rejects.toThrow("project_secrets_context_changed");
  });
  it("rejects mismatched contexts/receipts and oversized responses", async () => {
    fetcher.mockResolvedValueOnce(Response.json({ ...result(), context: "b".repeat(64) }));
    await expect(sendSecretCommand(context, command)).rejects.toThrow("project_secrets_unconfirmed");
    fetcher.mockResolvedValueOnce(Response.json({ ...result(), receipt: { ...result().receipt, operation_id: "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb" } }));
    await expect(readSecretOperation(context, operation)).rejects.toThrow("project_secrets_unconfirmed");
    fetcher.mockResolvedValueOnce(new Response("x".repeat(1024 * 1024 + 1)));
    await expect(readSecretPage()).rejects.toThrow("project_secrets_unconfirmed");
  });
  it("keeps native intent and refusal isolated from browser cookies", async () => {
    mocks.native.mockReturnValue(true);
    mocks.command.mockResolvedValueOnce(result());
    await expect(sendSecretCommand(context, command)).resolves.toEqual(result());
    expect(mocks.command).toHaveBeenCalledWith(context, command);
    mocks.operation.mockRejectedValueOnce("project_secrets_context_changed");
    await expect(readSecretOperation(context, operation)).rejects.toThrow("project_secrets_context_changed");
    expect(fetcher).not.toHaveBeenCalled();
  });
});
