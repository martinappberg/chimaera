/**
 * Run a chat's shell block in a terminal: a Run button beside Copy on every
 * settled fence a shell reads (```bash, ```console, ```sbatch …), which types
 * the block into the chat's terminal, Enter included. The host (`ChatView`,
 * through the `runCommandKey` context) decides which terminal; this module
 * owns which blocks qualify and what text goes in.
 *
 * Trust: agent output is untrusted, and a click is the only thing that sends
 * it anywhere. Only a button this module made runs anything (`isRunButton`):
 * sanitized agent HTML can carry a `<button class="md-run">` of its own, in a
 * quote or under any `pre`, and that one does nothing. The payload is read
 * from the live DOM at click time (`copyPayload`: what is rendered, nothing
 * hidden). Before anything reaches the PTY, control characters are dropped
 * (an ESC could drive the line editor), tabs become spaces (a tab would
 * complete), invisible zero-width characters go, and a block with bidi
 * controls is refused outright: what it shows is not what it would run.
 */

import { copyPayload } from "../shared/copyDecor";

/** The context key under which a chat hands its Markdown the run action. */
export const runCommandKey = Symbol("chimaera.runCommand");

export type RunCommand = (command: string) => void;

/** Fence names a shell reads: the block is commands, not a script to save. */
const SHELL_FENCES = new Set(["bash", "sh", "shell", "zsh", "ksh", "fish", "sbatch", "slurm"]);

/** Fence names for a terminal transcript: only the `$ ` lines are commands,
 *  so a block of these with no prompt is output and runs nothing. */
const TRANSCRIPT_FENCES = new Set(["console", "shell-session", "shellsession", "terminal"]);

/** The fence language of a rendered `pre`, from marked's `language-…` class. */
export function fenceLanguage(pre: Element): string {
  const code = pre.querySelector(":scope > code");
  for (const c of code?.classList ?? []) if (c.startsWith("language-")) return c.slice(9).toLowerCase();
  return "";
}

export function isShellFence(lang: string): boolean {
  const l = lang.toLowerCase();
  return SHELL_FENCES.has(l) || TRANSCRIPT_FENCES.has(l);
}

const PROMPT = /^\s*\$ /;
/** A continuation prompt a pasted transcript keeps: bash's `> `, zsh's `heredoc> `, `dquote> `. */
const PS2 = /^(?:heredoc|quote|dquote|bquote|cmdsubst|pipe)?> ?/;
/** A heredoc's start, `<<EOF` / `<<-'END'` after a space: never `x<<y` arithmetic or `<<<`. */
const HEREDOC = /(?:^|\s)<<-?\s*(['"]?)([A-Za-z_]\w*)\1/;
// eslint-disable-next-line no-control-regex
const CONTROL = /[\x00-\x09\x0b-\x1f\x7f-\x9f]/g;
const ZERO_WIDTH = /[​-‍⁠﻿]/g;
const BIDI = /[؜‎‏‪-‮⁦-⁩]/;

/**
 * The commands in a block as a person would type them, or "" when there is
 * nothing to run. A transcript-style block (any line starts with `$ `, and
 * always for a `console`-family fence) keeps only its prompted lines, prompt
 * removed, with their `\` continuations and heredoc bodies, each without the
 * `> ` continuation prompt a terminal shows; the rest is output.
 */
export function commandText(block: string, lang = ""): string {
  if (BIDI.test(block)) return "";
  const clean = block
    .replace(/\r\n?/g, "\n")
    .replace(/\t/g, "  ")
    .replace(CONTROL, "")
    .replace(ZERO_WIDTH, "");
  let lines = clean.split("\n");
  const transcript = TRANSCRIPT_FENCES.has(lang.toLowerCase());
  if (transcript || lines.some((l) => PROMPT.test(l))) {
    const kept: string[] = [];
    let continuing = false;
    let heredoc: string | null = null;
    for (const raw of lines) {
      if (heredoc !== null) {
        const body = raw.replace(PS2, "");
        kept.push(body);
        if (body.trim() === heredoc) heredoc = null;
        continue;
      }
      let line: string;
      if (continuing) line = raw.replace(PS2, "");
      else if (PROMPT.test(raw)) line = raw.replace(PROMPT, "");
      else continue;
      kept.push(line);
      continuing = line.trimEnd().endsWith("\\");
      heredoc = HEREDOC.exec(line)?.[2] ?? null;
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

/** The buttons `decorateRunTargets` made; nothing else runs. */
const ownButtons = new WeakSet<Element>();

export function isRunButton(el: Element): boolean {
  return ownButtons.has(el);
}

/** Add a Run button to every shell fence under `root` that lacks one and
 *  has something to run. APPEND-only inside the pre, like the copy button
 *  (`copyDecor.ts`). */
export function decorateRunTargets(root: HTMLElement): void {
  for (const pre of root.querySelectorAll("pre")) {
    if (pre.closest(".embed-card") !== null) continue;
    if ([...pre.querySelectorAll(":scope > button.md-run")].some(isRunButton)) continue;
    const lang = fenceLanguage(pre);
    if (!isShellFence(lang)) continue;
    if (commandText(pre.querySelector(":scope > code")?.textContent ?? "", lang) === "") continue;
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "md-run";
    btn.setAttribute("aria-label", "run in terminal");
    btn.title = "run in terminal";
    btn.innerHTML = RUN_SVG;
    ownButtons.add(btn);
    pre.appendChild(btn);
  }
}

/** The command one of our Run buttons runs, from its own block as rendered;
 *  "" for any other button. */
export function runPayload(btn: Element): string {
  const pre = btn.closest("pre");
  if (!isRunButton(btn) || pre === null) return "";
  return commandText(copyPayload(btn), fenceLanguage(pre));
}
