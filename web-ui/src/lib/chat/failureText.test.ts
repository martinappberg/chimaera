import { describe, expect, it } from "vitest";
import { failureText, HEADLINE_MAX } from "./failureText";

describe("failureText", () => {
  it("reads an API error as the API's own message and its status", () => {
    const reason =
      'API Error: 401 {"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"},"request_id":"req_011abc"}';
    const out = failureText(reason);
    expect(out.headline).toBe("invalid x-api-key (401)");
    expect(out.detail).toContain("API Error: 401");
    expect(out.detail).toContain('"request_id": "req_011abc"');
    expect(out.detail).toContain('"type": "authentication_error"');
  });

  it("keeps text around an API error's body in the detail", () => {
    const reason = 'API Error: 529 {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}} · retrying';
    const out = failureText(reason);
    expect(out.headline).toBe("Overloaded (529)");
    expect(out.detail?.endsWith("· retrying")).toBe(true);
  });

  it("falls back to the error's type when the body has no message", () => {
    expect(failureText('API Error: 500 {"error":{"type":"api_error"}}').headline).toBe("api_error (500)");
  });

  it("treats an API error without a readable body as plain text", () => {
    expect(failureText("API Error: 502 Bad Gateway")).toEqual({ headline: "API Error: 502 Bad Gateway", detail: null });
    const broken = 'API Error: 400 {"error": {"message": "x"';
    expect(failureText(broken)).toEqual({ headline: broken, detail: null });
  });

  it("reads a bare error body", () => {
    const out = failureText('{"error":{"message":"model not found","code":404}}');
    expect(out.headline).toBe("model not found");
    expect(out.detail).toContain('"code": 404');
  });

  it("shows a short plain reason whole, with nothing to open", () => {
    expect(failureText("You've hit your limit · resets 3pm")).toEqual({
      headline: "You've hit your limit · resets 3pm",
      detail: null,
    });
    expect(failureText("message not sent — reconnecting").detail).toBeNull();
  });

  it("clips a long plain reason to one line and keeps the whole text", () => {
    const reason = `${"the agent process ended unexpectedly while ".repeat(6)}writing`;
    const out = failureText(reason);
    expect(out.headline.length).toBeLessThanOrEqual(HEADLINE_MAX);
    expect(out.headline.endsWith("…")).toBe(true);
    expect(out.headline).not.toContain("  ");
    expect(out.detail).toBe(reason);
  });

  it("leads a multi-line reason with its first line", () => {
    const out = failureText("Error: spawn failed\n    at run (driver.js:10)");
    expect(out.headline).toBe("Error: spawn failed");
    expect(out.detail).toBe("Error: spawn failed\n    at run (driver.js:10)");
  });

  it("gives no headline for no reason", () => {
    expect(failureText("")).toEqual({ headline: "", detail: null });
    expect(failureText("   ")).toEqual({ headline: "", detail: null });
  });
});
