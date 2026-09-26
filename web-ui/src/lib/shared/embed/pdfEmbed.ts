/**
 * pdf.js for embed cards, set up exactly like the PDF viewer (PdfView): the
 * legacy build (pdf.js 6's modern build calls `Map.prototype.
 * getOrInsertComputed`, which WebKit and Chromium before 145 lack), the
 * worker and the font/CMap/wasm/ICC data bundled locally (no CDN), and
 * ranged reads only (`disableAutoFetch` + `disableStream`) so a card that
 * shows page 3 fetches page 3's bytes, not the file. Imported lazily: the
 * library loads the first time a card draws a PDF.
 */

import * as pdfjs from "pdfjs-dist/legacy/build/pdf.mjs";
import type { PDFDocumentProxy } from "pdfjs-dist";
import workerUrl from "pdfjs-dist/legacy/build/pdf.worker.min.mjs?url";

pdfjs.GlobalWorkerOptions.workerSrc = workerUrl;

/** Where the build ships pdf.js's data (see `pdfjsAssets` in vite.config.ts). */
const PDFJS_DATA = new URL(`${import.meta.env.BASE_URL}assets/pdfjs-${pdfjs.version}/`, location.href).href;

export interface OpenedPdf {
  doc: Promise<PDFDocumentProxy>;
  destroy(): void;
}

export function openPdf(url: string): OpenedPdf {
  const task = pdfjs.getDocument({
    url,
    disableAutoFetch: true,
    disableStream: true,
    cMapUrl: `${PDFJS_DATA}cmaps/`,
    cMapPacked: true,
    standardFontDataUrl: `${PDFJS_DATA}standard_fonts/`,
    wasmUrl: `${PDFJS_DATA}wasm/`,
    iccUrl: `${PDFJS_DATA}iccs/`,
  });
  return {
    doc: task.promise,
    destroy: () => void task.destroy(),
  };
}
