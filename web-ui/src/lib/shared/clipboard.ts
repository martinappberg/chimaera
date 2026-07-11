import { writeClipboard } from "../net/native";

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
