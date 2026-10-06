import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Importing the real store applies the theme to `document` at load; every
// notification setting defaults on, which is all these cases need.
vi.mock("../settings/store.svelte", () => ({ getSetting: () => true }));

import { deliverBrowserNotices, type Notice, type NoticeContext } from "./notices";

const posted: string[] = [];

class FakeNotification {
  static permission = "granted";
  onclick: (() => void) | null = null;
  onclose: (() => void) | null = null;
  constructor(title: string) {
    posted.push(title);
  }
  close(): void {}
}

function notice(id: number, session: string, workspace: string | null): Notice {
  return {
    id,
    kind: "done",
    blocking: false,
    session_id: session,
    workspace_id: workspace,
    workspace: null,
    agent: null,
    name: session,
    title: `${session} finished`,
    subtitle: "",
    body: "",
    at_ms: 0,
    age_ms: 0,
  };
}

function tab(over: Partial<NoticeContext>): NoticeContext {
  return { visible: [], workspaceId: null, onClick: () => {}, ...over };
}

/** Deliver one notice and let the sibling-claim wait elapse. */
function deliver(n: Notice, ctx: NoticeContext): string[] {
  posted.length = 0;
  deliverBrowserNotices([n], ctx);
  vi.advanceTimersByTime(1000);
  return [...posted];
}

describe("deliverBrowserNotices", () => {
  let focused = true;

  beforeEach(() => {
    vi.useFakeTimers();
    focused = true;
    vi.stubGlobal("Notification", FakeNotification);
    vi.stubGlobal("window", { isSecureContext: true });
    vi.stubGlobal("document", {
      visibilityState: "visible",
      hasFocus: () => focused,
    });
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("stays quiet about any session of the focused workspace tab, on screen or not", () => {
    const ctx = tab({ workspaceId: "ws1", visible: ["shown"] });
    expect(deliver(notice(1, "shown", "ws1"), ctx)).toEqual([]);
    expect(deliver(notice(2, "in-another-tab", "ws1"), ctx)).toEqual([]);
  });

  it("alerts about another workspace while this tab is focused", () => {
    const ctx = tab({ workspaceId: "ws1", visible: ["shown"] });
    expect(deliver(notice(3, "elsewhere", "ws2"), ctx)).toEqual(["elsewhere finished"]);
  });

  it("alerts when the tab is not focused, whatever it shows", () => {
    focused = false;
    const ctx = tab({ workspaceId: "ws1", visible: ["shown"] });
    expect(deliver(notice(4, "shown", "ws1"), ctx)).toEqual(["shown finished"]);
    expect(deliver(notice(5, "in-another-tab", "ws1"), ctx)).toEqual(["in-another-tab finished"]);
  });

  it("covers only what a torn-off pane shows", () => {
    const ctx = tab({ workspaceId: null, visible: ["pane"] });
    expect(deliver(notice(6, "pane", "ws1"), ctx)).toEqual([]);
    expect(deliver(notice(7, "in-the-main-window", "ws1"), ctx)).toEqual([
      "in-the-main-window finished",
    ]);
  });
});
