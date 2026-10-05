import { afterEach, describe, expect, it, vi } from "vitest";
import { fsValidate } from "./files";
import { followDocHref, type DocLinkHost } from "./docLinks";
import { setPathOpener } from "../shared/openPath";

vi.mock("./files", async (original) => ({
  ...await original<typeof import("./files")>(),
  fsValidate: vi.fn(),
}));

afterEach(() => {
  setPathOpener(null);
  vi.resetAllMocks();
});

describe("document link resolution", () => {
  it.each([false, true])("retains its gesture's pane across delayed validation (split=%s)", async (split) => {
    let resolve!: (value: Awaited<ReturnType<typeof fsValidate>>) => void;
    vi.mocked(fsValidate).mockReturnValue(new Promise((done) => { resolve = done; }));
    const host: DocLinkHost = {
      docPath: "/project/start.md", fromPane: "source-pane",
      wsRoot: "/project", workspaceId: "workspace",
      toAnchor: () => false, toLines: () => {}, hint: vi.fn(),
    };
    let focusedPane = "source-pane";
    const opened = vi.fn((_path, _kind, opts) => opts.fromPane ?? focusedPane);
    setPathOpener(opened);
    const pending = followDocHref("nested.md", split, host);
    expect(opened).not.toHaveBeenCalled();
    focusedPane = "other-pane";
    resolve({ valid: { "nested.md": { path: "/project/nested.md", kind: "file" } }, ambiguous: {}, unchecked: [] });
    await pending;
    expect(opened).toHaveBeenCalledWith("/project/nested.md", "file", {
      split, fromPane: "source-pane", documentFrom: "/project/start.md",
    });
    expect(opened.mock.results[0]?.value).toBe("source-pane");
  });
});
