import JSZip from "jszip";
import { describe, expect, it } from "vitest";
import { attr, scan, type XmlEl } from "./pptxXml";
import {
  buildStates,
  effectName,
  extractTiming,
  goldenSlide,
  parseTimeline,
  pathEnd,
  targetKey,
  TIMING_CAPS,
  type GoldenSlide,
  type SlideTimeline,
} from "./pptxTiming";

/**
 * The timing corpus (`fixtures/pptx-timing/`): every slide of every deck
 * PowerPoint wrote, parsed and compared with its golden file. The plugin's
 * Rust parser runs the same decks against the same goldens. With
 * TIMING_GOLDEN=write (`npm run timing:golden`) the goldens are rewritten
 * from this parser instead; review that diff as the contract change it is.
 */

// The UI's tsconfig carries no node types, so node's fs and env come in
// through a narrow cast (as in `settings/themes.contrast.test.ts`).
type NodeFs = {
  readFileSync(path: URL): Uint8Array;
  readFileSync(path: URL, encoding: "utf8"): string;
  readdirSync(path: URL): string[];
  writeFileSync(path: URL, data: string): void;
  existsSync(path: URL): boolean;
  mkdirSync(path: URL, options: { recursive: true }): void;
};
const proc = (
  globalThis as unknown as {
    process: { getBuiltinModule(id: "node:fs"): NodeFs; env: Record<string, string | undefined> };
  }
).process;
const fs = proc.getBuiltinModule("node:fs");
const WRITE = proc.env.TIMING_GOLDEN === "write";

const CORPUS = new URL("./fixtures/pptx-timing/", import.meta.url);
const listed = (dir: string, ext: string) =>
  fs
    .readdirSync(new URL(dir, CORPUS))
    .filter((n) => n.endsWith(ext))
    .map((n) => n.slice(0, -ext.length))
    .sort();
const DECKS = listed("decks/", ".pptx");
const CASES = listed("cases/", ".xml");

function children(el: XmlEl, local: string): XmlEl[] {
  return el.children.filter((c) => c.local === local);
}

/** The deck's slides in show order: part path and `p:sldId@id`. */
async function deckSlides(zip: JSZip): Promise<{ part: string; id: number; xml: string }[]> {
  const presXml = await zip.file("ppt/presentation.xml")!.async("string");
  const relsXml = await zip.file("ppt/_rels/presentation.xml.rels")!.async("string");
  const targets = new Map<string, string>();
  for (const rel of children(scan(relsXml), "Relationship")) {
    const target = attr(relsXml, rel, "Target") ?? "";
    targets.set(attr(relsXml, rel, "Id") ?? "", target.startsWith("/") ? target.slice(1) : `ppt/${target}`);
  }
  const lst = children(scan(presXml), "sldIdLst")[0];
  const out: { part: string; id: number; xml: string }[] = [];
  for (const s of lst === undefined ? [] : children(lst, "sldId")) {
    const part = targets.get(attr(presXml, s, "r:id") ?? "")!;
    out.push({ part, id: Number(attr(presXml, s, "id")), xml: await zip.file(part)!.async("string") });
  }
  return out;
}

async function deckGolden(name: string): Promise<{ deck: string; slides: GoldenSlide[] }> {
  const zip = await JSZip.loadAsync(fs.readFileSync(new URL(`decks/${name}.pptx`, CORPUS)));
  const slides = (await deckSlides(zip)).map((s) => goldenSlide(s.part, s.id, parseTimeline(s.xml)));
  return { deck: `${name}.pptx`, slides };
}

function caseXml(name: string): string {
  return fs.readFileSync(new URL(`cases/${name}.xml`, CORPUS), "utf8");
}

function caseGolden(name: string): GoldenSlide {
  return goldenSlide(`${name}.xml`, 0, parseTimeline(caseXml(name)));
}

async function slideOf(deck: string, n: number): Promise<SlideTimeline> {
  const zip = await JSZip.loadAsync(fs.readFileSync(new URL(`decks/${deck}.pptx`, CORPUS)));
  return parseTimeline((await deckSlides(zip))[n - 1]!.xml);
}

/** Compare with (or, in write mode, rewrite) one golden file. */
function golden(rel: string, got: unknown): void {
  const file = new URL(`goldens/${rel}`, CORPUS);
  if (WRITE) {
    if (proc.env.CI) throw new Error("timing:golden rewrites the contract; it never runs in CI");
    fs.mkdirSync(new URL(".", file), { recursive: true });
    fs.writeFileSync(file, `${JSON.stringify(got, null, 2)}\n`);
    return;
  }
  expect(fs.existsSync(file), `no golden ${rel}: run npm run timing:golden`).toBe(true);
  expect(got).toEqual(JSON.parse(fs.readFileSync(file, "utf8")));
}

describe("the PowerPoint timing corpus", () => {
  it("has decks and cases, and a golden for each and nothing else", () => {
    expect(DECKS.length).toBeGreaterThan(0);
    expect(CASES.length).toBeGreaterThan(0);
    if (WRITE) return;
    expect(listed("goldens/", ".json")).toEqual(DECKS);
    expect(listed("goldens/cases/", ".json")).toEqual(CASES);
  });

  for (const name of DECKS) {
    it(`${name}.pptx parses to its golden`, async () => {
      golden(`${name}.json`, await deckGolden(name));
    });
  }

  for (const name of CASES) {
    it(`cases/${name}.xml parses to its golden`, () => {
      golden(`cases/${name}.json`, caseGolden(name));
    });
  }

  it("reads the same timeline from a slide's extract as from the whole part", async () => {
    const same = (xml: string, label: string) => {
      const ex = extractTiming(xml);
      const tl = ex.source !== null ? parseTimeline(ex.source) : ex.timeline!;
      expect(goldenSlide("p", 0, tl), label).toEqual(goldenSlide("p", 0, parseTimeline(xml)));
      if (ex.source !== null) expect(ex.source.length, label).toBeLessThan(xml.length);
    };
    for (const name of DECKS) {
      const zip = await JSZip.loadAsync(fs.readFileSync(new URL(`decks/${name}.pptx`, CORPUS)));
      for (const s of await deckSlides(zip)) same(s.xml, `${name} ${s.part}`);
    }
    for (const name of CASES) same(caseXml(name), name);
  });

  it("reads a slide's show flag in its extract, and a part past the cap at once", () => {
    const ns = 'xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"';
    expect(extractTiming(`<p:sld ${ns} show="0"><p:cSld/></p:sld>`).hidden).toBe(true);
    expect(extractTiming(`<p:sld ${ns}><p:cSld/></p:sld>`)).toEqual({ source: `<p:sld ${ns}></p:sld>`, timeline: null, hidden: false });
    const big = extractTiming(`<p:sld ${ns}><p:cSld>${"x".repeat(TIMING_CAPS.partBytes + 1)}</p:cSld></p:sld>`);
    expect(big.source).toBe(null);
    expect(big.timeline?.approximate).toBe("part-too-large");
    expect(extractTiming("<p:sld").timeline?.approximate).toBe("unreadable");
  });

  it("starts an after effect when everything before it in its step has ended", async () => {
    for (const name of DECKS) {
      for (const slide of (await deckGolden(name)).slides) {
        for (const step of slide.steps) {
          step.forEach((e, i) => {
            if (e.trigger !== "after") return;
            const ended = Math.max(0, ...step.slice(0, i).map((p) => p.start + (p.dur ?? 0)));
            expect(e.start, `${name} ${slide.part}`).toBe(ended);
          });
        }
      }
    }
  });
});

describe("parseTimeline on the corpus", () => {
  it("chains with and after effects (triggers, slide 3)", async () => {
    const t = await slideOf("triggers", 3);
    expect(t.steps).toHaveLength(2);
    expect(t.steps[1]!.map((e) => [e.trigger, e.start, e.dur, e.target?.spid])).toEqual([
      ["click", 0, 0, "2"],
      ["after", 0, 250, "3"],
      ["after", 250, 1500, "4"],
      ["with", 250, 500, "5"],
    ]);
    expect(t.transition).toMatchObject({ kind: "fade", dur: 2000, advanceAfter: 3000, advanceOnClick: true });
  });

  it("reads click, with and after in one click, then a long fly (triggers, slide 1)", async () => {
    const t = await slideOf("triggers", 1);
    expect(t.steps.map((s) => s.length)).toEqual([0, 3, 1]);
    expect(t.steps[1]!.map((e) => [e.trigger, e.start])).toEqual([
      ["click", 0],
      ["with", 0],
      ["after", 500],
    ]);
    expect(t.steps[2]![0]).toMatchObject({ name: "fly-in", dir: "left", dur: 2000 });
  });

  it("runs a first group that opens without a click on entry (triggers, slide 2)", async () => {
    const t = await slideOf("triggers", 2);
    expect(t.steps.map((s) => s.map((e) => e.trigger))).toEqual([["with", "after"], ["click"]]);
    expect(buildStates(t).map((s) => s.hidden)).toEqual([["4"], []]);
  });

  it("rides child paragraphs on their parent's click (paragraphs, slide 2)", async () => {
    const t = await slideOf("paragraphs", 2);
    expect(t.steps.map((s) => s.map((e) => e.target && [e.trigger, e.target.para]))).toEqual([
      [],
      [
        ["click", [0, 0]],
        ["with", [1, 1]],
        ["with", [2, 2]],
      ],
      [
        ["click", [3, 3]],
        ["with", [4, 4]],
      ],
    ]);
    expect(buildStates(t).map((s) => s.hidden)).toEqual([
      ["2:p0-0", "2:p1-1", "2:p2-2", "2:p3-3", "2:p4-4"],
      ["2:p3-3", "2:p4-4"],
      [],
    ]);
    expect(t.builds).toEqual([{ spid: "2", group: 0, kind: "bldP", build: "p", level: null, animBg: false }]);
  });

  it("adds up paths on one shape (paths, slide 2)", async () => {
    const t = await slideOf("paths", 2);
    expect(buildStates(t).map((s) => s.moved)).toEqual([
      {},
      {},
      { "2": [0.25, 0] },
      { "2": [0.25, 0.25] },
    ]);
  });

  it("shows, keeps and hides one shape across three clicks (exits-emphasis, slide 3)", async () => {
    const t = await slideOf("exits-emphasis", 3);
    expect(buildStates(t).map((s) => s.hidden)).toEqual([["2"], [], [], ["2"]]);
  });

  it("takes Morph from its Choice, never the fade fallback", async () => {
    const options = [];
    for (const n of [2, 3, 4]) {
      const t = await slideOf("morph", n);
      expect(t.transition?.kind).toBe("morph");
      options.push(t.transition?.morph);
    }
    expect(options).toEqual(["byObject", "byWord", "byChar"]);
  });

  it("reads fade through black (transitions, slide 3)", async () => {
    expect((await slideOf("transitions", 3)).transition).toMatchObject({ kind: "fade-black", dur: 1500 });
  });
});

// --- hand-written slides --------------------------------------------------------------

const P = 'xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"';
const A = 'xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"';

function slide(body: string): string {
  return `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:sld ${A} ${P}><p:cSld><p:spTree/></p:cSld>${body}</p:sld>`;
}

/** One effect node: `beh` are its behaviours. */
function effect(id: number, nodeType: string, cls: string, preset: number, beh: string): string {
  return `<p:par><p:cTn id="${id}" presetID="${preset}" presetClass="${cls}" presetSubtype="0" fill="hold" nodeType="${nodeType}"><p:stCondLst><p:cond delay="0"/></p:stCondLst><p:childTnLst>${beh}</p:childTnLst></p:cTn></p:par>`;
}

function setVis(spid: string, to: string, delay = 0): string {
  return `<p:set><p:cBhvr><p:cTn id="1" dur="1" fill="hold"><p:stCondLst><p:cond delay="${delay}"/></p:stCondLst></p:cTn><p:tgtEl><p:spTgt spid="${spid}"/></p:tgtEl><p:attrNameLst><p:attrName>style.visibility</p:attrName></p:attrNameLst></p:cBhvr><p:to><p:strVal val="${to}"/></p:to></p:set>`;
}

function animEffect(spid: string, filter: string, dur = 500): string {
  return `<p:animEffect transition="in" filter="${filter}"><p:cBhvr><p:cTn id="1" dur="${dur}"/><p:tgtEl><p:spTgt spid="${spid}"/></p:tgtEl></p:cBhvr></p:animEffect>`;
}

/** A main sequence of click groups, each a list of effect nodes. */
function mainSeq(groups: string[][]): string {
  const pars = groups
    .map(
      (g) =>
        `<p:par><p:cTn id="3" fill="hold"><p:stCondLst><p:cond delay="indefinite"/></p:stCondLst><p:childTnLst><p:par><p:cTn id="4" fill="hold"><p:stCondLst><p:cond delay="0"/></p:stCondLst><p:childTnLst>${g.join("")}</p:childTnLst></p:cTn></p:par></p:childTnLst></p:cTn></p:par>`,
    )
    .join("");
  return `<p:timing><p:tnLst><p:par><p:cTn id="1" dur="indefinite" restart="never" nodeType="tmRoot"><p:childTnLst><p:seq concurrent="1" nextAc="seek"><p:cTn id="2" dur="indefinite" nodeType="mainSeq"><p:childTnLst>${pars}</p:childTnLst></p:cTn></p:seq></p:childTnLst></p:cTn></p:par></p:tnLst></p:timing>`;
}

function motionTo(spid: string, path: string): string {
  return `<p:animMotion origin="layout" path="${path}" pathEditMode="relative"><p:cBhvr><p:cTn id="1" dur="2000" fill="hold"/><p:tgtEl><p:spTgt spid="${spid}"/></p:tgtEl></p:cBhvr></p:animMotion>`;
}

describe("parseTimeline on hand-written slides", () => {
  it("is a plain slide without timing", () => {
    const t = parseTimeline(slide(""));
    expect(t).toMatchObject({ steps: [[]], transition: null, interactive: [], approximate: null });
    expect(buildStates(t)).toEqual([{ hidden: [], moved: {} }]);
  });

  it("degrades XML scan refuses to a plain slide, never throwing", () => {
    for (const xml of ["", "<p:sld", slide("<p:timing><p:tnLst>"), "<a></b>", "not xml at all", "<!DOCTYPE x><x/>"]) {
      expect(parseTimeline(xml)).toMatchObject({ steps: [[]], transition: null, approximate: "unreadable" });
    }
  });

  it("tolerates unexpected structure", () => {
    const odd = [
      slide("<p:timing/>"),
      slide("<p:timing><p:tnLst><p:par/></p:tnLst></p:timing>"),
      slide(mainSeq([["<p:par/>", "<p:par><p:cTn/></p:par>"]])),
      slide(mainSeq([[effect(5, "clickEffect", "entr", 10, "<p:set/><p:anim/><p:animMotion/>")]])),
    ];
    for (const xml of odd) {
      const t = parseTimeline(xml);
      expect(t.steps.length).toBeGreaterThan(0);
      expect(buildStates(t)).toHaveLength(t.steps.length);
    }
  });

  it("reads a transition's speed when it has no p14:dur", () => {
    expect(parseTimeline(slide("<p:transition/>")).transition).toMatchObject({ kind: "none", dur: 500 });
    expect(parseTimeline(slide('<p:transition spd="slow"><p:checker/></p:transition>')).transition).toMatchObject({
      kind: "other",
      element: "checker",
      dur: 1000,
    });
  });

  it("names an effect outside the vocabulary `other`, keeping its numbers and filter", () => {
    const t = parseTimeline(
      slide(mainSeq([[effect(5, "clickEffect", "entr", 5, setVis("7", "visible") + animEffect("7", "checkerboard(across)"))]])),
    );
    const e = t.steps[1]![0]!;
    expect(e).toMatchObject({ name: "other", presetClass: "entr", preset: 5, sets: "visible" });
    expect(e.behaviours.find((b) => b.kind === "effect")).toMatchObject({ filter: "checkerboard(across)" });
    expect(effectName("verb", 1)).toBe("other");
    expect(effectName("entr", 25)).toBe("other");
  });

  it("reads integers strictly", () => {
    const fx = (attrs: string) =>
      parseTimeline(
        slide(
          mainSeq([
            [
              `<p:par><p:cTn id="5" presetClass="entr" ${attrs} nodeType="clickEffect"><p:childTnLst>${setVis("2", "visible")}${animEffect("2", "fade", 500)}</p:childTnLst></p:cTn></p:par>`,
            ],
          ]),
        ),
      ).steps[1]![0]!;
    expect(fx('presetID=" 10" presetSubtype="1e1"')).toMatchObject({ preset: 0, subtype: 0 });
    expect(fx('presetID="0010" presetSubtype="-4"')).toMatchObject({ preset: 10, subtype: -4 });
    expect(fx('presetID="1234567890"').preset).toBe(0);
    expect(fx('repeatCount="2000.0"').dur).toBe(500);
  });

  it("keeps the 1 ms of a visibility set instant, and picks the set that ends last", () => {
    const t = parseTimeline(
      slide(mainSeq([[effect(5, "clickEffect", "emph", 99, setVis("2", "hidden", 0) + setVis("2", "visible", 300) + setVis("2", "hidden", 200))]])),
    );
    expect(t.steps[1]![0]).toMatchObject({ dur: 300, sets: "visible" });
  });

  it("refuses hostile strings: nodeType, spd and spid that name prototype members", () => {
    const t = parseTimeline(
      slide(
        '<p:transition spd="__proto__"><p:fade/></p:transition>' +
          mainSeq([
            [
              effect(5, "constructor", "entr", 1, setVis("2", "visible")),
              effect(6, "hasOwnProperty", "entr", 1, setVis("__proto__", "visible")),
              effect(7, "withEffect", "path", 0, motionTo("constructor", "M 0 0 L 0.5 0 E")),
            ],
          ]),
      ),
    );
    expect(t.transition).toMatchObject({ kind: "fade", dur: 500 });
    expect(t.steps[1]!.map((e) => [e.trigger, e.target])).toEqual([
      ["click", { spid: "2", bg: false, para: null }],
      ["with", null],
      ["with", null],
    ]);
    const states = buildStates(t);
    expect(states.map((s) => s.hidden)).toEqual([["2"], []]);
    expect(states.map((s) => Object.keys(s.moved))).toEqual([[], []]);
    expect(Object.getPrototypeOf(states[0]!.moved)).toBeNull();
    for (const id of ["", "0x1", "12345678901", " 2", "2 ", "+2", "-2", "٣"]) {
      const one = parseTimeline(slide(mainSeq([[effect(5, "clickEffect", "entr", 1, setVis(id, "visible"))]])));
      expect(one.steps[1]![0]!.target, JSON.stringify(id)).toBeNull();
    }
  });

  it("drops timing past the caps, keeping the transition", () => {
    const tr = "<p:transition><p:fade/></p:transition>";
    const fx = (n: number) => Array.from({ length: n }, (_, i) => effect(i, "clickEffect", "entr", 1, setVis(String(i + 1), "visible")));

    // Bare effect nodes, so the count (not the 512 KiB cap) is what trips.
    const bare = (n: number) => Array.from({ length: n }, () => '<p:par><p:cTn presetClass="entr" nodeType="withEffect"/></p:par>');
    const many = parseTimeline(slide(tr + mainSeq([bare(TIMING_CAPS.effects + 1)])));
    expect(many).toMatchObject({ steps: [[]], approximate: "too-many-effects" });
    expect(many.transition?.kind).toBe("fade");
    expect(parseTimeline(slide(tr + mainSeq([bare(TIMING_CAPS.effects)]))).approximate).toBeNull();

    const clicks = parseTimeline(slide(mainSeq(fx(TIMING_CAPS.clicks + 1).map((e) => [e]))));
    expect(clicks).toMatchObject({ steps: [[]], approximate: "too-many-clicks" });
    expect(parseTimeline(slide(mainSeq(fx(TIMING_CAPS.clicks).map((e) => [e])))).steps).toHaveLength(TIMING_CAPS.clicks + 1);

    // mainSeq() nests root par 1, seq 2, group 3, sub-group 4; each wrapper adds one.
    const nest = (n: number) => {
      let deep = effect(5, "clickEffect", "entr", 1, setVis("2", "visible"));
      for (let i = 0; i < n; i++) deep = `<p:par><p:cTn><p:childTnLst>${deep}</p:childTnLst></p:cTn></p:par>`;
      return parseTimeline(slide(mainSeq([[deep]])));
    };
    expect(nest(TIMING_CAPS.depth - 5).approximate).toBeNull(); // the effect par is at 32
    expect(nest(TIMING_CAPS.depth - 4)).toMatchObject({ steps: [[]], approximate: "too-deep" });

    const pad = (n: number) => `<!--${"x".repeat(n)}-->`;
    const sized = (n: number) => slide(mainSeq([[pad(n) + effect(5, "clickEffect", "entr", 1, setVis("2", "visible"))]]));
    const base = sized(0);
    const timingLen = base.indexOf("</p:timing>") + "</p:timing>".length - base.indexOf("<p:timing>");
    expect(parseTimeline(sized(TIMING_CAPS.timingBytes - timingLen)).approximate).toBeNull();
    expect(parseTimeline(sized(TIMING_CAPS.timingBytes - timingLen + 1)).approximate).toBe("timing-too-large");
    // Measured in UTF-8 bytes: "é" is two.
    const half = Math.ceil((TIMING_CAPS.timingBytes - timingLen + 1) / 2);
    const accented = slide(mainSeq([[`<!--${"é".repeat(half)}-->` + effect(5, "clickEffect", "entr", 1, setVis("2", "visible"))]]));
    expect(accented.length).toBeLessThan(TIMING_CAPS.timingBytes);
    expect(parseTimeline(accented).approximate).toBe("timing-too-large");
  });

  it("refuses a slide part over 4 MiB of UTF-8 before scanning it", () => {
    const filler = (s: string, n: number) => slide(`<!--${s.repeat(n)}-->`);
    const over = parseTimeline(filler("x", TIMING_CAPS.partBytes));
    expect(over).toMatchObject({ steps: [[]], transition: null, approximate: "part-too-large" });
    const fits = slide("");
    expect(parseTimeline(filler("x", TIMING_CAPS.partBytes - fits.length - 7)).approximate).toBeNull();
    // Under 4 Mi UTF-16 units but over 4 MiB of UTF-8.
    expect(parseTimeline(filler("€", Math.ceil(TIMING_CAPS.partBytes / 3))).approximate).toBe("part-too-large");
  });

  it("reads paths' end points", () => {
    expect(pathEnd("M 0 0 L 0.25 0 E")).toEqual([0.25, 0]);
    expect(pathEnd("M 0 0 C 0.1 0 0.2 0.1 0.3 0.4 E")).toEqual([0.3, 0.4]);
    expect(pathEnd("M 0 0 0.1 0.1 Z")).toEqual([0, 0]);
    expect(pathEnd("M 0.5 0.5 0.1 0.1 Z E")).toEqual([0.5, 0.5]);
    expect(pathEnd("M 0 0 L 0.1 0.2 X 5 5 L 1 1")).toEqual([0.1, 0.2]);
    expect(pathEnd("M 0 0 L 0.1 0.2 E L 1 1")).toEqual([0.1, 0.2]);
    expect(pathEnd("M 0 0 l 0.1 0.1 l 0.1 0.1 E")[0]).toBeCloseTo(0.2);
    expect(pathEnd("garbage")).toEqual([0, 0]);
    expect(pathEnd("M 0 0 L 0.1")).toEqual([0, 0]);
  });
});

describe("the hand-written cases", () => {
  it("an on-shape-click sequence is counted, kept out of the steps, and only hides its target at first", () => {
    const t = parseTimeline(caseXml("interactive-trigger"));
    expect(t.approximate).toBeNull();
    expect(t.interactive).toHaveLength(1);
    expect(t.interactive[0]!.trigger).toBe("2");
    expect(t.interactive[0]!.steps.map((s) => s.map((e) => [e.name, e.target?.spid]))).toEqual([[["fade-in", "3"]]]);
    expect(t.steps.map((s) => s.map((e) => e.target?.spid))).toEqual([[], ["4"], ["3"]]);
    expect(buildStates(t).map((s) => s.hidden)).toEqual([["3", "4"], ["3"], []]);
  });

  it("a filled box built by paragraph keeps bg, paragraph and shape keys apart", () => {
    const t = parseTimeline(caseXml("bg-paragraphs"));
    expect(t.steps.map((s) => s.map((e) => e.target && targetKey(e.target)))).toEqual([[], ["2:bg", "2:p0-0"], ["2:p1-1"], ["2"]]);
    expect(buildStates(t).map((s) => s.hidden)).toEqual([["2:bg", "2:p0-0", "2:p1-1"], ["2:p1-1"], [], ["2"]]);
  });

  it("repeatCount and autoRev multiply at both levels, rounded once", () => {
    const durs = parseTimeline(caseXml("repeat-autorev")).steps.slice(1).map((s) => s[0]!.dur);
    expect(durs).toEqual([1000, 1000, 3000, 500, 749, null, null]);
  });

  it("an effect without nodeType is a click first in its group, else with", () => {
    const t = parseTimeline(caseXml("default-trigger"));
    expect(t.steps.map((s) => s.map((e) => e.trigger))).toEqual([[], ["click", "with"], ["click", "with"]]);
  });

  it("an empty click group is not a click", () => {
    const t = parseTimeline(caseXml("empty-group"));
    expect(t.steps.map((s) => s.map((e) => e.target?.spid))).toEqual([[], ["2"], ["3"]]);
  });

  it("nesting past the cap is a plain slide that keeps its transition", () => {
    expect(parseTimeline(caseXml("over-nesting"))).toMatchObject({
      steps: [[]],
      approximate: "too-deep",
      transition: { kind: "fade", dur: 750 },
    });
  });

  it("a transition without AlternateContent, with advClick=0", () => {
    expect(parseTimeline(caseXml("transition-plain")).transition).toEqual({
      kind: "wipe",
      dir: "d",
      dur: 750,
      advanceAfter: 4000,
      advanceOnClick: false,
      morph: null,
      element: "wipe",
    });
  });

  it("a Choice that requires an unknown namespace loses to the Fallback", () => {
    expect(parseTimeline(caseXml("transition-unknown-choice")).transition).toMatchObject({ kind: "fade", dur: 1000 });
  });

  it("paths sum unrounded, then round half away from zero, never to -0", () => {
    const states = buildStates(parseTimeline(caseXml("path-edges")));
    expect(states.at(-1)!.moved).toEqual({
      "2": [0, 0],
      "3": [0.1, 0.2],
      "4": [0.0003, -0.0003],
      "5": [0, 0],
      "6": [0.1, 0.1],
      "7": [0.0001, 0],
    });
    expect(Object.is(states.at(-1)!.moved["5"]![0], 0)).toBe(true);
  });

  it("hostile strings reach no lookup", () => {
    const g = caseGolden("hostile-strings");
    expect(g.transition).toMatchObject({ kind: "fade", dur: 500 });
    expect(g.steps[1]!.map((e) => [e.trigger, e.target, e.effect])).toEqual([
      ["click", "2", "appear"],
      ["with", null, "fade-in"],
      ["with", null, "other"],
      ["with", null, "appear"],
      ["with", null, "appear"],
    ]);
    // " 500" is no integer (delay 0); "1e3" no dur, so only the instant set ends.
    expect(g.steps[2]![0]).toMatchObject({ start: 0, dur: 0, target: "3" });
    expect(g.states.map((s) => s.hidden)).toEqual([["2", "3"], ["3"], [], []]);
    expect(g.states.every((s) => Object.keys(s.moved).length === 0)).toBe(true);
  });
});
