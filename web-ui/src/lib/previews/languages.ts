/**
 * The languages the editor, the compare view and read-only code know by file
 * name or fence: CodeMirror's own (`@codemirror/language-data`, each loaded on
 * first use) plus Typst, whose files the Typst plugin builds. Typst brings its
 * parser and its editing aids (indentation, Enter in a list, heading folds) but
 * not its own highlight styles or syntax linter: it reads in the app's --syn-*
 * colors like every other language, and the plugin's build errors stay the only
 * marks in the gutter.
 */

import { HighlightStyle, LanguageDescription, LanguageSupport, syntaxHighlighting } from "@codemirror/language";
import { languages as shipped } from "@codemirror/language-data";

import { codeHighlightRules } from "./cm";

const typst = LanguageDescription.of({
  name: "Typst",
  alias: ["typ"],
  extensions: ["typ"],
  async load() {
    const m = await import("codemirror-lang-typst/lezer");
    // Raw blocks (```python …```) highlight in their own language.
    const { language } = m.typst_lezer({ codeLanguages: shipped });
    return new LanguageSupport(language, [
      m.typstLezerIndentService,
      m.typstLezerListKeymap,
      m.typstLezerFoldService,
      // The editors register the app's style as a fallback, which any
      // highlighter a language brings switches off: Typst's carries the
      // app's rules, plus the three tags it adds to the standard set.
      syntaxHighlighting(
        HighlightStyle.define([
          ...codeHighlightRules,
          { tag: m.typstTags.interpolated, color: "var(--syn-func)" },
          { tag: m.typstTags.mathDelimiter, color: "var(--syn-number)" },
          { tag: m.typstTags.listMarker, color: "var(--syn-def)" },
        ]),
      ),
    ]);
  },
});

export const languages: readonly LanguageDescription[] = [...shipped, typst];
