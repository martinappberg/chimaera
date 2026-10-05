import { describe, expect, it } from "vitest";
import { ModsController } from "./mods.svelte";
import { NativeUiTransport, type UiRecord } from "./nativeUi";

const settle = async () => { for (let i = 0; i < 8; i++) await Promise.resolve(); };

describe("native Mod attachment lifecycle", () => {
  it("shares one attachment, caps render fanout, and discards stale trees on reconnect", async () => {
    const frames: UiRecord[] = [];
    const transport = new NativeUiTransport((frame) => { frames.push(frame); return true; });
    const answer = (frame: UiRecord, result: UiRecord = {}) => transport.receive({ kind: "response", request_id: frame.request_id, result });
    const count = (method: string) => frames.filter((frame) => (frame.request as UiRecord).subtype === method);
    const controller = new ModsController(transport);
    transport.connected();
    const release1 = controller.retain(), release2 = controller.retain();
    expect(count("ui_attach")).toHaveLength(1);
    answer(count("ui_attach")[0]); await settle();
    answer(count("ui_panes")[0], { panes: [], shown_id: null, focused_id: "old-pane", focus_requested_id: "old-pane" }); await settle();
    expect(controller.focusRequested).toBe("old-pane");
    const updates: unknown[] = [];
    const sites = Array.from({ length: 12 }, (_, i) => controller.mount({ component: "Pane", instance_id: `${i}`, props: {} }, (render) => updates.push(render.tree)));
    expect(count("ui_render")).toHaveLength(4);
    const stale = count("ui_render")[0];
    transport.reset(true); await settle();
    expect(controller.attached).toBe(false);
    answer(stale, { hooked: true, tree: { type: "Text", children: ["stale"] } });
    expect(updates.every((tree) => tree === null)).toBe(true);
    sites.forEach((site) => site.dispose());
    transport.connected(); await settle();
    expect(controller.focusRequested).toBeNull();
    expect(controller.focused).toBeNull();
    expect(count("ui_attach")).toHaveLength(2);
    answer(count("ui_attach")[1]); await settle();
    answer(count("ui_panes")[1]); await settle();
    release1(); expect(count("ui_detach")).toHaveLength(0);
    release2(); expect(count("ui_detach")).toHaveLength(1);
    answer(count("ui_detach")[0]);
  });
});
