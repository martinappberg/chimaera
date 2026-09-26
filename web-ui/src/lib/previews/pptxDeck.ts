/**
 * Loading a .pptx for PptxView: parse the package with pptx-renderer (media
 * and slide nodes decoded lazily, zip limits for untrusted input), take every
 * external load target out of its relationships first (`stripExternalRels`;
 * the renderer would otherwise point <img>/<video> at remote URLs while it
 * lays a slide out attached to the page), and read the speaker notes the
 * renderer doesn't model, straight from the package.
 */

import JSZip from "jszip";
import {
  buildPresentation,
  parseZipLazyMedia,
  RECOMMENDED_ZIP_LIMITS,
  type PptxFiles,
  type PresentationData,
} from "@aiden0z/pptx-renderer";
import { stripExternalRels } from "./officeSafety";

export interface Deck {
  presentation: PresentationData;
  /** Speaker notes per slide (index-aligned; "" when a slide has none). */
  notes: string[];
  /** External load targets removed from the package. */
  blocked: number;
}

function stripMap(map: Map<string, string> | undefined): number {
  if (map === undefined) return 0;
  let n = 0;
  for (const [k, xml] of map) {
    const res = stripExternalRels(xml);
    if (res.removed > 0) map.set(k, res.xml);
    n += res.removed;
  }
  return n;
}

function stripFiles(files: PptxFiles): number {
  let n = 0;
  const pres = stripExternalRels(files.presentationRels);
  files.presentationRels = pres.xml;
  n += pres.removed;
  n += stripMap(files.slideRels);
  n += stripMap(files.slideLayoutRels);
  n += stripMap(files.slideMasterRels);
  n += stripMap(files.chartRels);
  return n;
}

/** Text of a notes slide's body placeholder, one line per paragraph. */
export function notesText(xml: string): string {
  const doc = new DOMParser().parseFromString(xml, "application/xml");
  if (doc.getElementsByTagName("parsererror").length > 0) return "";
  const out: string[] = [];
  for (const sp of Array.from(doc.getElementsByTagNameNS("*", "sp"))) {
    const ph = sp.getElementsByTagNameNS("*", "ph")[0];
    if (ph === undefined || ph.getAttribute("type") !== "body") continue;
    for (const p of Array.from(sp.getElementsByTagNameNS("*", "p"))) {
      if (p.namespaceURI !== null && !/drawingml/.test(p.namespaceURI)) continue;
      const runs = Array.from(p.getElementsByTagNameNS("*", "t")).map((t) => t.textContent ?? "");
      out.push(runs.join(""));
    }
  }
  return out.join("\n").replace(/\n{3,}/g, "\n\n").trim();
}

/** Where a slide's notes live, from its relationships (`notesSlide`). */
function notesPath(slidePath: string, rels: Map<string, { type: string; target: string }>): string | null {
  for (const rel of rels.values()) {
    if (!/\/notesSlide$/i.test(rel.type)) continue;
    const base = slidePath.split("/").slice(0, -1);
    for (const seg of rel.target.split("/")) {
      if (seg === "..") base.pop();
      else if (seg !== "." && seg !== "") base.push(seg);
    }
    return rel.target.startsWith("/") ? rel.target.slice(1) : base.join("/");
  }
  return null;
}

export async function loadDeck(bytes: ArrayBuffer): Promise<Deck> {
  const files = await parseZipLazyMedia(bytes, RECOMMENDED_ZIP_LIMITS);
  const blocked = stripFiles(files);
  const presentation = buildPresentation(files, { lazySlides: true });
  return { presentation, notes: presentation.slides.map(() => ""), blocked };
}

/** Fill `deck.notes` from the package (after the first slide is up: notes
 *  never hold up the stage). Bounded: at most 64 KB of text per slide. */
export async function loadNotes(bytes: ArrayBuffer, deck: Deck): Promise<boolean> {
  const zip = await JSZip.loadAsync(bytes);
  let any = false;
  await Promise.all(
    deck.presentation.slides.map(async (slide, i) => {
      const p = notesPath(slide.slidePath, slide.rels);
      const file = p === null ? null : zip.file(p);
      if (file === null || file === undefined) return;
      const text = notesText(await file.async("string")).slice(0, 64 * 1024);
      deck.notes[i] = text;
      if (text !== "") any = true;
    }),
  );
  return any;
}
