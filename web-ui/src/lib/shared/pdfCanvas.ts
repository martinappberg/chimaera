/** Shared raster limits for PDF panes, embed cards and region snapshots. */
const MAX_PIXELS = 12_000_000;
const MAX_EDGE = 8192;

export function pdfRaster(width: number, height: number, desiredScale: number): {
  width: number;
  height: number;
  scale: number;
} {
  if (![width, height, desiredScale].every((n) => Number.isFinite(n) && n > 0)) {
    throw new Error("invalid PDF page dimensions");
  }
  const scale = Math.min(desiredScale, MAX_EDGE / width, MAX_EDGE / height, Math.sqrt(MAX_PIXELS / width / height));
  return {
    width: Math.max(1, Math.floor(width * scale)),
    height: Math.max(1, Math.floor(height * scale)),
    scale,
  };
}

interface RenderTask {
  promise: Promise<void>;
  cancel(): void;
}

/** pdf.js can reject during graphics initialization without releasing its
 * canvas reservation. Cancel failed tasks too, before discarding the handle. */
export async function finishPdfRender(task: RenderTask): Promise<void> {
  try {
    await task.promise;
  } catch (error) {
    try {
      task.cancel();
    } catch {
      // Partially initialized graphics can fail cleanup; retain the real error.
    }
    throw error;
  }
}
