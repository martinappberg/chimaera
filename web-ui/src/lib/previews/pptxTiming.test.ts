import JSZip from "jszip";
import { describe, expect, it } from "vitest";
import { attr, scan, type XmlEl } from "./pptxXml";
import {
  buildStates,
  effectName,
  goldenSlide,
  parseTimeline,
  pathEnd,
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
const DECKS = fs
  .readdirSync(new URL("decks/", CORPUS))
  .filter((n) => n.endsWith(".pptx"))
  .sort();

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
  const zip = await JSZip.loadAsync(fs.readFileSync(new URL(`decks/${name}`, CORPUS)));
  const slides = (await deckSlides(zip)).map((s) => goldenSlide(s.part, s.id, parseTimeline(s.xml)));
  return { deck: name, slides };
}

async function slideOf(deck: string, n: number): Promise<SlideTimeline> {
  const zip = await JSZip.loadAsync(fs.readFileSync(new URL(`decks/${deck}.pptx`, CORPUS)));
  return parseTimeline((await deckSlides(zip))[n - 1]!.xml);
}

describe("the PowerPoint timing corpus", () => {
  it("has decks", () => {
    expect(DECKS.length).toBeGreaterThan(0);
  });

  for (const name of DECKS) {
    it(`${name} parses to its golden`, async () => {
      const got = await deckGolden(name);
      const file = new URL(`goldens/${name.replace(/\.pptx$/, "")}.json`, CORPUS);
      if (WRITE) {
        fs.mkdirSync(new URL("goldens/", CORPUS), { recursive: true });
        fs.writeFileSync(file, `${JSON.stringify(got, null, 2)}\n`);
        return;
      }
      expect(fs.existsSync(file), `no golden for ${name}: run npm run timing:golden`).toBe(true);
      expect(got).toEqual(JSON.parse(fs.readFileSync(file, "utf8")));
    });
  }

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

describe("parseTimeline on hand-written slides", () => {
  it("is a plain slide without timing", () => {
    const t = parseTimeline(slide(""));
    expect(t).toMatchObject({ steps: [[]], transition: null, interactive: [], approximate: null });
    expect(buildStates(t)).toEqual([{ hidden: [], moved: {} }]);
  });

  it("degrades malformed XML to a plain slide, never throwing", () => {
    for (const xml of ["", "<p:sld", slide("<p:timing><p:tnLst>"), "<a></b>", "not xml at all", "<!DOCTYPE x><x/>"]) {
      const t = parseTimeline(xml);
      expect(t.steps).toEqual([[]]);
      expect(t.approximate).toMatch(/unreadable XML/);
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

  it("reads a transition without AlternateContent, with its speed and advClick=0", () => {
    const t = parseTimeline(slide('<p:transition spd="med" advClick="0" advTm="4000"><p:wipe dir="d"/></p:transition>'));
    expect(t.transition).toEqual({
      kind: "wipe",
      dir: "d",
      dur: 750,
      advanceAfter: 4000,
      advanceOnClick: false,
      morph: null,
      element: "wipe",
    });
    expect(parseTimeline(slide("<p:transition/>")).transition).toMatchObject({ kind: "none", dur: 500 });
    expect(parseTimeline(slide('<p:transition spd="slow"><p:checker/></p:transition>')).transition).toMatchObject({
      kind: "other",
      element: "checker",
      dur: 1000,
    });
  });

  it("takes the Fallback when no Choice is understood", () => {
    const xml = slide(
      '<mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"><mc:Choice xmlns:x="urn:unknown" Requires="x"><p:transition><x:vortex/></p:transition></mc:Choice><mc:Fallback><p:transition spd="fast"><p:fade/></p:transition></mc:Fallback></mc:AlternateContent>',
    );
    expect(parseTimeline(xml).transition).toMatchObject({ kind: "fade", dur: 500 });
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

  it("drops timing past the caps, keeping the transition", () => {
    const tr = '<p:transition><p:fade/></p:transition>';
    const fx = (n: number) => Array.from({ length: n }, (_, i) => effect(i, "clickEffect", "entr", 1, setVis(String(i), "visible")));

    // Bare effect nodes, so the count (not the 512 KB cap) is what trips.
    const bare = Array.from({ length: TIMING_CAPS.effects + 1 }, () => '<p:par><p:cTn presetClass="entr" nodeType="withEffect"/></p:par>');
    const many = parseTimeline(slide(tr + mainSeq([bare])));
    expect(many).toMatchObject({ steps: [[]], approximate: expect.stringMatching(/effects/) });
    expect(many.transition?.kind).toBe("fade");

    const clicks = parseTimeline(slide(mainSeq(fx(TIMING_CAPS.clicks + 1).map((e) => [e]))));
    expect(clicks).toMatchObject({ steps: [[]], approximate: expect.stringMatching(/clicks/) });
    expect(parseTimeline(slide(mainSeq(fx(TIMING_CAPS.clicks).map((e) => [e])))).steps).toHaveLength(TIMING_CAPS.clicks + 1);

    let deep = "";
    for (let i = 0; i < TIMING_CAPS.depth; i++) deep = `<p:par><p:cTn>${deep ? `<p:childTnLst>${deep}</p:childTnLst>` : ""}</p:cTn></p:par>`;
    const nested = parseTimeline(slide(mainSeq([[deep]])));
    expect(nested).toMatchObject({ steps: [[]], approximate: expect.stringMatching(/deeper/) });

    const pad = `<!--${"x".repeat(TIMING_CAPS.xmlChars)}-->`;
    const big = parseTimeline(slide(mainSeq([[pad + effect(5, "clickEffect", "entr", 1, setVis("2", "visible"))]])));
    expect(big).toMatchObject({ steps: [[]], approximate: expect.stringMatching(/512 KB/) });
  });

  it("reads paths' end points", () => {
    expect(pathEnd("M 0 0 L 0.25 0 E")).toEqual([0.25, 0]);
    expect(pathEnd("M 0 0 C 0.1 0 0.2 0.1 0.3 0.4 E")).toEqual([0.3, 0.4]);
    expect(pathEnd("M 0 0 L 0.5 0.5 L 0.1 0.2 Z E")).toEqual([0, 0]);
    expect(pathEnd("M 0 0 l 0.1 0.1 l 0.1 0.1 E")[0]).toBeCloseTo(0.2);
    expect(pathEnd("garbage")).toEqual([0, 0]);
  });
});

describe("on-shape-click sequences", () => {
  const xml = fs.readFileSync(new URL("cases/interactive-trigger.xml", CORPUS), "utf8");

  it("are counted, kept out of the steps, and their targets hidden in every state", () => {
    const t = parseTimeline(xml);
    expect(t.approximate).toBeNull();
    expect(t.interactive).toHaveLength(1);
    expect(t.interactive[0]!.trigger).toBe("2");
    expect(t.interactive[0]!.steps.map((s) => s.map((e) => [e.name, e.target?.spid]))).toEqual([[["fade-in", "3"]]]);
    expect(t.steps.flat().map((e) => e.target?.spid)).toEqual(["4"]);
    expect(buildStates(t).map((s) => s.hidden)).toEqual([
      ["3", "4"],
      ["3"],
    ]);
    expect(goldenSlide("ppt/slides/slide1.xml", 256, t).interactive).toBe(1);
  });
});
