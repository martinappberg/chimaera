/** One transfer policy for clipboard paste and file-manager drops. The daemon
 * remains authoritative for symlinks, permissions, and destination collisions. */
import { basename, dirname, joinPath } from "../previews/files";
import { reportUploadError, trackFileOp } from "../net/uploads";
import { fsCopyOp, fsMoveOp } from "./fsEvents";

export interface FileSource {
  path: string;
  kind: "dir" | "file";
}

export type FileOperation = "copy" | "move";

function withoutTrailingSlash(path: string): string {
  return path.replace(/\/+$/, "") || "/";
}

export function transferBlock(source: FileSource, destDir: string, operation: FileOperation): string | null {
  const from = withoutTrailingSlash(source.path);
  const dir = withoutTrailingSlash(destDir);
  if (source.kind === "dir" && (from === "/" || dir === from || dir.startsWith(`${from}/`))) {
    return "A folder can't go inside itself";
  }
  if (operation === "move" && dirname(from) === dir) return "Already in this folder";
  return null;
}

/** Returns the new path only once the operation succeeds. Moves never replace
 * an existing entry; copies choose a free “name copy” sibling on collision. */
export async function transferInto(
  source: FileSource,
  destDir: string,
  operation: FileOperation,
): Promise<string | null> {
  const from = withoutTrailingSlash(source.path);
  const dir = withoutTrailingSlash(destDir);
  const blocked = transferBlock(source, destDir, operation);
  if (blocked !== null) {
    // Same-parent cuts are an intentional no-op, just like a file-manager drop.
    if (operation !== "move" || dirname(from) !== dir) reportUploadError(blocked);
    return null;
  }
  const name = basename(from);
  const dest = joinPath(dir, name);
  return trackFileOp(`${operation === "move" ? "Moving" : "Copying"} ${name}…`, () =>
    operation === "move" ? fsMoveOp(from, dest) : fsCopyOp(from, dest, "unique"),
  );
}
