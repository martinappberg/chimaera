/** macOS shells expect Option+Left/Right to send readline's Meta-b/f.
 * xterm 6 sends CSI 1;3D/C instead, which stock remote Bash may insert as
 * literal D/C. Keep full-screen TUIs and application-cursor mode on xterm's
 * native encoding, as well as every other modifier and Option character. */
export function wordNavigationInput(
  event: Pick<KeyboardEvent, "type" | "key" | "altKey" | "ctrlKey" | "metaKey" | "shiftKey" | "isComposing">,
  mac: boolean,
  buffer: "normal" | "alternate",
  applicationCursorKeys: boolean,
): string | null {
  if (
    !mac || event.type !== "keydown" || event.isComposing ||
    !event.altKey || event.ctrlKey || event.metaKey || event.shiftKey ||
    buffer !== "normal" || applicationCursorKeys
  ) return null;
  if (event.key === "ArrowLeft") return "\x1bb";
  if (event.key === "ArrowRight") return "\x1bf";
  return null;
}
