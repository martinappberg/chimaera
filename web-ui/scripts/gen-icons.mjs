// Generate src/lib/icons.ts from a curated subset of Tabler Icons (MIT).
//
// Tabler icons are 24x24 outline glyphs drawn with a single stroke; we strip
// the boilerplate <svg> wrapper and the leading transparent background rect,
// keeping just the stroked <path d="…"> commands. The runtime component
// (FileIcon.svelte) re-wraps them at 14–16px with our muted tints and the same
// stroke language as the hand-made session glyphs, so the whole set reads as
// one family.
//
// Run: `npm run gen:icons` (also runs in `prebuild`). Requires @tabler/icons
// as a dev dependency; no network access. Edit CURATION below to add mappings.

import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const ICON_DIR = join(here, "..", "node_modules", "@tabler", "icons", "icons", "outline");
const OUT = join(here, "..", "src", "lib", "icons.ts");

// glyph name -> tabler icon file (without .svg). One glyph can back many
// extensions; the extension/filename maps below point at these names.
const GLYPHS = {
  rust: "brand-rust",
  python: "brand-python",
  javascript: "brand-javascript",
  typescript: "brand-typescript",
  jsx: "file-type-jsx",
  tsx: "file-type-tsx",
  svelte: "brand-svelte",
  vue: "file-type-vue",
  rlang: "letter-r",
  shell: "terminal-2",
  golang: "brand-golang",
  cpp: "brand-cpp",
  csharp: "brand-c-sharp",
  swift: "brand-swift",
  kotlin: "brand-kotlin",
  php: "file-type-php",
  html: "file-type-html",
  css: "file-type-css",
  sql: "file-type-sql",
  code: "file-code",
  json: "json",
  braces: "braces",
  settings: "file-settings",
  adjustments: "adjustments",
  xml: "file-type-xml",
  csv: "file-type-csv",
  table: "table",
  database: "database",
  markdown: "markdown",
  text: "file-text",
  pdf: "file-type-pdf",
  image: "photo",
  svg: "file-type-svg",
  archive: "file-zip",
  notebook: "notebook",
  lock: "lock",
  license: "license",
  git: "brand-git",
  docker: "brand-docker",
  box: "package",
  // bio set
  dna: "dna",
  alignment: "assembly",
  variants: "microscope",
  intervals: "schema",
  flask: "flask",
};

// Muted tint categories (mapped to --ficon-* CSS vars in app.css).
const CAT = {
  rust: "lang", python: "lang", javascript: "lang", typescript: "lang",
  jsx: "lang", tsx: "lang", svelte: "lang", vue: "lang", rlang: "lang",
  shell: "lang", golang: "lang", cpp: "lang", csharp: "lang", swift: "lang",
  kotlin: "lang", php: "lang", html: "lang", css: "lang", sql: "data",
  code: "lang",
  json: "data", braces: "data", xml: "data", csv: "data", table: "data",
  database: "data",
  settings: "config", adjustments: "config", lock: "config", docker: "config",
  git: "vcs", license: "doc", box: "config",
  markdown: "doc", text: "doc", pdf: "doc", notebook: "doc",
  image: "media", svg: "media", archive: "archive",
  dna: "bio", alignment: "bio", variants: "bio", intervals: "bio", flask: "bio",
};

// extension (lowercase, no dot) -> glyph name.
const EXT = {
  rs: "rust",
  py: "python", pyi: "python", pyw: "python",
  js: "javascript", mjs: "javascript", cjs: "javascript",
  ts: "typescript", mts: "typescript", cts: "typescript",
  jsx: "jsx", tsx: "tsx",
  svelte: "svelte", vue: "vue",
  r: "rlang", rmd: "rlang",
  sh: "shell", bash: "shell", zsh: "shell", fish: "shell",
  go: "golang",
  c: "cpp", h: "cpp", cc: "cpp", cpp: "cpp", cxx: "cpp", hpp: "cpp", hxx: "cpp",
  cs: "csharp", swift: "swift", kt: "kotlin", kts: "kotlin",
  php: "php", rb: "code", java: "code", lua: "code", pl: "code",
  html: "html", htm: "html",
  css: "css", scss: "css", sass: "css", less: "css",
  sql: "sql",
  json: "json", jsonl: "json", ndjson: "json", json5: "json",
  toml: "settings", yaml: "settings", yml: "settings",
  ini: "adjustments", cfg: "adjustments", conf: "adjustments", env: "adjustments", properties: "adjustments",
  xml: "xml", svg: "svg",
  csv: "csv", tsv: "table", parquet: "database", feather: "database", arrow: "database",
  md: "markdown", markdown: "markdown", mdx: "markdown", rst: "markdown",
  txt: "text", text: "text", log: "text",
  pdf: "pdf",
  png: "image", jpg: "image", jpeg: "image", gif: "image", webp: "image",
  bmp: "image", tif: "image", tiff: "image", ico: "image", heic: "image", avif: "image",
  zip: "archive", tar: "archive", gz: "archive", bgz: "archive", tgz: "archive",
  xz: "archive", zst: "archive", bz2: "archive", "7z": "archive", rar: "archive",
  ipynb: "notebook",
  // bio set
  fasta: "dna", fa: "dna", fna: "dna", faa: "dna", ffn: "dna",
  fastq: "dna", fq: "dna",
  bam: "alignment", cram: "alignment", sam: "alignment", bai: "alignment", crai: "alignment",
  vcf: "variants", bcf: "variants",
  bed: "intervals", gtf: "intervals", gff: "intervals", gff3: "intervals",
  h5: "database", h5ad: "database", hdf5: "database", loom: "database", zarr: "database",
};

// exact filename (lowercase) -> glyph name. Filename specials win over ext.
const NAMES = {
  dockerfile: "docker",
  justfile: "settings",
  makefile: "settings",
  "cmakelists.txt": "settings",
  license: "license",
  "license.md": "license",
  "license.txt": "license",
  copying: "license",
  ".gitignore": "git",
  ".gitattributes": "git",
  ".gitmodules": "git",
  ".dockerignore": "docker",
  "cargo.lock": "lock",
  "package-lock.json": "lock",
  "yarn.lock": "lock",
  "pnpm-lock.yaml": "lock",
  "poetry.lock": "lock",
  "uv.lock": "lock",
  ".env": "adjustments",
  "snakefile": "flask",
  "nextflow.config": "flask",
};

function extractPaths(name) {
  const svg = readFileSync(join(ICON_DIR, `${name}.svg`), "utf8");
  const out = [];
  const re = /<path\b[^>]*\bd="([^"]+)"[^>]*\/?>/g;
  let m;
  while ((m = re.exec(svg)) !== null) {
    const d = m[1].trim();
    // Drop the transparent 24x24 background rect Tabler prefixes every icon with.
    if (d.startsWith("M0 0h24v24H0z")) continue;
    if (!out.includes(d)) out.push(d);
  }
  if (out.length === 0) throw new Error(`no paths extracted from ${name}.svg`);
  return out;
}

const glyphEntries = Object.entries(GLYPHS)
  .map(([glyph, file]) => {
    const paths = extractPaths(file);
    const cat = CAT[glyph] ?? "generic";
    const d = paths.map((p) => `"${p}"`).join(", ");
    return `  ${glyph}: { c: "${cat}", d: [${d}] },`;
  })
  .join("\n");

const extEntries = Object.entries(EXT)
  .map(([ext, glyph]) => `  ${JSON.stringify(ext)}: "${glyph}",`)
  .join("\n");
const nameEntries = Object.entries(NAMES)
  .map(([n, glyph]) => `  ${JSON.stringify(n)}: "${glyph}",`)
  .join("\n");

const banner = `// GENERATED by scripts/gen-icons.mjs — do not edit by hand.
//
// File-type glyphs vendored from Tabler Icons (https://tabler.io/icons),
// licensed MIT — Copyright (c) 2020-2026 Paweł Kuna. Only the stroked <path>
// data is inlined; see scripts/gen-icons.mjs for the curation and the
// normalization (24x24 outline, single stroke, re-tinted at render time).
//
// Full MIT license text: node_modules/@tabler/icons/LICENSE.
`;

const body = `${banner}
/** A vendored glyph: tint category + the stroked path commands (24x24). */
export interface Glyph {
  /** Tint category → the --ficon-<c> CSS var (see app.css). */
  c: string;
  /** Path \`d\` strings, drawn with a single currentColor stroke. */
  d: string[];
}

export const GLYPHS: Record<string, Glyph> = {
${glyphEntries}
};

/** Lowercased extension (no dot) → glyph name. */
export const EXT_GLYPH: Record<string, string> = {
${extEntries}
};

/** Exact lowercased filename → glyph name (wins over extension). */
export const NAME_GLYPH: Record<string, string> = {
${nameEntries}
};
`;

writeFileSync(OUT, body);
console.log(`wrote ${OUT} (${Object.keys(GLYPHS).length} glyphs, ${Object.keys(EXT).length} extensions, ${Object.keys(NAMES).length} filename specials)`);
