import { describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

// A public build: the vite plugin selects no application entry and emits
// exactly this module (`scripts/application-entry.mjs`). Vitest has no such
// plugin, so it is stated here; everything below runs the real modules.
vi.mock("virtual:chimaera-application-entry", () => ({ loadApplicationEntry: null }));

import { selectedApplication } from "../extensions/selected";
import { proTier } from "../net/plan";
import { socketKeepers } from "../net/reconnect";
import { pickupNote } from "../chat/transfer";
import { admitsAgentBrowserOpen, type AgentBrowserOpen } from "../browser/agentOpen";
import { defaultLayout, deserializeLayout, openSession, openTab, panes, serializeLayout } from "../layout/layout";
import { ChatStore } from "../chat/store.svelte";
import type { ChatSessionInfo, SeqEvent } from "../chat/chatWs";
import { currentNotice, parseUpdateStatus, updateState } from "../workspace/update.svelte";

// Every surface a public build shows must be origin/main's: the helpers that
// decide the gated Pro presentation give main's answer when there is no
// extension. Each one is checked here as the surface calls it.

const free = (): boolean => get(proTier) === "free";

describe("a build without the extension", () => {
  it("has no extension and is the free tier", () => {
    expect(selectedApplication).toBeNull();
    expect(get(proTier)).toBe("free");
  });

  it("keeps main's socket semantics (App sets keepers from the tier)", () => {
    expect(socketKeepers()).toBe(false);
  });

  it("quotes the restart pick-up in the Timeline as main did", () => {
    const restart =
      "The Chimaera daemon hosting this session restarted, so this conversation was resumed in a new agent process.";
    expect(pickupNote(restart, false, free())).toBeNull();
  });

  it("opens every agent browser frame, even before the roster lists its session", () => {
    const open: AgentBrowserOpen = {
      sessionId: "s-agent",
      workspaceId: "w-1",
      host: "localhost",
      port: 5173,
      path: "/",
    } as AgentBrowserOpen;
    expect(admitsAgentBrowserOpen(open, undefined, false, free())).toBe(true);
  });

  it("restores no Pro tabs from a saved layout", () => {
    let layout = openSession(defaultLayout(), "agent");
    layout = openTab(openTab(openTab(layout, { surface: "settings" }), { surface: "pro" }), { surface: "kept" });
    const restored = deserializeLayout(serializeLayout(layout), selectedApplication !== null);
    expect(panes(restored!.root).flatMap((pane) => pane.tabs)).toEqual([
      { surface: "terminal", sessionId: "agent" },
      { surface: "settings" },
    ]);
  });

  it("sends chat messages without ids and tracks nothing, against main's daemon", () => {
    const store = new ChatStore();
    // Before the first `ready` too.
    expect(store.sendsWithIds(free())).toBe(false);
    const session = { id: "s", agent: "claude", alive: true, exit_status: null } as unknown as ChatSessionInfo;
    store.onReady(session, 0, 0, { sendIds: false, reattach: false });
    expect(store.sendsWithIds(free())).toBe(false);
    const events: Record<string, unknown>[] = [
      { type: "turn_started", turn_id: "t1" },
      { type: "user_message", id: "q1", text: "steer", queued: true },
      { type: "exited", status: 1 },
    ];
    events.forEach((ev, i) => store.apply({ seq: i + 1, ts: i, ev } as SeqEvent));
    store.onExited(1);
    store.onCommandFailed("agent unavailable", "send", null, null);
    expect(store.sending).toEqual([]);
    expect(store.restoredDrafts).toEqual([]);
    expect(store.pendingSends.map((send) => send.uncertain)).toEqual([undefined]);
  });

  it("answers an update check as main did when the daemon does not update with the app", () => {
    // A browser window (no native shell).
    vi.stubGlobal("window", {});
    updateState.daemon = parseUpdateStatus({
      current: "0.42.1",
      dev: false,
      state: "current",
      available: false,
      latest: { version: "0.42.1", url: "https://example.test/v0.42.1" },
    });
    updateState.askError = null;
    updateState.asked = "answered";
    expect(currentNotice(null)).toEqual({ kind: "current", version: "0.42.1" });
    vi.unstubAllGlobals();
  });
});
