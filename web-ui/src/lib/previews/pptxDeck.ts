/**
 * Loading a .pptx for PptxView: parse the package with pptx-renderer (media
 * and slide nodes decoded lazily, zip limits for untrusted input), take every
 * external load target out of its relationships first (`stripExternalRels`;
 * the renderer would otherwise point <img>/<video> at remote URLs while it
 * lays a slide out attached to the page), and read the speaker notes the
 * renderer doesn't model, straight from the package — under the same entry
 * cap, and never inflating a notes part past its own small cap (a zip bomb in
 * `notesSlide*.xml` is skipped, not decompressed).
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
  /** Slides whose notes part was too large to read (skipped, never inflated). */
  notesSkipped: Set<number>;
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
  return { presentation, notes: presentation.slides.map(() => ""), notesSkipped: new Set(), blocked };
}

/** Most bytes one notes part may inflate to. A slide's notes are a few KB
 *  of XML, and at most NOTES_TEXT_MAX of their text is shown. */
export const NOTES_PART_MAX = 1024 * 1024;
/** Most bytes all notes parts together may declare. */
export const NOTES_TOTAL_MAX = 32 * 1024 * 1024;
/** Most characters of notes text kept per slide. */
const NOTES_TEXT_MAX = 64 * 1024;

/** JSZip's streaming reader: public in its source, missing from its types. */
interface ZipStream {
  on(event: "data", fn: (chunk: Uint8Array) => void): ZipStream;
  on(event: "error", fn: (e: Error) => void): ZipStream;
  on(event: "end", fn: () => void): ZipStream;
  pause(): ZipStream;
  resume(): ZipStream;
}

/** The size an entry's central directory declares, if it says. */
function declaredSize(file: JSZip.JSZipObject): number | null {
  const n = (file as unknown as { _data?: { uncompressedSize?: unknown } })._data?.uncompressedSize;
  return typeof n === "number" && Number.isFinite(n) && n >= 0 ? n : null;
}

/** An entry's bytes, inflated a chunk at a time and abandoned (null) once
 *  they pass `cap`: a declared size can lie, and a deflate stream of a few
 *  KB can expand a thousandfold. Nothing past the cap is kept; JSZip feeds
 *  the inflater 16 KB at a time, so pausing stops it within one block. */
function readCapped(file: JSZip.JSZipObject, cap: number): Promise<Uint8Array | null> {
  const stream = (file as unknown as { internalStream(type: "uint8array"): ZipStream }).internalStream("uint8array");
  return new Promise((resolve, reject) => {
    const parts: Uint8Array[] = [];
    let size = 0;
    let settled = false;
    stream
      .on("data", (chunk) => {
        if (settled) return;
        size += chunk.byteLength;
        if (size > cap) {
          settled = true;
          parts.length = 0;
          stream.pause();
          resolve(null);
          return;
        }
        parts.push(chunk);
      })
      .on("error", (e) => {
        if (settled) return;
        settled = true;
        reject(e);
      })
      .on("end", () => {
        if (settled) return;
        settled = true;
        const out = new Uint8Array(size);
        let at = 0;
        for (const p of parts) {
          out.set(p, at);
          at += p.byteLength;
        }
        resolve(out);
      })
      .resume();
  });
}

/** One slide's place in the package, as the notes reader needs it. */
export interface NotesSlideRef {
  slidePath: string;
  rels: Map<string, { type: string; target: string }>;
}

/**
 * Each slide's notes part as XML (index-aligned; null when it has none or
 * it was skipped), read under the deck's entry cap. A part is inflated only
 * when its declared size fits NOTES_PART_MAX and the NOTES_TOTAL_MAX budget,
 * and inflating stops at NOTES_PART_MAX whatever it declared; `skipped`
 * lists the slides whose notes were too large. One part at a time, so a
 * deck of thousands of slides never holds more than one part inflating.
 */
export async function readNotesParts(
  bytes: ArrayBuffer,
  slides: readonly NotesSlideRef[],
): Promise<{ xml: (string | null)[]; skipped: Set<number> }> {
  const xml: (string | null)[] = slides.map(() => null);
  const skipped = new Set<number>();
  const zip = await JSZip.loadAsync(bytes);
  if (Object.keys(zip.files).length > RECOMMENDED_ZIP_LIMITS.maxEntries) {
    slides.forEach((_, i) => skipped.add(i));
    return { xml, skipped };
  }
  const decoder = new TextDecoder();
  let budget = NOTES_TOTAL_MAX;
  for (const [i, slide] of slides.entries()) {
    const p = notesPath(slide.slidePath, slide.rels);
    const file = p === null ? null : zip.file(p);
    if (file === null || file === undefined) continue;
    const declared = declaredSize(file);
    if (declared === null || declared > NOTES_PART_MAX || declared > budget) {
      skipped.add(i);
      continue;
    }
    budget -= declared;
    const raw = await readCapped(file, NOTES_PART_MAX).catch(() => null);
    if (raw === null) {
      skipped.add(i);
      continue;
    }
    xml[i] = decoder.decode(raw);
  }
  return { xml, skipped };
}

/** Fill `deck.notes` from the package (after the first slide is up: notes
 *  never hold up the stage). Bounded: see `readNotesParts`, and at most
 *  64 KB of text per slide. True when any slide has notes, or had notes too
 *  large to show (the viewer says so on that slide). */
export async function loadNotes(bytes: ArrayBuffer, deck: Deck): Promise<boolean> {
  const { xml, skipped } = await readNotesParts(bytes, deck.presentation.slides);
  let any = skipped.size > 0;
  for (const [i, part] of xml.entries()) {
    if (part === null) continue;
    const text = notesText(part).slice(0, NOTES_TEXT_MAX);
    deck.notes[i] = text;
    if (text !== "") any = true;
  }
  deck.notesSkipped = skipped;
  return any;
}
