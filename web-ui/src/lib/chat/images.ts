/**
 * Image attachments for the chat composer: one downscale/encode pipeline
 * shared by clipboard paste (Composer) and OS-desktop drops (App), so both
 * intake paths produce identical, size-bounded attachments.
 */

import { frameWidth } from "../shared/embed/embed";

export interface ImageAttachment {
  media_type: string;
  data: string;
  /** Display label, e.g. "screenshot 412×280". */
  label: string;
  /** Encoded pixel size — the composer tile's aspect ratio. */
  width?: number;
  height?: number;
}

/** Downscale cap matching the API's optimal image size. */
export const IMAGE_MAX_DIM = 1568;
/** Post-encode payload cap; the journal stores a placeholder anyway. */
export const IMAGE_MAX_BASE64 = 2 * 1024 * 1024;
/** Per-message attachment bound. Without a count cap, repeated paste/drop
 *  could retain and serialize an unbounded number of 2 MiB base64 images. */
export const IMAGE_MAX_ATTACHMENTS = 4;

/**
 * Downscale an image blob to the API-optimal size, re-encode as PNG, and cap
 * the base64 payload. Null when the image is unreadable or too large even
 * after downscaling — callers skip it quietly.
 */
export async function imageToAttachment(blob: Blob): Promise<ImageAttachment | null> {
  try {
    const bitmap = await createImageBitmap(blob);
    const scale = Math.min(1, IMAGE_MAX_DIM / Math.max(bitmap.width, bitmap.height));
    const canvas = document.createElement("canvas");
    canvas.width = Math.round(bitmap.width * scale);
    canvas.height = Math.round(bitmap.height * scale);
    canvas.getContext("2d")?.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
    const url = canvas.toDataURL("image/png");
    const data = url.slice(url.indexOf(",") + 1);
    if (data.length > IMAGE_MAX_BASE64) return null;
    return {
      media_type: "image/png",
      data,
      label: `image ${canvas.width}×${canvas.height}`,
      width: canvas.width,
      height: canvas.height,
    };
  } catch {
    return null;
  }
}

/** The attachment as an `<img>` source, before it has a file anywhere. */
export function attachmentSrc(image: ImageAttachment): string {
  return `data:${image.media_type};base64,${image.data}`;
}

/** An attachment tile's box: one row height, the width from the picture's
 *  own aspect ratio (a square until the size is known), clamped so a
 *  panorama or a sliver stays a tile. A picture shorter than the row keeps
 *  its own size — never scaled up. */
export function tileBox(
  size: { width?: number | null; height?: number | null } | null,
  rowHeight: number,
  minWidth: number,
  maxWidth: number,
): { width: number; height: number } {
  const w = size?.width ?? 0;
  const h = size?.height ?? 0;
  if (w <= 0 || h <= 0) return { width: rowHeight, height: rowHeight };
  const width = Math.min(maxWidth, Math.max(minWidth, frameWidth({ w, h }, rowHeight)));
  return { width, height: Math.min(rowHeight, h) };
}
