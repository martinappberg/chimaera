import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";

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

class FakeXhr {
  static readonly HEADERS_RECEIVED = 2;
  static instances: FakeXhr[] = [];

  readonly upload: {
    onprogress: ((event: ProgressEvent) => void) | null;
    onload: (() => void) | null;
  } = { onprogress: null, onload: null };
  status = 0;
  responseText = "";
  readyState = 0;
  onreadystatechange: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onabort: (() => void) | null = null;
  onload: (() => void) | null = null;
  method = "";
  url = "";
  sent: Blob | null = null;

  constructor() {
    FakeXhr.instances.push(this);
  }

  open(method: string, url: string): void {
    this.method = method;
    this.url = url;
  }

  setRequestHeader(): void {}

  send(blob: Blob): void {
    this.sent = blob;
  }

  abort(): void {
    this.onabort?.();
  }

  progress(loaded: number, total: number): void {
    this.upload.onprogress?.({ lengthComputable: true, loaded, total } as ProgressEvent);
  }

  finish(result: object): void {
    this.status = 200;
    this.responseText = JSON.stringify(result);
    this.readyState = 4;
    this.onreadystatechange?.();
    this.onload?.();
  }
}

describe("streamed uploads", () => {
  beforeAll(() => {
    vi.stubGlobal("location", { hash: "", pathname: "/", search: "" });
    vi.stubGlobal("sessionStorage", new MemoryStorage());
    vi.stubGlobal("history", { replaceState: vi.fn() });
    vi.stubGlobal("XMLHttpRequest", FakeXhr);
  });

  afterAll(() => vi.unstubAllGlobals());

  it("reports byte progress and completes with the host result", async () => {
    const { uploadToDir } = await import("./uploads");
    const progress: number[] = [];
    let cancel: (() => void) | null = null;
    const pending = uploadToDir("/remote/data", new Blob(["0123456789"]), "data.bin", {
      onProgress: (value) => progress.push(value),
      onCancelReady: (fn) => (cancel = fn),
    });
    const xhr = FakeXhr.instances.at(-1)!;
    expect(xhr.method).toBe("POST");
    expect(xhr.url).toContain("/api/v1/fs/upload?");
    expect(cancel).toBeTypeOf("function");

    xhr.progress(5, 10);
    xhr.finish({ path: "/remote/data/data.bin", name: "data.bin", size: 10 });

    await expect(pending).resolves.toEqual({
      path: "/remote/data/data.bin",
      name: "data.bin",
      size: 10,
    });
    expect(progress).toEqual([0, 0.5, 1]);
  });

  it("exposes cancellation without waiting for the host", async () => {
    const { uploadToSession } = await import("./uploads");
    let cancel: (() => void) | null = null;
    const pending = uploadToSession("session-1", new Blob(["x"]), "x.txt", {
      onCancelReady: (fn) => (cancel = fn),
    });
    expect(cancel).toBeTypeOf("function");
    cancel!();
    await expect(pending).rejects.toThrow("upload cancelled");
  });
});
