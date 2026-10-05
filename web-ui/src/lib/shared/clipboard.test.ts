import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { copyFileToClipboard, hasLocalFiles, writeClipboard } from "../net/native";
import { copyFileToOs, copyText } from "./clipboard";

vi.mock("../net/native", () => ({
  copyFileToClipboard: vi.fn(),
  hasLocalFiles: vi.fn(),
  writeClipboard: vi.fn(),
}));
vi.mock("../previews/files", () => ({
  fsRawUrl: vi.fn(async (path: string) => `/raw/ticket${path}`),
  isImagePath: (path: string) => path.endsWith(".png"),
}));

describe("copying rendered text", () => {
  const browserWrite = vi.fn();

  beforeEach(() => {
    vi.mocked(writeClipboard).mockReset().mockResolvedValue(false);
    browserWrite.mockReset().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText: browserWrite } });
  });

  afterEach(() => vi.unstubAllGlobals());

  it("copies through the browser when no native write happened", async () => {
    expect(await copyText("Selected document text")).toBe(true);
    expect(browserWrite).toHaveBeenCalledWith("Selected document text");
  });

  it("keeps successful native copying independent of browser permissions", async () => {
    vi.mocked(writeClipboard).mockResolvedValue(true);
    expect(await copyText("Selected document text")).toBe(true);
    expect(browserWrite).not.toHaveBeenCalled();
  });

  it("reports failure when the browser refuses the fallback", async () => {
    browserWrite.mockRejectedValue(new Error("Clipboard denied"));
    expect(await copyText("Selected document text")).toBe(false);
  });
});

describe("copying a file to the OS clipboard", () => {
  const browserWrite = vi.fn();
  const png = new Blob(["png"], { type: "image/png" });

  beforeEach(() => {
    vi.mocked(copyFileToClipboard).mockReset().mockResolvedValue(true);
    vi.mocked(hasLocalFiles).mockReset().mockReturnValue(false);
    browserWrite.mockReset().mockImplementation(async (items: { data: Record<string, Promise<Blob>> }[]) => {
      await Promise.all(items.flatMap((item) => Object.values(item.data)));
    });
    vi.stubGlobal("navigator", { clipboard: { write: browserWrite } });
    vi.stubGlobal(
      "ClipboardItem",
      class {
        constructor(public data: Record<string, Promise<Blob>>) {}
      },
    );
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, headers: new Headers(), blob: async () => png })));
  });

  afterEach(() => vi.unstubAllGlobals());

  it("puts the file itself there when the files are this machine's", async () => {
    vi.mocked(hasLocalFiles).mockReturnValue(true);
    expect(await copyFileToOs("/work/figure.png", "file")).toBe(true);
    expect(copyFileToClipboard).toHaveBeenCalledWith("/work/figure.png");
    expect(browserWrite).not.toHaveBeenCalled();
  });

  it("copies a remote image as a picture, started inside the gesture", async () => {
    const copied = copyFileToOs("/work/figure.png", "file");
    // No await yet: the write is already under way.
    expect(browserWrite).toHaveBeenCalledTimes(1);
    expect(await copied).toBe(true);
    const item = browserWrite.mock.calls[0][0][0] as { data: Record<string, Promise<Blob>> };
    expect(await item.data["image/png"]).toBe(png);
    expect(fetch).toHaveBeenCalledWith("/raw/ticket/work/figure.png");
    expect(copyFileToClipboard).not.toHaveBeenCalled();
  });

  it("leaves the clipboard alone for a remote file with no picture", async () => {
    expect(await copyFileToOs("/work/notes.txt", "file")).toBe(false);
    expect(await copyFileToOs("/work/figures.png", "dir")).toBe(false);
    expect(browserWrite).not.toHaveBeenCalled();
  });

  it("refuses a picture too large to pull into the page", async () => {
    const headers = new Headers({ "content-length": String(65 * 1024 * 1024) });
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, headers, blob: async () => png })));
    expect(await copyFileToOs("/work/figure.png", "file")).toBe(false);
  });

  it("reports a refused picture write", async () => {
    browserWrite.mockRejectedValue(new Error("Clipboard denied"));
    expect(await copyFileToOs("/work/figure.png", "file")).toBe(false);
  });
});
