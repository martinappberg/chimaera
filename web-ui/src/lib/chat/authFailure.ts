/**
 * Whether an error the agent reported reads like a missing or rejected login
 * — the cue to ask the CLI's own status command again and, when it says
 * signed out, show the sign-in card in a LIVE chat. Codex's app-server starts
 * without a login and fails only at the first turn (`401 Unauthorized:
 * Missing bearer or basic authentication`); claude says `Please run /login`.
 * A match is a hint to re-check, never a verdict: the status command decides.
 */
const CUES = [
  /\b401\b/,
  /unauthori[sz]ed/i,
  /missing bearer/i,
  /not (?:logged|signed) in/i,
  /run \/login/i,
  /authentication required/i,
  /invalid api key/i,
  /login (?:required|expired)/i,
  /expired (?:token|credential|login)/i,
];

export function looksLikeAuthFailure(text: string | null | undefined): boolean {
  if (!text) return false;
  const head = text.slice(0, 2000);
  return CUES.some((cue) => cue.test(head));
}
