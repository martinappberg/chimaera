import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  fsFile: vi.fn(),
  fsMarkdown: vi.fn(),
  fsRawUrl: vi.fn(),
  fsTable: vi.fn(),
}));

vi.mock("./files", () => mocks);

import { FileEntry } from "./fileStore.svelte";

describe("FileEntry.ensureRawUrl", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("makes concurrent consumers wait for the same raw ticket", async () => {
    let resolveTicket!: (url: string) => void;
    mocks.fsRawUrl.mockReturnValue(
      new Promise<string>((resolve) => {
        resolveTicket = resolve;
      }),
    );

    const entry = new FileEntry("plots/umap.pdf");
    let firstFinished = false;
    let secondFinished = false;
    const first = entry.ensureRawUrl().then(() => {
      firstFinished = true;
    });
    const second = entry.ensureRawUrl().then(() => {
      secondFinished = true;
    });

    await Promise.resolve();
    expect(mocks.fsRawUrl).toHaveBeenCalledTimes(1);
    expect(firstFinished).toBe(false);
    expect(secondFinished).toBe(false);

    resolveTicket("/api/fs/raw/ticket");
    await Promise.all([first, second]);

    expect(entry.rawUrl).toBe("/api/fs/raw/ticket");
    expect(firstFinished).toBe(true);
    expect(secondFinished).toBe(true);
  });
});
