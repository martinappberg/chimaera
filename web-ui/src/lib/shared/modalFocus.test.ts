import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { modalFocus, modalOpen } from "./modalFocus";

// Controlled DOM events drive the actual shared action, not a copied focus
// algorithm. Browser layout/AX behavior remains a separate live acceptance gate.
class Element {
  parent: Element | null = null;
  children: Element[] = [];
  inert = false;
  disabled = false;
  isConnected = true;
  focusable = true;
  constructor(parent?: Element) { if (parent) { this.parent = parent; parent.children.push(this); } }
  contains(node: unknown): boolean { return node === this || this.children.some((child) => child.contains(node)); }
  closest(): Element | null { for (let node: Element | null = this; node; node = node.parent) if (node.inert) return node; return null; }
  querySelectorAll(): Element[] { return this.children.flatMap((child) => [...(child.focusable && !child.disabled ? [child] : []), ...child.querySelectorAll()]); }
  getClientRects(): object[] { return this.isConnected && this.closest() === null ? [{}] : []; }
  getAttribute(): null { return null; }
  matches(): boolean { return this.focusable && !this.disabled; }
  focus() { if (!this.isConnected || this.disabled || this.closest() !== null) return; doc.activeElement = this; emit("focusin", { target: this }); }
}
const listeners = new Map<string, Set<(event: unknown) => void>>();
const doc = { activeElement: null as Element | null, documentElement: new Element(),
  addEventListener(type: string, callback: (event: unknown) => void) {
    const entries = listeners.get(type) ?? new Set(); entries.add(callback); listeners.set(type, entries);
  },
  removeEventListener(type: string, callback: (event: unknown) => void) { listeners.get(type)?.delete(callback); },
};
function emit(type: string, event: unknown) { for (const callback of listeners.get(type) ?? []) callback(event); }
class Observer {
  static all: Observer[] = [];
  connected = false;
  options: MutationObserverInit | null = null;
  constructor(private callback: MutationCallback) { Observer.all.push(this); }
  observe(_: unknown, options: MutationObserverInit) { this.connected = true; this.options = options; }
  disconnect() { this.connected = false; }
  deliver(target: Element) {
    if (this.connected) this.callback([{ target } as unknown as MutationRecord], this as unknown as MutationObserver);
  }
}
const handles: { destroy(): void }[] = [];
function own(node: Element, priority = 0) {
  const action = modalFocus(node as unknown as HTMLElement, { priority });
  if (!action?.destroy) throw new Error("Modal action missing cleanup");
  const handle = { destroy: action.destroy }; handles.push(handle); return handle;
}
const flush = async () => { await Promise.resolve(); };
beforeEach(() => {
  doc.activeElement = null; doc.documentElement = new Element(); listeners.clear(); Observer.all = [];
  vi.stubGlobal("document", doc); vi.stubGlobal("HTMLElement", Element); vi.stubGlobal("Node", Element); vi.stubGlobal("MutationObserver", Observer);
});
afterEach(async () => { for (const handle of handles.splice(0)) handle.destroy(); await flush(); vi.unstubAllGlobals(); });

describe("shared modal keep-alive restoration", () => {
  it("restores the same safe control after hide/show and disconnects on final disposal", async () => {
    const trigger = new Element(); trigger.focus();
    const layer = new Element(), dialog = new Element(layer), destructive = new Element(dialog), cancel = new Element(dialog);
    const handle = own(dialog);
    // Registration preserves the component's deferred initial-focus choice.
    expect(doc.activeElement).toBe(trigger); cancel.focus();
    const observer = Observer.all[0];
    expect(observer.options).toEqual({ subtree: true, attributes: true, attributeFilter: ["inert"] });
    layer.inert = true; const show = new Element(); show.focus(); observer.deliver(layer);
    expect(modalOpen()).toBe(false); expect(doc.activeElement).toBe(show);
    layer.inert = false; observer.deliver(layer);
    expect(modalOpen()).toBe(true); expect(doc.activeElement).toBe(cancel); expect(doc.activeElement).not.toBe(destructive);
    const preventDefault = vi.fn();
    emit("keydown", { key: "Tab", shiftKey: false, preventDefault });
    expect(preventDefault).toHaveBeenCalledOnce(); expect(doc.activeElement).toBe(destructive);
    emit("keydown", { key: "Tab", shiftKey: true, preventDefault }); expect(doc.activeElement).toBe(cancel);
    handle.destroy(); handles.splice(handles.indexOf(handle), 1); await flush();
    expect(doc.activeElement).toBe(trigger); expect(modalOpen()).toBe(false); expect(observer.connected).toBe(false);
    expect([...listeners.values()].every((entries) => entries.size === 0)).toBe(true);
    observer.deliver(layer); expect(doc.activeElement).toBe(trigger);
  });
  it("does not steal focus from a higher-priority owner while an underlying layer shows", async () => {
    const layer = new Element(), dialog = new Element(layer), cancel = new Element(dialog);
    own(dialog); cancel.focus();
    const higher = new Element(), highCancel = new Element(higher); const top = own(higher, 10); highCancel.focus();
    expect(Observer.all).toHaveLength(1);
    layer.inert = true; Observer.all[0].deliver(layer);
    layer.inert = false; Observer.all[0].deliver(layer);
    expect(doc.activeElement).toBe(highCancel);
    top.destroy(); handles.splice(handles.indexOf(top), 1); await flush(); expect(doc.activeElement).toBe(cancel);
  });
  it("ignores unrelated inert changes and preserves a different focused control inside the modal", () => {
    const layer = new Element(), dialog = new Element(layer), first = new Element(dialog), cancel = new Element(dialog);
    own(dialog); cancel.focus();
    const unrelated = new Element(), outside = new Element(); doc.activeElement = outside;
    unrelated.inert = true; Observer.all[0].deliver(unrelated); expect(doc.activeElement).toBe(outside);
    first.focus(); Observer.all[0].deliver(layer); expect(doc.activeElement).toBe(first);
  });
  it("uses a live focusable fallback when its former safe control is disabled", () => {
    const layer = new Element(), dialog = new Element(layer), fallback = new Element(dialog), cancel = new Element(dialog);
    own(dialog); cancel.focus(); layer.inert = true; new Element().focus();
    cancel.disabled = true; layer.inert = false; Observer.all[0].deliver(layer); expect(doc.activeElement).toBe(fallback);
  });
  it("retirement while hidden cannot restore a detached control or a disposed observer", async () => {
    const layer = new Element(), dialog = new Element(layer), cancel = new Element(dialog);
    const handle = own(dialog); cancel.focus(); layer.inert = true; const successor = new Element(); successor.focus();
    const observer = Observer.all[0]; handle.destroy(); handles.splice(handles.indexOf(handle), 1); await flush();
    layer.inert = false; observer.deliver(layer); expect(doc.activeElement).toBe(successor); expect(modalOpen()).toBe(false);
  });
});
