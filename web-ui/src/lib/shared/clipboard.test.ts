import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { writeClipboard } from "../net/native";
import { copyText } from "./clipboard";

vi.mock("../net/native", () => ({ writeClipboard: vi.fn() }));

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
