import { describe, expect, it, vi } from "vitest";
import { createRailResize, type RailResizeHost } from "./railResize";
import { FILES_FRAC_MAX, FILES_FRAC_MIN, RAIL_DEFAULT, RAIL_MAX, RAIL_MIN } from "./railState";

// The two rail drags over a fake App host and a fake drag environment: what
// a pointer position becomes (the clamps), Escape restoring the start value,
// and the active/dragging flags around each drag.

class Target {
  listeners = new Map<string, Set<(e: unknown) => void>>();
  addEventListener(type: string, cb: (e: unknown) => void) {
    const set = this.listeners.get(type) ?? new Set();
    set.add(cb);
    this.listeners.set(type, set);
  }
  removeEventListener(type: string, cb: (e: unknown) => void) {
    this.listeners.get(type)?.delete(cb);
  }
  emit(type: string, e: unknown) {
    for (const cb of [...(this.listeners.get(type) ?? [])]) cb(e);
  }
}

function fakeEl(rect: Partial<DOMRect>): HTMLElement {
  return {
    getBoundingClientRect: () => ({ top: 0, left: 0, width: 0, height: 0, ...rect }),
    setPointerCapture: () => {},
  } as unknown as HTMLElement;
}

function setup() {
  const win = new Target();
  const frames: (() => void)[] = [];
  const state = {
    filesFrac: 0.4,
    filesDividerActive: false,
    railWidth: 230,
    railDividerActive: false,
    dragging: false,
  };
  const log: string[] = [];
  const host: RailResizeHost = {
    // The rail is 600px tall; the daemon strip starts at y=500.
    getRailEl: () => fakeEl({ height: 600, width: 230 }),
    getDaemonEl: () => fakeEl({ top: 500 }),
    getFilesFrac: () => state.filesFrac,
    setFilesFrac: (f) => {
      state.filesFrac = f;
    },
    setFilesDividerActive: (a) => {
      state.filesDividerActive = a;
      log.push(`files:${a}`);
    },
    getRailWidth: () => state.railWidth,
    setRailWidth: (w) => {
      state.railWidth = w;
    },
    setRailDividerActive: (a) => {
      state.railDividerActive = a;
      log.push(`rail:${a}`);
    },
    setDragging: (d) => {
      state.dragging = d;
      log.push(`drag:${d}`);
    },
    env: {
      target: win,
      requestFrame: (cb) => frames.push(cb),
      cancelFrame: (id) => {
        frames[id - 1] = () => {};
      },
    },
  };
  const rail = createRailResize(host);
  const down = (x: number, y: number) =>
    ({ button: 0, pointerId: 1, clientX: x, clientY: y, currentTarget: fakeEl({}), preventDefault: vi.fn() }) as unknown as PointerEvent;
  const move = (x: number, y: number) => win.emit("pointermove", { pointerId: 1, clientX: x, clientY: y });
  const up = () => win.emit("pointerup", { pointerId: 1 });
  const escape = () => win.emit("keydown", { key: "Escape", preventDefault: vi.fn(), stopPropagation: vi.fn() });
  const flush = () => frames.splice(0).forEach((f) => f());
  return { rail, state, log, down, move, up, escape, flush };
}

describe("the FILES divider drag", () => {
  it("turns the pointer's distance above the daemon strip into a rail fraction, one write per frame", () => {
    const t = setup();
    t.rail.onFilesDividerDown(t.down(100, 300));
    expect(t.state.filesDividerActive).toBe(true);
    t.move(100, 200); // the FILES section now reaches 300px above the strip
    expect(t.state.filesFrac).toBe(0.4); // not before the frame
    t.flush();
    expect(t.state.filesFrac).toBe(0.5);
    t.up();
    expect(t.state.filesDividerActive).toBe(false);
    expect(t.state.filesFrac).toBe(0.5);
  });

  it("clamps to the railState bounds", () => {
    const t = setup();
    t.rail.onFilesDividerDown(t.down(100, 300));
    t.move(100, -1000);
    t.flush();
    expect(t.state.filesFrac).toBe(FILES_FRAC_MAX);
    t.move(100, 1000);
    t.flush();
    expect(t.state.filesFrac).toBe(FILES_FRAC_MIN);
    t.up();
  });

  it("restores the starting fraction on Escape", () => {
    const t = setup();
    t.rail.onFilesDividerDown(t.down(100, 300));
    t.move(100, 200);
    t.flush();
    expect(t.state.filesFrac).toBe(0.5);
    t.escape();
    expect(t.state.filesFrac).toBe(0.4);
    expect(t.state.filesDividerActive).toBe(false);
  });

  it("ignores a secondary button and a missing rail", () => {
    const t = setup();
    t.rail.onFilesDividerDown({ ...t.down(0, 0), button: 2 } as PointerEvent);
    expect(t.log).toEqual([]);
  });
});

describe("the rail width drag", () => {
  it("adds the pointer's travel to the rail's measured width, clamped, and holds terminal fits for the drag", () => {
    const t = setup();
    t.rail.onRailResizeDown(t.down(230, 50));
    expect(t.log).toEqual(["rail:true", "drag:true"]);
    t.move(280, 50);
    t.flush();
    expect(t.state.railWidth).toBe(280);
    t.move(2000, 50);
    t.flush();
    expect(t.state.railWidth).toBe(RAIL_MAX);
    t.move(-2000, 50);
    t.flush();
    expect(t.state.railWidth).toBe(RAIL_MIN);
    t.up();
    // Active off, then the deferred fits flush.
    expect(t.log.slice(2)).toEqual(["rail:false", "drag:false"]);
  });

  it("restores the starting width on Escape and still releases the fits", () => {
    const t = setup();
    t.rail.onRailResizeDown(t.down(230, 50));
    t.move(300, 50);
    t.flush();
    expect(t.state.railWidth).toBe(300);
    t.escape();
    expect(t.state.railWidth).toBe(230);
    expect(t.state.dragging).toBe(false);
    expect(t.state.railDividerActive).toBe(false);
  });

  it("snaps back to the default width on reset", () => {
    const t = setup();
    t.state.railWidth = 400;
    t.rail.resetRailWidth();
    expect(t.state.railWidth).toBe(RAIL_DEFAULT);
  });
});
