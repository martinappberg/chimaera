/** Provider catalogs use picker aliases, resolved ids, and context suffixes. */
export interface ModelChoice {
  id: string;
  label: string;
  resolved?: string | null;
  description?: string | null;
}

export function isRealModel(value: unknown): value is string {
  return typeof value === "string" && value.trim().length > 0 && !value.trim().startsWith("<");
}

/** Custom IDs belong to the CLI's configured provider; retain their spelling. */
export function customModelSelection(raw: string): { id: string; error: null } | { id: null; error: string } {
  const id = raw.trim();
  if (!id) return { id: null, error: "Enter a model ID." };
  if (new TextEncoder().encode(id).length > 256) return { id: null, error: "Model IDs must be 256 bytes or fewer." };
  if (/[\s\p{Cc}\p{Cf}]/u.test(id)) return { id: null, error: "Use a model ID without spaces or control characters." };
  if (id.startsWith("-")) return { id: null, error: "Enter the model ID itself, without command-line flags." };
  if (id.startsWith("<")) return { id: null, error: "Replace the placeholder with a model ID." };
  if (!/^[A-Za-z0-9._\[\]/:@-]+$/.test(id)) return { id: null, error: "Use letters, numbers, or . _ - [ ] / : @ in the model ID." };
  return { id, error: null };
}

export function modelChoice<T extends ModelChoice>(choices: T[], target: string | null): T | undefined {
  if (!isRealModel(target)) return undefined;
  const picked = choices.find((m) => m.id === target);
  if (picked) return picked;
  const named = choices.filter((m) => m.id !== "default");
  const ordered = [...named, ...choices.filter((m) => m.id === "default")];
  const exact = ordered.find((m) => m.resolved === target);
  if (exact) return exact;
  // Claude can report the serving ID without its context-size suffix. Other
  // brackets, especially namespaced provider IDs, remain part of the identity.
  const bare = (s: string) => /^(?:claude-[\w.-]+|opus|sonnet|haiku)(?:\[\d+[km]\])?$/i.test(s) ? s.replace(/\[\d+[km]\]$/i, "") : s;
  return ordered.find((m) => bare(m.resolved ?? m.id) === bare(target));
}
