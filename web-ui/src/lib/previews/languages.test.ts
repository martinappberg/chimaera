import { describe, expect, it } from "vitest";

import { chunkLanguage, languageCalled, languageFor, languageNames, languageNamed, languages, shebangLanguage } from "./languages";
import { createPathMemory } from "./pathMemory";

const nameOf = (path: string, text: string | null = null) => languageFor(path, text)?.name ?? null;

describe("languageFor", () => {
  it.each([
    // Research workflows and cluster jobs.
    ["Snakefile", "Snakemake"],
    ["workflow/rules/align.smk", "Snakemake"],
    ["main.nf", "Nextflow"],
    ["nextflow.config", "Nextflow"],
    ["job.sbatch", "Shell"],
    ["run.slurm", "Shell"],
    ["array.pbs", "Shell"],
    ["Singularity", "Apptainer"],
    ["image.def", "Apptainer"],
    ["analysis.Rmd", "R Markdown"],
    ["report.qmd", "Quarto"],
    ["env.cwl", "YAML"],
    ["CITATION.cff", "YAML"],
    ["fit.m", "Octave"],
    ["solver.F90", "Fortran"],
    ["kernel.cu", "C++"],
    ["refs.bib", "LaTeX"],
    ["vignette.Rnw", "LaTeX"],
    [".Rprofile", "R"],
    // Build and config.
    ["Makefile", "Makefile"],
    ["Makefile.am", "Makefile"],
    ["rules.mk", "Makefile"],
    ["justfile", "Justfile"],
    ["Containerfile", "Dockerfile"],
    ["Dockerfile.gpu", "Dockerfile"],
    ["Dockerfile", "Dockerfile"],
    ["setup.cfg", "Properties files"],
    [".gitconfig", "Properties files"],
    [".gitignore", "Properties files"],
    ["requirements-dev.txt", "Properties files"],
    ["nginx.conf", "Nginx"],
    ["app.conf", "Properties files"],
    ["Cargo.lock", "TOML"],
    ["Pipfile", "TOML"],
    ["Pipfile.lock", "JSON"],
    ["tsconfig.jsonc", "JSON"],
    ["events.jsonl", "JSON"],
    ["Info.plist", "XML"],
    ["meson.build", "Python"],
    ["BUILD.bazel", "Python"],
    ["types.pyi", "Python"],
    ["Gemfile", "Ruby"],
    ["Brewfile", "Ruby"],
    // Shell dotfiles and env files.
    [".bashrc", "Shell"],
    [".zshrc", "Shell"],
    [".env", "Shell"],
    [".env.local", "Shell"],
    [".envrc", "Shell"],
    ["config.fish", "Shell"],
    // Web.
    ["App.svelte", "HTML"],
    ["page.mdx", "Markdown"],
    ["base.njk", "Jinja"],
    // The list's own coverage still answers first.
    ["src/main.rs", "Rust"],
    ["Cargo.toml", "TOML"],
    ["x.py", "Python"],
    ["x.R", "R"],
    ["CMakeLists.txt", "CMake"],
  ])("%s → %s", (path, lang) => {
    expect(nameOf(path)).toBe(lang);
  });

  it("leaves files no grammar fits as plain text", () => {
    for (const path of ["notes.txt", "main.tf", "flake.nix", "pipeline.wdl", "reads.fasta", "LICENSE"]) {
      expect(nameOf(path)).toBeNull();
    }
  });

  it("falls back to the #! line only when the name says nothing", () => {
    expect(nameOf("bin/run", "#!/usr/bin/env python3\nprint(1)\n")).toBe("Python");
    expect(nameOf("configure", "#!/bin/sh\n")).toBe("Shell");
    expect(nameOf("tool.rb", "#!/usr/bin/env python3\n")).toBe("Ruby");
    expect(nameOf("README", "plain words\n")).toBeNull();
  });

  it("names a language for every table entry", () => {
    // A misspelt target in a table would silently match nothing.
    const paths = ["x.sbatch", ".bashrc", ".env", "Containerfile", "a.conf", ".gitconfig", ".gitignore", "a.jsonc",
      ".babelrc", "a.cwl", "Cargo.lock", "a.plist", "a.svelte", "a.njk", "a.mdx", "a.pyi", "SConstruct", ".Rprofile",
      "a.rake", "a.gvy", "a.sbt", "a.lhs", "a.fsx", "a.hrl", "a.cu", "a.glsl", "a.F90", "a.psql", "a.hql", "a.sty",
      "a.rej", "a.rkt", "a.lsp", "a.rockspec", "a.http", "a.cfg", "a.m"];
    for (const p of paths) expect(languageFor(p), p).not.toBeNull();
  });
});

describe("shebangLanguage", () => {
  it.each([
    ["#!/bin/bash", "Shell"],
    ["#!/usr/bin/env bash", "Shell"],
    ["#!/usr/bin/env python3.11", "Python"],
    ["#!/usr/bin/env -S snakemake -s", "Snakemake"],
    ["#!/usr/bin/env Rscript", "R"],
    ["#!/usr/bin/env nextflow", "Nextflow"],
    ["#!/usr/bin/env LANG=C perl -w", "Perl"],
    ["#!/usr/bin/make -f", "Makefile"],
    ["#! /usr/bin/env node", "JavaScript"],
  ])("%s → %s", (line, lang) => {
    expect(shebangLanguage(line)?.name ?? null).toBe(lang);
  });

  it("ignores lines that are not a #! or name an unknown program", () => {
    expect(shebangLanguage("# a comment")).toBeNull();
    expect(shebangLanguage("#!/usr/bin/env frobnicate")).toBeNull();
    expect(shebangLanguage("#!")).toBeNull();
  });
});

describe("languageNamed", () => {
  it.each([
    ["python", "Python"],
    ["py", "Python"],
    ["ipython3", "Python"],
    ["bash", "Shell"],
    ["console", "Shell"],
    ["shell-session", "Shell"],
    ["rs", "Rust"],
    ["R", "R"],
    ["snakemake", "Snakemake"],
    ["nextflow", "Nextflow"],
    ["matlab", "Octave"],
    ["makefile", "Makefile"],
    ["make", "Makefile"],
    ["dockerfile", "Dockerfile"],
    ["jsonl", "JSON"],
    ["patch", "diff"],
  ])("%s → %s", (fence, lang) => {
    expect(languageNamed(fence)?.name ?? null).toBe(lang);
  });

  it("finds nothing for an empty or plain name", () => {
    expect(languageNamed("")).toBeNull();
    expect(languageNamed("text", false)).toBeNull();
  });
});

describe("chunkLanguage", () => {
  it("reads R Markdown and Quarto chunk headers", () => {
    expect(chunkLanguage("{r setup, include=FALSE}")).toBe("r");
    expect(chunkLanguage("{python}")).toBe("python");
    expect(chunkLanguage("bash")).toBe("bash");
    expect(chunkLanguage("")).toBe("");
  });
});

describe("the language list", () => {
  it("has unique names for the picker, and each resolves", () => {
    expect(new Set(languageNames).size).toBe(languageNames.length);
    for (const name of languageNames) expect(languageCalled(name)?.name).toBe(name);
    expect(languageNames.length).toBe(new Set(languages.map((l) => l.name)).size);
  });

  it("loads a borrowed grammar", async () => {
    const support = await languageCalled("Snakemake")!.load();
    expect(support.language.name).toBe("python");
  });

  it("loads R Markdown with its chunks nested", async () => {
    const support = await languageCalled("R Markdown")!.load();
    const tree = support.language.parser.parse("# t\n\n```{r}\nx <- 1\n```\n");
    expect(tree.length).toBeGreaterThan(0);
  });
});

describe("createPathMemory", () => {
  it("remembers, forgets and survives broken storage", () => {
    const data = new Map<string, string>();
    const store = { getItem: (k: string) => data.get(k) ?? null, setItem: (k: string, v: string) => void data.set(k, v) };
    const mem = createPathMemory(() => store, "k", (v): v is string => typeof v === "string", 2);
    mem.set("/a", "Python");
    mem.set("/b", "");
    expect(mem.get("/a")).toBe("Python");
    expect(mem.get("/b")).toBe("");
    mem.set("/c", "R");
    // Cap 2: reading "/b" last left "/a" the oldest, so "/c" pushes it out.
    expect(mem.get("/a")).toBeNull();
    mem.forget("/b");
    expect(mem.get("/b")).toBeNull();
    expect(mem.get("/c")).toBe("R");
    const broken = createPathMemory(
      () => {
        throw new Error("no storage");
      },
      "k",
      (v): v is string => typeof v === "string",
    );
    expect(broken.get("/a")).toBeNull();
    broken.set("/a", "x");
    broken.forget("/a");
  });
});
