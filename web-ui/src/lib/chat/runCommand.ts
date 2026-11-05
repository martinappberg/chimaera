/**
 * Run a chat's shell block in a terminal: a Run button beside Copy on every
 * settled fence a shell reads (```bash, ```console, ```sbatch …), which types
 * the block into the chat's terminal, Enter included. The host (`ChatView`,
 * through the `runCommandKey` context) decides which terminal; this module
 * owns which blocks qualify and what text goes in.
 *
 * Trust: agent output is untrusted, and a click is the only thing that sends
 * it anywhere. Like copy, the payload is read from the live DOM at click time
 * (`copyPayload`: what is rendered, nothing hidden), so a forged button can
 * only ever run its own visible block. Control characters are dropped and
 * tabs become spaces before anything reaches the PTY: an ESC could drive the
 * line editor, a tab would trigger completion.
 */

import { copyPayload } from "../shared/copyDecor";

/** The context key under which a chat hands its Markdown the run action. */
export const runCommandKey = Symbol("chimaera.runCommand");

export type RunCommand = (command: string) => void;

/** Fence names a shell reads: the block is commands, not a script to save. */
const SHELL_FENCES = new Set([
  "bash",
  "sh",
  "shell",
  "zsh",
  "ksh",
  "fish",
  "console",
  "shell-session",
  "shellsession",
  "terminal",
  "sbatch",
  "slurm",
]);

/** The fence language of a rendered `pre`, from marked's `language-…` class. */
export function fenceLanguage(pre: Element): string {
  const code = pre.querySelector(":scope > code");
  for (const c of code?.classList ?? []) if (c.startsWith("language-")) return c.slice(9).toLowerCase();
  return "";
}

export function isShellFence(lang: string): boolean {
  return SHELL_FENCES.has(lang.toLowerCase());
}

const PROMPT = /^\s*\$ /;

/**
 * The commands in a block as a person would type them. A transcript-style
 * block (any line starts with `$ `) keeps only its prompted lines, prompt
 * removed, with their `\` continuations; the rest is output. Control
 * characters go; CRLF and tabs are normalized.
 */
export function commandText(block: string): string {
  const clean = block
    .replace(/\r\n?/g, "\n")
    .replace(/\t/g, "  ")
    // eslint-disable-next-line no-control-regex
    .replace(/[\x00-\x09\x0b-\x1f\x7f-\x9f]/g, "");
  let lines = clean.split("\n");
  if (lines.some((l) => PROMPT.test(l))) {
    const kept: string[] = [];
    let continuing = false;
    for (const line of lines) {
      if (continuing) kept.push(line);
      else if (PROMPT.test(line)) kept.push(line.replace(PROMPT, ""));
      else continue;
      continuing = line.trimEnd().endsWith("\\");
    }
    lines = kept;
  }
  return lines.join("\n").trim();
}

/** What a click types into the PTY: each line as if typed, Enter after each. */
export function terminalInput(command: string): string {
  return command === "" ? "" : `${command.split("\n").join("\r")}\r`;
}

const RUN_SVG =
  '<svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">' +
  '<path d="M5 3.5v9l7-4.5z" fill="currentColor"/>' +
  "</svg>";

/** Add a Run button to every shell fence under `root` that lacks one.
 *  APPEND-only inside the pre, like the copy button (`copyDecor.ts`). */
export function decorateRunTargets(root: HTMLElement): void {
  for (const pre of root.querySelectorAll("pre")) {
    if (pre.closest(".embed-card") !== null) continue;
    if (pre.querySelector(":scope > button.md-run") !== null) continue;
    if (!isShellFence(fenceLanguage(pre))) continue;
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "md-run";
    btn.setAttribute("aria-label", "run in terminal");
    btn.title = "run in terminal";
    btn.innerHTML = RUN_SVG;
    pre.appendChild(btn);
  }
}

/** The command a Run button runs, from its own block as rendered. */
export function runPayload(btn: Element): string {
  return commandText(copyPayload(btn));
}
