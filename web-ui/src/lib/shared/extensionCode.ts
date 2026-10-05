/** Bounded, read-only code presentation shared by extension hosts. */
export const EXTENSION_CODE_CHARS = 100_000;
export const EXTENSION_CODE_LINES = 2_000;

export interface ExtensionCodeRow {
  text: string;
  kind: "source" | "context" | "add" | "remove" | "hunk";
  oldLine?: number;
  newLine?: number;
}

export function boundedCode(source: string): { source: string; truncated: boolean } {
  const chars = source.slice(0, EXTENSION_CODE_CHARS);
  const lines = chars.split("\n");
  const result = lines.slice(0, EXTENSION_CODE_LINES).join("\n");
  return { source: result, truncated: result.length < source.length };
}

/** Reject incomplete hunks instead of inventing line numbers after a cut. */
export function unifiedCodeRows(source: string): ExtensionCodeRow[] | null {
  const lines = source.replace(/\r\n/g, "\n").split("\n");
  if (lines.at(-1) === "") lines.pop();
  const rows: ExtensionCodeRow[] = [];
  let oldLine = 0;
  let newLine = 0;
  let oldLeft = 0;
  let newLeft = 0;
  let hunks = 0;
  let fileHeaders = 0;
  let gitHeaders = 0;
  for (const line of lines) {
    const header = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@(?: .*)?$/.exec(line);
    if (header) {
      if (oldLeft || newLeft) return null;
      const values = [Number(header[1]), Number(header[3]), Number(header[2] ?? 1), Number(header[4] ?? 1)];
      if (values.some((n) => !Number.isSafeInteger(n) || n > 1_000_000_000)) return null;
      [oldLine, newLine, oldLeft, newLeft] = values;
      if (hunks++) rows.push({ text: line, kind: "hunk" });
      continue;
    }
    if (line === "\\ No newline at end of file") continue;
    if (oldLeft === 0 && newLeft === 0) {
      // Separate files need separate labels; a literal patch preserves their
      // identity rather than making their hunks look like one continuous file.
      if (line.startsWith("--- ") && ++fileHeaders > 1) return null;
      if (line.startsWith("diff ") && ++gitHeaders > 1) return null;
      if (line.startsWith("diff ") || line.startsWith("index ") || line.startsWith("--- ") || line.startsWith("+++ ") || line === "") continue;
      return null;
    }
    const marker = line[0];
    if (marker === " " && oldLeft > 0 && newLeft > 0) {
      rows.push({ text: line.slice(1), kind: "context", oldLine: oldLine++, newLine: newLine++ });
      oldLeft--; newLeft--;
    } else if (marker === "-" && oldLeft > 0) {
      rows.push({ text: line.slice(1), kind: "remove", oldLine: oldLine++ });
      oldLeft--;
    } else if (marker === "+" && newLeft > 0) {
      rows.push({ text: line.slice(1), kind: "add", newLine: newLine++ });
      newLeft--;
    } else return null;
  }
  return hunks > 0 && oldLeft === 0 && newLeft === 0 ? rows : null;
}

export function sourceCodeRows(source: string, startLine?: number): ExtensionCodeRow[] {
  const numbered = startLine !== undefined && Number.isSafeInteger(startLine) && startLine > 0 && startLine <= 1_000_000_000;
  return source.split("\n").map((text, index) => ({ text, kind: "source", newLine: numbered ? startLine + index : undefined }));
}
