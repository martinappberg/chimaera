import { beforeEach, describe, expect, it, vi } from "vitest";
const mocks = vi.hoisted(() => ({ native: vi.fn(() => false), home: vi.fn(() => true), workspace: vi.fn(() => null as string | null), legacy: vi.fn(), mode: vi.fn(), catalog: vi.fn(), command: vi.fn(), status: vi.fn(), open: vi.fn() }));
vi.mock("../net/base", () => ({ isAccountHome: mocks.home, gatewayWorkspace: mocks.workspace }));
vi.mock("../net/native", () => ({ isNativeShell: mocks.native, proPersonalProviderMode: mocks.mode, proPersonalProviderCatalog: mocks.catalog, proPersonalProviderCommand: mocks.command, proPersonalProviderOperation: mocks.status, proPersonalProviderOpen: mocks.open }));
vi.mock("./cloudTransport", () => ({ peekCatalog: mocks.legacy, cloudRequest: mocks.legacy, cloudAction: mocks.legacy }));
import { ProviderTransport } from "./personalProviderTransport";
import { providerPage, providerResult, type ProviderOriginal, type ProviderPage } from "./personalProviders";
const context = "a".repeat(64);
const uuid = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
function page(): ProviderPage { return { version: 1, context, catalog: { providers_control: { version: 1, account_id: "account-a", holder_id: "worker-a", process_boot: uuid, registration_generation: 2, worker_credential_digest: "b".repeat(64) }, connections: ["claude", "codex", "github"].map(provider => ({ provider, state: "disconnected", generation: 1, revision: 0 })) as ProviderPage["catalog"]["connections"] } }; }
function result(original: ProviderOriginal, operation = original.operation_id) { return { version: 1, context, operation_id: operation, attempt: { id: uuid, provider_id: original.provider, operation: original.operation, phase: "waiting", expires_at: 2_000_000_000, action: { type: "browser", url: "https://claude.com/oauth", input: "authorization_code" }, error_code: null, control_version: 1, connection_generation: 1, credential_revision: 0, registration_generation: 2 } }; }
beforeEach(() => { vi.clearAllMocks(); mocks.native.mockReturnValue(false); mocks.home.mockReturnValue(true); mocks.workspace.mockReturnValue(null); vi.stubGlobal("fetch", vi.fn()); vi.stubGlobal("crypto", { randomUUID: vi.fn().mockReturnValueOnce("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").mockReturnValueOnce("cccccccc-cccc-cccc-cccc-cccccccccccc").mockReturnValue("dddddddd-dddd-dddd-dddd-dddddddddddd") }); });
function reply(value: unknown, status = 200) { return new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } }); }
describe("personal provider selection and original recovery", () => {
  it("requires a closed exact catalog and original attempt rather than opaque URLs/counters", () => {
    expect(providerPage(page())).toBe(true);
    const original: ProviderOriginal = { context, operation_id: uuid, provider: "claude", operation: "connect", expected_connection_generation: 1, registration: page().catalog.providers_control, attempt_id: null };
    expect(providerResult(result(original), original, uuid)).toBe(true);
    expect(providerResult({ ...result(original), operation_id: "wrong" }, original, uuid)).toBe(false);
    const changed = result(original); changed.attempt.action.url = "https://claude.com.attacker.invalid/";
    expect(providerResult(changed, original, uuid)).toBe(false);
    changed.attempt.action.url = "https://claude.com/oauth"; changed.attempt.registration_generation = 3;
    expect(providerResult(changed, original, uuid)).toBe(false);
    const invalid = page(); invalid.catalog.connections[1] = invalid.catalog.connections[0]; expect(providerPage(invalid)).toBe(false);
  });
  it("missing/new unsupported mode never calls legacy while explicit legacy stays separate", async () => {
    vi.mocked(fetch).mockResolvedValue(reply({ version: 1, error: "unsupported" }, 404));
    await expect(new ProviderTransport().catalog()).rejects.toThrow("providers_unsupported");
    expect(mocks.legacy).not.toHaveBeenCalled();
    vi.mocked(fetch).mockImplementation(async () => reply({ version: 1, context, mode: "legacy" })); mocks.legacy.mockResolvedValue({ available: true });
    await expect(new ProviderTransport().catalog()).resolves.toEqual({ available: true });
    expect(mocks.legacy).toHaveBeenCalledTimes(1);
    mocks.workspace.mockReturnValue("isolated-project");
    await expect(new ProviderTransport().catalog()).rejects.toThrow("providers_unsupported");
    expect(mocks.legacy).toHaveBeenCalledTimes(1);
  });
  it("lost Connect retains original parent and status needs no advanced catalog; Submit sends a distinct one-use child", async () => {
    let original: ProviderOriginal | undefined; const posts: Record<string, unknown>[] = []; const polls: string[] = [];
    vi.mocked(fetch).mockImplementation(async (input, init) => {
      const path = String(input);
      if (path.endsWith("/mode")) return reply({ version: 1, context, mode: "personal" });
      if (path === "/home/providers") return reply(page());
      if (init?.method === "POST") {
        const body = JSON.parse(String(init.body)); posts.push(body); original = body.original;
        if (posts.length === 1) throw new Error("lost synthetic reply");
        return reply(result(original!, body.command.operation_id));
      }
      const headers = init?.headers as Record<string, string>; polls.push(path);
      expect(headers["X-Chimaera-Provider-Original"]).not.toContain("synthetic-code");
      return reply(result(JSON.parse(headers["X-Chimaera-Provider-Original"])));
    });
    const transport = new ProviderTransport();
    await expect(transport.action({ operation: "provider_connect", provider_id: "claude" }, () => true)).rejects.toThrow("providers_unconfirmed");
    const parent = transport.pending()!.id;
    expect(posts).toHaveLength(1);
    await transport.request({ operation: "provider_connection", connection_id: parent });
    expect(polls[0]).toBe(`/home/providers/operations/${parent}`);
    await transport.request({ operation: "provider_submit", connection_id: parent, code: "synthetic-code" });
    const sent = posts[1] as { original: ProviderOriginal; command: { operation_id: string; command: { submission_nonce: string; code: string } } };
    expect(sent.original.operation_id).toBe(parent); expect(sent.command.operation_id).not.toBe(parent); expect(sent.command.command.submission_nonce).not.toBe(parent);
    expect(sent.command.command.code).toBe("synthetic-code"); expect(mocks.legacy).not.toHaveBeenCalled();
  });
  it("personal context replacement retires original without legacy recovery or resending", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(reply({ version: 1, context, mode: "personal" })).mockResolvedValueOnce(reply(page())).mockResolvedValueOnce(reply({ ...page(), context: "c".repeat(64) }));
    const transport = new ProviderTransport(); await transport.catalog();
    await expect(transport.catalog()).rejects.toThrow("providers_context_changed");
    await expect(transport.catalog()).rejects.toThrow("providers_context_changed"); expect(mocks.legacy).not.toHaveBeenCalled();
  });
  it("authoritative context/sign-in refusal clears an ambiguous parent and never restores it", async () => {
    for (const status of [401, 409]) {
      let posts = 0;
      vi.mocked(fetch).mockImplementation(async (input, init) => {
        if (String(input).endsWith("/mode")) return reply({ version: 1, context, mode: "personal" });
        if (String(input) === "/home/providers") return reply(page());
        if (init?.method === "POST") { posts++; return reply({ version: 1, error: status === 401 ? "sign_in_required" : "context_changed" }, status); }
        throw new Error("unexpected status request");
      });
      const transport = new ProviderTransport();
      await expect(transport.action({ operation: "provider_connect", provider_id: "claude" }, () => true)).rejects.toThrow(status === 401 ? "providers_sign_in_required" : "providers_context_changed");
      expect(transport.pending()).toBeNull();
      await expect(transport.catalog()).rejects.toThrow("providers_context_changed");
      expect(posts).toBe(1); expect(mocks.legacy).not.toHaveBeenCalled();
    }
  });
  it("ordinary free daemon keeps its old initial entry without trying new account routes", async () => {
    mocks.home.mockReturnValue(false); mocks.legacy.mockResolvedValue({ available: false });
    await expect(new ProviderTransport().catalog()).resolves.toEqual({ available: false });
    expect(fetch).not.toHaveBeenCalled();
  });
  it("routine mode revalidation preserves the original while an unsupported downgrade stays latched", async () => {
    mocks.native.mockReturnValue(true); mocks.mode.mockResolvedValue({ version: 1, context, mode: "personal" }); mocks.catalog.mockResolvedValue(page());
    mocks.command.mockImplementation(async (original, command) => result(original, command.operation_id));
    const transport = new ProviderTransport(); await transport.action({ operation: "provider_connect", provider_id: "claude" }, () => true);
    const parent = transport.pending()!.id; await transport.revalidate(); expect(transport.pending()!.id).toBe(parent);
    mocks.mode.mockResolvedValue({ version: 1, context, mode: "legacy" });
    await expect(transport.revalidate()).rejects.toThrow("providers_unsupported"); expect(transport.personal).toBe(true); expect(transport.pending()!.id).toBe(parent); expect(mocks.legacy).not.toHaveBeenCalled();
    mocks.mode.mockResolvedValue({ version: 1, context: "c".repeat(64), mode: "personal" });
    await expect(transport.revalidate()).rejects.toThrow("providers_context_changed"); expect(transport.pending()).toBeNull();
  });
  it("positive Legacy to Personal migration retires the old owner before any further legacy effect", async () => {
    mocks.native.mockReturnValue(true); mocks.mode.mockResolvedValue({ version: 1, context, mode: "legacy" }); mocks.legacy.mockResolvedValue({ available: true });
    const transport = new ProviderTransport(); await transport.catalog();
    expect(mocks.legacy).toHaveBeenCalledTimes(1);
    mocks.mode.mockResolvedValue({ version: 1, context, mode: "personal" });
    await expect(transport.revalidate()).rejects.toThrow("providers_context_changed");
    await expect(transport.action({ operation: "provider_connect", provider_id: "claude" }, () => true)).rejects.toThrow("providers_context_changed");
    expect(mocks.legacy).toHaveBeenCalledTimes(1); expect(transport.pending()).toBeNull();
  });
  it("coalesces concurrent routine status probes without retiring the original", async () => {
    mocks.native.mockReturnValue(true); mocks.mode.mockResolvedValue({ version: 1, context, mode: "personal" }); mocks.catalog.mockResolvedValue(page());
    mocks.command.mockImplementation(async (original, command) => result(original, command.operation_id));
    const transport = new ProviderTransport(); await transport.action({ operation: "provider_connect", provider_id: "claude" }, () => true); const parent = transport.pending()!.id;
    let release!: (value: unknown) => void; mocks.mode.mockImplementation(() => new Promise(resolve => { release = resolve; }));
    const before = mocks.mode.mock.calls.length; const a = transport.revalidate(); const b = transport.revalidate();
    expect(mocks.mode.mock.calls.length - before).toBe(1); release({ version: 1, context, mode: "personal" }); await Promise.all([a, b]);
    expect(transport.pending()!.id).toBe(parent);
  });
  it("browser opener rereads the parent and refuses a changed/unsafe action before navigating", async () => {
    const replace = vi.fn(); const close = vi.fn(); const popup = { opener: "old", location: { replace }, close };
    const open = vi.fn(() => popup); vi.stubGlobal("window", { open });
    let original: ProviderOriginal | null = null; let unsafe = false;
    vi.mocked(fetch).mockImplementation(async (input, init) => {
      if (String(input).endsWith("/mode")) return reply({ version: 1, context, mode: "personal" });
      if (String(input) === "/home/providers") return reply(page());
      if (init?.method === "POST") { const body = JSON.parse(String(init.body)); original = body.original; return reply(result(original!, body.command.operation_id)); }
      const value = result(JSON.parse((init?.headers as Record<string, string>)["X-Chimaera-Provider-Original"]));
      if (unsafe) value.attempt.action.url = "https://attacker.invalid/";
      return reply(value);
    });
    const transport = new ProviderTransport(); await transport.action({ operation: "provider_connect", provider_id: "claude" }, () => true); const parent = transport.pending()!.id;
    await transport.request({ operation: "open_provider_browser", connection_id: parent });
    expect(open).toHaveBeenCalledWith("about:blank", "_blank"); expect(popup.opener).toBeNull(); expect(replace).toHaveBeenCalledWith("https://claude.com/oauth");
    unsafe = true; replace.mockClear();
    await expect(transport.request({ operation: "open_provider_browser", connection_id: parent })).rejects.toThrow("providers_unconfirmed"); expect(replace).not.toHaveBeenCalled(); expect(close).toHaveBeenCalledTimes(1);
  });

});
