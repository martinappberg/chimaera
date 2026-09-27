import { describe, expect, it } from "vitest";
import { gatewayPrefix } from "./base";
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
