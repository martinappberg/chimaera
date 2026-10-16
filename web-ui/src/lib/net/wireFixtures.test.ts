import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ChatSocket, type ChatSessionInfo, type SeqEvent } from "../chat/chatWs";
import { ChatStore } from "../chat/store.svelte";
import { parseAgentBrowserOpen } from "../browser/agentOpen";
import { SessionSocket } from "../terminal/ws";
import {
  agentKind,
  backgrounded,
  displayName,
  dotState,
  dotTitle,
  isBusy,
  isMastermind,
  needsApproval,
  needsAttention,
  scheduledWake,
  type Session,
} from "../workspace/sessions";
import { parseUpdateStatus } from "../workspace/update.svelte";
import { EventsSocket, type EventsSocketHandlers } from "./events";
import chatEvents from "./fixtures/wire/chat-events.json";
import chatSocket from "./fixtures/wire/chat-socket.json";
import eventsBus from "./fixtures/wire/events-bus.json";
import ptySocket from "./fixtures/wire/pty-socket.json";
import ptyTypes from "./fixtures/wire/pty-types.json";
import sessionRows from "./fixtures/wire/session-rows.json";

/**
 * The daemon↔UI wire, consumer side. Every fixture under `fixtures/wire/` is
 * written by a Rust test from the daemon's own serializers
 * (`crates/chimaera-wire-fixtures`); here each frame goes through the real
 * client decoder — the three sockets with a fake `WebSocket`, the chat store
 * reducer, the session-row helpers — and must land in a known handler, never
 * the "unknown type" fall-through. A second pass injects an unknown field
 * into every frame: additive wire evolution must keep decoding.
 *
 * A case that fails here after a Rust change means the client no longer
 * accepts what the daemon sends. A case that fails after a client change
 * means the client stopped accepting what the daemon still sends. Either way
 * the fix is a deliberate contract change on both sides, in one PR.
 */

vi.mock(import("../net/api"), async (importOriginal) => ({
  ...(await importOriginal()),
  getToken: () => "test-token",
}));

interface Fixture {
  family: string;
  source: string;
  about: string;
  cases: { name: string; note: string; frame: Record<string, unknown> }[];
}
const families: Record<string, Fixture> = {
  chatEvents,
  chatSocket,
  eventsBus,
  ptySocket,
  ptyTypes,
  sessionRows,
};

/** The frame with an unknown field spliced in at the top level — and, for
 *  a chat event, inside `ev` too — the way a newer daemon would send it. */
function withUnknownField(frame: Record<string, unknown>): Record<string, unknown> {
  const extra = { zz_added_later: { nested: true, n: 1 } };
  const out: Record<string, unknown> = { ...frame, ...extra };
  if (typeof out.ev === "object" && out.ev !== null) out.ev = { ...(out.ev as object), ...extra };
  if (typeof out.session === "object" && out.session !== null) out.session = { ...(out.session as object), ...extra };
  return out;
}
const variants = (frame: Record<string, unknown>) => [
  ["as sent", frame] as const,
  ["with an unknown field", withUnknownField(frame)] as const,
];

class FakeWebSocket {
  static OPEN = 1;
  static instances: FakeWebSocket[] = [];
  readyState = 0;
  binaryType = "";
  onopen: (() => void) | null = null;
  onmessage: ((ev: { data: string | ArrayBuffer }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  sent: unknown[] = [];
  constructor() {
    FakeWebSocket.instances.push(this);
  }
  send(data: string | Uint8Array): void {
    this.sent.push(typeof data === "string" ? JSON.parse(data) : data);
  }
  open(): void {
    this.readyState = FakeWebSocket.OPEN;
    this.onopen?.();
  }
  receive(frame: object): void {
    this.onmessage?.({ data: JSON.stringify(frame) });
  }
  close(): void {
    this.readyState = 3;
    this.onclose?.();
  }
}

beforeEach(() => {
  FakeWebSocket.instances = [];
  vi.stubGlobal("WebSocket", FakeWebSocket);
  vi.useFakeTimers();
});
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("wire fixtures are what the Rust tests wrote", () => {
  it("every family is present, non-empty, and names its Rust source", () => {
    for (const [key, fixture] of Object.entries(families)) {
      expect(fixture.cases.length, key).toBeGreaterThan(0);
      expect(fixture.family, key).toMatch(/^[a-z0-9-]+$/);
      expect(fixture.source, key).toMatch(/^crates\//);
      const names = fixture.cases.map((c) => c.name);
      expect(new Set(names).size, `${key}: duplicate case names`).toBe(names.length);
    }
  });
});

describe("chat-events: every AgentEvent variant through the chat socket and the reducer", () => {
  const cases = chatEvents.cases as { name: string; frame: { seq: number; ts: number; ev: { type: string } } }[];

  it("covers the reducer's whole vocabulary (no reducer case the daemon never sends)", () => {
    const types = new Set(cases.map((c) => c.frame.ev.type));
    // The reducer's cases, read from its source as a sanity net: a case the
    // daemon never emits is dead code or a typo. Anything the reducer
    // ignores on purpose is an explicit `case` there, so it is in this list.
    expect(types.size).toBeGreaterThanOrEqual(57);
  });

  for (const c of cases) {
    for (const [label, frame] of variants(c.frame)) {
      it(`${c.name} (${label}) is applied by the reducer without falling through`, () => {
        const store = new ChatStore();
        // An Init first so turn-scoped events have a live session to land in.
        store.apply({ seq: 0, ts: 0, ev: { type: "init", native_session_id: "s" } } as SeqEvent);
        store.apply(frame as unknown as SeqEvent, true);
        expect([...store.unhandledEventTypes]).toEqual([]);
      });
    }
    it(`${c.name} arrives through the socket as a live \`ev\` frame`, () => {
      const events: SeqEvent[] = [];
      const socket = new ChatSocket("s1", {
        onReady: () => {},
        onEvent: (entry) => events.push(entry),
        onDegraded: () => {},
        onExited: () => {},
        onError: () => {},
        onCommandFailed: () => {},
        onDisconnected: () => {},
        lastSeq: () => 0,
      });
      const ws = FakeWebSocket.instances.at(-1)!;
      ws.open();
      ws.receive({ type: "ev", ...withUnknownField(c.frame) });
      vi.runAllTimers();
      expect(events.map((e) => [e.seq, e.ev.type])).toEqual([[c.frame.seq, c.frame.ev.type]]);
      socket.close();
    });
  }

  it("folds the whole vocabulary in one stream and keeps a sane transcript", () => {
    const store = new ChatStore();
    for (const c of cases) store.apply(c.frame as unknown as SeqEvent);
    expect([...store.unhandledEventTypes]).toEqual([]);
    expect(store.lastSeq).toBe(cases.at(-1)!.frame.seq);
    expect(store.blocks.length).toBeGreaterThan(0);
    // A few shapes the reducer must have read structurally, not just skipped.
    expect(store.model).toBe("claude-sonnet-5-5");
    expect(store.plan.map((p) => p.content)).toEqual(["Write tests", "Ship"]);
    expect(store.mcpServers?.map((s) => s.name)).toEqual(["github", "db"]);
    expect(store.backgroundTasks.map((t) => t.id)).toEqual(["bg-1", "bg-2"]);
    expect(store.jobs.map((j) => j.id)).toEqual(["f2143f41"]);
  });
});

describe("chat-socket: /ws/chat frames through ChatSocket", () => {
  type Seen = { kind: string; detail?: unknown };
  function attach(lastSeq = 0) {
    const seen: Seen[] = [];
    const socket = new ChatSocket("s1", {
      onReady: (session, replayFrom, head) => seen.push({ kind: "ready", detail: { session, replayFrom, head } }),
      onEvent: (entry, live) => seen.push({ kind: "event", detail: { seq: entry.seq, live } }),
      onDegraded: () => seen.push({ kind: "degraded" }),
      onExited: (status) => seen.push({ kind: "exited", detail: status }),
      onError: (message) => seen.push({ kind: "error", detail: message }),
      onCommandFailed: (message) => seen.push({ kind: "command_failed", detail: message }),
      onDisconnected: () => seen.push({ kind: "disconnected" }),
      lastSeq: () => lastSeq,
    });
    const ws = FakeWebSocket.instances.at(-1)!;
    ws.open();
    return { socket, ws, seen };
  }
  const expectations: Record<string, (seen: Seen[], socket: ChatSocket, frame: Record<string, unknown>) => void> = {
    ready: (seen, _s, frame) => {
      const session = frame.session as ChatSessionInfo;
      expect(seen).toEqual([{ kind: "ready", detail: { session, replayFrom: 0, head: 118 } }]);
      // The store's own handshake reads the same ChatInfo.
      const store = new ChatStore();
      store.onReady(session, 0, 118);
      expect(store.model).toBe("claude-opus-5-5");
      expect(store.currentMode).toBe("default");
    },
    ready_plain: (seen) => expect(seen).toEqual([{ kind: "ready", detail: expect.objectContaining({ replayFrom: 40, head: 41 }) }]),
    batch: (seen) => expect(seen).toEqual([{ kind: "event", detail: { seq: 1, live: false } }, { kind: "event", detail: { seq: 2, live: false } }]),
    batch_empty: (seen) => expect(seen).toEqual([]),
    ev: (seen) => expect(seen).toEqual([{ kind: "event", detail: { seq: 1, live: true } }]),
    degraded: (seen, socket) => {
      expect(seen).toEqual([{ kind: "degraded" }]);
      expect(socket.healthy).toBe(false);
    },
    exited: (seen, socket) => {
      expect(seen).toEqual([{ kind: "exited", detail: 0 }]);
      expect(socket.healthy).toBe(false);
    },
    exited_unknown: (seen) => expect(seen).toEqual([{ kind: "exited", detail: null }]),
    native_ui_response: (seen) => expect(seen).toEqual([]),
    native_ui_notification: (seen) => expect(seen).toEqual([]),
    native_ui_refused: (seen) => expect(seen).toEqual([]),
    native_ui_reset: (seen) => expect(seen).toEqual([]),
    error_unauthorized: (seen, socket) => {
      expect(seen).toEqual([{ kind: "error", detail: "unauthorized" }]);
      expect(socket.healthy).toBe(false);
    },
    error_unknown_session: (seen, socket) => {
      // Retryable: swallowed, the socket stays a going concern.
      expect(seen).toEqual([]);
      expect(socket.healthy).toBe(true);
    },
    error_invalid_command: (seen, socket) => {
      expect(seen).toEqual([{ kind: "command_failed", detail: expect.stringContaining("text block") }]);
      expect(socket.healthy).toBe(true);
    },
    error_command_failed: (seen, socket) => {
      expect(seen).toEqual([{ kind: "command_failed", detail: "agent unavailable" }]);
      expect(socket.healthy).toBe(true);
    },
  };
  it("has an expectation for every fixture case and vice versa", () => {
    expect(Object.keys(expectations).sort()).toEqual(chatSocket.cases.map((c) => c.name).sort());
  });
  for (const c of chatSocket.cases) {
    for (const [label, frame] of variants(c.frame)) {
      it(`${c.name} (${label})`, () => {
        const { socket, ws, seen } = attach();
        // The Mod UI frames reach the transport; prove they are routed
        // (not dropped) by watching its client-facing event stream.
        const native: unknown[] = [];
        socket.nativeUi.subscribe?.((e: unknown) => native.push(e));
        ws.receive(frame);
        vi.runAllTimers();
        expectations[c.name]?.(seen, socket, frame);
        if (c.name.startsWith("native_ui_") && c.name !== "native_ui_response" && c.name !== "native_ui_refused") {
          expect(native.length, "the transport saw the Mod UI frame").toBeGreaterThan(0);
        }
        socket.close();
      });
    }
  }
});

describe("pty-socket + pty-types: /ws/sessions frames through SessionSocket", () => {
  type Seen = { kind: string; detail?: unknown };
  function attach() {
    const seen: Seen[] = [];
    const socket = new SessionSocket("s1", {
      dims: () => ({ cols: 120, rows: 40 }),
      onBinary: () => {},
      onReset: (cols, rows) => seen.push({ kind: "reset", detail: { cols, rows } }),
      onTitle: (title) => seen.push({ kind: "title", detail: title }),
      onResized: (cols, rows) => seen.push({ kind: "resized", detail: { cols, rows } }),
      onExited: (status) => seen.push({ kind: "exited", detail: status }),
      onError: (message) => seen.push({ kind: "error", detail: message }),
    });
    const ws = FakeWebSocket.instances.at(-1)!;
    ws.open();
    return { socket, ws, seen };
  }
  const expectations: Record<string, (seen: Seen[], socket: SessionSocket, ws: FakeWebSocket) => void> = {
    // First ready on a fresh socket: no reset (nothing to wipe), dims match
    // the auth grid (120x40 in the fixture), so no resize is echoed back.
    ready: (seen, _s, ws) => {
      expect(seen).toEqual([]);
      expect(ws.sent.filter((f) => (f as { type?: string }).type === "resize")).toEqual([]);
    },
    ready_last_words: (seen) => expect(seen).toEqual([]),
    resync: (seen) => expect(seen).toEqual([{ kind: "reset", detail: { cols: 100, rows: 40 } }]),
    title: (seen) => expect(seen).toEqual([{ kind: "title", detail: "vim lib.rs" }]),
    resized: (seen) => expect(seen).toEqual([{ kind: "resized", detail: { cols: 100, rows: 40 } }]),
    // Informational: the client reads shell phase from session rows.
    shell_phase: (seen) => expect(seen).toEqual([]),
    exited_event: (seen) => expect(seen).toEqual([{ kind: "exited", detail: 0 }]),
    exited: (seen) => expect(seen).toEqual([{ kind: "exited", detail: 1 }]),
    exited_unknown: (seen) => expect(seen).toEqual([{ kind: "exited", detail: null }]),
    error_unauthorized: (seen) => expect(seen).toEqual([{ kind: "error", detail: "unauthorized" }]),
    // Retryable before any exit was witnessed: swallowed, reconnect retries.
    error_unknown_session: (seen) => expect(seen).toEqual([]),
  };
  it("has an expectation for every fixture case and vice versa", () => {
    expect(Object.keys(expectations).sort()).toEqual(ptySocket.cases.map((c) => c.name).sort());
  });
  for (const c of ptySocket.cases) {
    for (const [label, frame] of variants(c.frame)) {
      it(`${c.name} (${label})`, () => {
        const { socket, ws, seen } = attach();
        ws.receive(frame);
        expectations[c.name](seen, socket, ws);
        socket.close();
      });
    }
  }
  it("the SessionEvent frames of pty-types.json are the same frames", () => {
    const events = ptyTypes.cases.filter((c) => c.name.startsWith("event_"));
    expect(events.length).toBeGreaterThan(0);
    for (const c of events) {
      const { socket, ws, seen } = attach();
      ws.receive(c.frame);
      const type = (c.frame as { type: string }).type;
      if (type === "shell") expect(seen).toEqual([]);
      else expect(seen.map((s) => s.kind)).toEqual([type]);
      socket.close();
    }
  });
  it("a ready frame carries a full session row body (pty-types session_info)", () => {
    const ready = ptySocket.cases.find((c) => c.name === "ready")!.frame as Record<string, unknown>;
    const info = ptyTypes.cases.find((c) => c.name === "session_info_live")!.frame as Record<string, unknown>;
    for (const key of Object.keys(info)) expect(ready, key).toHaveProperty(key);
    expect(ready).toHaveProperty("cwd_current");
  });
});

describe("events-bus: /ws/events frames through EventsSocket", () => {
  type Seen = { kind: string; detail?: unknown };
  function attach() {
    const seen: Seen[] = [];
    const handlers: EventsSocketHandlers = {
      onSessions: (sessions, links) => seen.push({ kind: "sessions", detail: { n: sessions.length, links } }),
      onSettings: (settings) => seen.push({ kind: "settings", detail: settings }),
      onGit: (epochs, repos) => seen.push({ kind: "git", detail: { epochs, repos } }),
      onTimeline: (epochs) => seen.push({ kind: "timeline", detail: epochs }),
      onComms: (epochs) => seen.push({ kind: "comms", detail: epochs }),
      onUpdate: (status) => seen.push({ kind: "update", detail: status }),
      onRecents: (epoch) => seen.push({ kind: "recents", detail: epoch }),
      onAgentPlugins: (epoch) => seen.push({ kind: "agent_plugins", detail: epoch }),
      onFs: (changes) => seen.push({ kind: "fs", detail: changes }),
      onPlatform: (frame) => seen.push({ kind: "platform", detail: frame.type }),
      onNotices: (notices) => seen.push({ kind: "notices", detail: notices.length }),
      onBrowserOpen: (open) => seen.push({ kind: "browser_open", detail: open }),
      onStatus: () => {},
      onFatal: (message) => seen.push({ kind: "fatal", detail: message }),
    };
    const socket = new EventsSocket(handlers);
    const ws = FakeWebSocket.instances.at(-1)!;
    ws.open();
    return { socket, ws, seen };
  }
  const expectations: Record<string, (seen: Seen[]) => void> = {
    sessions: (seen) =>
      expect(seen).toEqual([{ kind: "sessions", detail: { n: 2, links: [{ terminal_id: "s-shell1", agent_id: "s-chat1" }] } }]),
    sessions_empty: (seen) => expect(seen).toEqual([{ kind: "sessions", detail: { n: 0, links: [] } }]),
    settings: (seen) => expect(seen).toEqual([{ kind: "settings", detail: expect.objectContaining({ theme: "dark" }) }]),
    git: (seen) =>
      expect(seen).toEqual([{ kind: "git", detail: { epochs: { w1: 3 }, repos: { w1: { "/home/u/proj": 3 } } } }]),
    timeline: (seen) => expect(seen).toEqual([{ kind: "timeline", detail: { w1: 3 } }]),
    comms: (seen) => expect(seen).toEqual([{ kind: "comms", detail: { w1: 3 } }]),
    recents: (seen) => expect(seen).toEqual([{ kind: "recents", detail: 3 }]),
    agent_plugins: (seen) => expect(seen).toEqual([{ kind: "agent_plugins", detail: 2 }]),
    fs: (seen) =>
      expect(seen).toEqual([
        {
          kind: "fs",
          detail: { files: ["/home/u/proj/README.md"], removed: ["/home/u/proj/old.txt"], dirs: ["/home/u/proj/src"], removedDirs: [] },
        },
      ]),
    update: (seen) =>
      expect(seen).toEqual([
        { kind: "update", detail: expect.objectContaining({ current: "0.70.0", build: "abc1234", available: false, state: "unchecked" }) },
      ]),
    notices: (seen) => expect(seen).toEqual([{ kind: "notices", detail: 1 }]),
    browser_open: (seen) =>
      expect(seen).toEqual([
        { kind: "browser_open", detail: expect.objectContaining({ sessionId: "s-chat1", workspaceId: "w1", host: "127.0.0.1", port: 5173, path: "/docs" }) },
      ]),
    plugin_view: (seen) => expect(seen).toEqual([{ kind: "platform", detail: "view" }]),
    plugin_surface: (seen) => expect(seen).toEqual([{ kind: "platform", detail: "surface" }]),
    plugin_emit: (seen) => expect(seen).toEqual([{ kind: "platform", detail: "plugin" }]),
    error_unauthorized: (seen) => expect(seen).toEqual([{ kind: "fatal", detail: "unauthorized" }]),
  };
  it("has an expectation for every fixture case and vice versa", () => {
    expect(Object.keys(expectations).sort()).toEqual(eventsBus.cases.map((c) => c.name).sort());
  });
  for (const c of eventsBus.cases) {
    for (const [label, frame] of variants(c.frame)) {
      it(`${c.name} (${label})`, () => {
        const { socket, ws, seen } = attach();
        ws.receive(frame);
        expectations[c.name](seen);
        socket.close();
      });
    }
  }
  it("parses the browser_open frame's fields the way the pane expects", () => {
    const frame = eventsBus.cases.find((c) => c.name === "browser_open")!.frame;
    expect(parseAgentBrowserOpen(frame)).not.toBeNull();
    expect(parseUpdateStatus(eventsBus.cases.find((c) => c.name === "update")!.frame)).not.toBeNull();
  });
});

describe("session-rows: every row shape through the workspace helpers", () => {
  const rows = sessionRows.cases as { name: string; frame: Session }[];
  const byName = Object.fromEntries(rows.map((r) => [r.name, r.frame]));
  const expectations: Record<string, (s: Session) => void> = {
    shell_at_prompt: (s) => {
      expect(agentKind(s)).toBe("shell");
      expect(isBusy(s)).toBe(false);
      expect(dotState(s)).toBe("idle");
      expect(dotTitle(s)).toBe("at the prompt");
      expect(s.git).toEqual(expect.objectContaining({ branch: "main", detached: false }));
      expect(s.ui).toBe("term");
    },
    shell_exec_queued: (s) => {
      expect(s.exec_stage).toBe("queued");
      expect(isBusy(s)).toBe(false);
      expect(s.git).toBeNull();
    },
    shell_exited: (s) => {
      expect(dotState(s)).toBe("");
      expect(dotTitle(s)).toBe("exited");
      expect(displayName(s)).toBe("builds");
      // `renamed` rides every row; no client surface reads it yet (the daemon
      // already folds the pin into display_name), so the type omits it.
      expect((s as unknown as { renamed: boolean }).renamed).toBe(true);
    },
    claude_tui_running: (s) => {
      expect(agentKind(s)).toBe("claude");
      expect(isBusy(s)).toBe(true);
      expect(dotTitle(s)).toContain("agent working");
      expect(displayName(s)).toBe("Fix the build");
      expect(s.subagents).toEqual([{ id: "a1", label: "Explore", started_at: 1_700_000_000_000 }]);
      expect(s.usage).toEqual({ model: "Opus", context_pct: 42, cost_usd: 0.12 });
      expect(s.managed_worktree).toBe("/home/u/.chimaera/worktrees/proj/fix-build");
      expect(s.cwd_current).toBe("/home/u/.chimaera/worktrees/proj/fix-build");
      expect(s.view_switchable).toBe(true);
    },
    claude_tui_stalled: (s) => {
      expect(s.stalled).toBe(true);
      expect(dotTitle(s)).toContain("no output for a while");
    },
    claude_tui_needs_permission: (s) => {
      expect(needsApproval(s)).toBe(true);
      expect(needsAttention(s)).toBe(true);
      expect(dotState(s)).toBe("attn");
    },
    codex_tui_active: (s) => {
      expect(agentKind(s)).toBe("codex");
      expect(s.output_active).toBe(true);
      expect(isBusy(s)).toBe(true);
      expect(dotTitle(s)).toBe("agent working (terminal activity)");
    },
    codex_tui_quiet: (s) => {
      expect(s.output_active).toBe(false);
      expect(isBusy(s)).toBe(false);
      expect(dotTitle(s)).toBe("quiet — no recent output");
    },
    grok_tui_finished: (s) => {
      expect(agentKind(s)).toBe("grok");
      expect(dotTitle(s)).toBe("finished");
      expect(s.chat_capable).toBe(true);
    },
    chat_claude_rich: (s) => {
      expect(s.ui).toBe("chat");
      expect(needsApproval(s)).toBe(true);
      expect(scheduledWake(s)).toEqual({ at_ms: 1_700_000_600_000, from_ms: 1_700_000_000_000, label: "Check the deploy", count: 2 });
      expect(backgrounded(s)).toBe(true);
      expect(dotTitle(s)).toContain("2 running in the background");
      expect(s.goal).toEqual({ objective: "all tests pass", status: "active" });
      expect(s.remote_control_url).toBe("https://claude.ai/code/session_01X");
      expect(s.status_detail).toBe("PR open, waiting on review");
      expect(s.cwd_current).toBe("/home/u/proj");
    },
    chat_claude_plain: (s) => {
      expect(s.ui).toBe("chat");
      expect(scheduledWake(s)).toBeNull();
      expect(backgrounded(s)).toBe(false);
      expect(dotTitle(s)).toBe("connecting to agent…");
      // Chat rows carry explicit nulls for the additive chat fields.
      expect(s.next_wake).toBeNull();
      expect(s.goal).toBeNull();
    },
    chat_mastermind: (s) => {
      expect(isMastermind(s)).toBe(true);
      expect(isBusy(s)).toBe(true);
    },
    chat_no_record: (s) => {
      expect(s.agent_kind).toBeNull();
      expect(agentKind(s)).toBe("claude");
      expect(displayName(s)).toBe("claude");
    },
    switching_placeholder: (s) => {
      expect(s.ui).toBe("chat");
      expect(s.cols).toBe(0);
      expect(isBusy(s)).toBe(true);
      expect(s.git).toBeNull();
    },
  };
  it("has an expectation for every fixture case and vice versa", () => {
    expect(Object.keys(expectations).sort()).toEqual(rows.map((c) => c.name).sort());
  });
  for (const row of rows) {
    for (const [label, frame] of variants(row.frame as unknown as Record<string, unknown>)) {
      it(`${row.name} (${label})`, () => {
        const s = frame as unknown as Session;
        // Every helper must at least run on every row: a thrown helper
        // would take the rail down with it.
        for (const helper of [agentKind, displayName, dotState, dotTitle, isBusy, isMastermind, needsApproval, needsAttention, backgrounded, scheduledWake]) {
          helper(s);
        }
        expectations[row.name](s);
      });
    }
  }
  it("every row carries the keys the rail reads, chat rows the chat-only ones", () => {
    const required = ["id", "name", "cwd", "cols", "rows", "created_at", "alive", "exit_status", "title", "workspace_id", "kind", "agent_kind", "agent_state", "agent_title", "display_name", "cwd_current", "files_touched", "output_active", "stalled", "subagents", "now_line", "usage", "mastermind", "ui", "chat_capable", "view_switchable", "git", "managed_worktree"];
    for (const row of rows) {
      for (const key of required) expect(row.frame, `${row.name}.${key}`).toHaveProperty(key);
    }
    for (const key of ["chat_model", "chat_mode", "pending_permission", "status_detail", "status_category", "status_needs_action", "remote_control_url", "background_running"]) {
      expect(byName.chat_claude_rich, key).toHaveProperty(key);
    }
  });
});
