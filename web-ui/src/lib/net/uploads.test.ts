import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

class MemoryStorage {
  private values = new Map<string, string>();

  getItem(key: string): string | null {
    return this.values.get(key) ?? null;
  }

  setItem(key: string, value: string): void {
    this.values.set(key, value);
  }

  removeItem(key: string): void {
    this.values.delete(key);
  }
}

/** The daemon's upload routes, just enough of them: `fetch` as the page
 *  calls it. The transport's own behavior is `chunkedUpload.test.ts`. */
class FakeDaemon {
  requests: { method: string; url: string }[] = [];
  private uploads = new Map<string, { name: string; size: number; offset: number; dest: string }>();
  /** Requests answered with a network error before the daemon sees them. */
  dropNext = 0;

  fetch = async (input: RequestInfo | URL, init: RequestInit = {}): Promise<Response> => {
    const url = new URL(String(input), "http://127.0.0.1");
    const method = init.method ?? "GET";
    this.requests.push({ method, url: url.pathname + url.search });
    if (init.signal?.aborted) throw new DOMException("aborted", "AbortError");
    if (this.dropNext > 0) {
      this.dropNext -= 1;
      throw new TypeError("network error");
    }
    const json = (status: number, body: unknown) =>
      new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
    const parts = url.pathname.replace("/api/v1/", "").split("/");
    if (parts.length === 1) {
      const req = JSON.parse(init.body as string) as { name: string; size: number; dir?: string; session?: string };
      const id = `up${this.uploads.size + 1}`;
      const dest = req.dir ?? `/uploads/${req.session}`;
      this.uploads.set(id, { name: req.name, size: req.size, offset: 0, dest });
      return json(200, { id, offset: 0, chunk_max: 16 * 1024 * 1024 });
    }
    const upload = this.uploads.get(parts[1]);
    if (method === "DELETE") {
      this.uploads.delete(parts[1]);
      return new Response(null, { status: 204 });
    }
    if (upload === undefined) return json(404, { error: "unknown upload" });
    if (method === "PUT") {
      upload.offset += (init.body as ArrayBuffer).byteLength;
      return json(200, { offset: upload.offset });
    }
    if (method === "GET") return json(200, { offset: upload.offset, size: upload.size, done: false });
    return json(200, { path: `${upload.dest}/${upload.name}`, name: upload.name, size: upload.size });
  };
}

describe("uploads", () => {
  let daemon: FakeDaemon;

  beforeAll(() => {
    vi.stubGlobal("location", { hash: "", pathname: "/", search: "" });
    vi.stubGlobal("sessionStorage", new MemoryStorage());
    vi.stubGlobal("history", { replaceState: vi.fn() });
  });

  beforeEach(() => {
    daemon = new FakeDaemon();
    vi.stubGlobal("fetch", daemon.fetch);
  });

  afterAll(() => vi.unstubAllGlobals());

  it("uploads into a folder and completes with the host result", async () => {
    const { uploadToDir } = await import("./uploads");
    const progress: number[] = [];
    let cancel: (() => void) | null = null;
    const result = await uploadToDir("/remote/data", new Blob(["0123456789"]), "data.bin", {
      onProgress: (value) => progress.push(value),
      onCancelReady: (fn) => (cancel = fn),
    });

    expect(result).toEqual({ path: "/remote/data/data.bin", name: "data.bin", size: 10 });
    expect(cancel).toBeTypeOf("function");
    expect(daemon.requests.map((r) => `${r.method} ${r.url}`)).toEqual([
      "POST /api/v1/uploads",
      "PUT /api/v1/uploads/up1?offset=0",
      "POST /api/v1/uploads/up1/finish",
    ]);
    expect(progress).toEqual([0, 0.99, 1]);
  });

  it("uploads to a session's landing pad", async () => {
    const { uploadToSession } = await import("./uploads");
    const result = await uploadToSession("session-1", new Blob(["x"]), "x.txt");
    expect(result.path).toBe("/uploads/session-1/x.txt");
  });

  it("cancels: the caller gets `upload cancelled` and the host is told to drop its copy", async () => {
    const { uploadToSession } = await import("./uploads");
    let cancel: (() => void) | null = null;
    const pending = uploadToSession("session-1", new Blob(["x"]), "x.txt", {
      onCancelReady: (fn) => (cancel = fn),
    });
    expect(cancel).toBeTypeOf("function");
    cancel!();
    await expect(pending).rejects.toThrow("upload cancelled");
  });

  it("shows a job's chip waiting on the connection, then clears it when the upload lands", async () => {
    vi.useFakeTimers();
    try {
      const { trackFileOp, uploadJobs, uploadToDir } = await import("./uploads");
      const names: string[] = [];
      const stop = uploadJobs.subscribe((jobs) => {
        const name = jobs[0]?.name;
        if (name !== undefined && names.at(-1) !== name) names.push(name);
      });
      daemon.dropNext = 1;
      const pending = trackFileOp("Uploading data.bin…", (progress) =>
        uploadToDir("/remote", new Blob(["0123456789"]), "data.bin", progress),
      );
      await vi.runAllTimersAsync();
      await expect(pending).resolves.toMatchObject({ name: "data.bin" });
      stop();
      expect(names).toEqual([
        "Uploading data.bin…",
        "Uploading data.bin… waiting for the connection",
        "Uploading data.bin…",
      ]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("a failed upload inserts nothing and says why on its chip", async () => {
    const { setUploadPathInserter, uploadAndInsert, uploadJobs } = await import("./uploads");
    const inserted: string[] = [];
    setUploadPathInserter((_session, path) => inserted.push(path));
    vi.stubGlobal("fetch", async () =>
      new Response(JSON.stringify({ error: "big.zip is 3.2 GB; a file dropped on a session can be at most 64 MB" }), {
        status: 413,
      }),
    );
    let error: string | null = null;
    const stop = uploadJobs.subscribe((jobs) => {
      error = jobs.find((job) => job.error !== null)?.error ?? error;
    });
    await uploadAndInsert("session-1", new Blob(["x"]), "big.zip");
    stop();
    setUploadPathInserter(null);
    expect(inserted).toEqual([]);
    expect(error).toBe("big.zip is 3.2 GB; a file dropped on a session can be at most 64 MB");
  });
});
