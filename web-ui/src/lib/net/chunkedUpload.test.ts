import { describe, expect, it } from "vitest";
import {
  CHUNK_MAX,
  CHUNK_MIN,
  CHUNK_START,
  CHUNK_TARGET_MS,
  STALL_MS,
  UploadCancelled,
  humanBytes,
  nextChunkSize,
  uploadChunked,
  type UploadIo,
} from "./chunkedUpload";

type Bytes = Uint8Array<ArrayBuffer>;

const sha256 = async (bytes: Uint8Array): Promise<Bytes> =>
  new Uint8Array(await crypto.subtle.digest("SHA-256", bytes as BufferSource));
const hex = (bytes: Uint8Array): string => Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
/** Hex SHA-256: what a test compares a landed file with its source by. */
const sum = async (bytes: Uint8Array): Promise<string> => hex(await sha256(bytes));

/** Deterministic bytes that differ everywhere a slicing bug would show. */
function pattern(size: number): Bytes {
  const out: Bytes = new Uint8Array(new ArrayBuffer(size));
  let x = 0x9e3779b9;
  for (let i = 0; i < size; i += 1) {
    x = (Math.imul(x, 1664525) + 1013904223) >>> 0;
    out[i] = x >>> 24;
  }
  return out;
}

interface Seen {
  method: string;
  /** begin | chunk | status | finish | cancel */
  kind: string;
  offset?: number;
  bytes?: number;
  /** 1-based count of requests of this kind. */
  nth: number;
}

/** What the link does to one request: nothing, lose it before the daemon
 *  sees it, lose the daemon's answer, or answer with a status of its own. */
type Fault = undefined | "lost" | "answer-lost" | number;

/** The daemon's side of the protocol, in memory, behind a link that can
 *  fail on demand. Mirrors `upload::chunked`: offsets, checksums, chain. */
class FakeHost {
  uploads = new Map<
    string,
    { size: number; name: string; data: Bytes; offset: number; chain: Bytes; done: boolean }
  >();
  seen: Seen[] = [];
  landed: Bytes | null = null;
  fault: (req: Seen) => Fault = () => undefined;
  /** Refuse `begin` with this, when set. */
  refuseBegin: { status: number; error: string } | null = null;
  /** Bytes per second and round-trip latency of the pretend link. */
  rate = 1_000_000;
  rttMs = 40;
  clock = 0;
  inFlight = 0;
  maxInFlight = 0;
  private counts = new Map<string, number>();
  private ids = 0;

  io(hash: boolean = true): UploadIo {
    return {
      now: () => this.clock,
      sleep: async (ms) => {
        this.clock += ms;
      },
      sha256: async (bytes) => (hash ? sha256(new Uint8Array(bytes as ArrayBuffer)) : null),
      request: async (path, init, _timeoutMs, signal) => {
        if (signal?.aborted) throw new DOMException("aborted", "AbortError");
        this.inFlight += 1;
        this.maxInFlight = Math.max(this.maxInFlight, this.inFlight);
        try {
          return await this.handle(path, init);
        } finally {
          this.inFlight -= 1;
        }
      },
    };
  }

  private json(status: number, body: unknown): Response {
    return new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
  }

  private async handle(path: string, init: RequestInit): Promise<Response> {
    const method = init.method ?? "GET";
    const url = new URL(path, "http://host");
    const parts = url.pathname.split("/").filter(Boolean); // uploads, id?, finish?
    const id = parts[1];
    const kind =
      parts.length === 1
        ? "begin"
        : parts[2] === "finish"
          ? "finish"
          : method === "PUT"
            ? "chunk"
            : method === "DELETE"
              ? "cancel"
              : "status";
    const body = kind === "chunk" ? new Uint8Array(init.body as ArrayBuffer) : null;
    const nth = (this.counts.get(kind) ?? 0) + 1;
    this.counts.set(kind, nth);
    const req: Seen = { method, kind, nth };
    if (kind === "chunk") {
      req.offset = Number(url.searchParams.get("offset"));
      req.bytes = body!.length;
    }
    this.seen.push(req);
    this.clock += this.rttMs + (body === null ? 0 : (body.length / this.rate) * 1000);

    const fault = this.fault(req);
    if (fault === "lost") throw new TypeError("network error");
    if (typeof fault === "number") return this.json(fault, { error: `status ${fault}` });

    const answer = await this.serve(kind, id, init, url, body);
    if (fault === "answer-lost") throw new TypeError("network error");
    return answer;
  }

  private async serve(
    kind: string,
    id: string,
    init: RequestInit,
    url: URL,
    body: Uint8Array | null,
  ): Promise<Response> {
    if (kind === "begin") {
      if (this.refuseBegin !== null) return this.json(this.refuseBegin.status, { error: this.refuseBegin.error });
      const req = JSON.parse(init.body as string) as { name: string; size: number };
      const newId = `up${++this.ids}`;
      this.uploads.set(newId, {
        size: req.size,
        name: req.name,
        data: new Uint8Array(new ArrayBuffer(req.size)),
        offset: 0,
        chain: new Uint8Array(32),
        done: false,
      });
      return this.json(200, { id: newId, offset: 0, chunk_max: 16 * 1024 * 1024 });
    }
    const upload = this.uploads.get(id);
    if (kind === "cancel") {
      this.uploads.delete(id);
      return new Response(null, { status: 204 });
    }
    if (upload === undefined) return this.json(404, { error: "the host no longer has this upload" });
    if (kind === "status") return this.json(200, { offset: upload.offset, size: upload.size, done: upload.done });
    if (kind === "chunk") {
      const offset = Number(url.searchParams.get("offset"));
      if (offset !== upload.offset) {
        return this.json(409, { error: "the upload is not at that offset", offset: upload.offset });
      }
      const digest = await sha256(body!);
      const claimed = new Headers(init.headers).get("x-chunk-sha256");
      if (claimed !== null && claimed !== hex(digest)) return this.json(422, { error: "arrived changed" });
      upload.data.set(body!, offset);
      upload.offset += body!.length;
      upload.chain = await sha256(Uint8Array.from([...upload.chain, ...digest]));
      return this.json(200, { offset: upload.offset });
    }
    // finish
    if (upload.offset !== upload.size) return this.json(409, { error: "incomplete", offset: upload.offset });
    const { chain } = JSON.parse((init.body as string) || "{}") as { chain?: string };
    if (chain !== undefined && chain !== hex(upload.chain)) {
      return this.json(422, { error: "the upload did not verify" });
    }
    upload.done = true;
    this.landed = upload.data;
    return this.json(200, { path: `/remote/${upload.name}`, name: upload.name, size: upload.size });
  }

  chunks(): Seen[] {
    return this.seen.filter((req) => req.kind === "chunk");
  }
}

const SIZE = 3 * 1024 * 1024 + 4321;

describe("the chunked upload transport", () => {
  it("lands the whole file, one piece in flight at a time, with confirmed-byte progress", async () => {
    const host = new FakeHost();
    const source = pattern(SIZE);
    const progress: number[] = [];
    const result = await uploadChunked(
      { dir: "/remote" },
      new Blob([source]),
      "data.bin",
      { onProgress: (p) => progress.push(p) },
      host.io(),
    );

    expect(result).toEqual({ path: "/remote/data.bin", name: "data.bin", size: SIZE });
    expect(await sum(host.landed!)).toBe(await sum(source));
    expect(host.maxInFlight).toBe(1);
    expect(host.chunks().length).toBeGreaterThan(3);
    // With the answer in hand it tells the host to forget the upload.
    expect(host.seen.at(-1)!.kind).toBe("cancel");
    expect(host.uploads.size).toBe(0);
    // Each step is what the host confirmed: never ahead, never backwards.
    expect(progress[0]).toBe(0);
    expect(progress.at(-1)).toBe(1);
    expect([...progress].sort((a, b) => a - b)).toEqual(progress);
    expect(progress.slice(0, -1).every((p) => p <= 0.99)).toBe(true);
  });

  it("names its destination: a session's landing pad or a folder", async () => {
    const begun: unknown[] = [];
    for (const destination of [{ session: "s-1" }, { dir: "/remote/data" }]) {
      const host = new FakeHost();
      const io = host.io();
      const request = io.request;
      io.request = (path, init, timeout, signal) => {
        if (path === "/uploads") begun.push(JSON.parse(init.body as string));
        return request(path, init, timeout, signal);
      };
      await uploadChunked(destination, new Blob(["hello"]), "a.txt", {}, io);
    }
    expect(begun).toEqual([
      { session: "s-1", name: "a.txt", size: 5 },
      { dir: "/remote/data", name: "a.txt", size: 5 },
    ]);
  });

  it("sizes pieces to the link: about half a second each, so nothing queues behind an upload", async () => {
    for (const rate of [20_000, 250_000, 1_000_000, 50_000_000]) {
      const host = new FakeHost();
      host.rate = rate;
      const size = Math.min(64 * 1024 * 1024, rate * 30);
      await uploadChunked({ dir: "/remote" }, new Blob([new Uint8Array(new ArrayBuffer(size))]), "f", {}, host.io());

      const pieces = host.chunks().map((req) => req.bytes!);
      expect(pieces[0]).toBe(CHUNK_START);
      const durations = pieces.map((bytes) => (bytes / rate) * 1000 + host.rttMs);
      // No piece holds the link for long: the app's tunnel health check
      // gives up after 2 s.
      expect(Math.max(...durations)).toBeLessThan(1800);
      // Once it has measured the link, a piece takes the target time (or is
      // as big / as small as a piece may be).
      const settled = pieces[Math.floor(pieces.length / 2)];
      const ideal = (rate * (CHUNK_TARGET_MS - host.rttMs)) / 1000;
      const expected = Math.max(CHUNK_MIN, Math.min(CHUNK_MAX, ideal));
      expect(settled).toBeGreaterThan(expected * 0.8);
      expect(settled).toBeLessThan(expected * 1.2);
    }
  });

  it("resends a piece the link lost, from where the host stands", async () => {
    const host = new FakeHost();
    host.fault = (req) => (req.kind === "chunk" && (req.nth === 3 || req.nth === 4) ? "lost" : undefined);
    const source = pattern(SIZE);
    const waiting: boolean[] = [];
    await uploadChunked(
      { dir: "/remote" },
      new Blob([source]),
      "data.bin",
      { onWaiting: (w) => waiting.push(w) },
      host.io(),
    );

    expect(await sum(host.landed!)).toBe(await sum(source));
    const sent = host.chunks();
    expect(sent[2].offset).toBe(sent[3].offset);
    expect(sent[3].offset).toBe(sent[4].offset);
    // It asked before each resend instead of assuming.
    expect(host.seen.filter((req) => req.kind === "status")).toHaveLength(2);
    expect(waiting).toEqual([true, false]);
  });

  it("never applies a piece twice when only the answer was lost", async () => {
    const host = new FakeHost();
    host.fault = (req) => (req.kind === "chunk" && req.nth % 3 === 0 ? "answer-lost" : undefined);
    const source = pattern(SIZE);
    await uploadChunked({ dir: "/remote" }, new Blob([source]), "data.bin", {}, host.io());

    expect(await sum(host.landed!)).toBe(await sum(source));
    // The host had already committed those pieces, so none was sent again.
    const offsets = host.chunks().map((req) => req.offset);
    expect(new Set(offsets).size).toBe(offsets.length);
  });

  it("rides out a link that is down for a while, then goes on", async () => {
    const host = new FakeHost();
    let down = 0;
    host.fault = (req) => {
      // From the 4th piece on, the next 12 requests of any kind go nowhere.
      if (req.kind === "chunk" && req.nth === 4 && down === 0) down = 12;
      if (down > 0) {
        down -= 1;
        return down % 2 === 0 ? "lost" : 502;
      }
      return undefined;
    };
    const source = pattern(SIZE);
    await uploadChunked({ dir: "/remote" }, new Blob([source]), "data.bin", {}, host.io());
    expect(await sum(host.landed!)).toBe(await sum(source));
  });

  it("gives up with what happened when the link never comes back, and drops the host's partial copy", async () => {
    const host = new FakeHost();
    let dead = false;
    host.fault = (req) => {
      if (req.kind === "chunk" && req.nth === 5) dead = true;
      return dead && req.kind !== "cancel" ? "lost" : undefined;
    };
    const started = host.clock;
    await expect(
      uploadChunked({ dir: "/remote" }, new Blob([pattern(SIZE)]), "data.bin", {}, host.io()),
    ).rejects.toThrow(/^upload stopped at .+ of 3 MB: the connection to the host did not come back$/);

    expect(host.clock - started).toBeGreaterThanOrEqual(STALL_MS);
    expect(host.clock - started).toBeLessThan(STALL_MS * 2);
    expect(host.seen.at(-1)!.kind).toBe("cancel");
    expect(host.uploads.size).toBe(0);
    expect(host.landed).toBeNull();
  });

  it("sends the file again from the start when the host forgot the upload, once", async () => {
    const host = new FakeHost();
    let restarted = false;
    host.fault = (req) => {
      if (req.kind === "chunk" && req.nth === 4 && !restarted) {
        restarted = true;
        host.uploads.clear();
      }
      return undefined;
    };
    const source = pattern(SIZE);
    const progress: number[] = [];
    await uploadChunked(
      { dir: "/remote" },
      new Blob([source]),
      "data.bin",
      { onProgress: (p) => progress.push(p) },
      host.io(),
    );
    expect(await sum(host.landed!)).toBe(await sum(source));
    expect(host.seen.filter((req) => req.kind === "begin")).toHaveLength(2);
    // Progress says so: it goes back to the start with the bytes.
    expect(progress.filter((p) => p === 0)).toHaveLength(2);

    const twice = new FakeHost();
    twice.fault = (req) => {
      if (req.kind === "chunk" && req.nth % 3 === 0) twice.uploads.clear();
      return undefined;
    };
    await expect(
      uploadChunked({ dir: "/remote" }, new Blob([source]), "data.bin", {}, twice.io()),
    ).rejects.toThrow("the host lost it twice");
  });

  it("reports the host's refusal before sending a byte", async () => {
    const host = new FakeHost();
    host.refuseBegin = { status: 413, error: "big.zip is 3.2 GB; a file dropped on a session can be at most 64 MB" };
    await expect(
      uploadChunked({ session: "s" }, new Blob([pattern(1000)]), "big.zip", {}, host.io()),
    ).rejects.toThrow("big.zip is 3.2 GB; a file dropped on a session can be at most 64 MB");
    expect(host.chunks()).toHaveLength(0);
  });

  it("resends a piece that arrived changed, and fails if it keeps arriving changed", async () => {
    const host = new FakeHost();
    host.fault = (req) => (req.kind === "chunk" && req.nth === 2 ? 422 : undefined);
    const source = pattern(SIZE);
    await uploadChunked({ dir: "/remote" }, new Blob([source]), "data.bin", {}, host.io());
    expect(await sum(host.landed!)).toBe(await sum(source));

    const broken = new FakeHost();
    broken.fault = (req) => (req.kind === "chunk" && req.nth >= 2 ? 422 : undefined);
    await expect(
      uploadChunked({ dir: "/remote" }, new Blob([source]), "data.bin", {}, broken.io()),
    ).rejects.toThrow("status 422");
    expect(broken.landed).toBeNull();
    expect(broken.uploads.size).toBe(0);
  });

  it("fails rather than publish when the host's digest of the pieces differs", async () => {
    const host = new FakeHost();
    const io = host.io();
    const digest = io.sha256;
    // A client whose running digest goes wrong: the 64-byte chain step.
    io.sha256 = async (bytes) => {
      const out = await digest(bytes);
      if ((bytes as Uint8Array).byteLength === 64) out![0] ^= 1;
      return out;
    };
    await expect(
      uploadChunked({ dir: "/remote" }, new Blob([pattern(SIZE)]), "data.bin", {}, io),
    ).rejects.toThrow("the upload did not verify");
    expect(host.landed).toBeNull();
  });

  it("cancels: rejects as cancelled and tells the host to drop its copy", async () => {
    const host = new FakeHost();
    const controller = new AbortController();
    host.fault = (req) => {
      if (req.kind === "chunk" && req.nth === 3) controller.abort();
      return undefined;
    };
    await expect(
      uploadChunked(
        { dir: "/remote" },
        new Blob([pattern(SIZE)]),
        "data.bin",
        { signal: controller.signal },
        host.io(),
      ),
    ).rejects.toBeInstanceOf(UploadCancelled);
    expect(host.seen.at(-1)!.kind).toBe("cancel");
    expect(host.uploads.size).toBe(0);
  });

  it("uploads without checksums where the page cannot hash", async () => {
    const host = new FakeHost();
    const source = pattern(200_000);
    await uploadChunked({ dir: "/remote" }, new Blob([source]), "data.bin", {}, host.io(false));
    expect(await sum(host.landed!)).toBe(await sum(source));
  });

  it("uploads an empty file", async () => {
    const host = new FakeHost();
    const result = await uploadChunked({ dir: "/remote" }, new Blob([]), "empty", {}, host.io());
    expect(result.size).toBe(0);
    expect(host.chunks()).toHaveLength(0);
  });
});

describe("nextChunkSize", () => {
  it("aims a piece at the target duration, round trip included", () => {
    // 1 MB/s with nothing else: half a second of bytes.
    expect(nextChunkSize(1_000_000, 500_000, 500)).toBe(500_000);
    // The same link with 100 ms of latency settles lower.
    expect(nextChunkSize(400_000, 400_000, 500)).toBe(400_000);
  });

  it("grows at most fourfold per piece and stays within the limits", () => {
    expect(nextChunkSize(CHUNK_START, CHUNK_START, 1)).toBe(CHUNK_START * 4);
    expect(nextChunkSize(CHUNK_MAX, CHUNK_MAX, 1)).toBe(CHUNK_MAX);
    expect(nextChunkSize(CHUNK_START, CHUNK_START, 60_000)).toBe(CHUNK_MIN);
    expect(nextChunkSize(CHUNK_MAX, CHUNK_MAX, 1, 1024 * 1024)).toBe(1024 * 1024);
  });
});

describe("humanBytes", () => {
  it("writes sizes the way the host's messages do", () => {
    expect(humanBytes(512)).toBe("512 bytes");
    expect(humanBytes(64 * 1024 * 1024)).toBe("64 MB");
    expect(humanBytes(3.2 * 1024 * 1024 * 1024)).toBe("3.2 GB");
    expect(humanBytes(1536)).toBe("1.5 KB");
  });
});
