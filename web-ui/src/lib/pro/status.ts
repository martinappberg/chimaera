import type { ProStatus } from "../net/native";

/** Informational messages older native shells reported through `error` while
 * the always-on connection came up, and the code newer shells send instead in
 * `connection_warning`. */
const LEGACY_WARNINGS: ReadonlyMap<string, string> = new Map([
  ["You're signed in. Your Pro connection is preparing; Chimaera will reconnect automatically.", "connection_preparing"],
  ["Your account is up to date. The Pro connection is not ready yet; Chimaera will retry automatically.", "connection_retrying"],
]);

type StatusFields = Pick<ProStatus, "error" | "connection_warning">;

/** Codes for a browser sign-in that ended without signing in. The person is
 * simply signed out: the page keeps its plans and shows one quiet line. */
const SIGN_IN_NOTES: ReadonlySet<string> = new Set(["sign_in_timed_out", "sign_in_incomplete", "browser_unavailable"]);

/** A real account failure. An informational connection message or a sign-in
 * attempt that ended is never one: entitlement, the plan badge and plan
 * choice must not depend on it. */
export function accountFailure(status: StatusFields | null | undefined): string | null {
  const error = status?.error ?? null;
  return error !== null && (LEGACY_WARNINGS.has(error) || SIGN_IN_NOTES.has(error)) ? null : error;
}

/** The code of the last browser sign-in that ended without signing in, while
 * still signed out; null otherwise (`presentation.ts` `signInNoteCopy`). */
export function signInNote(status: Pick<ProStatus, "error" | "signed_in"> | null | undefined): string | null {
  const error = status?.error ?? null;
  return error !== null && !status?.signed_in && SIGN_IN_NOTES.has(error) ? error : null;
}

/** A failure the app rechecks on its own, so the page offers no manual check:
 * a service that doesn't support this version is re-read every ten minutes. */
export function rechecksItself(failure: string | null | undefined): boolean {
  return failure === "service_unsupported";
}

/** A failed or overdue payment. It is resolved in billing, never by checkout. */
export function paymentDue(status: Pick<ProStatus, "signed_in" | "payment_due"> | null | undefined): boolean {
  return status?.signed_in === true && status.payment_due === true;
}

/** Which informational connection state the account is in, if any:
 * `connection_preparing`, `connection_retrying`, `account_unreachable`, or a
 * newer shell's code (older shells' two messages in `error` map to the first
 * two). Local work and the account itself are unaffected, so each shows as one
 * quiet line (`presentation.ts` `connectionWarningCopy`) with no action, and
 * none is a failure or an entitlement signal. Null when there is none. */
export function connectionWarningCode(status: StatusFields | null | undefined): string | null {
  if (status?.connection_warning) return status.connection_warning;
  return status?.error != null ? LEGACY_WARNINGS.get(status.error) ?? null : null;
}
