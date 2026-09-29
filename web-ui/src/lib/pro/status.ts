import type { ProStatus } from "../net/native";

/** Informational messages older native shells reported through `error` while
 * the always-on connection came up. Newer shells send `connection_warning`. */
const LEGACY_WARNINGS: ReadonlySet<string> = new Set([
  "You're signed in. Your Pro connection is preparing; Chimaera will reconnect automatically.",
  "Your account is up to date. The Pro connection is not ready yet; Chimaera will retry automatically.",
]);

type StatusFields = Pick<ProStatus, "error" | "connection_warning">;

/** A real account failure. An informational connection message is never one:
 * entitlement, the plan badge and plan choice must not depend on it. */
export function accountFailure(status: StatusFields | null | undefined): string | null {
  const error = status?.error ?? null;
  return error !== null && LEGACY_WARNINGS.has(error) ? null : error;
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

/** The connection behind cloud features is still coming up. Local work and the
 * account itself are unaffected, so this is shown quietly, with no action. */
export function connectionWarning(status: StatusFields | null | undefined): boolean {
  return Boolean(status?.connection_warning) || (status?.error != null && LEGACY_WARNINGS.has(status.error));
}
