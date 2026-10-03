import type { AskpassPrompt } from "../net/native";

type Presentation =
  | { type: "secret" }
  | { type: "host_key"; host: string; fingerprint: string }
  | { type: "unsupported" };

/** Prompt prose never selects a trust action, including a keeper MFA challenge. */
export function askpassPresentation(prompt: AskpassPrompt): Presentation {
  if (prompt.kind === undefined) return { type: "secret" };
  const kind = prompt.kind;
  if (
    prompt.source?.type !== "local" ||
    kind === null ||
    kind.type !== "host_key" ||
    typeof kind.host !== "string" ||
    kind.host.length === 0 ||
    kind.host.length > 1024 ||
    /[\s\x00-\x1f\x7f]/.test(kind.host) ||
    typeof kind.fingerprint !== "string" ||
    !/^SHA256:[A-Za-z0-9+/]{43}$/.test(kind.fingerprint)
  ) {
    return { type: "unsupported" };
  }
  return { type: "host_key", host: kind.host, fingerprint: kind.fingerprint };
}
