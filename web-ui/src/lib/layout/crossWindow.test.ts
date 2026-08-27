import { describe, it, expect, afterEach } from "vitest";
import {
  ACK_LATE_MS,
  ACK_TIMEOUT_MS,
  broadcastTransport,
  mintTransfer,
  ROSTER_COLLECT_MS,
  TransferLedger,
  type XwinTransport,
} from "./crossWindow";

// The cross-window move protocol: the sender-side ledger's remove-only-on-ok
// policy (the invariant that a surface is never LOST, only at worst briefly
// duplicated), and the browser transport's BroadcastChannel framing.

describe("TransferLedger (remove-only-on-ack policy)", () => {
  it("an ok ack inside the window applies the move exactly once", () => {
    const l = new TransferLedger<string>();
    l.add(1, "tab", 1000);
    expect(l.ack(1, true, 1500)).toBe("tab");
    expect(l.ack(1, true, 1600)).toBeNull(); // consumed
  });

  it("a declined ack never applies, and consumes the entry", () => {
    const l = new TransferLedger<string>();
    l.add(1, "tab", 1000);
    expect(l.has(1)).toBe(true);
    expect(l.ack(1, false, 1500)).toBeNull();
    expect(l.has(1)).toBe(false);
  });

  it("the soft timeout reports each entry once and keeps waiting", () => {
    const l = new TransferLedger<string>();
    l.add(1, "a", 1000);
    l.add(2, "b", 1000);
    expect(l.timeouts(1000 + ACK_TIMEOUT_MS - 1)).toEqual([]);
    expect(l.timeouts(1000 + ACK_TIMEOUT_MS + 1).sort()).toEqual(["a", "b"]);
    expect(l.timeouts(1000 + ACK_TIMEOUT_MS + 500)).toEqual([]); // reported once
    // A LATE ok still applies — the receiver did adopt; leaving both copies
    // standing forever would be the duplication the ack exists to prevent.
    expect(l.ack(1, true, 1000 + ACK_LATE_MS - 1)).toBe("a");
  });

  it("past the hard deadline an ack is dead and expire() forgets", () => {
    const l = new TransferLedger<string>();
    l.add(1, "a", 1000);
    expect(l.ack(1, true, 1000 + ACK_LATE_MS + 1)).toBeNull();
    l.add(2, "b", 1000);
    l.expire(1000 + ACK_LATE_MS + 1);
    expect(l.size).toBe(0);
  });

  it("acks for unknown transfers are inert", () => {
    const l = new TransferLedger<string>();
    expect(l.ack(99, true, 0)).toBeNull();
    expect(l.has(99)).toBe(false);
  });
});

describe("broadcastTransport (browser windows on one workspace)", () => {
  const open: XwinTransport[] = [];
  afterEach(() => {
    for (const t of open.splice(0)) t.close();
  });

  function win(wsId: string, id: string, label: string, detached = false): XwinTransport {
    const t = broadcastTransport(wsId, {
      winId: () => id,
      label: () => label,
      detached: () => detached,
    });
    open.push(t);
    return t;
  }

  const tick = (ms: number) => new Promise((r) => setTimeout(r, ms));

  it("adopt reaches only the addressed window; the ack comes back", async () => {
    const a = win("ws1", "win-a", "main");
    const b = win("ws1", "win-b", "solo", true);
    const c = win("ws1", "win-c", "other");
    const dropsB: unknown[] = [];
    const dropsC: unknown[] = [];
    b.incoming({ onOver: () => {}, onLeave: () => {}, onDrop: (d) => dropsB.push(d) });
    c.incoming({ onOver: () => {}, onLeave: () => {}, onDrop: (d) => dropsC.push(d) });
    const acks: [number, boolean][] = [];
    a.acks((transfer, ok) => acks.push([transfer, ok]));

    const transfer = mintTransfer();
    await a.adoptInto("win-b", { v: 1 }, transfer);
    await tick(50);
    expect(dropsC).toEqual([]);
    expect(dropsB).toHaveLength(1);
    const drop = dropsB[0] as { transfer: number; payload: unknown };
    expect(drop.transfer).toBe(transfer);
    expect(drop.payload).toEqual({ v: 1 });

    b.ack(drop.transfer, true);
    await tick(50);
    expect(acks).toEqual([[transfer, true]]);
  });

  it("roster collects sibling pongs, not itself, within the window", async () => {
    const a = win("ws2", "win-a", "main");
    win("ws2", "win-b", "zsh — proj", true);
    const roster = await a.roster();
    expect(roster).toEqual([{ winId: "win-b", label: "zsh — proj", detached: true }]);
  }, ROSTER_COLLECT_MS + 4000);

  it("windows on another workspace's channel hear nothing", async () => {
    const a = win("ws3", "win-a", "main");
    const other = win("ws4", "win-x", "elsewhere");
    const drops: unknown[] = [];
    other.incoming({ onOver: () => {}, onLeave: () => {}, onDrop: (d) => drops.push(d) });
    await a.adoptInto("win-x", {}, mintTransfer());
    await tick(50);
    expect(drops).toEqual([]);
  });

  it("drag routing is inert in the browser (no sibling geometry exists)", async () => {
    const a = win("ws5", "win-a", "main");
    expect(await a.track(10, 10, 1)).toBe(false);
    expect(await a.drop(10, 10, 1, mintTransfer(), {})).toEqual({ routed: false });
  });

  it("mintTransfer stays inside the u64-safe JS integer range", () => {
    for (let i = 0; i < 64; i++) {
      const id = mintTransfer();
      expect(Number.isSafeInteger(id)).toBe(true);
      expect(id).toBeGreaterThanOrEqual(0);
    }
  });
});
