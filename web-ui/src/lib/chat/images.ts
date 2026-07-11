/**
 * Image attachments for the chat composer: one downscale/encode pipeline
 * shared by clipboard paste (Composer) and OS-desktop drops (App), so both
 * intake paths produce identical, size-bounded attachments.
 */

export interface ImageAttachment {
  media_type: string;
  data: string;
  /** Display label, e.g. "screenshot 412×280". */
  label: string;
}

/** Downscale cap matching the API's optimal image size. */
export const IMAGE_MAX_DIM = 1568;
/** Post-encode payload cap; the journal stores a placeholder anyway. */
export const IMAGE_MAX_BASE64 = 2 * 1024 * 1024;

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
    };
  } catch {
    return null;
  }
}
