import { describe, expect, it } from "vitest";
import {
  createReconnectListenerGate,
  selectRemoteReconnectSurface,
} from "./remoteReconnect";

describe("remote reconnect listener gate", () => {
  it("does not start recovery before the non-replayed listener is attached", async () => {
    const gate = createReconnectListenerGate();
    let reconnectStarted = false;
    const reconnect = gate.ready.then(() => {
      reconnectStarted = true;
    });

    await Promise.resolve();
    expect(reconnectStarted).toBe(false);

    gate.attached();
    await reconnect;
    expect(reconnectStarted).toBe(true);
  });

  it("surfaces listener setup failure instead of waiting forever", async () => {
    const gate = createReconnectListenerGate();
    gate.failed(new Error("listener unavailable"));
    await expect(gate.ready).rejects.toThrow("listener unavailable");
  });
});

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
