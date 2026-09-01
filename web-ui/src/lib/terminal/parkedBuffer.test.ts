import { describe, expect, it } from "vitest";
import { ParkedBuffer } from "./parkedBuffer";

const MAX = 64;

function bytes(n: number, fill = 0): Uint8Array {
  return new Uint8Array(n).fill(fill);
}

describe("ParkedBuffer", () => {
  it("passes writes through while visible", () => {
    const buf = new ParkedBuffer(MAX);
    expect(buf.binary(bytes(8))).toBe("write");
    // Adopt on a never-parked entry is a no-op.
    expect(buf.adopt()).toEqual({ flush: [], resync: false });
  });

  it("buffers while parked and replays in order on adopt", () => {
    const buf = new ParkedBuffer(MAX);
    buf.park();
    const a = bytes(4, 1);
    const b = bytes(4, 2);
    const c = bytes(4, 3);
    expect(buf.binary(a)).toBe("buffered");
    expect(buf.binary(b)).toBe("buffered");
    expect(buf.binary(c)).toBe("buffered");
    expect(buf.adopt()).toEqual({ flush: [a, b, c], resync: false });
    // Unparked again: live writes go straight through, nothing lingers.
    expect(buf.binary(bytes(4))).toBe("write");
    expect(buf.adopt().flush).toEqual([]);
  });

  it("reports the parked lifecycle for the socket's parked auth", () => {
    const buf = new ParkedBuffer(MAX);
    expect(buf.isParked()).toBe(false);
    buf.park();
    expect(buf.isParked()).toBe(true);
    buf.adopt();
    expect(buf.isParked()).toBe(false);
  });

  it("desync while parked discards and latches; adopt resyncs", () => {
    const buf = new ParkedBuffer(MAX);
    buf.park();
    expect(buf.binary(bytes(8))).toBe("buffered");
    buf.desync();
    expect(buf.binary(bytes(8))).toBe("dropped");
    expect(buf.adopt()).toEqual({ flush: [], resync: true });
  });

  it("desync while visible is a no-op (a live grid needs no resync)", () => {
    const buf = new ParkedBuffer(MAX);
    buf.desync();
    expect(buf.binary(bytes(8))).toBe("write");
  });

  it("desync after adopt is a no-op (the parked-ready race)", () => {
    // adopt() ran while the parked connection's ready frame was in flight:
    // the late onParkedReady must not desync the now-visible entry.
    const buf = new ParkedBuffer(MAX);
    buf.park();
    buf.adopt();
    buf.desync();
    expect(buf.binary(bytes(8))).toBe("write");
  });

  it("latches desynced on overflow and drops everything after", () => {
    const buf = new ParkedBuffer(MAX);
    buf.park();
    expect(buf.binary(bytes(MAX))).toBe("buffered");
    expect(buf.binary(bytes(1))).toBe("dropped"); // crosses the cap
    expect(buf.binary(bytes(1))).toBe("dropped"); // stays latched
    const adopt = buf.adopt();
    expect(adopt.resync).toBe(true);
    expect(adopt.flush).toEqual([]);
  });

  it("keeps the desynced latch until a resync is actually issued", () => {
    const buf = new ParkedBuffer(MAX);
    buf.park();
    buf.binary(bytes(MAX + 1));
    expect(buf.adopt().resync).toBe(true);
    // The resync was refused (fatal socket): the latch must survive a
    // re-park so a later adopt tries again instead of faking freshness.
    buf.park();
    expect(buf.binary(bytes(1))).toBe("dropped");
    expect(buf.adopt().resync).toBe(true);
    buf.resyncIssued();
    buf.park();
    expect(buf.binary(bytes(1))).toBe("buffered");
  });

  it("reset supersedes the buffer and writes the snapshot through", () => {
    const buf = new ParkedBuffer(MAX);
    buf.park();
    expect(buf.binary(bytes(8, 9))).toBe("buffered");
    buf.reset();
    // The next binary frame IS the snapshot (wire adjacency): parse it now.
    expect(buf.binary(bytes(MAX * 2))).toBe("write");
    // Live bytes after the snapshot buffer again.
    const live = bytes(4, 5);
    expect(buf.binary(live)).toBe("buffered");
    expect(buf.adopt()).toEqual({ flush: [live], resync: false });
  });

  it("reset recovers a desynced entry without a reconnect", () => {
    const buf = new ParkedBuffer(MAX);
    buf.park();
    buf.binary(bytes(MAX + 1)); // overflow -> desynced
    buf.reset();
    expect(buf.binary(bytes(8))).toBe("write"); // snapshot write-through
    expect(buf.binary(bytes(8))).toBe("buffered");
    expect(buf.adopt().resync).toBe(false);
  });

  it("a foreign resize discards old-width bytes and forces resync", () => {
    const buf = new ParkedBuffer(MAX);
    buf.park();
    buf.binary(bytes(8));
    buf.resized();
    expect(buf.binary(bytes(8))).toBe("dropped");
    expect(buf.adopt().resync).toBe(true);
  });

  it("a resize while visible changes nothing (xterm reflows itself)", () => {
    const buf = new ParkedBuffer(MAX);
    buf.resized();
    expect(buf.binary(bytes(8))).toBe("write");
  });

  it("exited flushes the buffered tail once, in order", () => {
    const buf = new ParkedBuffer(MAX);
    buf.park();
    const a = bytes(4, 1);
    const b = bytes(4, 2);
    buf.binary(a);
    buf.binary(b);
    expect(buf.exited()).toEqual([a, b]);
    // A parked exit cannot trust the buffer to be the complete tail (a
    // park-aware server withheld output): adopt resyncs into the server's
    // last-words replay for the authoritative final screen.
    expect(buf.adopt()).toEqual({ flush: [], resync: true });
  });

  it("a visible exit stays in sync (no resync on adopt)", () => {
    const buf = new ParkedBuffer(MAX);
    expect(buf.exited()).toEqual([]);
    expect(buf.adopt()).toEqual({ flush: [], resync: false });
  });

  it("exited after overflow keeps the resync latch for last-words replay", () => {
    const buf = new ParkedBuffer(MAX);
    buf.park();
    buf.binary(bytes(MAX + 1));
    expect(buf.exited()).toEqual([]);
    expect(buf.adopt().resync).toBe(true);
  });

  it("exited while awaiting a snapshot degrades to resync, never a stale grid", () => {
    const buf = new ParkedBuffer(MAX);
    buf.park();
    buf.reset();
    expect(buf.exited()).toEqual([]);
    expect(buf.adopt().resync).toBe(true);
  });

  it("adopt clears awaiting-snapshot so a re-park cannot misroute live bytes", () => {
    const buf = new ParkedBuffer(MAX);
    buf.park();
    buf.reset();
    // Adopted before the snapshot landed: it will arrive as a direct write.
    expect(buf.adopt()).toEqual({ flush: [], resync: false });
    buf.park();
    // A chunk arriving parked now must buffer, not write through.
    expect(buf.binary(bytes(8))).toBe("buffered");
  });
});
