import { describe, expect, it } from "vitest";
import { looksLikeAuthFailure } from "./authFailure";

describe("auth-shaped agent errors", () => {
  it("recognizes each runtime's signed-out wording", () => {
    expect(looksLikeAuthFailure("unexpected status 401 Unauthorized: Missing bearer or basic authentication in header, url: https://api.openai.com/v1/responses")).toBe(true);
    expect(looksLikeAuthFailure("codex: Falling back from WebSockets to HTTPS transport. unexpected status 401 Unauthorized")).toBe(true);
    expect(looksLikeAuthFailure("Not logged in · Please run /login")).toBe(true);
    expect(looksLikeAuthFailure("Invalid API key · Please run /login")).toBe(true);
    expect(looksLikeAuthFailure("Cursor CLI is not signed in on this host.")).toBe(true);
    expect(looksLikeAuthFailure("Authentication required. Please run 'agent login' first")).toBe(true);
  });
  it("leaves other failures alone", () => {
    expect(looksLikeAuthFailure("turn failed")).toBe(false);
    expect(looksLikeAuthFailure("GLIBC_2.28 not found")).toBe(false);
    expect(looksLikeAuthFailure("rate limit reached, retry in 40s (status 429)")).toBe(false);
    expect(looksLikeAuthFailure("")).toBe(false);
    expect(looksLikeAuthFailure(null)).toBe(false);
  });
});
