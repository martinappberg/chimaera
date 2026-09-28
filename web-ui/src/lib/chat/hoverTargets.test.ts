import { describe, expect, it } from "vitest";
import { HoverTargets, refFragment } from "./hoverTargets";

describe("refFragment", () => {
  it("keeps a link's own fragment, else names the lines", () => {
    expect(refFragment({ path: "notes.md" }, "notes.md#Results")).toBe("Results");
    expect(refFragment({ path: "paper.pdf" }, "paper.pdf#page=3")).toBe("page=3");
    expect(refFragment({ path: "src/a.rs", line: 12 })).toBe("L12");
    expect(refFragment({ path: "src/a.rs", line: 12, endLine: 20 })).toBe("L12-L20");
    expect(refFragment({ path: "src/a.rs", line: 12 }, "src/a.rs")).toBe("L12");
    expect(refFragment({ path: "notes.md" }, "notes.md#")).toBeNull();
    expect(refFragment({ path: "notes.md" })).toBeNull();
  });
});

describe("HoverTargets", () => {
  it("answers only for elements it was given, escaping the path", () => {
    const reg = new HoverTargets();
    const el = {} as Element;
    const forged = {} as Element;
    reg.set(el, { path: "/scratch/run#2/50% plan.md", fragment: "Results", note: "changed after this turn" });
    expect(reg.targetOf(el)).toEqual({
      target: { kind: "file", target: "/scratch/run%232/50%25%20plan.md#Results", fragment: "Results", byName: false },
      note: "changed after this turn",
    });
    expect(reg.targetOf(forged)).toBeNull();
    reg.delete(el);
    expect(reg.targetOf(el)).toBeNull();
  });
});
