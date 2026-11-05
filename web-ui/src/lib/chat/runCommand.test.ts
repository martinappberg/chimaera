import { describe, expect, it } from "vitest";

import { commandText, isShellFence, terminalInput } from "./runCommand";

describe("isShellFence", () => {
  it("takes the fences a shell reads, and nothing else", () => {
    for (const l of ["bash", "sh", "zsh", "console", "shell-session", "sbatch", "Shell"]) expect(isShellFence(l), l).toBe(true);
    for (const l of ["", "python", "r", "text", "powershell", "json"]) expect(isShellFence(l), l).toBe(false);
  });
});

describe("commandText", () => {
  it("keeps a plain block as written, trimmed", () => {
    expect(commandText("\ncd ~/proj\nsnakemake -n\n\n")).toBe("cd ~/proj\nsnakemake -n");
  });

  it("keeps only the prompted lines of a transcript, with their continuations", () => {
    const block = ["$ ls data", "a.fq b.fq", "$ sbatch --time=1:00:00 \\", "    run.sbatch", "Submitted batch job 42"].join("\n");
    expect(commandText(block)).toBe("ls data\nsbatch --time=1:00:00 \\\n    run.sbatch");
  });

  it("drops control characters and turns tabs into spaces", () => {
    expect(commandText("echo\thi\u001b[31m\u0007\r\n")).toBe("echo  hi[31m");
    expect(commandText("a\u0000b\u009bc")).toBe("abc");
  });

  it("yields nothing for an empty block", () => {
    expect(commandText("  \n ")).toBe("");
  });
});

describe("terminalInput", () => {
  it("types each line with Enter after it", () => {
    expect(terminalInput("ls")).toBe("ls\r");
    expect(terminalInput("cd x\nmake")).toBe("cd x\rmake\r");
    expect(terminalInput("")).toBe("");
  });
});
