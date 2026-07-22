import { describe, expect, it } from "vitest";
import { selectRemoteReconnectSurface } from "./remoteReconnect";

describe("remote reconnect recovery surface", () => {
  it("downgrades a dismissed failure to a persistent retry", () => {
    expect(
      selectRemoteReconnectSurface({
        open: true,
        error: "host offline",
        authBlocked: true,
      }),
    ).toBe("failure");
    expect(
      selectRemoteReconnectSurface({
        open: false,
        error: "host offline",
        authBlocked: true,
      }),
    ).toBe("retry");
  });

  it("keeps retry reachable whenever native authorization is still blocked", () => {
    expect(
      selectRemoteReconnectSurface({ open: false, error: null, authBlocked: true }),
    ).toBe("retry");
  });

  it("lets ordinary transient status dismiss when no failure remains", () => {
    expect(
      selectRemoteReconnectSurface({ open: true, error: null, authBlocked: false }),
    ).toBe("status");
    expect(
      selectRemoteReconnectSurface({ open: false, error: null, authBlocked: false }),
    ).toBe("hidden");
  });
});
