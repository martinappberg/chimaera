import { describe, expect, it } from "vitest";

import type { SeqEvent } from "./chatWs";
import { ChatStore } from "./store.svelte";
import journal from "./transferJournal.fixture.json";

/**
 * A chat journal that crossed a machine (`transferJournal.fixture.json`, from
 * the Pro end-to-end harness): a turn in flight on the computer, the daemon's
 * `moved` pick-up on the cloud machine, then phone messages answered there.
 * The journal is what the daemon streams to every view, so folding it is the
 * whole client path: a transcript must show each reply once.
 *
 * The harness's fake `claude` answers each cloud reply with the text TWICE
 * inside the single `message_chunk` (it gave its streamed and its complete
 * frame different message ids, so the driver — which dedupes on that id —
 * emitted both). That doubling is in the journal itself, not something the
 * reducer adds; these tests pin that the store adds no second block or copy
 * of its own, across a transfer's restarted turn ids, mid-transcript `init`s,
 * and a replay re-delivered over a live socket.
 */
const events = journal as SeqEvent[];

function fold(entries: SeqEvent[], into = new ChatStore()): ChatStore {
  for (const entry of entries) into.apply(entry);
  return into;
}

/** The reply text each journal turn carried, in journal order. */
function journaledReplies(): string[] {
  return events
    .filter((e) => e.ev.type === "message_chunk")
    .map((e) => e.ev.text as string);
}

describe("a transferred conversation's transcript", () => {
  it("renders one assistant block per turn, holding exactly the journaled text", () => {
    const blocks = fold(events).blocks;
    const messages = blocks.filter((b) => b.kind === "message");
    expect(messages.map((b) => b.text)).toEqual(journaledReplies());
    // Six turns (the laptop's, the cloud's pick-up, four phone sends), plus
    // the laptop turn's reply before the move: seven replies, none split.
    expect(messages).toHaveLength(7);
  });

  it("puts exactly one reply between the user rows either side of it", () => {
    const shape = fold(events).blocks.flatMap((b) => {
      if (b.kind === "user") return [b.origin === "moved" ? "pickup" : "user"];
      if (b.kind === "message") return ["reply"];
      return [];
    });
    expect(shape).toEqual([
      "user", "reply", // the laptop turn, before the move
      "pickup", "reply", // the daemon's `moved` pick-up, answered in the cloud
      "user", "reply", "user", "reply", "user", "reply", "user", "reply", "user", "reply",
    ]);
  });

  it("keeps the pre-transfer (laptop) reply single and the pick-up's origin", () => {
    const blocks = fold(events).blocks;
    const first = blocks.find((b) => b.kind === "message");
    expect(first).toMatchObject({ text: "Wrote laptop.txt; now running the long step." });
    const pickUp = blocks.find((b) => b.kind === "user" && b.origin === "moved");
    expect(pickUp).toMatchObject({ kind: "user", origin: "moved" });
  });

  it("does not repeat a reply when the journal is delivered again over a live store", () => {
    const store = fold(events);
    const before = store.blocks.length;
    // A reconnect replays from the gap; frames at or below `lastSeq` are dropped.
    fold(events, store);
    expect(store.blocks).toHaveLength(before);
    // Replay-then-live in two slices folds to the same transcript as one pass.
    const split = fold(events.slice(0, 20));
    fold(events.slice(20), split);
    expect(split.blocks.map((b) => b.kind === "message" && b.text)).toEqual(
      fold(events).blocks.map((b) => b.kind === "message" && b.text),
    );
  });
});
