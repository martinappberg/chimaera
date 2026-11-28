/**
 * A page's `reference` picture (web/1) as a Blob: the one a frame posted, a
 * native view's data URL, or a rect of the native view the host captures
 * itself (`PaneViewControl.snapshot`). Bounded like the image area tool's
 * crops: past `CROP_CAP` px a side it is scaled down.
 */
import type { PaneViewControl } from "./paneView";
import type { WebCrop } from "./paneViewBridge";

const CROP_CAP = 1568;

async function fromDataUrl(url: string): Promise<Blob | null> {
  try {
    return await (await fetch(url)).blob();
  } catch {
    return null;
  }
}

async function canvasBlob(c: HTMLCanvasElement): Promise<Blob | null> {
  return new Promise((resolve) => c.toBlob((b) => resolve(b), "image/png"));
}

/** The picture `crop` names, or null when there is none to take. */
export async function cropBlob(crop: WebCrop | null, control: PaneViewControl | null): Promise<Blob | null> {
  if (crop === null) return null;
  if ("blob" in crop) return crop.blob;
  if ("dataUrl" in crop) return fromDataUrl(crop.dataUrl);
  if (control === null) return null;
  const shot = await control.snapshot();
  if (shot === null || shot.width <= 0 || shot.height <= 0) return null;
  const png = await fromDataUrl(`data:image/png;base64,${shot.png}`);
  if (png === null) return null;
  let bitmap: ImageBitmap;
  try {
    bitmap = await createImageBitmap(png);
  } catch {
    return null;
  }
  // The snapshot is the viewport in device pixels; the rect is CSS px.
  const scale = bitmap.width / shot.width;
  const x = Math.max(0, crop.rect.x * scale);
  const y = Math.max(0, crop.rect.y * scale);
  const w = Math.min(bitmap.width - x, crop.rect.width * scale);
  const h = Math.min(bitmap.height - y, crop.rect.height * scale);
  if (w < 1 || h < 1) return null;
  const fit = Math.min(1, CROP_CAP / Math.max(w, h));
  const canvas = document.createElement("canvas");
  canvas.width = Math.round(w * fit);
  canvas.height = Math.round(h * fit);
  canvas.getContext("2d")?.drawImage(bitmap, x, y, w, h, 0, 0, canvas.width, canvas.height);
  bitmap.close();
  return canvasBlob(canvas);
}

/** A reference's words for the agent: the page's text, then the file and
 *  line it points at when it named one. */
export function referenceText(text: string, file: string | null, line: number | null): string {
  if (file === null) return text;
  const at = line !== null ? `${file}:${line}` : file;
  return text.trim() === "" ? at : `${text} (${at})`;
}
