import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { dismiss, type DismissParams, type DismissReason } from "./dismiss";

// A minimal DOM: the action only needs contains/closest and window listeners.
class FakeElement {
  parent: FakeElement | null = null;
  inert = false;
  constructor(parent?: FakeElement) { this.parent = parent ?? null; }
  contains(node: unknown): boolean {
    for (let n = node instanceof FakeElement ? node : null; n; n = n.parent) if (n === this) return true;
    return false;
  }
  closest(selector: string): FakeElement | null {
    for (let n: FakeElement | null = this; n; n = n.parent) if (selector === "[inert]" && n.inert) return n;
    return null;
  }
}
const listeners = new Map<string, Set<(event: unknown) => void>>();
const win = {
  addEventListener(type: string, callback: (event: unknown) => void) {
    const set = listeners.get(type) ?? new Set(); set.add(callback); listeners.set(type, set);
  },
  removeEventListener(type: string, callback: (event: unknown) => void) { listeners.get(type)?.delete(callback); },
};
function emit(type: string, target: FakeElement, key?: string) {
  const event = { target, key, stopPropagation: vi.fn() };
  for (const callback of listeners.get(type) ?? []) callback(event);
  return event;
}
function mount(node: FakeElement, params: DismissParams) {
  const handle = dismiss(node as unknown as HTMLElement, params);
  if (!handle?.destroy) throw new Error("dismiss action missing cleanup");
  return handle;
}

beforeEach(() => {
  listeners.clear();
  vi.stubGlobal("window", win);
  vi.stubGlobal("Element", FakeElement);
  vi.stubGlobal("Node", FakeElement);
});
afterEach(() => { vi.unstubAllGlobals(); });

describe("dismiss", () => {
  it("names the reason and keeps presses inside the host open", () => {
    const outside = new FakeElement(), host = new FakeElement(), child = new FakeElement(host);
    const reasons: DismissReason[] = [];
    const handle = mount(host, { enabled: true, onDismiss: (r) => reasons.push(r) });
    emit("pointerdown", child);
    emit("pointerdown", outside);
    const esc = emit("keydown", child, "Escape");
    emit("keydown", child, "Enter");
    expect(reasons).toEqual(["outside", "escape"]);
    expect(esc.stopPropagation).toHaveBeenCalledOnce();
    handle.destroy?.();
    expect([...listeners.values()].every((set) => set.size === 0)).toBe(true);
  });

  it("a pane-local host answers only its region and leaves other panes' Escape alone", () => {
    const root = new FakeElement();
    const pane = new FakeElement(root), veil = new FakeElement(pane), dialog = new FakeElement(veil);
    const composer = new FakeElement(pane), otherPane = new FakeElement(root), terminal = new FakeElement(otherPane);
    const onDismiss = vi.fn();
    mount(dialog, { enabled: true, onDismiss, region: () => pane as unknown as Element });

    const elsewhere = emit("keydown", terminal, "Escape");
    emit("pointerdown", terminal);
    expect(onDismiss).not.toHaveBeenCalled();
    expect(elsewhere.stopPropagation).not.toHaveBeenCalled();

    // Focus Tabbed out to the composer behind the veil: Escape still closes,
    // and is stopped before the composer's interrupt sees it.
    const behind = emit("keydown", composer, "Escape");
    expect(onDismiss).toHaveBeenLastCalledWith("escape");
    expect(behind.stopPropagation).toHaveBeenCalledOnce();

    emit("pointerdown", veil);
    expect(onDismiss).toHaveBeenLastCalledWith("outside");
    emit("pointerdown", dialog);
    expect(onDismiss).toHaveBeenCalledTimes(2);
  });

  it("a pane-local host parked in an inert keep-alive layer ignores everything", () => {
    const layer = new FakeElement(), dialog = new FakeElement(layer), inPane = new FakeElement(layer);
    const onDismiss = vi.fn();
    mount(dialog, { enabled: true, onDismiss, region: () => layer as unknown as Element });
    layer.inert = true;
    const esc = emit("keydown", inPane, "Escape");
    emit("pointerdown", inPane);
    expect(onDismiss).not.toHaveBeenCalled();
    expect(esc.stopPropagation).not.toHaveBeenCalled();
    layer.inert = false;
    emit("keydown", inPane, "Escape");
    expect(onDismiss).toHaveBeenCalledOnce();
  });

  it("does nothing while disabled, and update re-enables it", () => {
    const host = new FakeElement(), outside = new FakeElement();
    const onDismiss = vi.fn();
    const handle = mount(host, { enabled: false, onDismiss });
    const esc = emit("keydown", outside, "Escape");
    emit("pointerdown", outside);
    expect(onDismiss).not.toHaveBeenCalled();
    expect(esc.stopPropagation).not.toHaveBeenCalled();
    handle.update?.({ enabled: true, onDismiss });
    emit("pointerdown", outside);
    expect(onDismiss).toHaveBeenCalledWith("outside");
    handle.destroy?.();
  });
});
