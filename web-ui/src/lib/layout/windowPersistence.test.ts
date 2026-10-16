import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const vs = vi.hoisted(() => ({
  saves: [] as Array<[string, unknown]>,
  flushes: 0,
  store: new Map<string, unknown>(),
  loads: [] as string[],
}));
vi.mock("./viewState", () => ({
  saveViewState: (key: string, state: unknown) => void vs.saves.push([key, state]),
  flushViewState: () => {
    vs.flushes += 1;
    return Promise.resolve();
  },
  loadViewState: (key: string) => {
    vs.loads.push(key);
    return Promise.resolve(vs.store.has(key) ? vs.store.get(key) : null);
  },
}));

import { isDetachedWindow, setDetachedWindow } from "../net/api";
import {
  defaultLayout,
  openFile,
  openSession,
  serializeLayout,
  splitPane,
  type Layout,
} from "./layout";
import { surfacesOf } from "./surfaces";
import {
  blobMatches,
  buildViewBlob,
  createViewPersistence,
  leavingViewBlob,
  pickRestoreBlob,
  pruneDeadFiles,
  restoreFromBlob,
  stateKey,
  wsKey,
  type ViewPersistenceHost,
} from "./windowPersistence";

/** The blob App.svelte's persist effect built inline before the move,
 *  verbatim: the oracle the extracted builder must match byte for byte. */
function legacyEffectBlob(
  activeWsId: string | null,
  layout: Layout,
  root: string | null,
  detachedWindow: boolean,
  detachOrigin: string | null,
): Record<string, unknown> {
  const blob: Record<string, unknown> = {
    v: 1,
    ws: activeWsId,
    layout: serializeLayout(layout),
    surfaces: surfacesOf(layout, root),
  };
  if (detachedWindow) {
    blob.dt = 1;
    if (detachOrigin !== null) blob.origin = detachOrigin;
  }
  return blob;
}

function sample(): Layout {
  let l = openSession(defaultLayout(), "s1");
  l = splitPane(l, l.focusedPaneId, "row");
  return openFile(l, "/ws/qc/qc.R");
}

describe("view-state keys", () => {
  it("composes (window, workspace) and the workspace mirror as before", () => {
    expect(stateKey("win-1", null)).toBe("win-1");
    expect(stateKey("win-1", "w-abc")).toBe("win-1_w-abc");
    expect(wsKey("w-abc")).toBe("ws_w-abc");
  });
});

describe("buildViewBlob (the PUT body other windows and the daemon read)", () => {
  it("matches the persist effect's blob byte for byte, attached", () => {
    const l = sample();
    const got = buildViewBlob({ ws: "w-1", layout: l, wsRoot: "/ws", detached: false, origin: "x" });
    expect(JSON.stringify(got)).toBe(JSON.stringify(legacyEffectBlob("w-1", l, "/ws", false, "x")));
    expect(Object.keys(got)).toEqual(["v", "ws", "layout", "surfaces"]);
  });

  it("adds dt then origin for a detached window, in that order", () => {
    const l = sample();
    const got = buildViewBlob({ ws: "w-1", layout: l, wsRoot: "/ws", detached: true, origin: "win-0" });
    expect(JSON.stringify(got)).toBe(JSON.stringify(legacyEffectBlob("w-1", l, "/ws", true, "win-0")));
    expect(Object.keys(got)).toEqual(["v", "ws", "layout", "surfaces", "dt", "origin"]);
  });

  it("omits origin when a detached window has none, and keeps ws null on Home", () => {
    const l = defaultLayout();
    const got = buildViewBlob({ ws: null, layout: l, wsRoot: null, detached: true, origin: null });
    expect(JSON.stringify(got)).toBe(JSON.stringify(legacyEffectBlob(null, l, null, true, null)));
    expect(got).toMatchObject({ v: 1, ws: null, dt: 1 });
    expect("origin" in got).toBe(false);
  });

  it("matches detachOut's pre-seeded blob (dt:1, origin = the source window)", () => {
    const l = sample();
    const detachOutBlob = {
      v: 1,
      ws: "w-1",
      layout: serializeLayout(l),
      surfaces: surfacesOf(l, "/ws"),
      dt: 1,
      origin: "win-src",
    };
    const got = buildViewBlob({ ws: "w-1", layout: l, wsRoot: "/ws", detached: true, origin: "win-src" });
    expect(JSON.stringify(got)).toBe(JSON.stringify(detachOutBlob));
  });

  it("the leaving blob strips dt and surfaces (activateWorkspace's outgoing write)", () => {
    const l = sample();
    const got = leavingViewBlob("w-1", l);
    expect(JSON.stringify(got)).toBe(
      JSON.stringify({ v: 1, ws: "w-1", layout: serializeLayout(l), surfaces: [] }),
    );
  });
});

describe("pickRestoreBlob (boot's restore precedence)", () => {
  const own = { v: 1, ws: "w-1", layout: null, src: "own" };
  const legacy = { v: 1, ws: "w-1", layout: null, src: "legacy" };
  const mirror = { v: 1, ws: "w-1", layout: null, src: "mirror" };

  function loader(entries: Record<string, unknown>) {
    const calls: string[] = [];
    const load = (key: string) => {
      calls.push(key);
      return Promise.resolve(key in entries ? entries[key] : null);
    };
    return { calls, load };
  }

  it("takes the window's own (window, workspace) blob first", async () => {
    const { calls, load } = loader({ "win_w-1": own, win: legacy, "ws_w-1": mirror });
    const raw = await pickRestoreBlob({ winKey: "win", ws: "w-1", isDetached: () => false, load });
    expect(raw).toBe(own);
    expect(calls).toEqual(["win_w-1"]);
  });

  it("falls back to the bare window key, then the workspace mirror", async () => {
    const a = loader({ win: legacy, "ws_w-1": mirror });
    expect(await pickRestoreBlob({ winKey: "win", ws: "w-1", isDetached: () => false, load: a.load })).toBe(legacy);
    expect(a.calls).toEqual(["win_w-1", "win"]);
    const b = loader({ win: { v: 1, ws: "w-other" }, "ws_w-1": mirror });
    expect(await pickRestoreBlob({ winKey: "win", ws: "w-1", isDetached: () => false, load: b.load })).toBe(mirror);
    expect(b.calls).toEqual(["win_w-1", "win", "ws_w-1"]);
  });

  it("a detached window takes its own blob or nothing", async () => {
    const { calls, load } = loader({ win: legacy, "ws_w-1": mirror });
    expect(await pickRestoreBlob({ winKey: "win", ws: "w-1", isDetached: () => true, load })).toBeNull();
    expect(calls).toEqual(["win_w-1"]);
  });

  it("Home (no workspace) never falls back", async () => {
    const { calls, load } = loader({});
    expect(await pickRestoreBlob({ winKey: "win", ws: null, isDetached: () => false, load })).toBeNull();
    expect(calls).toEqual(["win"]);
  });

  it("a hung daemon resolves null after the shared timeout", async () => {
    vi.useFakeTimers();
    try {
      const p = pickRestoreBlob({
        winKey: "win",
        ws: "w-1",
        isDetached: () => false,
        load: () => new Promise(() => {}),
      });
      await vi.advanceTimersByTimeAsync(3000);
      expect(await p).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("restoreFromBlob", () => {
  it("ignores a blob for another workspace or version", () => {
    expect(blobMatches({ v: 1, ws: "w-2" }, "w-1")).toBe(false);
    expect(blobMatches({ v: 2, ws: "w-1" }, "w-1")).toBe(false);
    expect(blobMatches(null, null)).toBe(false);
    expect(restoreFromBlob({ v: 1, ws: "w-2" }, "w-1", defaultLayout(), false)).toBeNull();
  });

  it("restores the serialized tree and stays attached", () => {
    const l = sample();
    const cur = defaultLayout();
    const r = restoreFromBlob(buildViewBlob({ ws: "w-1", layout: l, wsRoot: null, detached: false, origin: null }), "w-1", cur, false);
    expect(r).not.toBeNull();
    expect(serializeLayout(r!.layout)).toEqual(serializeLayout(l));
    expect(r).toMatchObject({ detached: false, origin: null, refreshHint: false });
  });

  it("keeps the current layout when the stored tree is unreadable", () => {
    const cur = defaultLayout();
    const r = restoreFromBlob({ v: 1, ws: "w-1", layout: "garbage" }, "w-1", cur, false);
    expect(r!.layout).toBe(cur);
  });

  it("a blob-only dt:1 marks detached, refreshes the hint, keeps a valid origin, heals focus mode", () => {
    const cur = defaultLayout();
    const r = restoreFromBlob({ v: 1, ws: "w-1", layout: "garbage", dt: 1, origin: "win-0" }, "w-1", cur, false);
    expect(r).toMatchObject({ detached: true, origin: "win-0", refreshHint: true });
    expect(r!.layout.focusMode).toBe(true);
    expect(r!.layout.root).toBe(cur.root);
  });

  it("the hint alone makes it detached; an invalid origin is dropped", () => {
    const r = restoreFromBlob({ v: 1, ws: "w-1", origin: "bad key!" }, "w-1", defaultLayout(), true);
    expect(r).toMatchObject({ detached: true, origin: null, refreshHint: false });
  });

  it("an attached window ignores a stored origin", () => {
    const r = restoreFromBlob({ v: 1, ws: "w-1", origin: "win-0" }, "w-1", defaultLayout(), false);
    expect(r).toMatchObject({ detached: false, origin: null });
  });
});

describe("pruneDeadFiles", () => {
  it("drops only definitively dead files, against the layout as it is when probes settle", async () => {
    let l = openFile(defaultLayout(), "/a");
    l = openFile(l, "/b");
    l = openFile(l, "/c");
    let current = l;
    const p = pruneDeadFiles(
      () => current,
      (next) => (current = next),
      async (path) => (path === "/a" ? "dead" : path === "/b" ? "unknown" : "ok"),
    );
    // A change while the probes are in flight survives the prune.
    current = openSession(current, "s9");
    await p;
    const tabs = current.root.type === "pane" ? current.root.tabs : [];
    expect(tabs.map((t) => (t.surface === "file" ? t.path : t.surface))).toEqual(["/b", "/c", "terminal"]);
  });

  it("never sets when nothing is dead or there are no files", async () => {
    const set = vi.fn();
    await pruneDeadFiles(() => defaultLayout(), set, async () => "dead");
    await pruneDeadFiles(() => openFile(defaultLayout(), "/a"), set, async () => "unknown");
    expect(set).not.toHaveBeenCalled();
  });
});

describe("createViewPersistence", () => {
  let state: {
    ws: string | null;
    root: string | null;
    layout: Layout;
    ready: boolean;
    detached: boolean;
    origin: string | null;
  };
  let log: string[];
  let host: ViewPersistenceHost;

  beforeEach(() => {
    vs.saves.length = 0;
    vs.flushes = 0;
    vs.store.clear();
    vs.loads.length = 0;
    setDetachedWindow(false);
    log = [];
    state = { ws: "w-1", root: "/ws", layout: sample(), ready: false, detached: false, origin: null };
    host = {
      winKey: "win",
      getWs: () => state.ws,
      getWsRoot: () => state.root,
      getLayout: () => state.layout,
      setLayout: (next) => {
        log.push(`layout(ready=${state.ready})`);
        state.layout = next;
      },
      isReady: () => state.ready,
      setReady: (r) => {
        log.push(`ready=${r}`);
        state.ready = r;
      },
      isDetached: () => state.detached,
      getOrigin: () => state.origin,
      setDetached: (d, o) => {
        log.push(`detached=${d}`);
        state.detached = d;
        state.origin = o;
      },
      onBooted: () => log.push("booted"),
    };
  });
  afterEach(() => setDetachedWindow(false));

  it("persist writes nothing until ready, then the window key and the ws mirror", () => {
    const p = createViewPersistence(host);
    p.persist();
    expect(vs.saves).toEqual([]);
    state.ready = true;
    p.persist();
    const blob = legacyEffectBlob("w-1", state.layout, "/ws", false, null);
    expect(vs.saves.map(([k, b]) => [k, JSON.stringify(b)])).toEqual([
      ["win_w-1", JSON.stringify(blob)],
      ["ws_w-1", JSON.stringify(blob)],
    ]);
  });

  it("a detached window never writes the ws mirror; Home writes only the bare key", () => {
    const p = createViewPersistence(host);
    state.ready = true;
    state.detached = true;
    state.origin = "win-0";
    p.persist();
    expect(vs.saves.map(([k]) => k)).toEqual(["win_w-1"]);
    expect(vs.saves[0][1]).toMatchObject({ dt: 1, origin: "win-0" });
    vs.saves.length = 0;
    state.detached = false;
    state.ws = null;
    p.persist();
    expect(vs.saves.map(([k]) => k)).toEqual(["win"]);
  });

  it("boot assigns the restored layout BEFORE ready flips, then runs the boot hook", async () => {
    const stored = sample();
    vs.store.set("win_w-1", buildViewBlob({ ws: "w-1", layout: stored, wsRoot: null, detached: false, origin: null }));
    state.layout = defaultLayout();
    await createViewPersistence(host).boot();
    expect(log).toEqual(["layout(ready=false)", "detached=false", "ready=true", "booted"]);
    expect(serializeLayout(state.layout)).toEqual(serializeLayout(stored));
  });

  it("boot with nothing stored keeps the layout and still flips ready", async () => {
    const cur = state.layout;
    await createViewPersistence(host).boot();
    expect(state.layout).toBe(cur);
    expect(log).toEqual(["ready=true", "booted"]);
    expect(vs.loads).toEqual(["win_w-1", "win", "ws_w-1"]);
  });

  it("a blob-only dt:1 refreshes the detached hint", async () => {
    vs.store.set("win_w-1", { v: 1, ws: "w-1", layout: null, dt: 1, origin: "win-0" });
    await createViewPersistence(host).boot();
    expect(state).toMatchObject({ detached: true, origin: "win-0", ready: true });
    expect(state.layout.focusMode).toBe(true);
    expect(isDetachedWindow()).toBe(true);
  });

  it("a superseded boot drops its result", async () => {
    let release!: (v: unknown) => void;
    vs.store.set("win_w-1", new Promise((r) => (release = r)));
    const p = createViewPersistence(host);
    const first = p.boot();
    state.ws = "w-2";
    const second = p.boot();
    release(null);
    await Promise.all([first, second]);
    expect(log.filter((e) => e === "booted").length).toBe(1);
  });

  it("leaveWorkspace strips a detached window's dt, saves the leaving blob, then flushes", () => {
    state.detached = true;
    state.origin = "win-0";
    setDetachedWindow(true);
    const l = state.layout;
    createViewPersistence(host).leaveWorkspace();
    expect(state).toMatchObject({ detached: false, origin: null });
    expect(isDetachedWindow()).toBe(false);
    expect(vs.saves).toEqual([["win_w-1", leavingViewBlob("w-1", l)]]);
    expect(vs.flushes).toBe(1);
  });

  it("leaveWorkspace on an attached window only flushes", () => {
    createViewPersistence(host).leaveWorkspace();
    expect(vs.saves).toEqual([]);
    expect(vs.flushes).toBe(1);
  });

  it("resetForBoot drops ready BEFORE installing the default layout", () => {
    state.ready = true;
    createViewPersistence(host).resetForBoot();
    expect(log).toEqual(["ready=false", "layout(ready=false)"]);
    expect(serializeLayout(state.layout)).toMatchObject({ root: { tabs: [] } });
  });
});
