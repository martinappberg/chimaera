import { describe, expect, it } from "vitest";
import { gatewayPrefix, gatewayWorkspace } from "./base";
describe("browser host prefixes", () => {
  it("keeps host selection in each URL", () => {
    expect(gatewayPrefix("/app/host-one/")).toBe("/app/host-one");
    expect(gatewayPrefix("/app/host-two/api/v1/sessions")).toBe("/app/host-two");
    expect(gatewayPrefix("/")).toBe("");
    expect(gatewayPrefix("/application/host-one/")).toBe("");
  });
  it("rejects encoded and ambiguous host segments", () => {
    for (const path of ["/app/../", "/app/a%2fb/", "/app/a.b/", "/app//", `/app/${"x".repeat(129)}/`])
      expect(gatewayPrefix(path)).toBe("");
  });
});

describe("logical workspace routes", () => {
  it("keeps workspace identity independent from a holder or hash", () => {
    expect(gatewayPrefix("/workspace/w-one/api/v1/sessions")).toBe("/workspace/w-one");
    expect(gatewayWorkspace("/workspace/w-one/")).toBe("w-one");
    expect(gatewayWorkspace("/app/device-one/")).toBeNull();
    for (const path of ["/workspace/a%2fb/", "/workspace/../", "/workspace//", `/workspace/${"x".repeat(129)}/`]) expect(gatewayWorkspace(path)).toBeNull();
  });
});
