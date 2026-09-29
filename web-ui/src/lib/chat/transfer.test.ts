import { describe, expect, it } from "vitest";

import type { SeqEvent } from "./chatWs";
import { ChatStore } from "./store.svelte";
import { isTransferOrigin, transferNote } from "./transfer";

// Stand-ins shaped like the daemon's `transfer_context` wording.
const MOVED = "This Chimaera session is now on another host. Re-check tools and paths.";
const RECOVERED = `${MOVED} This is an automatic continuation from the last acknowledged checkpoint after the previous host stopped responding.`;

describe("transfer pick-up notes", () => {
  it("keeps the daemon's pick-up as a user row with its origin and journal time", () => {
    const store = new ChatStore();
    store.apply({ seq: 1, ts: 1_000, ev: { type: "user_message", text: "hi", id: "u1" } } as SeqEvent);
    store.apply({ seq: 2, ts: 5_000, ev: { type: "user_message", text: MOVED, origin: "moved" } } as SeqEvent);
    const [typed, pickUp] = store.blocks;
    expect(typed).toMatchObject({ kind: "user", origin: null, sentAtMs: 1_000 });
    expect(pickUp).toMatchObject({ kind: "user", origin: "moved", text: MOVED, sentAtMs: 5_000 });
    // Only the daemon's own transfer rows become notes.
    expect(isTransferOrigin(typed.kind === "user" ? typed.origin : null)).toBe(false);
    expect(isTransferOrigin(pickUp.kind === "user" ? pickUp.origin : null)).toBe(true);
    for (const origin of ["remote", "restart", "worker", null]) expect(isTransferOrigin(origin)).toBe(false);
  });

  it("says where the conversation went, and when it continued after a machine stopped answering", () => {
    const notes = [
      transferNote("moved", MOVED),
      transferNote("home", MOVED),
      transferNote("moved", RECOVERED),
      transferNote("home", RECOVERED),
    ];
    expect(notes[0]).toBe("Continued in the cloud");
    expect(notes[1]).toBe("Back on your computer");
    expect(notes[2]).toBe("Continued in the cloud after this computer stopped responding");
    expect(new Set(notes).size).toBe(notes.length);
    // Read on a phone, the computer that stopped is yours, not this one.
    expect(transferNote("moved", RECOVERED, true)).toBe("Continued in the cloud after your computer stopped responding");
    // The agent-facing words never reach the note.
    for (const note of notes) expect(note).not.toMatch(/host|checkpoint|session/i);
  });
});
