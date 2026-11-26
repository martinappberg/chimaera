/**
 * An arriving prompt (an approval, a question) takes the keyboard so Enter,
 * Esc and the number keys answer it — but never out from under typing: a
 * draft in this chat's composer, or a terminal, an editor or another chat
 * that has the focus, keeps it. Keys meant for those must not answer a
 * prompt that appeared mid-keystroke.
 */

/** The text field that has the focus, as the rule reads it; null when the
 *  focus is on anything else (a button, the page). */
export interface FocusedField {
  text: string;
  /** Inside the chat pane the prompt belongs to (its composer). */
  inChat: boolean;
}

/** A field elsewhere is someone typing there; this chat's own composer
 *  yields only while it is empty. */
export function yieldsFocus(field: FocusedField | null): boolean {
  if (field === null) return true;
  return field.inChat && field.text.trim() === "";
}

export function mayTakeFocus(card: Element, active: Element | null = document.activeElement): boolean {
  if (!(active instanceof HTMLElement)) return true;
  const text =
    active instanceof HTMLTextAreaElement || active instanceof HTMLInputElement
      ? active.value
      : active.isContentEditable
        ? (active.textContent ?? "")
        : null;
  return yieldsFocus(text === null ? null : { text, inChat: card.closest(".chat")?.contains(active) === true });
}
