// The page-world recorder for pane views an agent drives (the agent browser's
// app engine, and a pane the user shares with an agent). Injected by
// `chimaera-app`'s `shell/native_view/mac.rs` into the PAGE's own world at
// document start (main frame), or into a running page on `instrument`.
//
// It notes what an agent needs to debug a page and can't see from the
// bridge's isolated world: console output, uncaught errors and unhandled
// rejections, failed or slow `fetch`/`XMLHttpRequest`s and the number still
// in flight, and resources that failed to load. Each note goes to the pane
// bridge (`pane_bridge.js`, its own content world) as a `chimaera:rec`
// CustomEvent on `document` whose `detail` is a JSON string — a string
// crosses worlds, an object would not. The bridge keeps the rings and caps
// every entry; this side only keeps its own work small.
//
// The page can see and forge all of this (it is the page's world), so what
// it records is the page's own testimony, never something the shell acts
// on. Original behaviour is kept: every wrapper calls through.
(() => {
  "use strict";
  const FLAG = Symbol.for("chimaera.paneAgent");
  if (globalThis[FLAG] === true) return;
  try {
    Object.defineProperty(globalThis, FLAG, { value: true });
  } catch {
    return;
  }

  const TEXT_MAX = 1800;
  const URL_MAX = 1500;
  const SLOW_MS = 1000;
  // Keys an object's JSON may visit before the rest is left out: a page
  // logging a huge object must not stall on our account.
  const KEYS_MAX = 200;

  const dispatch = EventTarget.prototype.dispatchEvent;
  const Custom = CustomEvent;
  const stringify = JSON.stringify;
  const now = () => Date.now();
  const clock = () => performance.now();

  // A page whose own `chimaera:rec` listener logs must not loop us.
  let busy = false;
  const send = (rec) => {
    if (busy) return;
    busy = true;
    try {
      dispatch.call(document, new Custom("chimaera:rec", { detail: stringify(rec) }));
    } catch {
      /* the document is going */
    } finally {
      busy = false;
    }
  };

  const clip = (s, max) => (s.length > max ? `${s.slice(0, max - 1)}…` : s);
  const show = (v) => {
    if (typeof v === "string") return v;
    if (v instanceof Error) return `${v.name}: ${v.message}`;
    if (v === null || (typeof v !== "object" && typeof v !== "function")) return String(v);
    if (typeof v === "function") return `[function ${v.name || "anonymous"}]`;
    try {
      let n = 0;
      const s = stringify(v, (_k, x) => (++n > KEYS_MAX ? undefined : x));
      if (s !== undefined) return s;
    } catch {
      /* cyclic, BigInt, a throwing getter */
    }
    try {
      return String(v);
    } catch {
      return "[unprintable]";
    }
  };
  const withStack = (e) => {
    if (!(e instanceof Error)) return show(e);
    const stack = typeof e.stack === "string" && e.stack !== "" ? `\n${e.stack}` : "";
    return `${e.name}: ${e.message}${stack}`;
  };
  const abs = (u) => {
    try {
      return new URL(u, location.href).href;
    } catch {
      return String(u);
    }
  };

  const logged = (level, text) => send({ t: "console", level, text: clip(text, TEXT_MAX), at: now() });
  const failed = (method, url, status, error, ms) =>
    send({
      t: "net",
      method: String(method || "GET").toUpperCase().slice(0, 16),
      url: clip(abs(url), URL_MAX),
      status,
      error: error === null ? null : clip(String(error), 300),
      ms: Math.round(ms),
      at: now(),
    });

  // --- console --------------------------------------------------------------------
  for (const level of ["log", "info", "warn", "error", "debug"]) {
    const original = console[level];
    if (typeof original !== "function") continue;
    console[level] = function (...args) {
      try {
        logged(level, args.map(show).join(" "));
      } catch {
        /* never break the page's logging */
      }
      return original.apply(this, args);
    };
  }

  // --- uncaught errors and failed resources ------------------------------------
  addEventListener(
    "error",
    (e) => {
      if (e instanceof ErrorEvent) {
        const where = e.filename ? ` (${e.filename}:${e.lineno}:${e.colno})` : "";
        logged("error", `Uncaught ${e.error !== undefined && e.error !== null ? withStack(e.error) : e.message}${where}`);
        return;
      }
      // A resource's error event doesn't bubble; capture on window sees it.
      const el = e.target;
      if (!(el instanceof Element)) return;
      const url = el.currentSrc || el.src || el.href;
      if (typeof url === "string" && url !== "") failed("GET", url, null, `<${el.tagName.toLowerCase()}> failed to load`, 0);
    },
    true,
  );
  addEventListener("unhandledrejection", (e) => logged("error", `Unhandled rejection: ${withStack(e.reason)}`));

  // --- requests ---------------------------------------------------------------------
  let inflight = 0;
  let pending = false;
  // One report per turn however many requests start or end in it.
  const moved = (by) => {
    inflight = Math.max(0, inflight + by);
    if (pending) return;
    pending = true;
    queueMicrotask(() => {
      pending = false;
      send({ t: "inflight", n: inflight });
    });
  };
  const aborted = (e) => e !== null && typeof e === "object" && e.name === "AbortError";

  const fetch0 = globalThis.fetch;
  if (typeof fetch0 === "function") {
    globalThis.fetch = function (input, init) {
      let method = "GET";
      let url = "";
      try {
        method = init?.method ?? (input instanceof Request ? input.method : "GET");
        url = typeof input === "string" ? input : input instanceof URL ? input.href : (input?.url ?? String(input));
      } catch {
        /* an exotic input: fetch itself will say */
      }
      const t0 = clock();
      moved(1);
      let p;
      try {
        p = fetch0.apply(this, arguments);
      } catch (e) {
        moved(-1);
        throw e;
      }
      // Watching the promise marks it handled: an unawaited failing fetch
      // no longer raises `unhandledrejection`, but its failure is noted here.
      Promise.resolve(p).then(
        (r) => {
          moved(-1);
          const ms = clock() - t0;
          if (r.status >= 400 || ms >= SLOW_MS) failed(method, url, r.status, null, ms);
        },
        (e) => {
          moved(-1);
          if (!aborted(e)) failed(method, url, null, e?.message ?? show(e), clock() - t0);
        },
      );
      return p;
    };
  }

  const XHR = globalThis.XMLHttpRequest;
  if (typeof XHR === "function") {
    const open0 = XHR.prototype.open;
    const send0 = XHR.prototype.send;
    const meta = new WeakMap();
    XHR.prototype.open = function (method, url) {
      meta.set(this, { method: String(method), url: String(url), t0: 0, abort: false });
      return open0.apply(this, arguments);
    };
    XHR.prototype.send = function () {
      const m = meta.get(this);
      if (m !== undefined) {
        m.t0 = clock();
        m.abort = false;
        moved(1);
        const xhr = this;
        xhr.addEventListener("abort", () => (m.abort = true), { once: true });
        xhr.addEventListener(
          "loadend",
          () => {
            moved(-1);
            const ms = clock() - m.t0;
            if (m.abort) return;
            if (xhr.status === 0) failed(m.method, m.url, null, "network error", ms);
            else if (xhr.status >= 400 || ms >= SLOW_MS) failed(m.method, m.url, xhr.status, null, ms);
          },
          { once: true },
        );
      }
      return send0.apply(this, arguments);
    };
  }
})();
