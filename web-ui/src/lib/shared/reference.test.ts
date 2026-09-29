import { get } from "svelte/store";
import { afterEach, describe, expect, it } from "vitest";
import {
  activeSelection,
  agentMention,
  composeAgentPathReference,
  composeChatQuote,
  composeProvenanceSuffix,
  composeSelectionReference,
  cutAt,
  needsCropUpload,
  quoteRuns,
  referenceNow,
  setReferenceHandler,
  type FileSelection,
} from "./reference";

const crop = new Blob([new Uint8Array([137, 80, 78, 71])], { type: "image/png" });

function file(over: Partial<FileSelection>): FileSelection {
  return { kind: "file", path: "/w/x", startLine: null, endLine: null, text: "", ...over };
}

describe("composeSelectionReference", () => {
  it("keeps the line format for text views", () => {
    const sel = file({ startLine: 3, endLine: 7, text: "let x = 1;\nlet y = 2;" });
    expect(composeSelectionReference("src/a.ts", sel, "terminal")).toBe('@src/a.ts#L3-L7 "let x = 1; let y = 2;" ');
    expect(composeSelectionReference("src/a.ts", sel, "chat")).toBe('@src/a.ts#L3-L7 "let x = 1; let y = 2;" ');
  });
  it("a PDF text selection: page + quote", () => {
    const sel = file({ fragment: "page=3", text: "the effect held\nacross cohorts", label: "p. 3" });
    expect(composeSelectionReference("paper.pdf", sel, "terminal")).toBe(
      '@paper.pdf#page=3 "the effect held across cohorts" ',
    );
  });
  it("a markdown selection names its heading", () => {
    const sel = file({ startLine: 40, endLine: 42, text: "p < 0.01", context: "§ Results" });
    expect(composeSelectionReference("report.md", sel, "chat")).toBe('@report.md#L40-L42 (§ Results) "p < 0.01" ');
  });
  describe("a region with a crop", () => {
    const sel = file({ fragment: "page=3&xywh=72,90,200,120", text: "Figure 2", crop, label: "p. 3 region" });
    it("terminal target: the uploaded crop's path is typed", () => {
      expect(composeSelectionReference("paper.pdf", sel, "terminal", "/h/.chimaera/uploads/s1/ref-1.png")).toBe(
        '@paper.pdf#page=3&xywh=72,90,200,120 "Figure 2" (region image: /h/.chimaera/uploads/s1/ref-1.png) ',
      );
    });
    it("terminal target, upload failed: the locator still goes", () => {
      expect(composeSelectionReference("paper.pdf", sel, "terminal", null)).toBe(
        '@paper.pdf#page=3&xywh=72,90,200,120 "Figure 2" ',
      );
    });
    it("chat target: no path (the crop rides as an attachment)", () => {
      expect(composeSelectionReference("paper.pdf", sel, "chat", "/ignored.png")).toBe(
        '@paper.pdf#page=3&xywh=72,90,200,120 "Figure 2" ',
      );
    });
    it("only a terminal target uploads", () => {
      expect(needsCropUpload(sel, "terminal")).toBe(true);
      expect(needsCropUpload(sel, "chat")).toBe(false);
      expect(needsCropUpload(file({ text: "x" }), "terminal")).toBe(false);
    });
    it("an image region with no text sends no quote", () => {
      const img = file({ fragment: "xywh=160,120,320,240", crop });
      expect(composeSelectionReference("figs/umap.png", img, "terminal", "/u/ref-2.png")).toBe(
        "@figs/umap.png#xywh=160,120,320,240 (region image: /u/ref-2.png) ",
      );
    });
  });
  it("a table block quotes its TSV as built (spacing is data)", () => {
    const sel = file({ fragment: "cell=5,2-6,3", text: "", quote: "gene\\tlog2FC\\nTP53  x\\t2.1" });
    expect(composeSelectionReference("de.tsv", sel, "terminal")).toBe(
      '@de.tsv#cell=5,2-6,3 "gene\\tlog2FC\\nTP53  x\\t2.1" ',
    );
  });
  it("a moment, a notebook cell and a slide", () => {
    expect(composeSelectionReference("demo.mp4", file({ fragment: "t=12.5,20" }), "terminal")).toBe(
      "@demo.mp4#t=12.5,20 ",
    );
    expect(composeSelectionReference("nb.ipynb", file({ fragment: "cell=7", text: "df.head()" }), "chat")).toBe(
      '@nb.ipynb#cell=7 "df.head()" ',
    );
    expect(composeSelectionReference("deck.md", file({ fragment: "slide=3", text: "Results" }), "chat")).toBe(
      '@deck.md#slide=3 "Results" ',
    );
  });
  it("never contains a newline or tab, whatever the producer handed it", () => {
    const nasty = file({
      fragment: "row=1",
      text: "a\nb",
      quote: "x\ny\tz\r",
      context: "head\ning",
      crop,
    });
    for (const target of ["chat", "terminal"] as const) {
      const out = composeSelectionReference("f.csv", nasty, target, "/p\n.png");
      expect(out).not.toMatch(/[\r\n\t]/);
      expect(out.endsWith(" ")).toBe(true);
    }
  });
});

describe("provenance and one-click references", () => {
  afterEach(() => setReferenceHandler(null));
  it("the provenance suffix carries a locator", () => {
    expect(composeProvenanceSuffix(file({ fragment: "page=2" }), "paper.pdf", null)).toBe(" [from @paper.pdf#page=2] ");
  });
  it("a chat snippet names the chat it came from", () => {
    const chat = { kind: "chat", sessionId: "c-1", text: "" } as const;
    expect(composeProvenanceSuffix(chat, null, "claude")).toBe(" [from claude reply] ");
    expect(composeProvenanceSuffix(chat, null, null)).toBe(" [from agent reply] ");
  });
  it("referenceNow publishes, references through the handler, then clears", () => {
    const seen: unknown[] = [];
    setReferenceHandler(() => seen.push(get(activeSelection)));
    const sel = file({ fragment: "cell=7" });
    referenceNow("nb", sel);
    expect(seen).toEqual([sel]);
    expect(get(activeSelection)).toBeNull();
  });
});

describe("agentMention: a mention claude reads whole", () => {
  it("stays bare when claude's bare form reads the whole path", () => {
    expect(agentMention("src/a.ts")).toBe("@src/a.ts");
    expect(agentMention("src/lib/")).toBe("@src/lib/");
    expect(agentMention("/pad/Screenshot-2026-09-26-at-12.30.png")).toBe("@/pad/Screenshot-2026-09-26-at-12.30.png");
    expect(agentMention("\u5831\u544a", "L3-L9")).toBe("@\u5831\u544a#L3-L9");
  });
  it("quotes whitespace, keeping the locator inside the quotes", () => {
    expect(agentMention("raw data/qc.tsv")).toBe('@"raw data/qc.tsv"');
    expect(agentMention("raw data/qc.tsv", "L3-L9")).toBe('@"raw data/qc.tsv#L3-L9"');
    expect(agentMention("raw data/")).toBe('@"raw data/"');
  });
  it("quotes a tail claude's word boundary would cut", () => {
    expect(agentMention("/pad/\u5831\u544a")).toBe('@"/pad/\u5831\u544a"');
    expect(agentMention("notes(1)")).toBe('@"notes(1)"');
    expect(agentMention("v1.")).toBe('@"v1."');
  });
  it("leaves a path holding a double quote bare (no escape exists)", () => {
    expect(agentMention('my "final" notes.md')).toBe('@my "final" notes.md');
  });
  it("every composer writes mentions through it", () => {
    expect(composeAgentPathReference("raw data/qc.tsv")).toBe('@"raw data/qc.tsv" ');
    expect(composeSelectionReference("raw data/qc.tsv", file({ startLine: 3, endLine: 9, text: "x" }), "terminal")).toBe(
      '@"raw data/qc.tsv#L3-L9" "x" ',
    );
    expect(composeProvenanceSuffix(file({ startLine: 3, endLine: 9 }), "raw data/qc.tsv", null)).toBe(
      ' [from @"raw data/qc.tsv#L3-L9"] ',
    );
  });
});

describe("composeChatQuote: a transcript passage quoted into the reply", () => {
  it("quotes every line and leaves a blank line to write under", () => {
    expect(composeChatQuote("the median is noisier")).toBe("> the median is noisier\n\n");
  });
  it("keeps a table's rows and cells as selected", () => {
    const table = "\tN = 5\tN = 101\nMean\t0.13\t0.03\nMedian\t0.19\t0.05\n";
    expect(composeChatQuote(table)).toBe("> \tN = 5\tN = 101\n> Mean\t0.13\t0.03\n> Median\t0.19\t0.05\n\n");
  });
  it("folds blank-line runs, trims the ends, and marks a kept blank line", () => {
    expect(composeChatQuote("\n\none  \n\n\n\ntwo\r\n\n")).toBe("> one\n>\n> two\n\n");
  });
  it("sends nothing for a whitespace-only selection", () => {
    expect(composeChatQuote(" \n\t\n ")).toBe("");
  });
  it("turns control characters into spaces", () => {
    expect(composeChatQuote("a\u0007b\u001bc")).toBe("> a b c\n\n");
  });
  it("never cuts an emoji in half at the cap", () => {
    const quoted = composeChatQuote(`${"x".repeat(9)}😀tail`, 10);
    expect(quoted).toBe(`> ${"x".repeat(9)}…\n\n`);
    expect(cutAt("ab😀", 3)).toBe("ab");
    expect(cutAt("ab😀", 4)).toBe("ab😀");
  });
  it("caps a long passage with an ellipsis", () => {
    const quoted = composeChatQuote(`${"x".repeat(50)}\n${"y".repeat(50)}`, 60);
    expect(quoted).toBe(`> ${"x".repeat(50)}\n> ${"y".repeat(9)}…\n\n`);
  });
});

describe("quoteRuns: a sent message's quoted passages", () => {
  it("splits whole lines losslessly", () => {
    const text = "> a\n> b\n\nwhy?\n>c";
    const runs = quoteRuns(text);
    expect(runs).toEqual([
      { quote: true, text: "> a\n> b\n" },
      { quote: false, text: "\nwhy?\n" },
      { quote: true, text: ">c" },
    ]);
    expect(runs.map((r) => r.text).join("")).toBe(text);
  });
  it("reads a marker behind up to three spaces, not four or mid-line", () => {
    expect(quoteRuns("   > q").map((r) => r.quote)).toEqual([true]);
    expect(quoteRuns("    > code").map((r) => r.quote)).toEqual([false]);
    expect(quoteRuns("a > b").map((r) => r.quote)).toEqual([false]);
  });
  it("round-trips what composeChatQuote writes", () => {
    const runs = quoteRuns(`${composeChatQuote("one\n\ntwo")}the question`);
    expect(runs).toEqual([
      { quote: true, text: "> one\n>\n> two\n" },
      { quote: false, text: "\nthe question" },
    ]);
  });
  it("a message without quotes is one plain run", () => {
    expect(quoteRuns("plain\ntext")).toEqual([{ quote: false, text: "plain\ntext" }]);
    expect(quoteRuns("")).toEqual([{ quote: false, text: "" }]);
  });
});
