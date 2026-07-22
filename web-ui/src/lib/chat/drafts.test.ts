import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { saveDraft, volatileChatDrafts } from "./drafts";

class MemoryStorage {
  private values = new Map<string, string>();

  get length(): number {
    return this.values.size;
  }

  key(index: number): string | null {
    return [...this.values.keys()][index] ?? null;
  }

  getItem(key: string): string | null {
    return this.values.get(key) ?? null;
  }

  setItem(key: string, value: string): void {
    this.values.set(key, value);
  }

  removeItem(key: string): void {
    this.values.delete(key);
  }
}

describe("composer reload safety", () => {
  const ids = [
    "small",
    "image",
    "large",
    "blocked",
    ...Array.from({ length: 26 }, (_, index) => `bounded-${index}`),
  ];

  beforeEach(() => vi.stubGlobal("sessionStorage", new MemoryStorage()));

  afterEach(() => {
    for (const id of ids) saveDraft(id, "", []);
    vi.unstubAllGlobals();
  });

  it("treats successfully stored text as reload-safe", () => {
    saveDraft("small", "kept across reload", []);
    expect(get(volatileChatDrafts).has("small")).toBe(false);
  });

  it("holds reloads for memory-only attachments and oversized text", () => {
    saveDraft("image", "caption", [
      { media_type: "image/png", data: "AA==", label: "image 1×1" },
    ]);
    saveDraft("large", "x".repeat(64 * 1024 + 1), []);
    expect([...get(volatileChatDrafts)].sort()).toEqual(["image", "large"]);
  });

  it("holds reloads when session storage rejects the draft", () => {
    vi.stubGlobal("sessionStorage", {
      length: 0,
      key: () => null,
      getItem: () => null,
      removeItem: () => {},
      setItem: () => {
        throw new Error("quota");
      },
    });
    saveDraft("blocked", "cannot persist", []);
    expect(get(volatileChatDrafts).has("blocked")).toBe(true);
    saveDraft("blocked", "", []);
    expect(get(volatileChatDrafts).has("blocked")).toBe(false);
  });

  it("does not evict another draft when updating a key at the storage bound", () => {
    for (let index = 0; index < 24; index += 1) {
      saveDraft(`bounded-${index}`, `draft ${index}`, []);
    }
    saveDraft("bounded-0", "updated", []);
    expect(sessionStorage.getItem("chimaera.chatDraft.bounded-1")).toBe("draft 1");
    expect(sessionStorage.length).toBe(24);
  });
});
