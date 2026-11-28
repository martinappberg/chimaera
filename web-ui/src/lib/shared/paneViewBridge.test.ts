import { describe, expect, it } from "vitest";
import {
  chordInit,
  decodeFrameMessage,
  decodeNativeViewEvent,
  isChord,
  referencePill,
  samePill,
  withBridge,
  withPageApi,
  PAGE_TEXT_MAX,
} from "./paneViewBridge";

const key = { key: "r", code: "KeyR", metaKey: true, ctrlKey: false, altKey: false, shiftKey: true };

describe("decodeNativeViewEvent", () => {
  it("reads every kind the shell sends", () => {
    expect(decodeNativeViewEvent({ id: "v1", kind: "focus" })).toEqual({ id: "v1", kind: "focus" });
    expect(decodeNativeViewEvent({ id: "v1", kind: "reference" })).toEqual({ id: "v1", kind: "reference" });
    expect(decodeNativeViewEvent({ id: "v1", kind: "key", ...key })).toEqual({ id: "v1", kind: "key", ...key });
    expect(
      decodeNativeViewEvent({ id: "v1", kind: "page", url: "http://h/raw/t/a.html", title: "Report" }),
    ).toEqual({ id: "v1", kind: "page", url: "http://h/raw/t/a.html", title: "Report" });
    expect(
      decodeNativeViewEvent({
        id: "v1",
        kind: "selection",
        text: "abc",
        context: " Results ",
        rect: { x: 1, y: 2, width: 3, height: 4 },
      }),
    ).toEqual({ id: "v1", kind: "selection", text: "abc", context: "Results", rect: { x: 1, y: 2, width: 3, height: 4 } });
  });

  it("tolerates extra fields and absent optional parts", () => {
    expect(decodeNativeViewEvent({ id: "v1", kind: "focus", extra: 1 })).toEqual({ id: "v1", kind: "focus" });
    expect(decodeNativeViewEvent({ id: "v1", kind: "selection", text: "", context: null, rect: null })).toEqual({
      id: "v1",
      kind: "selection",
      text: "",
      context: null,
      rect: null,
    });
    // A blank heading or a malformed rect reads as none, not as an error.
    expect(
      decodeNativeViewEvent({ id: "v1", kind: "selection", text: "x", context: "  ", rect: { x: "1" } }),
    ).toMatchObject({ context: null, rect: null });
  });

  it("drops anything malformed", () => {
    for (const bad of [
      null,
      "focus",
      { kind: "focus" },
      { id: 1, kind: "focus" },
      { id: "v1", kind: "nope" },
      { id: "v1", kind: "key", ...key, metaKey: "yes" },
      { id: "v1", kind: "key", code: "KeyR" },
      { id: "v1", kind: "selection" },
      { id: "v1", kind: "page", url: "u" },
    ]) {
      expect(decodeNativeViewEvent(bad)).toBeNull();
    }
  });
});

describe("decodeFrameMessage", () => {
  it("reads the bridge's messages without an id", () => {
    expect(decodeFrameMessage({ chimaeraPaneView: { kind: "reference" } })).toEqual({ kind: "reference" });
    expect(decodeFrameMessage({ chimaeraPaneView: { kind: "selection", text: "t", context: null, rect: null } })).toEqual({
      kind: "selection",
      text: "t",
      context: null,
      rect: null,
    });
    expect(decodeFrameMessage({ chimaeraPaneView: { kind: "page", url: "http://h/raw/t/a.html", title: "A" } })).toEqual({
      kind: "page",
      url: "http://h/raw/t/a.html",
      title: "A",
    });
  });

  it("ignores other pages' messages and malformed ones", () => {
    expect(decodeFrameMessage({ type: "webpackOk" })).toBeNull();
    expect(decodeFrameMessage("hello")).toBeNull();
    expect(decodeFrameMessage({ chimaeraPaneView: "focus" })).toBeNull();
    // The framed bridge has no region op any more: a reply is not an event.
    expect(decodeFrameMessage({ chimaeraPaneView: { kind: "region", reqId: 3, text: "", scrollX: 0, scrollY: 0, title: "" } })).toBeNull();
  });
});

describe("chords", () => {
  it("replays a relayed key as a cancelable, bubbling keydown init", () => {
    expect(chordInit(key)).toEqual({ ...key, bubbles: true, cancelable: true });
  });

  it("matches exactly the given chord", () => {
    const ref = { meta: true, ctrl: false, alt: false, shift: true, key: "r" };
    expect(isChord(key, ref)).toBe(true);
    expect(isChord({ ...key, key: "R" }, ref)).toBe(true);
    expect(isChord({ ...key, shiftKey: false }, ref)).toBe(false);
    expect(isChord({ ...key, altKey: true }, ref)).toBe(false);
    expect(isChord({ ...key, key: "t" }, ref)).toBe(false);
  });
});

describe("referencePill", () => {
  it("speaks like the shared chip", () => {
    expect(referencePill({ name: "fix CI" }, undefined, "⇧⌘R")).toEqual({
      text: "reference in agent",
      title: "reference in fix CI (⇧⌘R)",
      enabled: true,
    });
    expect(referencePill({ name: "fix CI" }, "region", "⇧⌘R").title).toBe("reference region in fix CI (⇧⌘R)");
    expect(referencePill(null, undefined, "⇧⌘R")).toEqual({
      text: "reference in agent",
      title: "no agent session in this workspace — start one to reference",
      enabled: false,
    });
  });

  it("compares by content", () => {
    const a = referencePill(null, undefined, "x");
    expect(samePill(a, { ...a })).toBe(true);
    expect(samePill(a, referencePill({ name: "n" }, undefined, "x"))).toBe(false);
    expect(samePill(null, null)).toBe(true);
    expect(samePill(a, null)).toBe(false);
  });
});

describe("withBridge", () => {
  it("adds the query, keeping what the URL carries", () => {
    expect(withBridge("/raw/t-1/report.html")).toBe("/raw/t-1/report.html?bridge=1");
    expect(withBridge("/raw/t-1/a.html?v=2")).toBe("/raw/t-1/a.html?v=2&bridge=1");
    expect(withBridge("/raw/t-1/a.html#top")).toBe("/raw/t-1/a.html?bridge=1#top");
    expect(withBridge("http://h:9/raw/t-1/a%20b.html")).toBe("http://h:9/raw/t-1/a%20b.html?bridge=1");
  });

  it("is idempotent", () => {
    expect(withBridge("/raw/t/a.html?bridge=1")).toBe("/raw/t/a.html?bridge=1");
    expect(withBridge(withBridge("/raw/t/a.html?x=1"))).toBe("/raw/t/a.html?x=1&bridge=1");
  });
});

describe("crashes", () => {
  it("reads the shell's crash report", () => {
    expect(decodeNativeViewEvent({ id: "v1", kind: "crashed" })).toEqual({ id: "v1", kind: "crashed" });
  });
  it("never takes one from a framed page", () => {
    expect(decodeFrameMessage({ chimaeraPaneView: { kind: "crashed" } })).toBeNull();
  });
});

describe("user input (agent browser take-over)", () => {
  it("reads the shell's report", () => {
    expect(decodeNativeViewEvent({ id: "v1", kind: "user_input" })).toEqual({ id: "v1", kind: "user_input" });
  });
  it("never takes one from a framed page", () => {
    expect(decodeFrameMessage({ chimaeraPaneView: { kind: "user_input" } })).toBeNull();
  });
});

describe("page messages", () => {
  it("reads a page's send and draft, from the shell and from a frame", () => {
    expect(decodeNativeViewEvent({ id: "v1", kind: "page_send", text: "a = FORMAT", title: " Quiz\n 1 " })).toEqual({
      id: "v1",
      kind: "page_send",
      text: "a = FORMAT",
      title: "Quiz 1",
    });
    expect(decodeFrameMessage({ chimaeraPaneView: { kind: "page_draft", text: "hi", title: "T" } })).toEqual({
      kind: "page_draft",
      text: "hi",
      title: "T",
    });
  });

  it("drops an empty or malformed one, and caps a long one", () => {
    expect(decodeNativeViewEvent({ id: "v1", kind: "page_send", text: "  ", title: "" })).toBeNull();
    expect(decodeNativeViewEvent({ id: "v1", kind: "page_send", text: 3 })).toBeNull();
    const long = decodeNativeViewEvent({ id: "v1", kind: "page_draft", text: "x".repeat(PAGE_TEXT_MAX + 50) });
    expect(long !== null && "text" in long && long.text.length).toBe(PAGE_TEXT_MAX);
    // A missing title is the page's silence, not a fault.
    expect(decodeNativeViewEvent({ id: "v1", kind: "page_draft", text: "ok" })).toEqual({
      id: "v1",
      kind: "page_draft",
      text: "ok",
      title: "",
    });
  });
});

describe("withPageApi", () => {
  it("asks a framed page for the bridge, a native one for the API alone", () => {
    expect(withPageApi("/raw/t/a.html", { framed: true, theme: null })).toBe("/raw/t/a.html?bridge=1");
    expect(withPageApi("/raw/t/a.html", { framed: false, theme: null })).toBe("/raw/t/a.html?api=1");
  });

  it("carries the theme, keeping the URL's own query and fragment", () => {
    expect(withPageApi("/raw/t/a.html?v=2#top", { framed: false, theme: "eyJt" })).toBe(
      "/raw/t/a.html?v=2&api=1&chimaera-theme=eyJt#top",
    );
    expect(withPageApi("/raw/t/a.html", { framed: true, theme: "" })).toBe("/raw/t/a.html?bridge=1");
  });
});

describe("page size", () => {
  it("reads a page's height and refuses a bad one", () => {
    expect(decodeNativeViewEvent({ id: "v1", kind: "page_size", height: 412.5 })).toEqual({ id: "v1", kind: "page_size", height: 412.5 });
    expect(decodeFrameMessage({ chimaeraPaneView: { kind: "page_size", height: 9e9 } })).toEqual({ kind: "page_size", height: 100_000 });
    expect(decodeNativeViewEvent({ id: "v1", kind: "page_size", height: -1 })).toBeNull();
    expect(decodeNativeViewEvent({ id: "v1", kind: "page_size", height: "400" })).toBeNull();
  });

  it("marks an inline page in its URL", () => {
    expect(withPageApi("/raw/t/a.html", { framed: true, theme: null, inline: 720 })).toBe(
      "/raw/t/a.html?bridge=1&chimaera-inline=720",
    );
  });
});

describe("hover", () => {
  it("reads the pointer coming over the page and leaving it", () => {
    expect(decodeNativeViewEvent({ id: "v1", kind: "hover", inside: true })).toEqual({ id: "v1", kind: "hover", inside: true });
    expect(decodeFrameMessage({ chimaeraPaneView: { kind: "hover", inside: false } })).toEqual({ kind: "hover", inside: false });
    expect(decodeNativeViewEvent({ id: "v1", kind: "hover", inside: "yes" })).toBeNull();
  });
});

describe("page themed", () => {
  it("reads a page saying it wears the theme", () => {
    expect(decodeNativeViewEvent({ id: "v1", kind: "page_themed" })).toEqual({ id: "v1", kind: "page_themed" });
    expect(decodeFrameMessage({ chimaeraPaneView: { kind: "page_themed" } })).toEqual({ kind: "page_themed" });
  });
});

describe("web/1 page events", () => {
  it("decodes act, status and reference, bounded", () => {
    expect(decodeNativeViewEvent({ id: "v", kind: "web_act", action: "ping", payload: { n: 1 } })).toEqual({
      id: "v",
      kind: "web_act",
      action: "ping",
      payload: { n: 1 },
    });
    expect(decodeNativeViewEvent({ id: "v", kind: "web_act", action: "" })).toBeNull();
    expect(decodeNativeViewEvent({ id: "v", kind: "web_act", action: "x".repeat(65) })).toBeNull();
    expect(decodeNativeViewEvent({ id: "v", kind: "web_status", state: "busy", text: "loading" })).toEqual({
      id: "v",
      kind: "web_status",
      state: "busy",
      text: "loading",
    });
    expect(decodeNativeViewEvent({ id: "v", kind: "web_status", state: "purple" })).toBeNull();
    const ref = decodeNativeViewEvent({
      id: "v",
      kind: "web_reference",
      text: "the drawing",
      file: "a.csv",
      line: 3,
      crop: { x: 1, y: 2, width: 30, height: 40 },
      title: "  Page  ",
    });
    expect(ref).toEqual({
      id: "v",
      kind: "web_reference",
      text: "the drawing",
      file: "a.csv",
      line: 3,
      crop: { rect: { x: 1, y: 2, width: 30, height: 40 } },
      title: "Page",
    });
    // A crop that isn't a picture the host can attach is dropped, not the reference.
    const bad = decodeNativeViewEvent({ id: "v", kind: "web_reference", text: "t", crop: "javascript:alert(1)" });
    expect(bad).toMatchObject({ kind: "web_reference", crop: null });
    const png = decodeNativeViewEvent({ id: "v", kind: "web_reference", text: "t", crop: "data:image/png;base64,iVBORw0KGgo=" });
    expect(png).toMatchObject({ crop: { dataUrl: "data:image/png;base64,iVBORw0KGgo=" } });
    // The plugins a page needs come from the daemon, never the page.
    expect(decodeNativeViewEvent({ id: "v", kind: "page_needs", plugins: [{ plugin: "x", name: "X", library: "x" }] })).toBeNull();
  });

  it("carries a framed page's Blob crop", () => {
    const blob = new Blob(["x"], { type: "image/png" });
    const ev = decodeFrameMessage({ chimaeraPaneView: { kind: "web_reference", text: "t", crop: blob } });
    expect(ev).toMatchObject({ kind: "web_reference", crop: { blob } });
  });
});
