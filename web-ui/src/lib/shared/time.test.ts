import { describe, expect, it } from "vitest";

import {
  formatMessageTimestamp,
  messageTimestampRefreshIn,
} from "./time";

const at = (year: number, month: number, day: number, hour: number, minute = 0, second = 0) =>
  new Date(year, month - 1, day, hour, minute, second).getTime();

describe("formatMessageTimestamp", () => {
  const now = at(2026, 7, 20, 16);
  const time = (value: number) =>
    new Intl.DateTimeFormat("en-US", { hour: "numeric", minute: "2-digit" }).format(value);

  it("uses the compact relative ladder through the first two hours", () => {
    expect(formatMessageTimestamp(now - 59_000, now, "en-US")).toBe("now");
    expect(formatMessageTimestamp(now - 60_000, now, "en-US")).toBe("1m ago");
    expect(formatMessageTimestamp(now - 59 * 60_000, now, "en-US")).toBe("59m ago");
    expect(formatMessageTimestamp(now - 90 * 60_000, now, "en-US")).toBe("1h ago");
  });

  it("switches to local calendar labels after two hours", () => {
    const today = at(2026, 7, 20, 13, 30);
    const yesterday = at(2026, 7, 19, 14, 30);
    const nearby = at(2026, 7, 18, 14, 30);
    expect(formatMessageTimestamp(today, now, "en-US")).toBe(`${time(today)} today`);
    expect(formatMessageTimestamp(yesterday, now, "en-US")).toBe(
      `${time(yesterday)} yesterday`,
    );
    expect(formatMessageTimestamp(nearby, now, "en-US")).toBe(`Sat ${time(nearby)}`);
  });

  it("uses a dated local format outside the nearby-week window", () => {
    const thisYear = at(2026, 7, 7, 14, 30);
    const previousYear = at(2025, 7, 7, 14, 30);
    const datedTime = new Intl.DateTimeFormat("en-US", {
      month: "short",
      day: "numeric",
      hour: "numeric",
      minute: "2-digit",
    }).format(thisYear);
    const datedYear = new Intl.DateTimeFormat("en-US", {
      year: "numeric",
      month: "short",
      day: "numeric",
    }).format(previousYear);
    expect(formatMessageTimestamp(thisYear, now, "en-US")).toBe(datedTime);
    expect(formatMessageTimestamp(previousYear, now, "en-US")).toBe(datedYear);
  });

  it("honors the locale's hour cycle", () => {
    const sent = at(2026, 7, 20, 13, 30);
    const expected = new Intl.DateTimeFormat("sv-SE", {
      hour: "numeric",
      minute: "2-digit",
    }).format(sent);
    expect(formatMessageTimestamp(sent, now, "sv-SE")).toBe(`${expected} today`);
  });
});

describe("messageTimestampRefreshIn", () => {
  it("schedules relative boundaries without per-message polling", () => {
    const sent = at(2026, 7, 20, 12);
    expect(messageTimestampRefreshIn(sent, sent)).toBe(60_025);
    expect(messageTimestampRefreshIn(sent, sent + 61_000)).toBe(59_025);
    expect(messageTimestampRefreshIn(sent, sent + 90 * 60_000)).toBe(30 * 60_000 + 25);
  });

  it("caps the first refresh when a new message is ahead of the view clock", () => {
    const sent = at(2026, 7, 20, 16);
    const staleViewClock = at(2026, 7, 20, 12);
    expect(messageTimestampRefreshIn(sent, staleViewClock)).toBe(60_025);
  });
});
