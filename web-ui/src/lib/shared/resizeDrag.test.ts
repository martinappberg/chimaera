import { describe, expect, it, vi } from "vitest";
import { resizeKeydown, startResizeDrag, type ResizeDragOptions } from "./resizeDrag";

// Fake window + handle + frame queue drive the real helper headless.
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
  count(): number {
    return [...this.listeners.values()].reduce((n, s) => n + s.size, 0);
  }
}

class Handle extends Target {
  captured: number | null = null;
  setPointerCapture(id: number) {
    this.captured = id;
  }
}

function setup(over: Partial<ResizeDragOptions> = {}) {
  const win = new Target();
  const handle = new Handle();
  const frames: (() => void)[] = [];
  const applied: number[] = [];
  const onEnd = vi.fn();
  const down = { pointerId: 7, clientX: 100, clientY: 50, currentTarget: handle, preventDefault: vi.fn() };
  const drag = startResizeDrag(down as unknown as PointerEvent, {
    axis: "x",
    initial: 0.5,
    compute: (pos, start) => 0.5 + (pos - start) / 100,
    apply: (v) => applied.push(v),
    onEnd,
    env: {
      target: win,
      requestFrame: (cb) => frames.push(cb),
      cancelFrame: (id) => {
        frames[id - 1] = () => {};
      },
    },
    ...over,
  });
  const flush = () => frames.splice(0).forEach((f) => f());
  const move = (x: number, id = 7) => win.emit("pointermove", { pointerId: id, clientX: x, clientY: 0 });
  const key = (k: string) => {
    const e = { key: k, preventDefault: vi.fn(), stopPropagation: vi.fn() };
    win.emit("keydown", e);
    return e;
  };
  return { win, handle, frames, applied, onEnd, down, drag, flush, move, key };
}

describe("startResizeDrag", () => {
  it("captures the pointer and prevents the default", () => {
    const t = setup();
    expect(t.handle.captured).toBe(7);
    expect(t.down.preventDefault).toHaveBeenCalled();
  });

  it("throttles to one apply per frame with the latest position", () => {
    const t = setup();
    t.move(110);
    t.move(120);
    t.move(130);
    expect(t.applied).toEqual([]);
    expect(t.frames.length).toBe(1);
    t.flush();
    expect(t.applied).toEqual([0.8]);
  });

  it("applies synchronously when frame throttling is off", () => {
    const t = setup({ frame: false });
    t.move(110);
    t.move(120);
    expect(t.applied).toEqual([0.6, 0.7]);
  });

  it("ignores other pointers and a null compute", () => {
    const t = setup({ frame: false, compute: (pos) => (pos > 150 ? null : pos) });
    t.move(120, 9);
    t.move(160);
    expect(t.applied).toEqual([]);
    t.move(140);
    expect(t.applied).toEqual([140]);
  });

  it("holds moves under the threshold", () => {
    const t = setup({ frame: false, threshold: 5 });
    t.move(103);
    expect(t.applied).toEqual([]);
    t.move(106);
    t.move(101); // past the threshold once, every move applies
    expect(t.applied).toEqual([0.56, 0.51]);
  });

  it("pointerup commits and tears every listener down", () => {
    const t = setup();
    t.move(120);
    t.win.emit("pointerup", { pointerId: 7 });
    t.flush(); // the dropped pending frame never applies
    expect(t.applied).toEqual([]);
    expect(t.onEnd).toHaveBeenCalledWith(false);
    expect(t.win.count()).toBe(0);
  });

  it("Escape restores the initial value and stops the event", () => {
    const t = setup({ frame: false });
    t.move(140);
    const e = t.key("Escape");
    expect(e.stopPropagation).toHaveBeenCalled();
    expect(e.preventDefault).toHaveBeenCalled();
    expect(t.applied).toEqual([0.9, 0.5]);
    expect(t.onEnd).toHaveBeenCalledWith(true);
    t.win.emit("pointerup", { pointerId: 7 });
    expect(t.onEnd).toHaveBeenCalledTimes(1);
  });

  it("escape: false leaves Escape alone", () => {
    const t = setup({ escape: false });
    const e = t.key("Escape");
    expect(e.stopPropagation).not.toHaveBeenCalled();
    expect(t.onEnd).not.toHaveBeenCalled();
  });

  it("pointercancel restores by default, commits with cancelRestores: false", () => {
    const a = setup({ frame: false });
    a.move(120);
    a.win.emit("pointercancel", { pointerId: 7 });
    expect(a.applied).toEqual([0.7, 0.5]);
    expect(a.onEnd).toHaveBeenCalledWith(true);

    const b = setup({ frame: false, cancelRestores: false });
    b.move(120);
    b.win.emit("pointercancel", { pointerId: 7 });
    expect(b.applied).toEqual([0.7]);
    expect(b.onEnd).toHaveBeenCalledWith(false);
  });

  it("endOnLostCapture ends the drag from the handle", () => {
    const t = setup({ endOnLostCapture: true });
    t.handle.emit("lostpointercapture", { pointerId: 7 });
    expect(t.onEnd).toHaveBeenCalledWith(false);
    expect(t.handle.count()).toBe(0);
  });

  it("reads clientY on the y axis", () => {
    const win = new Target();
    const applied: number[] = [];
    startResizeDrag({ pointerId: 1, clientX: 0, clientY: 10, currentTarget: null, preventDefault() {} } as unknown as PointerEvent, {
      axis: "y",
      initial: 0,
      frame: false,
      compute: (pos, start) => pos - start,
      apply: (v) => applied.push(v),
      env: { target: win, requestFrame: () => 0, cancelFrame: () => {} },
    });
    win.emit("pointermove", { pointerId: 1, clientX: 99, clientY: 25 });
    expect(applied).toEqual([15]);
  });
});

describe("resizeKeydown", () => {
  const key = (k: string) => ({ key: k, preventDefault: vi.fn(), stopPropagation: vi.fn() }) as unknown as KeyboardEvent;

  it("steps with arrows, inverted for a pane after the handle, and clamps", () => {
    const out: number[] = [];
    const o = { axis: "x" as const, value: 300, min: 240, max: 320, step: 20, invert: true, apply: (v: number) => out.push(v) };
    expect(resizeKeydown(key("ArrowLeft"), o)).toBe(true);
    expect(resizeKeydown(key("ArrowRight"), o)).toBe(true);
    expect(resizeKeydown(key("Home"), o)).toBe(true);
    expect(resizeKeydown(key("End"), o)).toBe(true);
    expect(out).toEqual([320, 280, 240, 320]);
  });

  it("leaves other keys (including Escape) unhandled", () => {
    const e = key("Escape");
    expect(resizeKeydown(e, { axis: "x", value: 1, min: 0, max: 2, step: 1, apply: () => {} })).toBe(false);
    expect(e.preventDefault).not.toHaveBeenCalled();
    expect(resizeKeydown(key("ArrowLeft"), { axis: "y", value: 1, min: 0, max: 2, step: 1, apply: () => {} })).toBe(false);
  });
});
