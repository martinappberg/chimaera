import { describe, expect, it } from "vitest";
import { dictationParts, insertDictation, joinParts, joinSpoken, PauseDetector } from "./voice.svelte";
import { DICTATION_LANGUAGES } from "./voiceLanguages";

describe("insertDictation", () => {
  it("fills an empty draft", () => {
    expect(insertDictation("", 0, "run the tests")).toEqual({ draft: "run the tests", caret: 13 });
  });

  it("spaces the words from what's around them, like typed words", () => {
    expect(insertDictation("please", 6, "run it")).toEqual({ draft: "please run it", caret: 13 });
    expect(insertDictation("and commit", 0, "run it")).toEqual({
      draft: "run it and commit",
      caret: 6,
    });
    expect(insertDictation("please  now", 7, "run it")).toEqual({
      draft: "please run it now",
      caret: 13,
    });
  });

  it("clamps a stale caret into the draft", () => {
    expect(insertDictation("abc", 99, "d")).toEqual({ draft: "abc d", caret: 5 });
    expect(insertDictation("abc", -4, "d")).toEqual({ draft: "d abc", caret: 1 });
  });
});

describe("dictationParts", () => {
  it("adds nothing until words arrive", () => {
    const p = dictationParts("please", " now", "", "");
    expect(joinParts(p)).toBe("please now");
  });

  it("spaces the words from the draft around them", () => {
    const p = dictationParts("please", "now", "run the", "tests");
    expect(p).toEqual({ before: "please ", finals: "run the", gap: " ", interim: "tests", after: " now" });
    expect(joinParts(p)).toBe("please run the tests now");
  });

  it("keeps the user's own whitespace", () => {
    expect(joinParts(dictationParts("please\n", "\nthanks", "run it", ""))).toBe("please\nrun it\nthanks");
    expect(joinParts(dictationParts("", "", "", " forming "))).toBe("forming");
  });
});

describe("joinSpoken", () => {
  it("joins utterances with one space and drops empty ones", () => {
    expect(joinSpoken("Hello there.", "", "  How are you? ")).toBe("Hello there. How are you?");
    expect(joinSpoken("", "")).toBe("");
  });
});

describe("dictation languages", () => {
  it("mirrors the daemon's list (voice::LANGUAGES)", () => {
    expect(DICTATION_LANGUAGES.map((l) => l.code).sort()).toEqual(
      ["en", "es", "fr", "ja", "de", "pt", "it", "ko", "hi", "id", "ru", "pl", "tr", "nl", "uk", "el", "cs", "da", "sv", "no"].sort(),
    );
  });
});

describe("PauseDetector", () => {
  const run = (levels: number[]) => {
    const d = new PauseDetector();
    return levels.flatMap((l, i) => (d.feed(l) ? [i] : []));
  };
  const speech = (n: number, level = 0.15) => new Array(n).fill(level);
  const quiet = (n: number, level = 0.002) => new Array(n).fill(level);

  it("ends a phrase at the first real pause after enough speech", () => {
    // 1 s of speech, then silence: the 8th silent chunk (800 ms) ends it.
    expect(run([...speech(10), ...quiet(12)])).toEqual([17]);
  });

  it("ignores short gaps between words and pauses after too little speech", () => {
    expect(run([...speech(10), ...quiet(5), ...speech(10)])).toEqual([]);
    expect(run([...speech(3), ...quiet(20)])).toEqual([]);
  });

  it("finds each phrase of a longer dictation", () => {
    expect(run([...speech(10), ...quiet(8), ...speech(10), ...quiet(8)])).toEqual([17, 35]);
  });

  it("ends a long unbroken phrase at a shorter breath", () => {
    // 3 s in, a 500 ms breath is no pause; 11 s in, it is (the 4th chunk).
    expect(run([...speech(30), ...quiet(5), ...speech(1)])).toEqual([]);
    expect(run([...speech(110), ...quiet(5)])).toEqual([113]);
  });

  it("judges silence against the speaker's own loudness", () => {
    // A noisy room (0.02) under loud speech (0.3) still reads as a pause.
    expect(run([...speech(10, 0.3), ...quiet(8, 0.02)])).toEqual([17]);
    // Steady noise alone never becomes a phrase.
    expect(run(quiet(40, 0.02))).toEqual([]);
  });
});
