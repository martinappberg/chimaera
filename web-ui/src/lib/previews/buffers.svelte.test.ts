import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { EditorState, StateEffect, type TransactionSpec } from "@codemirror/state";
import { EditorView, type ViewUpdate } from "@codemirror/view";

const mocks = vi.hoisted(() => ({
  fsFile: vi.fn(),
  fsWrite: vi.fn(),
  settings: { "editor.autosave": "off", "editor.autosaveDelay": 1000 } as Record<string, unknown>,
  drafts: {
    WRITER: "this-window",
    journal: vi.fn(async () => ({ local: true, remote: "ok" })),
    journaled: (r: { local: boolean; remote: string }) => r.local || r.remote === "ok",
    clear: vi.fn(async () => {}),
    find: vi.fn(async () => null),
  },
}));

vi.mock("./files", async (orig) => ({
  ...(await orig<typeof import("./files")>()),
  fsFile: mocks.fsFile,
  fsWrite: mocks.fsWrite,
}));
vi.mock("./drafts", () => mocks.drafts);
vi.mock("../settings/store.svelte", () => ({
  getSetting: (id: string) => mocks.settings[id],
}));

import { FileConflictError, type FileChunk, type WriteResult } from "./files";
import {
  bufferFor,
  openBuffer,
  Presence,
  PRESENCE_HIDDEN_TTL_MS,
  PRESENCE_TTL_MS,
  releaseBuffer,
  type Buffer,
  type PresenceBus,
  type PresenceMsg,
} from "./buffers.svelte";
import { retain, release } from "./fileStore.svelte";
import { dirtyFiles } from "../shared/editing";
import { lastFsMutation } from "../workspace/fsEvents";

const enc = (s: string) => new TextEncoder().encode(s);
const dec = (b: Uint8Array) => new TextDecoder().decode(b);

function chunk(text: string | Uint8Array, hash: string | null, mtime: string): FileChunk {
  const bytes = typeof text === "string" ? enc(text) : text;
  return { bytes, size: bytes.length, truncated: false, mtime, hash };
}

/**
 * A DOM-free stand-in for EditorView: applies transactions and notifies the
 * state's update listeners the way a real view does, so "the user typed"
 * reaches the buffer through the same path as in the browser.
 */
class FakeView {
  state: EditorState;
  constructor(state: EditorState) {
    this.state = state;
  }
  dispatch(spec: TransactionSpec): void {
    const tr = this.state.update(spec);
    this.state = tr.state;
    const u = { view: this, docChanged: tr.docChanged, transactions: [tr], state: tr.state };
    for (const l of this.state.facet(EditorView.updateListener)) l(u as unknown as ViewUpdate);
  }
  scrollSnapshot() {
    return StateEffect.define<null>().of(null);
  }
  type(at: number, text: string): void {
    this.dispatch({ changes: { from: at, insert: text } });
  }
  get text(): string {
    return this.state.doc.toString();
  }
}

let seq = 0;
function fresh(path: string, text: string, hash = "h0", mtime = "m0") {
  const buf = openBuffer(path, chunk(text, hash, mtime));
  const view = new FakeView(buf.stateFor([]));
  buf.attach(view as unknown as EditorView, () => {});
  return { buf, view };
}

function close(buf: Buffer, view: FakeView): void {
  buf.detach(view as unknown as EditorView);
  releaseBuffer(buf);
}

/** Resolve the next fsWrite with a controllable promise. */
function deferredWrite() {
  let resolve!: (r: WriteResult) => void;
  let reject!: (e: unknown) => void;
  mocks.fsWrite.mockImplementationOnce(
    () =>
      new Promise<WriteResult>((res, rej) => {
        resolve = res;
        reject = rej;
      }),
  );
  return { resolve: (r: WriteResult) => resolve(r), reject: (e: unknown) => reject(e) };
}

/**
 * Move the path's store token, as the daemon's disk watch does. Vitest runs
 * Svelte's server build, where `$effect` never fires, so the buffer's watch
 * handler is called the way its effect would.
 */
function diskMoved(path: string, mtime: string): void {
  const e = retain(path);
  e.mtime = mtime;
  release(path);
  bufferFor(path)?.diskSignal();
}

beforeEach(() => {
  mocks.fsFile.mockReset();
  mocks.fsWrite.mockReset();
  mocks.drafts.clear.mockClear();
  mocks.drafts.journal.mockClear();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("buffer lifetime", () => {
  it("keeps a dirty buffer (text, undo) across an unmount, and forgets a clean one", () => {
    const path = `/w/keep-${++seq}.md`;
    const a = fresh(path, "one\n");
    a.view.type(0, "zero ");
    expect(get(dirtyFiles).has(path)).toBe(true);
    close(a.buf, a.view);

    // Remount: the same buffer, the same text.
    const again = openBuffer(path, chunk("one\n", "h0", "m0"));
    expect(again).toBe(a.buf);
    const view = new FakeView(again.stateFor([]));
    again.attach(view as unknown as EditorView, () => {});
    expect(view.text).toBe("zero one\n");

    // Discard → clean → the last view's release forgets it.
    again.discard();
    expect(view.text).toBe("one\n");
    close(again, view);
    expect(bufferFor(path)).toBeUndefined();
    expect(get(dirtyFiles).has(path)).toBe(false);
  });

  it("hands the buffer back to a superseded view when the live one unmounts first", () => {
    const path = `/w/standby-${++seq}.md`;
    const buf = openBuffer(path, chunk("one\n", "h0", "m0"));
    const older = new FakeView(buf.stateFor([]));
    const events: string[] = [];
    const resumeOlder = () => {
      older.state = buf.stateFor([]);
      events.push("resumed");
      buf.attach(older as unknown as EditorView, () => events.push("superseded"), resumeOlder);
    };
    buf.attach(older as unknown as EditorView, () => events.push("superseded"), resumeOlder);

    // A second view of the path (a tab mid-move) takes over and is typed in.
    const newer = new FakeView(openBuffer(path, chunk("one\n", "h0", "m0")).stateFor([]));
    buf.attach(newer as unknown as EditorView, () => {}, () => {});
    expect(events).toEqual(["superseded"]);
    newer.type(0, "zero ");

    // It unmounts first: the older view resumes with the current text and
    // is live again (its keystrokes reach the buffer).
    close(buf, newer);
    expect(events).toEqual(["superseded", "resumed"]);
    expect(older.text).toBe("zero one\n");
    older.type(0, "! ");
    expect(buf.current.doc.toString()).toBe("! zero one\n");
    expect(buf.dirty).toBe(true);

    // A superseded view that unmounts is simply forgotten.
    const third = new FakeView(openBuffer(path, chunk("one\n", "h0", "m0")).stateFor([]));
    buf.attach(third as unknown as EditorView, () => {}, () => {});
    close(buf, older);
    close(buf, third);
    expect(events).toEqual(["superseded", "resumed", "superseded"]);
    buf.discard();
    expect(bufferFor(path)).toBeUndefined();
  });

  it("follows a rename of a parent folder", () => {
    const path = `/w/dir-${++seq}/notes.md`;
    const dir = path.slice(0, path.lastIndexOf("/"));
    const { buf, view } = fresh(path, "x\n");
    view.type(0, "y");
    lastFsMutation.set({ seq: 10_000 + seq, kind: "rename", from: dir, to: `${dir}-moved` });
    expect(buf.path).toBe(`${dir}-moved/notes.md`);
    expect(bufferFor(`${dir}-moved/notes.md`)).toBe(buf);
    expect(get(dirtyFiles).has(path)).toBe(false);
    expect(get(dirtyFiles).has(`${dir}-moved/notes.md`)).toBe(true);
    buf.discard();
    close(buf, view);
  });
});

describe("saving", () => {
  it("keeps keys typed during a save dirty (save generations)", async () => {
    const path = `/w/gen-${++seq}.txt`;
    const { buf, view } = fresh(path, "a\n");
    view.type(0, "b");
    const w = deferredWrite();
    const saving = buf.save();
    await Promise.resolve();
    expect(mocks.fsWrite).toHaveBeenCalledWith(
      path,
      enc("ba\n"),
      expect.objectContaining({ expectHash: "h0" }),
    );
    view.type(0, "c"); // lands while the PUT is in flight
    w.resolve({ hash: "h1", mtime: "m1" });
    expect(await saving).toBe(true);
    expect(buf.dirty).toBe(true);
    expect(view.text).toBe("cba\n");

    mocks.fsWrite.mockResolvedValueOnce({ hash: "h2", mtime: "m2" });
    expect(await buf.save()).toBe(true);
    expect(mocks.fsWrite).toHaveBeenLastCalledWith(
      path,
      enc("cba\n"),
      expect.objectContaining({ expectHash: "h1" }),
    );
    expect(buf.dirty).toBe(false);
    expect(mocks.drafts.clear).toHaveBeenCalledWith(path, { writer: "this-window", text: "cba\n" });
    close(buf, view);
  });

  it("writes CRLF and the BOM back exactly as the file had them", async () => {
    const path = `/w/crlf-${++seq}.txt`;
    const bytes = new Uint8Array([0xef, 0xbb, 0xbf, ...enc("a\r\nb\r\n")]);
    const buf = openBuffer(path, chunk(bytes, "h0", "m0"));
    const view = new FakeView(buf.stateFor([]));
    buf.attach(view as unknown as EditorView, () => {});
    expect(view.text).toBe("a\nb\n");
    view.type(view.text.length, "c\n");
    mocks.fsWrite.mockResolvedValueOnce({ hash: "h1", mtime: "m1" });
    await buf.save();
    const sent = mocks.fsWrite.mock.calls[0][1] as Uint8Array;
    expect([...sent.slice(0, 3)]).toEqual([0xef, 0xbb, 0xbf]);
    expect(dec(sent.slice(3))).toBe("a\r\nb\r\nc\r\n");
    close(buf, view);
  });

  it("opens mixed line endings and invalid UTF-8 read-only", () => {
    const mixed = openBuffer(`/w/mixed-${++seq}.txt`, chunk("a\r\nb\n", "h", "m"));
    expect(mixed.editable).toBe(false);
    expect(mixed.note).toMatch(/mixed line endings/);
    releaseBuffer(mixed);
    const latin = openBuffer(`/w/latin-${++seq}.txt`, chunk(new Uint8Array([0x63, 0xe9, 0x0a]), "h", "m"));
    expect(latin.editable).toBe(false);
    expect(latin.note).toMatch(/UTF-8/);
    releaseBuffer(latin);
    const gz = openBuffer(`/w/data-${++seq}.txt.gz`, chunk("plain\n", null, "m"));
    expect(gz.editable).toBe(false);
    releaseBuffer(gz);
  });

  it("retries once across a lost reply and treats the landed write as saved", async () => {
    vi.useFakeTimers();
    const path = `/w/retry-${++seq}.txt`;
    const { buf, view } = fresh(path, "a\n");
    view.type(0, "x");
    mocks.fsWrite.mockRejectedValueOnce(new TypeError("Failed to fetch"));
    // An older daemon answers the retry of a write that DID land with 409.
    mocks.fsWrite.mockRejectedValueOnce(new FileConflictError());
    mocks.fsFile.mockResolvedValueOnce(chunk("xa\n", null, "m1"));
    const saving = buf.save();
    await vi.advanceTimersByTimeAsync(0);
    expect(buf.saveState).toBe("retrying");
    await vi.advanceTimersByTimeAsync(2_000);
    expect(await saving).toBe(true);
    expect(mocks.fsWrite).toHaveBeenCalledTimes(2);
    expect(buf.dirty).toBe(false);
    expect(buf.conflict).toBeNull();
    close(buf, view);
  });

  it("merges on a 409 when the disk changed elsewhere, then saves against the disk's hash", async () => {
    const path = `/w/merge409-${++seq}.md`;
    const { buf, view } = fresh(path, "# A\none\n\n# B\ntwo\n");
    view.type(view.text.indexOf("one"), "my ");
    mocks.fsWrite.mockRejectedValueOnce(new FileConflictError());
    mocks.fsFile.mockResolvedValueOnce(chunk("# A\none\n\n# B\ntwo, by the agent\n", "hD", "mD"));
    expect(await buf.save()).toBe(false);
    expect(view.text).toBe("# A\nmy one\n\n# B\ntwo, by the agent\n");
    expect(buf.dirty).toBe(true);
    expect(buf.notice).not.toBeNull();
    mocks.fsWrite.mockResolvedValueOnce({ hash: "h9", mtime: "m9" });
    expect(await buf.save()).toBe(true);
    expect(mocks.fsWrite).toHaveBeenLastCalledWith(
      path,
      enc("# A\nmy one\n\n# B\ntwo, by the agent\n"),
      expect.objectContaining({ expectHash: "hD" }),
    );
    close(buf, view);
  });
});

describe("disk changes", () => {
  it("reloads a clean buffer in place", async () => {
    const path = `/w/clean-${++seq}.txt`;
    const { buf, view } = fresh(path, "a\nb\n");
    mocks.fsFile.mockResolvedValueOnce(chunk("a\nb\nc\n", "h1", "m1"));
    diskMoved(path, "m1");
    await vi.waitFor(() => expect(view.text).toBe("a\nb\nc\n"));
    expect(buf.dirty).toBe(false);
    close(buf, view);
  });

  it("never clobbers a key typed during the reload fetch (merges instead)", async () => {
    const path = `/w/race-${++seq}.md`;
    const { buf, view } = fresh(path, "top\n\nmiddle\n\nbottom\n");
    let respond!: (c: FileChunk) => void;
    mocks.fsFile.mockImplementationOnce(() => new Promise<FileChunk>((r) => (respond = r)));
    diskMoved(path, "m1");
    await vi.waitFor(() => expect(mocks.fsFile).toHaveBeenCalled());
    view.type(0, "my "); // lands while the disk read is in flight
    respond(chunk("top\n\nmiddle\n\nbottom, edited\n", "h1", "m1"));
    await vi.waitFor(() => expect(view.text).toBe("my top\n\nmiddle\n\nbottom, edited\n"));
    expect(buf.dirty).toBe(true);
    expect(buf.conflict).toBeNull();
    close(buf, view);
  });

  it("raises a conflict for overlapping edits, refuses a plain save, and resolves either way", async () => {
    const path = `/w/conflict-${++seq}.txt`;
    const { buf, view } = fresh(path, "line\n");
    view.type(4, " (mine)");
    mocks.fsFile.mockResolvedValueOnce(chunk("line (theirs)\n", "hT", "mT"));
    diskMoved(path, "mT");
    await vi.waitFor(() => expect(buf.conflict?.kind).toBe("changed"));
    expect(view.text).toBe("line (mine)\n");

    expect(await buf.save()).toBe(false);
    expect(mocks.fsWrite).not.toHaveBeenCalled();
    expect(buf.conflictNudge).toBe(1);

    buf.keepMine();
    mocks.fsWrite.mockResolvedValueOnce({ hash: "h2", mtime: "m2" });
    expect(await buf.save()).toBe(true);
    expect(mocks.fsWrite).toHaveBeenCalledWith(
      path,
      enc("line (mine)\n"),
      expect.objectContaining({ expectHash: "hT" }),
    );

    view.type(0, "x");
    mocks.fsFile.mockResolvedValueOnce(chunk("LINE\n", "hU", "mU"));
    diskMoved(path, "mU");
    await vi.waitFor(() => expect(buf.conflict?.kind).toBe("changed"));
    buf.takeDiskVersion();
    expect(view.text).toBe("LINE\n");
    expect(buf.dirty).toBe(false);
    close(buf, view);
  });

  it("ignores a disk read that started before our own save landed", async () => {
    const path = `/w/stale-${++seq}.txt`;
    const { buf, view } = fresh(path, "a\n");
    let respond!: (c: FileChunk) => void;
    mocks.fsFile.mockImplementationOnce(() => new Promise<FileChunk>((r) => (respond = r)));
    diskMoved(path, "m-touch");
    await vi.waitFor(() => expect(mocks.fsFile).toHaveBeenCalled());
    view.type(0, "b");
    mocks.fsWrite.mockResolvedValueOnce({ hash: "h1", mtime: "m1" });
    expect(await buf.save()).toBe(true);
    expect(buf.dirty).toBe(false);
    // The pre-save read comes back last; it must not revert the saved text.
    respond(chunk("a\n", "h0", "m-touch"));
    await new Promise((r) => setTimeout(r, 0));
    expect(view.text).toBe("ba\n");
    expect(buf.conflict).toBeNull();
    close(buf, view);
  });

  it("recognizes its own write when the fs event beats the PUT reply", async () => {
    const path = `/w/own-${++seq}.txt`;
    const { buf, view } = fresh(path, "a\n");
    view.type(0, "b");
    const w = deferredWrite();
    const saving = buf.save();
    await Promise.resolve();
    // The daemon's fs frame for our write arrives while the save is pending.
    mocks.fsFile.mockResolvedValue(chunk("ba\n", "h1", "m1"));
    diskMoved(path, "m1");
    w.resolve({ hash: "h1", mtime: "m1" });
    expect(await saving).toBe(true);
    await new Promise((r) => setTimeout(r, 0));
    expect(buf.dirty).toBe(false);
    expect(buf.conflict).toBeNull();
    expect(buf.notice).toBeNull();
    expect(view.text).toBe("ba\n");
    mocks.fsFile.mockReset();
    close(buf, view);
  });

  it("adopts a metadata-only change without touching the text", async () => {
    const path = `/w/touch-${++seq}.txt`;
    const { buf, view } = fresh(path, "same\n");
    view.type(0, "dirty ");
    mocks.fsFile.mockResolvedValueOnce(chunk("same\n", "h0", "m-touched"));
    diskMoved(path, "m-touched");
    await vi.waitFor(() => expect(mocks.fsFile).toHaveBeenCalled());
    await Promise.resolve();
    expect(view.text).toBe("dirty same\n");
    expect(buf.conflict).toBeNull();
    expect(buf.notice).toBeNull();
    buf.discard();
    close(buf, view);
  });
});

describe("the journal", () => {
  it("never marks text journaled when its write lands after a save cleared the draft", async () => {
    const path = `/w/late-journal-${++seq}.txt`;
    const { buf, view } = fresh(path, "x");
    view.type(1, "a"); // "xa"
    type Result = { local: boolean; remote: "ok" };
    let land!: (r: Result) => void;
    mocks.drafts.journal.mockImplementationOnce(() => new Promise<Result>((r) => (land = r)));
    const journaling = buf.journal(); // "xa", still in flight
    view.type(2, "b"); // "xab"
    mocks.fsWrite.mockResolvedValueOnce({ hash: "h1", mtime: "m1" });
    expect(await buf.save()).toBe(true); // clean: the draft is cleared
    // This window's record, or anyone's holding exactly the saved text.
    expect(mocks.drafts.clear).toHaveBeenCalledWith(path, { writer: "this-window", text: "xab" });
    land({ local: true, remote: "ok" }); // the stale write completes last
    await journaling;

    // Back to exactly the text that write carried: it is unsaved again and
    // must be journaled, not skipped as "already journaled".
    view.dispatch({ changes: { from: 2, to: 3 } });
    expect(buf.dirty).toBe(true);
    mocks.drafts.journal.mockClear();
    await buf.journal();
    expect(mocks.drafts.journal).toHaveBeenCalledTimes(1);
    expect(mocks.drafts.journal).toHaveBeenCalledWith(expect.objectContaining({ path, text: "xa" }), false);
    buf.discard();
    close(buf, view);
  });

  it("re-journals unchanged text on the hide/pagehide flush (another window may have overwritten it)", async () => {
    const path = `/w/flush-${++seq}.txt`;
    const { buf, view } = fresh(path, "x");
    view.type(1, "y");
    await buf.journal();
    await buf.journal(); // unchanged: the memo skips it
    expect(mocks.drafts.journal).toHaveBeenCalledTimes(1);
    await buf.journal(true); // the flush always writes
    expect(mocks.drafts.journal).toHaveBeenCalledTimes(2);
    expect(mocks.drafts.journal).toHaveBeenLastCalledWith(expect.objectContaining({ text: "xy" }), true);
    buf.discard();
    // A discard drops only this window's record.
    expect(mocks.drafts.clear).toHaveBeenLastCalledWith(path, { writer: "this-window" });
    close(buf, view);
  });

  it("clears a draft found on open by its writer or its text, not as this window's", async () => {
    const path = `/w/found-${++seq}.txt`;
    const found = { path, baseHash: "h0", baseText: null, text: "theirs", updatedMs: 1, writer: "dead-window" };
    mocks.drafts.find.mockImplementationOnce(async () => found as never);
    const { buf, view } = fresh(path, "x");
    await vi.waitFor(() => expect(buf.recovered).not.toBeNull());
    buf.discardDraft();
    expect(mocks.drafts.clear).toHaveBeenLastCalledWith(path, { writer: "dead-window", text: "theirs" });
    close(buf, view);
  });
});

describe("presence across windows of this origin", () => {
  /** Windows on one fake BroadcastChannel; a window can crash (go silent). */
  function bus() {
    const ends = new Set<PresenceBus>();
    const open = (): PresenceBus & { crash(): void } => {
      const end: PresenceBus & { crash(): void } = {
        onmessage: null,
        postMessage(msg: PresenceMsg) {
          if (!ends.has(end)) return;
          for (const other of ends) {
            if (other !== end) other.onmessage?.({ data: structuredClone(msg) } as MessageEvent<PresenceMsg>);
          }
        },
        crash: () => void ends.delete(end),
      };
      ends.add(end);
      return end;
    };
    return open;
  }

  function window_(open: () => PresenceBus & { crash(): void }, dirty: string[] = [], hidden = false) {
    const state = { dirty, hidden };
    const end = open();
    const p = new Presence(end, () => state.dirty, () => state.hidden);
    return { p, end, state };
  }

  afterEach(() => {
    vi.useRealTimers();
  });

  it("forgets a window that died without saying goodbye", () => {
    vi.useFakeTimers();
    const open = bus();
    const b = window_(open);
    const a = window_(open, ["/w/p.md"]);
    a.p.announce("/w/p.md", true);
    expect(b.p.elsewhere.has("/w/p.md")).toBe(true);

    // Alive, its heartbeat keeps it held well past the TTL.
    vi.advanceTimersByTime(PRESENCE_TTL_MS * 3);
    expect(b.p.heldElsewhere("/w/p.md")).toBe(true);

    // Crashed: silent, so gone once the TTL runs out (the peer's own timer).
    a.end.crash();
    vi.advanceTimersByTime(PRESENCE_TTL_MS + 1_000);
    expect(b.p.elsewhere.has("/w/p.md")).toBe(false);
    expect(b.p.heldElsewhere("/w/p.md")).toBe(false);
    a.p.leave();
    b.p.leave();
  });

  it("gives a hidden window longer, and beats only while it holds unsaved edits", () => {
    vi.useFakeTimers();
    const open = bus();
    const b = window_(open);
    const a = window_(open, ["/w/q.md"]);
    a.p.announce("/w/q.md", true);
    a.state.hidden = true;
    a.p.visibilityChanged();
    a.end.crash(); // e.g. frozen or killed while hidden
    vi.advanceTimersByTime(PRESENCE_TTL_MS * 2);
    expect(b.p.heldElsewhere("/w/q.md")).toBe(true);
    vi.advanceTimersByTime(PRESENCE_HIDDEN_TTL_MS);
    expect(b.p.heldElsewhere("/w/q.md")).toBe(false);
    a.p.leave();

    // A window with nothing unsaved leaves no timer behind.
    const c = window_(open, ["/w/r.md"]);
    c.p.announce("/w/r.md", true);
    c.state.dirty = [];
    c.p.announce("/w/r.md", false);
    vi.advanceTimersByTime(PRESENCE_TTL_MS * 2);
    b.p.leave();
    c.p.leave();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("drops a window at once on its goodbye, and answers a newcomer's hello", () => {
    const open = bus();
    const a = window_(open, ["/w/s.md"]);
    a.p.announce("/w/s.md", true);
    const late = window_(open); // says hello; `a` answers with its state
    expect(late.p.elsewhere.has("/w/s.md")).toBe(true);
    a.p.leave();
    expect(late.p.elsewhere.has("/w/s.md")).toBe(false);
    late.p.leave();
  });
});
