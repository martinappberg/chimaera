import { afterEach, describe, expect, it, vi } from "vitest";
import { NativeUiTransport, isNativeUiAction, parseUiTree, safeUiHref, uiStyle, type UiRecord } from "./nativeUi";
import { prepareClientBundle, reserveClientWorker, clientKeyEvent, clientPointerEvent, clientViewport, CLIENT_CLOCK, CLIENT_WORKER } from "./modClient";
import { decorationRuns } from "./nativeComposer";

afterEach(() => vi.useRealTimers());

describe("ephemeral native UI requests", () => {
  it("separates explicit controls from automatic RPCs without accepting an intent flag", () => {
    for (const subtype of ["ui_press", "ui_input", "ui_select", "ui_client_press"]) expect(isNativeUiAction({ subtype })).toBe(true);
    for (const subtype of ["ui_attach", "ui_detach", "ui_render", "ui_panes", "ui_pane_show", "ui_pane_focus", "ui_close", "ui_scroll", "ui_focus", "ui_client_module", "ui_message", "ui_prompt_edit", "ui_host_response", "unknown"]) {
      expect(isNativeUiAction({ subtype, interaction: true })).toBe(false);
    }
    for (const request of [null, [], {}, { subtype: 1 }]) expect(isNativeUiAction(request)).toBe(false);
  });
  it("refuses disconnected sends and rejects outstanding actions without replaying them", async () => {
    const frames: UiRecord[] = [];
    const transport = new NativeUiTransport((frame) => { frames.push(frame); return true; });
    await expect(transport.request({ subtype: "ui_press" })).rejects.toThrow("not connected");
    transport.connected();
    const press = transport.request({ subtype: "ui_press", handle: 4 });
    transport.reset(true);
    await expect(press).rejects.toThrow("disconnected");
    transport.connected();
    expect(frames).toHaveLength(1);
    transport.receive({ kind: "response", request_id: frames[0].request_id, result: { handled: true } });
    const render = transport.request({ subtype: "ui_render" });
    transport.receive({ kind: "response", request_id: frames[1].request_id, result: { tree: { type: "engine", ref: 0 } } });
    await expect(render).resolves.toEqual({ tree: { type: "engine", ref: 0 } });
  });

  it("bounds concurrent actions and expires a missing native response", async () => {
    vi.useFakeTimers();
    const transport = new NativeUiTransport(() => true);
    transport.connected();
    const pending = Array.from({ length: 32 }, () => transport.request({ subtype: "ui_panes" }).catch((error: Error) => error.message));
    await expect(transport.request({ subtype: "ui_panes" })).rejects.toThrow("busy");
    vi.advanceTimersByTime(20_000);
    expect(await Promise.all(pending)).toEqual(Array(32).fill("Claude UI did not respond; try again"));
  });

  it("cleans up requests when the websocket closes between ready and send", async () => {
    vi.useFakeTimers();
    const transport = new NativeUiTransport(() => false);
    transport.connected();
    await expect(transport.request({ subtype: "ui_close" })).rejects.toThrow("disconnected");
    expect(vi.getTimerCount()).toBe(0);
  });
});

describe("Mod tree boundary", () => {
  it("rejects executable DOM, malformed trees, and excessive nesting", () => {
    expect(() => parseUiTree({ type: "script", children: ["alert(1)"] })).toThrow("unsupported");
    let tree: unknown = { type: "Text", children: ["hello"] };
    for (let i = 0; i < 45; i++) tree = { type: "Box", children: [tree] };
    expect(() => parseUiTree(tree)).toThrow("size limit");
    expect(() => parseUiTree({ type: "Box", children: [null] })).toThrow("unsupported");
  });
  it("preserves first-engine order and local closure identities independently", () => {
    const tree = parseUiTree({ type: "Box", children: [{ type: "engine", ref: 8 }, { type: "Button", props: { key: "add" }, held: 20 }, { type: "engine", ref: 0 }] })!;
    expect(tree.children[0]).toMatchObject({ engineOrdinal: 0, ref: 8 });
    expect(tree.children[1]).toMatchObject({ held: 20 });
    expect(tree.children[2]).toMatchObject({ engineOrdinal: 1, ref: 0 });
  });
  it("allows secure links and localhost while rejecting script and host-origin paths", () => {
    expect(safeUiHref("https://example.com/docs")).toBe("https://example.com/docs");
    expect(safeUiHref("http://127.0.0.1:3000/")).toBe("http://127.0.0.1:3000/");
    for (const href of ["javascript:alert(1)", "data:text/html,foo", "/api/v1/sessions", "http://example.com", "//example.com"]) expect(safeUiHref(href)).toBeNull();
    expect(uiStyle({ position: "fixed", color: "url(https://example.com)", padding: 1e9, width: "999%", bold: true }, "Text")).toBe("padding:8em;font-weight:600");
  });
});

describe("isolated Client module graph", () => {
  const bundle = (files: { key: string; source: string }[]) => ({ runtime: "runtime", modules: [{ module: "counter.js", entry: "entry", component: "default" }], files: [{ key: "runtime", source: "export const install = () => {};" }, ...files] });
  it("links native static and literal dynamic imports without admitting the network", async () => {
    const linked = await prepareClientBundle(bundle([{ key: "entry", source: 'import { x } from "util"; export default () => import("util");' }, { key: "util", source: "export const x = 1;" }]), "counter.js");
    expect(linked.files[1].imports.map((item) => [item.key, item.dynamic])).toEqual([["util", false], ["util", true]]);
    await expect(prepareClientBundle(bundle([{ key: "entry", source: 'import "https://example.com/payload.js";' }]), "counter.js")).rejects.toThrow("outside");
    await expect(prepareClientBundle(bundle([{ key: "entry", source: 'export default (path) => import(path);' }]), "counter.js")).rejects.toThrow("outside");
  });
  it("reports circular imports without recursing indefinitely", async () => {
    await expect(prepareClientBundle(bundle([{ key: "entry", source: 'import "util";' }, { key: "util", source: 'import "entry";' }]), "counter.js")).rejects.toThrow("circular");
  });
  it("bounds active workers and returns slots exactly once", () => {
    const releases = Array.from({ length: 16 }, reserveClientWorker);
    expect(releases.every(Boolean)).toBe(true);
    expect(reserveClientWorker()).toBeNull();
    releases[0]!(); releases[0]!();
    const one = reserveClientWorker(); expect(one).not.toBeNull(); expect(reserveClientWorker()).toBeNull();
    one!(); for (const release of releases) release!();
  });
});

describe("native composer decorations", () => {
  it("merges later styles while preserving Unicode graphemes and text", () => {
    const text = "A👩🏽‍💻BC";
    const runs = decorationRuns(text, [{ start: 2, end: 8, bold: true }, { start: 8, end: 999, italic: true }]);
    expect(runs.map((run) => run.text).join("")).toBe(text);
    expect(runs.find((run) => run.text.includes("👩"))?.props.bold).toBe(true);
    expect(runs.at(-1)?.props.italic).toBe(true);
    expect(decorationRuns("abc", [{ start: -5, end: 1, bold: true }, { start: 0, end: 1, bold: false }])[0]).toMatchObject({ text: "a", props: { bold: false } });
  });
});

describe("native Client input coordinates", () => {
  it("uses the containing pane's full height while bounding native grid dimensions", () => {
    expect(clientViewport(600, 900, 10, 20)).toEqual({ columns: 100, rows: 45 });
    expect(clientViewport(100_000, 100_000, 10, 20)).toEqual({ columns: 512, rows: 128 });
    expect(clientViewport(0, 0, NaN, NaN)).toEqual({ columns: 1, rows: 1 });
    expect(clientViewport(NaN, Infinity, 10, 20)).toEqual({ columns: 80, rows: 24 });
  });
  it("preserves native key names and only active modifiers", () => {
    expect(clientKeyEvent({ key: "ArrowUp", ctrlKey: false, shiftKey: true, metaKey: false, altKey: true })).toEqual({ key: "up", shift: true, meta: true });
    expect(clientKeyEvent({ key: "é", ctrlKey: false, shiftKey: false, metaKey: false, altKey: false })).toEqual({ key: "é" });
    expect(clientKeyEvent({ key: " ", ctrlKey: false, shiftKey: false, metaKey: false, altKey: false })).toEqual({ key: "space" });
  });
  it("keeps fractional and negative drag positions and remembers the last cell on leave", () => {
    const event = { clientX: 95, clientY: 135, button: -1, buttons: 1, shiftKey: false, altKey: false, ctrlKey: true };
    const geometry = { left: 100, top: 100, cellWidth: 10, cellHeight: 20 };
    expect(clientPointerEvent("move", event, geometry)).toEqual({ type: "move", x: -1, y: 1, fine: { x: -.5, y: 1.75 }, button: "left", ctrl: true });
    expect(clientPointerEvent("leave", event, geometry, { x: 4, y: 6 })).toEqual({ type: "leave", x: 4, y: 6, ctrl: true });
  });
});

describe("Client presentation suspension", () => {
  interface Clock {
    render(): void;
    schedule(): void;
    startTimer(ms: number): number;
    stopTimer(id: number): void;
    suspend(value: boolean): void;
  }
  const makeClock = new Function(`return (${CLIENT_CLOCK});`)() as (callbacks: { render(): void; tick(id: number): void; error(error: unknown): void }) => Clock;

  it("ships a syntactically complete worker with the tested clock", () => {
    expect(() => new Function(CLIENT_WORKER)).not.toThrow();
  });
  it("pauses all scheduled callbacks, retains local state, and resumes without missed-tick replay", () => {
    vi.useFakeTimers();
    let state = 0;
    const snapshots: number[] = [];
    const tick = vi.fn((_id: number) => { state++; });
    const clock = makeClock({ render: () => snapshots.push(state), tick, error: vi.fn() });
    const timer = clock.startTimer(100);
    vi.advanceTimersByTime(100);
    expect(state).toBe(1);
    clock.suspend(true);
    expect(vi.getTimerCount()).toBe(0);
    clock.schedule(); clock.render();
    vi.advanceTimersByTime(30_000);
    expect(state).toBe(1);
    expect(snapshots).toEqual([]);
    clock.suspend(false);
    expect(snapshots).toEqual([1]);
    expect(vi.getTimerCount()).toBe(1);
    vi.advanceTimersByTime(134);
    expect(state).toBe(2);
    expect(tick.mock.calls.map(([id]) => id)).toEqual([timer, timer]);
    expect(snapshots).toEqual([1, 2]);
    clock.suspend(true);
  });
  it("supports registering and disposing timers while hidden without creating work", () => {
    vi.useFakeTimers();
    const tick = vi.fn();
    const render = vi.fn();
    const clock = makeClock({ render, tick, error: vi.fn() });
    clock.suspend(true);
    const discarded = clock.startTimer(100);
    const retained = clock.startTimer(200);
    clock.stopTimer(discarded);
    expect(vi.getTimerCount()).toBe(0);
    clock.suspend(false); clock.suspend(false);
    expect(render).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(1);
    vi.advanceTimersByTime(200);
    expect(tick).toHaveBeenCalledExactlyOnceWith(retained);
    clock.suspend(true);
    clock.stopTimer(retained);
    clock.suspend(false);
    expect(vi.getTimerCount()).toBe(0);
  });
  it("keeps timer limits and reports callback failures across suspension", () => {
    vi.useFakeTimers();
    const error = vi.fn();
    const clock = makeClock({ render: () => { throw new Error("failed render"); }, tick: vi.fn(), error });
    clock.suspend(true);
    for (let i = 0; i < 32; i++) clock.startTimer(100);
    expect(() => clock.startTimer(100)).toThrow("timer limit");
    expect(vi.getTimerCount()).toBe(0);
    clock.suspend(false);
    expect(error).toHaveBeenCalledWith(expect.objectContaining({ message: "failed render" }));
    expect(vi.getTimerCount()).toBe(32);
    clock.suspend(true);
  });
});
