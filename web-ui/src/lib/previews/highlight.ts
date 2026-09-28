/**
 * Static syntax highlighting for read-only code blocks (notebook cells):
 * the language parsers CodeMirror already ships (`@codemirror/language-data`,
 * each loaded on first use) run once over the text, and the result is built
 * as text nodes and classed spans. The classes map onto the same `--syn-*`
 * tokens and tag groups as the editor's highlight style (`cm.ts`), so a cell
 * reads like the same code opened in a tab.
 */

import { LanguageDescription } from "@codemirror/language";
import { languages } from "@codemirror/language-data";
import { highlightCode, tagHighlighter, tags as t } from "@lezer/highlight";
import type { Parser } from "@lezer/common";

export const codeHighlighter = tagHighlighter([
  { tag: [t.keyword, t.operatorKeyword, t.modifier, t.self], class: "hl-keyword" },
  { tag: [t.string, t.special(t.string), t.character, t.regexp, t.escape], class: "hl-string" },
  { tag: [t.comment, t.lineComment, t.blockComment], class: "hl-comment" },
  { tag: [t.number, t.integer, t.float, t.bool, t.null, t.atom], class: "hl-number" },
  { tag: [t.typeName, t.className, t.namespace, t.macroName, t.tagName], class: "hl-type" },
  { tag: [t.function(t.variableName), t.function(t.propertyName)], class: "hl-func" },
  { tag: [t.definition(t.variableName), t.constant(t.variableName)], class: "hl-def" },
  { tag: [t.propertyName, t.attributeName], class: "hl-prop" },
  { tag: t.invalid, class: "hl-invalid" },
]);

const parsers = new Map<string, Promise<Parser | null>>();

/** The parser for a language name ("python", "R", "julia"…), or null. */
export function parserFor(name: string | null): Promise<Parser | null> {
  const key = (name ?? "").toLowerCase();
  let p = parsers.get(key);
  if (p === undefined) {
    const alias = key === "ipython" || key === "ipython3" ? "python" : key;
    const desc = alias === "" ? null : LanguageDescription.matchLanguageName(languages, alias, true);
    p =
      desc === null
        ? Promise.resolve(null)
        : desc.load().then(
            (support) => support.language.parser,
            () => {
              parsers.delete(key);
              return null;
            },
          );
    parsers.set(key, p);
  }
  return p;
}

/** Replace `el`'s content with `code`, highlighted when a parser is given. */
export function renderCode(el: HTMLElement, code: string, parser: Parser | null): void {
  const frag = document.createDocumentFragment();
  if (parser === null) {
    frag.appendChild(document.createTextNode(code));
  } else {
    highlightCode(
      code,
      parser.parse(code),
      codeHighlighter,
      (text, classes) => {
        if (classes === "") {
          frag.appendChild(document.createTextNode(text));
        } else {
          const span = document.createElement("span");
          span.className = classes;
          span.textContent = text;
          frag.appendChild(span);
        }
      },
      () => frag.appendChild(document.createTextNode("\n")),
    );
  }
  el.replaceChildren(frag);
}
