import { describe, expect, it } from "vitest";
import { retryableCaffeinateError } from "./caffeinateRetry";

describe("Caffeinate reconnect classification", () => {
  it("retries common offline and tunnel transport failures", () => {
    expect(retryableCaffeinateError("ssh: Could not resolve hostname cluster", true)).toBe(true);
    expect(retryableCaffeinateError("connect: Network is unreachable", true)).toBe(true);
    expect(retryableCaffeinateError("ssh tunnel exited early: exit status 255", true)).toBe(true);
    expect(retryableCaffeinateError("anything while WebKit reports offline", false)).toBe(true);
  });

  it("leaves authentication and configuration failures manual", () => {
    expect(retryableCaffeinateError("Permission denied (publickey,password)", true)).toBe(false);
    expect(retryableCaffeinateError("REMOTE HOST IDENTIFICATION HAS CHANGED", true)).toBe(false);
    expect(retryableCaffeinateError("failed to spawn ssh tunnel: No such file", true)).toBe(false);
    expect(retryableCaffeinateError("no published release provides chimaera-x", true)).toBe(false);
  });
});
