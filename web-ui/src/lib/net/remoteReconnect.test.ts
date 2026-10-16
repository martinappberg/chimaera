import { describe, expect, it } from "vitest";
import type { HostStatusEvent } from "./native";
import {
  clusterStrandedMessage,
  connectResultError,
  createReconnectListenerGate,
  hostStatusAction,
  selectRemoteReconnectSurface,
  type HostStatusAction,
  type HostStatusContext,
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

describe("host-status decision", () => {
  const login: HostStatusContext = {
    canReconnect: true,
    statusAlias: "devbox",
    hostAlias: "devbox",
    isJobWindow: false,
    jobEnded: false,
    port: "41000",
    token: "own",
  };
  const job: HostStatusContext = {
    ...login,
    statusAlias: "devbox#job42~ws1",
    isJobWindow: true,
  };
  const ev = (over: Partial<HostStatusEvent> & Pick<HostStatusEvent, "status">): HostStatusEvent => ({
    alias: "devbox",
    local_port: null,
    ...over,
  });

  const cases: [string, HostStatusEvent, HostStatusContext, HostStatusAction][] = [
    [
      "connected on a new port and token re-homes",
      ev({ status: "connected", local_port: 42000, token: "fresh" }),
      login,
      { kind: "healed", port: 42000, token: "fresh", moved: true },
    ],
    [
      "connected with only a new token re-homes",
      ev({ status: "connected", local_port: 41000, token: "fresh" }),
      login,
      { kind: "healed", port: 41000, token: "fresh", moved: true },
    ],
    [
      "connected on the same port and token heals in place",
      ev({ status: "connected", local_port: 41000, token: "own" }),
      login,
      { kind: "healed", port: 41000, token: "own", moved: false },
    ],
    [
      "connected without a port keeps the current origin",
      ev({ status: "connected", token: "own" }),
      login,
      { kind: "healed", port: "41000", token: "own", moved: false },
    ],
    [
      "a job window's tokenless connected carries its own token",
      ev({ alias: "devbox#job42~ws1", status: "connected", local_port: 43000 }),
      job,
      { kind: "healed", port: 43000, token: "own", moved: true },
    ],
    [
      "a login window's tokenless connected does not invent a token",
      ev({ status: "connected", local_port: 41000 }),
      login,
      { kind: "healed", port: 41000, token: null, moved: false },
    ],
    [
      "a tokenless connected on a new port is never a re-home without a token",
      ev({ status: "connected", local_port: 42999 }),
      login,
      { kind: "ignore" },
    ],
    [
      "down starts a reconnect with the shell's reason",
      ev({ status: "down", reason: "ssh exited" }),
      login,
      { kind: "reconnect", reason: "ssh exited" },
    ],
    [
      "down without a reason still reconnects",
      ev({ status: "down" }),
      login,
      { kind: "reconnect", reason: "The remote connection stopped responding." },
    ],
    ["error is left to the attempt that failed", ev({ status: "error", error: "no route" }), login, { kind: "ignore" }],
    [
      "ended on a job window's key ends the job",
      ev({ alias: "devbox#job42~ws1", status: "ended", reason: "TIMEOUT" }),
      job,
      { kind: "jobEnded", reason: "TIMEOUT" },
    ],
    ["ended on a login window is not a job ending", ev({ status: "ended" }), login, { kind: "ignore" }],
    [
      "a job that already ended ignores a later connected",
      ev({ alias: "devbox#job42~ws1", status: "connected", local_port: 43000 }),
      { ...job, jobEnded: true },
      { kind: "ignore" },
    ],
    [
      "cluster strands the window instead of healing it",
      ev({ status: "cluster" }),
      login,
      { kind: "stranded", message: clusterStrandedMessage("devbox") },
    ],
    [
      "an unknown status never heals",
      ev({ status: "paused" as HostStatusEvent["status"], local_port: 42000 }),
      login,
      { kind: "ignore" },
    ],
    [
      "another host's events are ignored",
      ev({ alias: "other", status: "connected", local_port: 42000, token: "theirs" }),
      login,
      { kind: "ignore" },
    ],
    [
      "the login alias's events are not a job window's",
      ev({ status: "down" }),
      job,
      { kind: "ignore" },
    ],
    [
      "a window that cannot reconnect ignores everything",
      ev({ status: "down" }),
      { ...login, canReconnect: false },
      { kind: "ignore" },
    ],
  ];

  for (const [name, event, ctx, expected] of cases) {
    it(name, () => {
      expect(hostStatusAction(event, ctx)).toEqual(expected);
    });
  }

  it("tolerates a missing token on every status but connected", () => {
    for (const status of ["down", "error", "ended", "cluster"] as const) {
      const action = hostStatusAction(ev({ status }), login);
      expect(action.kind).not.toBe("healed");
    }
  });
});

describe("connect result", () => {
  it("leaves a connected result to the host-status event", () => {
    expect(connectResultError({ status: "connected", error: null }, "devbox")).toBeNull();
  });

  it("treats a cluster verdict as a failure, not a heal", () => {
    expect(connectResultError({ status: "cluster", error: null }, "devbox")).toBe(
      clusterStrandedMessage("devbox"),
    );
  });

  it("keeps any other status as a failure", () => {
    expect(connectResultError({ status: "disconnected", error: "gone" }, "devbox")).toBe("gone");
    expect(connectResultError({ status: "connecting", error: null }, "devbox")).toBe(
      "devbox did not connect.",
    );
  });
});
