import { describe, expect, it } from "vitest";

import { RevealLedger } from "./revealLedger";

describe("RevealLedger: reveal-cursor carry", () => {
  it("a fresh tail hides everything; ticks reveal and advance the cursor", () => {
    const ledger = new RevealLedger();
    expect(ledger.rebuildTail(10)).toBe(0);
    expect(ledger.pending).toBe(10);
    const t = ledger.take();
    expect(t.fromPrefix).toBe(0);
    expect(t.fromTail).toBe(2); // max(2, ceil(10/6)) = 2
    expect(ledger.tailRevealed).toBe(2);
    expect(ledger.pending).toBe(8);
  });

  it("closing a fully-revealed head consumes the cursor and adds no prefix debt", () => {
    const ledger = new RevealLedger();
    ledger.rebuildTail(5);
    while (ledger.pending > 0) ledger.take();
    expect(ledger.tailRevealed).toBe(5);
    expect(ledger.closeSegment(5)).toBe(5); // all shown up front
    expect(ledger.prefixHidden).toBe(0);
    expect(ledger.tailRevealed).toBe(0);
  });

  it("closing a partially-revealed head splits shown vs prefix-hidden", () => {
    const ledger = new RevealLedger();
    ledger.rebuildTail(10);
    ledger.take(); // 2 revealed
    // The whole 10-word tail closes: 2 stay shown, 8 become prefix debt.
    expect(ledger.closeSegment(10)).toBe(2);
    expect(ledger.prefixHidden).toBe(8);
    expect(ledger.tailRevealed).toBe(0);
    // The follow-up rebuild declares the NEW tail (3 words, all hidden).
    expect(ledger.rebuildTail(3)).toBe(0);
    expect(ledger.pending).toBe(11);
  });

  it("prefix drains before tail, in document order", () => {
    const ledger = new RevealLedger();
    ledger.rebuildTail(12);
    ledger.take(); // 2 revealed
    ledger.closeSegment(12); // 2 shown, 10 prefix-hidden
    ledger.rebuildTail(12); // fresh tail, 12 hidden
    // pending 22 → take = max(2, ceil(22/6)) = 4, all from prefix first.
    expect(ledger.take()).toEqual({ fromPrefix: 4, fromTail: 0 });
    // Drain the remaining 6 prefix + spill into tail.
    let spill = { fromPrefix: 0, fromTail: 0 };
    while (ledger.prefixHidden > 0) spill = ledger.take();
    expect(spill.fromTail).toBeGreaterThanOrEqual(0);
    const t = ledger.take();
    expect(t.fromPrefix).toBe(0);
    expect(t.fromTail).toBeGreaterThan(0);
  });

  it("the duplicate-tail trap: close + MANDATORY rebuild keeps shown words shown", () => {
    // Old tail: one paragraph, 4 words, fully revealed.
    const ledger = new RevealLedger();
    ledger.rebuildTail(4);
    while (ledger.pending > 0) ledger.take();
    expect(ledger.tailRevealed).toBe(4);
    // The paragraph closes; the NEW open tail is a string-equal duplicate
    // paragraph (4 words, none of them seen yet). Skipping the rebuild
    // because the source looks unchanged would leave tailRevealed consumed
    // (0) while 4 shown-looking words hang in the DOM — the re-hide/re-fade
    // bug. The contract: rebuild ALWAYS follows a close.
    expect(ledger.closeSegment(4)).toBe(4);
    expect(ledger.rebuildTail(4)).toBe(0); // duplicate content: genuinely new words, hidden
    expect(ledger.pending).toBe(4);
    expect(ledger.tailRevealed).toBe(0);
  });

  it("a tail rebuild never re-hides revealed words (clamp, monotone)", () => {
    const ledger = new RevealLedger();
    ledger.rebuildTail(6);
    while (ledger.pending > 0) ledger.take();
    // The tail grows (same head, more words): the first 6 stay shown.
    expect(ledger.rebuildTail(9)).toBe(6);
    expect(ledger.tailHidden).toBe(3);
    // A shrunk tail (rewrite fallback) clamps rather than going negative.
    expect(ledger.rebuildTail(2)).toBe(2);
    expect(ledger.tailHidden).toBe(0);
    expect(ledger.tailRevealed).toBe(2);
  });

  it("multi-segment close in one advance carries the cursor across all of them", () => {
    const ledger = new RevealLedger();
    ledger.rebuildTail(10);
    ledger.take();
    ledger.take(); // 4 revealed
    // The tail's head closes as TWO segments (3 + 3 words): the cursor covers
    // the first fully and one word of the second.
    expect(ledger.closeSegment(3)).toBe(3);
    expect(ledger.closeSegment(3)).toBe(1);
    expect(ledger.prefixHidden).toBe(2);
    expect(ledger.tailRevealed).toBe(0);
    expect(ledger.rebuildTail(4)).toBe(0);
    expect(ledger.pending).toBe(6);
  });

  it("take() on an empty ledger is a no-op; reset clears everything", () => {
    const ledger = new RevealLedger();
    expect(ledger.take()).toEqual({ fromPrefix: 0, fromTail: 0 });
    ledger.rebuildTail(5);
    ledger.take();
    ledger.reset();
    expect(ledger.pending).toBe(0);
    expect(ledger.tailRevealed).toBe(0);
  });
});
