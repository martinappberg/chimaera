import { describe, expect, it } from "vitest";
import { ParkedViews } from "./paneView";

/** A clock the test advances by hand. */
function fakeClock() {
  let now = 0;
  let seq = 0;
  const due = new Map<number, { at: number; fn: () => void }>();
  return {
    set: (fn: () => void, ms: number) => {
      seq += 1;
      due.set(seq, { at: now + ms, fn });
      return seq as unknown as ReturnType<typeof setTimeout>;
    },
    clear: (t: ReturnType<typeof setTimeout>) => void due.delete(t as unknown as number),
    advance(ms: number) {
      now += ms;
      for (const [k, d] of [...due]) {
        if (d.at <= now) {
          due.delete(k);
          d.fn();
        }
      }
    },
  };
}

function lot() {
  const hidden: string[] = [];
  const closed: string[] = [];
  const clock = fakeClock();
  const views = new ParkedViews(
    (id) => hidden.push(id),
    (id) => closed.push(id),
    2500,
    clock,
  );
  return { views, hidden, closed, clock };
}

describe("ParkedViews (a move keeps its page)", () => {
  it("hides a parked view and hands it to the next mount with its URL", () => {
    const { views, hidden, closed, clock } = lot();
    views.park("browser:t1", "v1", "/proxy/p1/lab", "http://h/proxy/p1/lab/tree");
    expect(hidden).toEqual(["v1"]);
    expect(views.take("browser:t1")).toEqual({
      id: "v1",
      url: "/proxy/p1/lab",
      page: "http://h/proxy/p1/lab/tree",
    });
    clock.advance(10_000);
    expect(closed).toEqual([]);
  });

  it("closes a view nobody takes back, once", () => {
    const { views, closed, clock } = lot();
    views.park("html:/r.html", "v1", "/raw/t/r.html");
    clock.advance(2499);
    expect(closed).toEqual([]);
    clock.advance(1);
    expect(closed).toEqual(["v1"]);
    expect(views.take("html:/r.html")).toBeNull();
  });

  it("gives a view only to its own keep", () => {
    const { views } = lot();
    views.park("browser:t1", "v1", "/proxy/p1/");
    expect(views.take("browser:t2")).toBeNull();
    expect(views.take("browser:t1")?.id).toBe("v1");
    expect(views.take("browser:t1")).toBeNull();
  });

  it("closes an older view parked under the same keep at once", () => {
    const { views, closed, clock } = lot();
    views.park("html:/r.html", "v1", "/raw/a/r.html");
    views.park("html:/r.html", "v2", "/raw/b/r.html");
    expect(closed).toEqual(["v1"]);
    expect(views.take("html:/r.html")?.id).toBe("v2");
    clock.advance(10_000);
    expect(closed).toEqual(["v1"]);
  });
});
