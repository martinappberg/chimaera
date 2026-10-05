import { copyFileToClipboard, hasLocalFiles, writeClipboard } from "../net/native";
import { fsRawUrl, isImagePath } from "../previews/files";

/**
 * Copy to the OS clipboard, native-shell first. WKWebView rejects
 * `navigator.clipboard.writeText` from a NON-gesture callback (an agent's OSC 52,
 * a selection change) with NotAllowedError — so on a remote window (app-only)
 * those copies silently failed. `writeClipboard` routes through the Rust process
 * (no gesture gate) inside the shell, and returns false in a plain browser, where
 * we fall back to `navigator.clipboard` (Chromium allows a focused-document write).
 *
 * Returns whether a write happened, so callers can gate "copied" feedback on it.
 */
export async function copyText(text: string): Promise<boolean> {
  if (await writeClipboard(text)) return true;
  try {
    await navigator.clipboard?.writeText(text);
    return true;
  } catch {
    // clipboard unavailable (denied, or no gesture in a plain browser) — nothing more to do
    return false;
  }
}

/** The bytes at `url` as a PNG — the one image type every clipboard takes. */
async function pngBlob(url: string): Promise<Blob> {
  let source = url;
  let fetched: string | null = null;
  try {
    const res = await fetch(url);
    if (!res.ok) throw new Error(`image request failed with status ${res.status}`);
    const blob = await res.blob();
    // Already PNG: hand over the file's own bytes, not a re-encode.
    if (blob.type === "image/png") return blob;
    fetched = URL.createObjectURL(blob);
    source = fetched;
  } catch {
    // Not fetchable from here (a data: URL under the page's policy): the
    // decoder below can still load what an <img> could.
  }
  try {
    const img = new Image();
    img.decoding = "async";
    img.src = source;
    await img.decode();
    const canvas = document.createElement("canvas");
    // An SVG without intrinsic dimensions reports 0: give it a usable box.
    canvas.width = img.naturalWidth || 1024;
    canvas.height = img.naturalHeight || 1024;
    const ctx = canvas.getContext("2d");
    if (ctx === null) throw new Error("no 2d canvas");
    ctx.drawImage(img, 0, 0, canvas.width, canvas.height);
    return await new Promise<Blob>((resolve, reject) => {
      canvas.toBlob((b) => (b === null ? reject(new Error("image could not be encoded")) : resolve(b)), "image/png");
    });
  } finally {
    if (fetched !== null) URL.revokeObjectURL(fetched);
  }
}

/**
 * Copy the picture at `src` (a URL, or one still being minted) to the OS
 * clipboard as an image, so it pastes into a chat, a mail or an editor.
 *
 * The write starts before the bytes exist — the item carries a promise —
 * because WebKit only honours a clipboard write made inside the user's
 * gesture, and fetching first would outlive it.
 */
export async function copyImage(src: string | Promise<string>): Promise<boolean> {
  if (typeof ClipboardItem === "undefined" || navigator.clipboard?.write === undefined) return false;
  const png = Promise.resolve(src).then(pngBlob);
  // A write refused up front never awaits the item; its failure is the write's.
  png.catch(() => {});
  try {
    await navigator.clipboard.write([new ClipboardItem({ "image/png": png })]);
    return true;
  } catch {
    return false;
  }
}

/**
 * Copy a daemon file to the OS clipboard, as far as this window can reach it:
 * the file itself where the daemon's files are this machine's (the native
 * shell on a local daemon — it pastes into Finder, a mail, a chat app), else
 * an image file's picture. Any other file on a remote daemon or in a browser
 * has no OS-clipboard form, and the call reports false.
 */
export async function copyFileToOs(path: string, kind: "dir" | "file"): Promise<boolean> {
  // One writer, chosen before any await: two would race for the clipboard,
  // and the image write must start inside the gesture.
  if (hasLocalFiles()) return copyFileToClipboard(path);
  if (kind === "file" && isImagePath(path)) return copyImage(fsRawUrl(path));
  return false;
}
