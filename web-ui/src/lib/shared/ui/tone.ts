/**
 * The shared UI pieces (Badge, Callout, Row, KeyValue, FileCard) take the
 * `ui/1` prop names and its semantic tones — Knowledge draws with them, and
 * the plugin screen renderer (docs/plugin-platform-plan.md) will too. A
 * tone is never a colour on its own: every toned piece carries its word.
 */
export type Tone = "neutral" | "accent" | "good" | "warn" | "bad";

const TONES: ReadonlySet<string> = new Set(["neutral", "accent", "good", "warn", "bad"]);

/** A provider's tone string as a tone ("neutral" for anything else). */
export function tone(t: string | null | undefined): Tone {
  return t !== null && t !== undefined && TONES.has(t) ? (t as Tone) : "neutral";
}

export interface BadgeSpec {
  text: string;
  tone?: Tone;
}
