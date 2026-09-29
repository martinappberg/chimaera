/**
 * The composer textarea's handling of upload short forms (`uploadTokens.ts`)
 * as whole units — chips in a plain textarea. Attached to the field, it:
 *
 * - keeps the caret out of a short form and a selection from cutting one
 *   (a click inside lands at its nearer edge; a double-click, a drag or a
 *   Shift+arrow takes it whole);
 * - steps the arrow keys over one, and lets Backspace / Delete at its edge
 *   remove all of it (it selects, the key deletes: one native undo step);
 * - shapes an edit that would glue text onto one before it happens: typing
 *   or pasting against its end puts a space first, and deleting the space
 *   that keeps it apart from the next word does nothing (`keepsTokens`);
 * - settles, after the fact, the rare edit it could not shape (a word or
 *   line deletion that reached into one, an IME commit) — `settleEdit`;
 * - copies and cuts the whole mention, and shows a pasted whole mention of
 *   a landing-pad file short again.
 *
 * Every change it makes goes through `execCommand`, one edit each, so the
 * field's own undo stays whole.
 *
 * Its listeners are native, so they run before the composer's own
 * (delegated) key handling. The textarea still owns every glyph, the caret,
 * IME and undo.
 */

import type { Attachment } from "svelte/attachments";
import {
  collapseUploadMentions,
  expandUploadMentions,
  keepsTokens,
  settleEdit,
  snapRange,
  tokenSpans,
  type UploadTokens,
} from "./uploadTokens";

/** Replace `el.value[from, to)` with `text` as a user edit would (native
 *  undo keeps it); a field without `execCommand` still gets the text. */
function replaceRange(el: HTMLTextAreaElement, from: number, to: number, text: string): void {
  el.setSelectionRange(from, to);
  const done =
    text.length > 0 ? document.execCommand("insertText", false, text) : from === to || document.execCommand("delete");
  if (done) return;
  el.setRangeText(text, from, to, "end");
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

/** Rewrite the field from `current` to `next`, touching only what differs. */
function rewrite(el: HTMLTextAreaElement, current: string, next: string): void {
  let head = 0;
  while (head < current.length && head < next.length && current[head] === next[head]) head++;
  let tail = 0;
  while (
    tail < current.length - head &&
    tail < next.length - head &&
    current[current.length - 1 - tail] === next[next.length - 1 - tail]
  ) {
    tail++;
  }
  replaceRange(el, head, current.length - tail, next.slice(head, next.length - tail));
}

/**
 * @param tokens the composer's short forms (read at every event)
 * @param moved called after the caret or selection was moved here, so the
 *   composer's caret-driven completions follow
 */
export function uploadChips(tokens: UploadTokens, moved: () => void): Attachment<HTMLTextAreaElement> {
  return (el) => {
    /** The field as it was before the edit in flight (set at `beforeinput`,
     *  or at composition start for the whole composition). */
    let base: string | null = null;
    let composing = false;
    let settling = false;
    let pointerDown = false;
    let commit: ReturnType<typeof setTimeout> | null = null;

    const spans = () => tokenSpans(el.value, tokens);

    function snap(): void {
      if (tokens.size === 0) return;
      const start = el.selectionStart;
      const end = el.selectionEnd;
      const [a, b] = snapRange(spans(), start, end);
      if (a === start && b === end) return;
      el.setSelectionRange(a, b, el.selectionDirection);
      moved();
    }

    function settle(before: string): void {
      if (settling) return;
      const after = el.value;
      const fix = settleEdit(before, after, el.selectionStart, tokens);
      if (fix === null) return;
      settling = true;
      try {
        rewrite(el, after, fix.text);
        el.setSelectionRange(fix.caret, fix.caret);
      } finally {
        settling = false;
      }
      moved();
    }

    function onKeydown(e: KeyboardEvent): void {
      if (e.isComposing || e.keyCode === 229 || e.metaKey || e.ctrlKey || tokens.size === 0) return;
      const key = e.key;
      if (key !== "ArrowLeft" && key !== "ArrowRight" && key !== "Backspace" && key !== "Delete") return;
      const start = el.selectionStart;
      const end = el.selectionEnd;
      if (key === "Backspace" || key === "Delete") {
        // A selection is already whole-chip (snapped): the key deletes it.
        if (start !== end) return;
        const hit = spans().find((s) => (key === "Backspace" ? s.end === start : s.start === start));
        // Select it; the key's own deletion then takes it in one undo step.
        if (hit !== undefined) {
          el.setSelectionRange(hit.start, hit.end);
          return;
        }
        // One character going (a word or line deletion's reach is the
        // browser's, settled after): the space that keeps a short form apart
        // from the next word stays.
        if (e.altKey) return;
        const from = key === "Backspace" ? start - 1 : start;
        if (from >= 0 && from < el.value.length && !keepsTokens(el.value, from, from + 1, "", tokens)) {
          e.preventDefault();
        }
        return;
      }
      const backward = el.selectionDirection === "backward";
      const focus = start === end || backward ? start : end;
      const anchor = start === end ? start : backward ? end : start;
      // Collapsing a selection with a plain arrow is the browser's to do.
      if (!e.shiftKey && start !== end) return;
      const left = key === "ArrowLeft";
      const hit = spans().find((s) => (left ? s.end === focus : s.start === focus));
      if (hit === undefined) return;
      const next = left ? hit.start : hit.end;
      e.preventDefault();
      if (e.shiftKey) {
        el.setSelectionRange(Math.min(anchor, next), Math.max(anchor, next), next < anchor ? "backward" : "forward");
      } else {
        el.setSelectionRange(next, next);
      }
      moved();
    }

    function onBeforeInput(e: InputEvent): void {
      if (composing || settling) return;
      base = el.value;
      // Typing against a short form's end: a space first, in the same edit.
      if (e.inputType !== "insertText" || e.isComposing || e.data === null || tokens.size === 0) return;
      const start = el.selectionStart;
      const end = el.selectionEnd;
      if (keepsTokens(el.value, start, end, e.data, tokens)) return;
      if (!keepsTokens(el.value, start, end, ` ${e.data}`, tokens)) return;
      e.preventDefault();
      settling = true;
      try {
        replaceRange(el, start, end, ` ${e.data}`);
      } finally {
        settling = false;
      }
      base = null;
      moved();
    }

    function onInput(e: Event): void {
      if (composing || (e as InputEvent).isComposing || settling || base === null) return;
      const before = base;
      base = null;
      settle(before);
    }

    function onCompositionStart(): void {
      composing = true;
      base = el.value;
    }

    function onCompositionEnd(): void {
      composing = false;
      // WebKit may still deliver the committing input after this event;
      // settle once everything for the commit has landed.
      if (commit !== null) clearTimeout(commit);
      commit = setTimeout(() => {
        commit = null;
        if (base === null || composing) return;
        const before = base;
        base = null;
        settle(before);
      }, 0);
    }

    function onCopy(e: ClipboardEvent, cut: boolean): void {
      const start = el.selectionStart;
      const end = el.selectionEnd;
      if (start === end || e.clipboardData === null) return;
      const picked = el.value.slice(start, end);
      const whole = expandUploadMentions(picked, tokens);
      if (whole === picked) return;
      e.preventDefault();
      e.clipboardData.setData("text/plain", whole);
      if (cut) replaceRange(el, start, end, "");
    }

    function onPaste(e: ClipboardEvent): void {
      const data = e.clipboardData;
      // A picture paste is the composer's attachment path.
      if (data === null || [...data.items].some((i) => i.type.startsWith("image/"))) return;
      const text = data.getData("text/plain");
      if (text === "" || (tokens.size === 0 && !text.includes("/uploads/"))) return;
      const start = el.selectionStart;
      const end = el.selectionEnd;
      let shown = text.includes("/uploads/")
        ? collapseUploadMentions(text, tokens, el.value.slice(0, start) + el.value.slice(end))
        : text;
      // Pasted against a short form's end: a space first, as typing does.
      if (!keepsTokens(el.value, start, end, shown, tokens) && keepsTokens(el.value, start, end, ` ${shown}`, tokens)) {
        shown = ` ${shown}`;
      }
      if (shown === text) return;
      e.preventDefault();
      replaceRange(el, start, end, shown);
    }

    function onSelectionChange(): void {
      if (document.activeElement === el && !pointerDown) snap();
    }

    function onPointerDown(): void {
      pointerDown = true;
    }

    function onPointerUp(): void {
      if (!pointerDown) return;
      pointerDown = false;
      if (document.activeElement === el) snap();
    }

    const copy = (e: ClipboardEvent) => onCopy(e, false);
    const cut = (e: ClipboardEvent) => onCopy(e, true);
    el.addEventListener("keydown", onKeydown);
    el.addEventListener("beforeinput", onBeforeInput);
    el.addEventListener("input", onInput);
    el.addEventListener("compositionstart", onCompositionStart);
    el.addEventListener("compositionend", onCompositionEnd);
    el.addEventListener("copy", copy);
    el.addEventListener("cut", cut);
    el.addEventListener("paste", onPaste);
    el.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("pointerup", onPointerUp);
    window.addEventListener("pointercancel", onPointerUp);
    document.addEventListener("selectionchange", onSelectionChange);
    return () => {
      if (commit !== null) clearTimeout(commit);
      el.removeEventListener("keydown", onKeydown);
      el.removeEventListener("beforeinput", onBeforeInput);
      el.removeEventListener("input", onInput);
      el.removeEventListener("compositionstart", onCompositionStart);
      el.removeEventListener("compositionend", onCompositionEnd);
      el.removeEventListener("copy", copy);
      el.removeEventListener("cut", cut);
      el.removeEventListener("paste", onPaste);
      el.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("pointerup", onPointerUp);
      window.removeEventListener("pointercancel", onPointerUp);
      document.removeEventListener("selectionchange", onSelectionChange);
    };
  };
}
