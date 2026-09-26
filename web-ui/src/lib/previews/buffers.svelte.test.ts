import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { EditorState, StateEffect, type TransactionSpec } from "@codemirror/state";
import { EditorView, type ViewUpdate } from "@codemirror/view";

const mocks = vi.hoisted(() => ({
  fsFile: vi.fn(),
  fsWrite: vi.fn(),
  settings: { "editor.autosave": "off", "editor.autosaveDelay": 1000 } as Record<string, unknown>,
  drafts: {
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
import { bufferFor, openBuffer, releaseBuffer, type Buffer } from "./buffers.svelte";
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
    expect(mocks.drafts.clear).toHaveBeenCalledWith(path);
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
    expect(mocks.drafts.clear).toHaveBeenCalledWith(path);
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
});
