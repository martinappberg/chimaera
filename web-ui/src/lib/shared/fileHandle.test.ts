import { describe, expect, it } from "vitest";
import type { ContextMenuItem } from "./contextMenu.svelte";
import { fileMenuEntries, REFERENCE_IN_CHAT } from "./fileHandle";

const labels = (entries: ReturnType<typeof fileMenuEntries>) =>
  entries.map((e) => (e === "separator" ? "-" : e.label.replace(/ \(.*\)$/, "")));

describe("fileMenuEntries", () => {
  const noop = { open: () => {}, reference: () => {} };

  it("offers the reference where an input can take it", () => {
    expect(labels(fileMenuEntries({ path: "/w/figs/umap.png" }, { canReference: true, reveal: [] }, noop))).toEqual([
      "Open",
      "Open Beside",
      "-",
      REFERENCE_IN_CHAT,
    ]);
  });

  it("leaves the reference out when nothing can take it (the chat ended)", () => {
    expect(labels(fileMenuEntries({ path: "/w/figs/umap.png" }, { canReference: false, reveal: [] }, noop))).toEqual([
      "Open",
      "Open Beside",
    ]);
  });

  it("a folder opens in one place only", () => {
    expect(labels(fileMenuEntries({ path: "/w/figs", kind: "dir" }, { canReference: true, reveal: [] }, noop))).toEqual([
      "Open",
      "-",
      REFERENCE_IN_CHAT,
    ]);
  });

  it("appends the file-manager row where the files are this computer's", () => {
    const reveal = [{ label: "Reveal in Finder", onSelect: () => {} }];
    expect(labels(fileMenuEntries({ path: "/w/a.png" }, { canReference: true, reveal }, noop)).slice(-2)).toEqual([
      "-",
      "Reveal in Finder",
    ]);
  });

  it("routes each row to its action", () => {
    const calls: string[] = [];
    const entries = fileMenuEntries(
      { path: "/w/a.png" },
      { canReference: true, reveal: [] },
      { open: (split) => calls.push(split ? "beside" : "open"), reference: () => calls.push("reference") },
    );
    for (const e of entries) if (e !== "separator") (e as ContextMenuItem).onSelect();
    expect(calls).toEqual(["open", "beside", "reference"]);
  });
});
