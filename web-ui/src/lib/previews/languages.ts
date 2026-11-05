/**
 * The languages the editor, the compare and diff views and read-only code know,
 * and how a file or a fence picks one. Every caller goes through `languageFor`
 * (a file) or `languageNamed` (a fence's info string), never CodeMirror's
 * matchers directly, so a name added here works everywhere at once.
 *
 * The list is CodeMirror's own (`@codemirror/language-data`, each loaded on
 * first use) plus a few dialects that research code is full of. Those borrow a
 * shipped grammar under their own name: Snakemake reads as Python, Nextflow as
 * Groovy, Makefiles, justfiles and Apptainer definitions as Shell. Close
 * enough to read, and nothing to maintain; a real grammar is an extension's
 * job. R Markdown and Quarto get Markdown with their `{r}` chunks highlighted.
 * Typst brings its parser and its editing aids (indentation, Enter in a list,
 * heading folds) but not its own highlight styles or syntax linter: it reads
 * in the app's --syn-* colors like every other language, and the Typst
 * plugin's build errors stay the only marks in the gutter.
 *
 * A file the list does not name by its name or extension (`job.sbatch`,
 * `.bashrc`, `Containerfile`) is placed by `FILE_NAMES`; one with no
 * extension at all by its `#!` line.
 */

import { HighlightStyle, LanguageDescription, LanguageSupport, syntaxHighlighting } from "@codemirror/language";
import { languages as shipped } from "@codemirror/language-data";

import { codeHighlightRules } from "./cm";
import { createPathMemory, type PathMemory } from "./pathMemory";

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

function shippedNamed(name: string): LanguageDescription {
  const d = shipped.find((l) => l.name === name);
  if (d === undefined) throw new Error(`@codemirror/language-data has no ${name}`);
  return d;
}

/** A dialect read with another language's grammar, under its own name. */
function borrowed(
  name: string,
  base: string,
  spec: { alias?: string[]; extensions?: string[]; filename?: RegExp },
): LanguageDescription {
  const from = shippedNamed(base);
  return LanguageDescription.of({ name, ...spec, load: () => from.load() });
}

/** `{r setup, include=FALSE}` / `{python}` / `r` → the chunk's language. */
export function chunkLanguage(info: string): string {
  const m = /^\s*\{?\s*([\w+#-]+)/.exec(info);
  return m === null ? "" : m[1];
}

const notebookMarkdown: Pick<LanguageDescription, "load">["load"] = async () => {
  const { markdown, markdownLanguage } = await import("@codemirror/lang-markdown");
  return markdown({
    base: markdownLanguage,
    codeLanguages: (info) => {
      const name = chunkLanguage(info);
      return name === "" ? null : languageNamed(name);
    },
  });
};

const dialects: LanguageDescription[] = [
  borrowed("Snakemake", "Python", { alias: ["snakefile", "smk"], extensions: ["smk"], filename: /^Snakefile$/ }),
  borrowed("Nextflow", "Groovy", { alias: ["nf"], extensions: ["nf"], filename: /^nextflow\.config$/ }),
  borrowed("Makefile", "Shell", { alias: ["make", "mk"], extensions: ["mk", "mak", "make"], filename: /^(GNUmakefile|[Mm]akefile)(\.(am|in))?$/ }),
  borrowed("Justfile", "Shell", { alias: ["just"], extensions: ["just"], filename: /^\.?[Jj]ustfile$/ }),
  borrowed("Apptainer", "Shell", { alias: ["singularity"], extensions: ["def"], filename: /^Singularity(\..+)?$/ }),
  LanguageDescription.of({
    name: "R Markdown",
    alias: ["rmd", "rmarkdown"],
    extensions: ["Rmd", "rmd", "Rmarkdown"],
    load: notebookMarkdown,
  }),
  LanguageDescription.of({ name: "Quarto", alias: ["qmd"], extensions: ["qmd"], load: notebookMarkdown }),
];

export const languages: readonly LanguageDescription[] = [...shipped, typst, ...dialects];

/** Every language by name, for a picker: unique, sorted. */
export const languageNames: readonly string[] = [...new Set(languages.map((l) => l.name))].sort((a, b) =>
  a.localeCompare(b, undefined, { sensitivity: "base" }),
);

/** The language called exactly `name` (a picker's value), or null. */
export function languageCalled(name: string): LanguageDescription | null {
  return languages.find((l) => l.name === name) ?? null;
}

/**
 * Names and extensions the list gets wrong for the files people actually
 * have, checked before it: `.cfg` is setup.cfg-style INI, not TTCN; `.m` is
 * MATLAB (read with Octave's grammar) far more often than Mathematica or
 * Objective-C, which stay a pick away.
 */
const OVERRIDES: readonly [string, RegExp][] = [
  ["Properties files", /\.cfg$/],
  ["Octave", /\.m$/],
];

/**
 * Files the list does not place at all: tested against the base name, in
 * order, after its own names and extensions. A file whose language has no
 * grammar close enough to read (HCL, Nix, WDL, Stata, batch files) stays
 * plain text rather than being dressed in a wrong one; the picker is there.
 */
const FILE_NAMES: readonly [string, RegExp][] = [
  // Cluster job scripts are shell scripts with #SBATCH / #PBS comments.
  ["Shell", /\.(sbatch|slurm|pbs|lsf|qsub|bats|zsh|zsh-theme|fish|csh|tcsh|command|ebuild|eclass)$/],
  ["Shell", /^\.(bash(rc|_profile|_login|_logout|_aliases)|profile|zshrc|zprofile|zshenv|zlogin|zlogout|kshrc|cshrc|tcshrc|envrc|xinitrc|xprofile|Renviron)$/],
  ["Shell", /^\.env(\..+)?$|^APKBUILD$/],
  ["Dockerfile", /^(Docker|Container)file(\..+)?$|\.(docker|container)file$/i],
  ["Properties files", /\.(conf|cnf|desktop|service|timer|socket|mount|target|path|Rproj|pylintrc)$/],
  [
    "Properties files",
    /^\.(gitconfig|gitmodules|editorconfig|npmrc|yarnrc|pypirc|pylintrc|flake8|coveragerc|hgrc|wgetrc|curlrc|my\.cnf|tmux\.conf|inputrc)$/,
  ],
  // Ignore lists and pip requirements: `#` comments and one entry a line.
  ["Properties files", /^\.[\w-]*ignore$|^CODEOWNERS$|^(requirements|constraints)([-_.][\w.-]*)?\.(txt|in)$/],
  [
    "JSON",
    /\.(jsonc|json5|geojson|topojson|webmanifest|har|ndjson|jsonl|avsc|code-workspace|sublime-[a-z]+|tfstate)$/,
  ],
  ["JSON", /^\.(babelrc|eslintrc|prettierrc|swcrc|jshintrc|stylelintrc)$|^(Pipfile|composer|flake)\.lock$/],
  ["YAML", /\.(cff|cwl|sls)$|^\.(clang-format|clang-tidy|condarc|yamllint)$|^Procfile$/],
  ["TOML", /^(Cargo|poetry|uv|pdm)\.lock$|^Pipfile$/],
  [
    "XML",
    /\.(plist|csproj|vbproj|fsproj|vcxproj|props|targets|resx|xaml|kml|gpx|graphml|sbml|xlf|xliff|nuspec|pom|rss|atom|xul|tmx|xmi|launch|urdf|sdf)$/,
  ],
  ["HTML", /\.(svelte|astro|xhtml|shtml|ejs|erb|mustache|cshtml|razor)$/],
  ["Jinja", /\.(njk|nunjucks|twig|jinja2?\.[a-z]+)$/],
  ["Markdown", /\.(mdx|mdown|mkdn|markdn|mdwn|ronn|livemd)$/],
  ["Python", /\.(pyi|ipy|star|bazel|sky|gyp|gypi|wsgi|rpy|pyt|tac)$/],
  ["Python", /^(SConstruct|SConscript|wscript|WORKSPACE|Tiltfile|meson\.build|meson_options\.txt)$/],
  ["R", /^\.Rprofile$|\.(Rprofile|rprofile)$/],
  ["Ruby", /\.(rake|gemspec|ru|podspec|jbuilder|ex|exs)$|^(Podfile|Fastfile|Brewfile|Vagrantfile|Guardfile|Capfile|Berksfile|Dangerfile)$/],
  ["Groovy", /\.(gvy|gy|gsh)$/],
  ["Scala", /\.(sc|sbt)$/],
  ["Haskell", /\.lhs$/],
  ["F#", /\.(fsx|fsi)$/],
  ["Erlang", /\.(hrl|escript)$|^rebar\.config$/],
  ["C++", /\.(cu|cuh|ipp|tpp|inl|metal|ixx|cppm|mpp)$/],
  ["C", /\.(glsl|vert|frag|geom|tesc|tese|comp|hlsl|cginc)$/],
  ["Fortran", /\.(F|F77|F90|F95|F03|F08|f03|f08|fpp|FPP)$/],
  ["PostgreSQL", /\.(psql|pgsql)$/],
  ["SQL", /\.(hql|ddl|dml|prql)$/],
  ["LaTeX", /\.(sty|cls|dtx|ins|bbx|cbx|bib|Rnw|Snw|Rd)$/],
  ["diff", /\.rej$/],
  ["Scheme", /\.(rkt|sld|sps)$/],
  ["Common Lisp", /\.(lsp|asd)$/],
  ["Lua", /\.rockspec$/],
  ["HTTP", /\.(http|rest)$/],
];

/** Interpreters on a `#!` line → a language name the list knows. */
const INTERPRETERS: Readonly<Record<string, string>> = {
  sh: "Shell",
  bash: "Shell",
  zsh: "Shell",
  ksh: "Shell",
  dash: "Shell",
  fish: "Shell",
  csh: "Shell",
  tcsh: "Shell",
  python: "Python",
  pypy: "Python",
  rscript: "R",
  r: "R",
  node: "JavaScript",
  bun: "JavaScript",
  deno: "TypeScript",
  "ts-node": "TypeScript",
  tsx: "TypeScript",
  perl: "Perl",
  ruby: "Ruby",
  elixir: "Ruby",
  julia: "Julia",
  lua: "Lua",
  luajit: "Lua",
  nextflow: "Nextflow",
  snakemake: "Snakemake",
  make: "Makefile",
  just: "Justfile",
  groovy: "Groovy",
  scala: "Scala",
  swift: "Swift",
  pwsh: "PowerShell",
  php: "PHP",
  tclsh: "Tcl",
  wish: "Tcl",
  octave: "Octave",
  sbcl: "Common Lisp",
  guile: "Scheme",
  racket: "Scheme",
  escript: "Erlang",
  runghc: "Haskell",
  runhaskell: "Haskell",
  ocaml: "OCaml",
  crystal: "Crystal",
  dart: "Dart",
};

/**
 * The language a `#!` first line names: `#!/bin/bash`, `#!/usr/bin/env
 * python3`, `#!/usr/bin/env -S snakemake -s`. The interpreter's version
 * suffix is dropped (`python3.11` → python).
 */
export function shebangLanguage(firstLine: string): LanguageDescription | null {
  if (!firstLine.startsWith("#!")) return null;
  const words = firstLine.slice(2).trim().split(/\s+/);
  let i = 0;
  const base = (w: string) => w.slice(w.lastIndexOf("/") + 1);
  if (base(words[0] ?? "") === "env") {
    i = 1;
    // env's own flags and VAR=value assignments come before the program.
    while (i < words.length && (words[i].startsWith("-") || words[i].includes("="))) i++;
  }
  const program = base(words[i] ?? "")
    .toLowerCase()
    .replace(/[\d.]+$/, "");
  const name = INTERPRETERS[program];
  return name === undefined ? null : languageCalled(name);
}

function firstMatch(table: readonly [string, RegExp][], name: string): LanguageDescription | null {
  for (const [lang, test] of table) if (test.test(name)) return languageCalled(lang);
  return null;
}

/**
 * A file's language: its name (`OVERRIDES`, the list's own names and
 * extensions, then `FILE_NAMES`), else the `#!` line of `text`.
 */
export function languageFor(path: string, text: string | null = null): LanguageDescription | null {
  const name = path.slice(path.lastIndexOf("/") + 1);
  const byName =
    firstMatch(OVERRIDES, name) ?? LanguageDescription.matchFilename(languages, name) ?? firstMatch(FILE_NAMES, name);
  if (byName !== null || text === null) return byName;
  const nl = text.indexOf("\n");
  return shebangLanguage(nl === -1 ? text : text.slice(0, nl));
}

/**
 * Fence and notebook names that are not a language's name or alias in the
 * list: the short forms people and agents write (```py, ```console, ```rs).
 */
const FENCE_ALIASES: Readonly<Record<string, readonly string[]>> = {
  Shell: ["console", "shell-session", "shellsession", "terminal", "sbatch", "slurm", "fish", "csh", "tcsh", "ksh", "env", "dotenv"],
  Python: ["py", "py3", "python3", "ipython", "ipython3", "pyi", "gyp", "starlark", "bazel", "sage"],
  JavaScript: ["mjs", "cjs", "nodejs"],
  TypeScript: ["mts", "cts", "deno"],
  JSON: ["jsonc", "jsonl", "ndjson", "geojson", "webmanifest"],
  YAML: ["cwl", "cff"],
  Rust: ["rs"],
  Go: ["golang"],
  Ruby: ["elixir", "ex", "exs", "gemspec", "podspec"],
  Kotlin: ["kt", "kts"],
  Haskell: ["hs", "lhs"],
  Julia: ["jl"],
  Perl: ["pl", "pm"],
  Markdown: ["md", "mdx", "gfm"],
  LaTeX: ["bib", "bibtex", "sty"],
  Octave: ["matlab", "m"],
  Fortran: ["f90", "f95", "f03", "f08"],
  "C++": ["cu", "cuda", "hpp", "cc", "cxx"],
  C: ["h", "glsl", "hlsl"],
  OCaml: ["ml"],
  Clojure: ["clj", "cljc"],
  "Common Lisp": ["elisp", "emacs-lisp"],
  Scheme: ["racket", "rkt"],
  PowerShell: ["ps1", "pwsh", "ps"],
  "Properties files": ["cfg", "conf", "editorconfig", "gitconfig"],
  XML: ["svg", "plist", "xsl"],
  HTML: ["svelte", "astro", "erb", "ejs", "htm"],
  Dockerfile: ["docker", "containerfile"],
  diff: ["patch", "udiff"],
  PostgreSQL: ["psql", "pgsql", "postgres"],
  Scala: ["sc", "sbt"],
  "F#": ["fs", "fsx"],
  Erlang: ["erl"],
  Groovy: ["gradle", "jenkinsfile"],
  HTTP: ["http", "rest"],
};

const fenceAlias = new Map<string, string>(
  Object.entries(FENCE_ALIASES).flatMap(([lang, names]) => names.map((n): [string, string] => [n, lang])),
);

/**
 * The language a fence or a notebook kernel names (`python`, `R`, `console`,
 * `snakemake`), or null. Exact names and aliases first, then CodeMirror's
 * fuzzy match (`{r}`, `python title="x"`).
 */
export function languageNamed(name: string, fuzzy = true): LanguageDescription | null {
  const key = name.trim().toLowerCase();
  if (key === "") return null;
  const exact = LanguageDescription.matchLanguageName(languages, key, false);
  if (exact !== null) return exact;
  const mapped = fenceAlias.get(key);
  if (mapped !== undefined) return languageCalled(mapped);
  return fuzzy ? LanguageDescription.matchLanguageName(languages, key, true) : null;
}

/** The language a person picked for a file (`""`: plain text), remembered
 *  in this browser; no entry means "detect". */
export const languageOverrides: PathMemory<string> = createPathMemory(
  () => (typeof localStorage === "undefined" ? null : localStorage),
  "chimaera.editorLanguages",
  (v): v is string => typeof v === "string",
);
