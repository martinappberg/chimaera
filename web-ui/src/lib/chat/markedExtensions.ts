import type { MarkedExtension } from "marked";
import { markdownMath } from "./math";
import { markdownTables } from "./tables";

/** The chat parser's exact marked configuration, in registration order. One
 *  list, consumed by `Markdown.svelte` AND by the streaming ⇄ settled parity
 *  pins in `streamSegments.test.ts`, so an extension can't reach the
 *  transcript without those pins rendering through it too. */
export const chatMarkedExtensions: MarkedExtension[] = [markdownMath, markdownTables];
