import { describe, expect, it } from "vitest";

import type { SeqEvent } from "./chatWs";
import { ChatStore } from "./store.svelte";
import { isTransferOrigin, transferNote } from "./transfer";

// Stand-ins shaped like the daemon's `transfer_context` wording.
const MOVED = "Chimaera moved this conversation to the cloud. You now run in the cloud.";
const RECOVERED = "Chimaera moved this conversation to the cloud because the other machine stopped responding. You now run in the cloud, continuing from the last saved point.";
const RECOVERED_HOME = "Chimaera moved this conversation back to the user's computer because the other machine stopped responding. You now run on the user's computer, continuing from the last saved point.";

describe("transfer pick-up notes", () => {
  it("keeps the daemon's pick-up as a user row with its origin and journal time", () => {
    const store = new ChatStore();
    store.apply({ seq: 1, ts: 1_000, ev: { type: "user_message", text: "hi", id: "u1" } } as SeqEvent);
    store.apply({ seq: 2, ts: 5_000, ev: { type: "user_message", text: MOVED, origin: "moved" } } as SeqEvent);
    store.apply({ seq: 3, ts: 9_000, ev: { type: "user_message", text: RECOVERED, origin: "recovered" } } as SeqEvent);
    const [typed, pickUp, recovered] = store.blocks;
    expect(typed).toMatchObject({ kind: "user", origin: null, sentAtMs: 1_000 });
    expect(pickUp).toMatchObject({ kind: "user", origin: "moved", text: MOVED, sentAtMs: 5_000 });
    expect(recovered).toMatchObject({ kind: "user", origin: "recovered", sentAtMs: 9_000 });
    // Only the daemon's own transfer rows become notes.
    for (const origin of ["moved", "home", "recovered"]) expect(isTransferOrigin(origin)).toBe(true);
    for (const origin of ["remote", "restart", "worker", null]) expect(isTransferOrigin(origin)).toBe(false);
  });

  it("says where the conversation went, keyed by its origin tag", () => {
    expect(transferNote("moved", MOVED)).toBe("Continued in the cloud");
    expect(transferNote("home", MOVED)).toBe("Back on your computer");
    // A recovery is its own tag now: the text no longer decides it.
    expect(transferNote("moved", RECOVERED)).toBe("Continued in the cloud");
    expect(transferNote("recovered", RECOVERED)).toBe("Continued in the cloud after this computer stopped responding");
    // Its direction comes from the daemon's sentence.
    expect(transferNote("recovered", RECOVERED_HOME)).toBe("Back on your computer after the cloud stopped responding");
    // Read on a phone, the computer that stopped is yours, not this one.
    expect(transferNote("recovered", RECOVERED, true)).toBe("Continued in the cloud after your computer stopped responding");
    // The agent-facing words never reach the note.
    for (const note of [transferNote("recovered", RECOVERED), transferNote("recovered", RECOVERED_HOME)]) {
      expect(note).not.toMatch(/host|checkpoint|session|saved point/i);
    }
  });
});
