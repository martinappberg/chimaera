// The pane bridge: the one script that lets the workbench point at things
// in a page it shows in a pane (an HTML report, a proxied web app) — text
// selections, a region's text, focus and chords — without reading the page
// from outside, which a separate process or a sandboxed frame forbids.
//
// Two transports, one script:
// - Native pane view (the app): injected by `chimaera-app`'s
//   `shell/native_view.rs` into a WebKit content world of its own, so the
//   page's scripts can't see these listeners, this object, or the
//   `chimaeraPaneView` message handler. The shell calls `region`/`chip`
//   through `callAsyncJavaScript` in that world.
// - Framed (a browser tab, or an app build without pane views): the daemon
//   appends it to a previewed HTML page; messages go to the parent window,
//   which accepts them only from that frame and only while the user is in
//   it. The page shares this world there, so it could forge a selection or
//   a reference — the parent never sends one or replays a chord for it.
//
// Only trusted (user-made) events act: a chord the page itself handled stays
// with the page, a selection counts only right after real input, and a link
// click is reported (native path) so the shell hands a URL to the user's
// browser only for a click the user made. Nothing here changes the page
// except the reference pill and (a pane an agent drives) the agent's
// cursor, each drawn in a closed shadow root.
//
// Native views also carry what agent driving needs (`chimaera-app`'s
// `native_view_drive`): trusted pointer/key/wheel input reported for
// take-over, the console/network rings the page-world recorder
// (`pane_agent.js`) fills, and the agent cursor. None of it exists in the
// framed transport.
(() => {
  "use strict";
  if (globalThis.__chimaeraPaneView !== undefined) return;
  const native = globalThis.webkit?.messageHandlers?.chimaeraPaneView ?? null;
  const framed = native === null && window.parent !== window;
  if (native === null && !framed) return;

  const post = (msg) => {
    try {
      if (native !== null) native.postMessage(JSON.stringify(msg));
      else window.parent.postMessage({ chimaeraPaneView: msg }, "*");
    } catch {
      /* the host went away */
    }
  };

  const TEXT_MAX = 20000;
  const REGION_MAX = 4000;
  const HEADING_MAX = 120;

  // --- agent driving (native views only) ------------------------------------------
  // Trusted input, reported so the shell can tell its own injected events
  // from the user's (take-over): it matches each report against what it
  // just injected, and only an unmatched one is the user. Wheel events come
  // in bursts; one report per burst is enough to notice a hand.
  let lastWheel = 0;
  const reportInput = (e) => {
    if (!e.isTrusted) return;
    if (e.type === "wheel") {
      const t = Date.now();
      if (t - lastWheel < 200) return;
      lastWheel = t;
    }
    post({ kind: "input", type: e.type });
  };
  if (native !== null) {
    addEventListener("pointerdown", reportInput, true);
    addEventListener("keydown", reportInput, true);
    addEventListener("wheel", reportInput, { capture: true, passive: true });
  }

  // The rings: what the page-world recorder (`pane_agent.js`, present only
  // in a view an agent drives) noted, kept here where the page can't reach
  // them. Ids count from 1 per ring; `since` asks for ids ≥ it; an entry
  // pushed out of a full ring is simply gone.
  const RING_MAX = 300;
  const rings = { console: [], network: [], consoleNext: 1, networkNext: 1, inflight: 0 };
  const capText = (v, max) => {
    const s = typeof v === "string" ? v : v === null || v === undefined ? "" : String(v);
    return s.length > max ? `${s.slice(0, max - 1)}…` : s;
  };
  const finite = (v) => (typeof v === "number" && Number.isFinite(v) ? v : 0);
  const LEVELS = new Set(["log", "info", "warn", "error", "debug"]);
  const pushRing = (ring, entry) => {
    ring.push(entry);
    if (ring.length > RING_MAX) ring.splice(0, ring.length - RING_MAX);
  };
  if (native !== null) {
    document.addEventListener("chimaera:rec", (e) => {
      const detail = e.detail;
      if (typeof detail !== "string" || detail.length > 16384) return;
      let r;
      try {
        r = JSON.parse(detail);
      } catch {
        return;
      }
      if (r === null || typeof r !== "object") return;
      if (r.t === "inflight") {
        rings.inflight = Math.min(100000, Math.max(0, Math.trunc(finite(r.n))));
      } else if (r.t === "console") {
        pushRing(rings.console, {
          id: rings.consoleNext++,
          level: LEVELS.has(r.level) ? r.level : "log",
          text: capText(r.text, 1900),
          at: finite(r.at),
        });
      } else if (r.t === "net") {
        pushRing(rings.network, {
          id: rings.networkNext++,
          method: capText(r.method, 16),
          url: capText(r.url, 1600),
          status: Number.isInteger(r.status) ? r.status : null,
          error: typeof r.error === "string" ? capText(r.error, 300) : null,
          ms: finite(r.ms),
          at: finite(r.at),
        });
      }
    });
  }
  const readRings = (arg) => {
    const cs = finite(Number(arg?.console_since));
    const ns = finite(Number(arg?.network_since));
    return {
      console: rings.console.filter((x) => x.id >= cs),
      network: rings.network.filter((x) => x.id >= ns),
      inflight: rings.inflight,
      console_next: rings.consoleNext,
      network_next: rings.networkNext,
    };
  };

  // The agent's cursor on a pane the user watches (shared or handed over):
  // where the agent points and a ripple where it clicks. A closed shadow
  // root that never takes a hit, faded out when the agent goes quiet.
  let cursorHost = null;
  let cursorArrow = null;
  let cursorTag = null;
  let cursorFade = 0;
  const ensureCursor = () => {
    if (cursorHost !== null && cursorHost.isConnected) return;
    cursorHost = document.createElement("chimaera-pane-cursor");
    cursorHost.style.cssText =
      "all:initial;position:fixed;left:0;top:0;width:0;height:0;z-index:2147483647;pointer-events:none;display:block;";
    const root = cursorHost.attachShadow({ mode: "closed" });
    const style = document.createElement("style");
    style.textContent =
      "*{pointer-events:none;box-sizing:border-box}" +
      ".c{position:fixed;left:0;top:0;transition:transform 90ms ease-out,opacity .4s;will-change:transform}" +
      ".c.idle{opacity:0}" +
      "svg{display:block;filter:drop-shadow(0 1px 2px rgba(0,0,0,.35))}" +
      ".t{position:absolute;left:16px;top:18px;padding:2px 7px;border-radius:9px;white-space:nowrap;" +
      "font:600 11px/16px -apple-system,system-ui,sans-serif;color:#fff;background:var(--a)}" +
      ".t:empty{display:none}" +
      ".r{position:fixed;width:28px;height:28px;margin:-14px 0 0 -14px;border-radius:50%;" +
      "border:2px solid var(--a);animation:r .5s ease-out forwards}" +
      "@keyframes r{from{transform:scale(.3);opacity:.9}to{transform:scale(1.6);opacity:0}}";
    cursorArrow = document.createElement("div");
    cursorArrow.className = "c idle";
    // Built node by node: a page under Trusted Types refuses innerHTML.
    const SVG = "http://www.w3.org/2000/svg";
    const svg = document.createElementNS(SVG, "svg");
    for (const [k, v] of [["width", "18"], ["height", "22"], ["viewBox", "0 0 18 22"]]) svg.setAttribute(k, v);
    const path = document.createElementNS(SVG, "path");
    for (const [k, v] of [
      ["d", "M1.5 1.5v16.2l4.3-4.1 2.9 6.6 2.7-1.2-2.9-6.5h6z"],
      ["fill", "var(--a)"],
      ["stroke", "#fff"],
      ["stroke-width", "1.5"],
      ["stroke-linejoin", "round"],
    ])
      path.setAttribute(k, v);
    svg.append(path);
    cursorArrow.append(svg);
    cursorTag = document.createElement("span");
    cursorTag.className = "t";
    cursorArrow.append(cursorTag);
    root.append(style, cursorArrow);
    document.documentElement.append(cursorHost);
  };
  const drawCursor = (arg) => {
    if (arg === null || typeof arg !== "object" || arg.hide === true) {
      if (cursorHost !== null) cursorHost.remove();
      cursorHost = null;
      return null;
    }
    if (document.documentElement === null) return null;
    ensureCursor();
    const color = typeof arg.color === "string" && /^#[0-9a-fA-F]{6}$/.test(arg.color) ? arg.color : "#6d5dfc";
    cursorHost.style.setProperty("--a", color);
    cursorTag.textContent = typeof arg.label === "string" ? arg.label.slice(0, 40) : "";
    const x = finite(Number(arg.x));
    const y = finite(Number(arg.y));
    cursorArrow.style.transform = `translate(${x - 1}px,${y - 1}px)`;
    cursorArrow.classList.remove("idle");
    if (arg.click === true) {
      const ripple = document.createElement("div");
      ripple.className = "r";
      ripple.style.left = `${x}px`;
      ripple.style.top = `${y}px`;
      cursorArrow.before(ripple);
      setTimeout(() => ripple.remove(), 600);
    }
    clearTimeout(cursorFade);
    cursorFade = setTimeout(() => cursorArrow?.classList.add("idle"), 4000);
    return null;
  };

  // --- focus, input and chords ------------------------------------------------
  /** When the user last pressed, released or typed in the page. */
  let lastInput = 0;
  const INPUT_FRESH_MS = 1500;
  const noteInput = (e) => {
    if (!e.isTrusted) return;
    lastInput = Date.now();
    // A selection the user finished (a drag released, a shift+arrow let
    // go) is reported now: a slow drag's last selectionchange can fall
    // outside INPUT_FRESH_MS, and nothing changes after the release.
    if (e.type === "pointerup" || e.type === "keyup") scheduleSelection();
  };
  addEventListener("pointerdown", (e) => e.isTrusted && post({ kind: "focus" }), true);
  addEventListener("focus", (e) => e.isTrusted && post({ kind: "focus" }));
  for (const type of ["pointerdown", "pointerup", "keydown", "keyup"]) addEventListener(type, noteInput, true);
  // A trusted click on a link: the shell lets that exact URL out to the
  // user's browser, and nothing else (a script's `a.click()` is untrusted).
  const noteLink = (e) => {
    if (!e.isTrusted || native === null) return;
    const a = e.target instanceof Element ? e.target.closest("a[href], area[href]") : null;
    if (a !== null && typeof a.href === "string") post({ kind: "link", href: a.href });
  };
  addEventListener("click", noteLink, true);
  addEventListener("auxclick", noteLink, true);
  // Edit chords stay with the page; another ⌘/Ctrl chord reaches the
  // workbench (close tab, split, palette…) only if the page didn't handle
  // it itself — checked after the event has gone through the page.
  const EDIT_KEYS = new Set(["a", "c", "v", "x", "z"]);
  addEventListener(
    "keydown",
    (e) => {
      if (!e.isTrusted || !(e.metaKey || e.ctrlKey)) return;
      if (!e.shiftKey && !e.altKey && EDIT_KEYS.has(e.key.toLowerCase())) return;
      if (e.key === "Meta" || e.key === "Control") return;
      setTimeout(() => {
        if (e.defaultPrevented) return;
        post({
          kind: "key",
          key: e.key,
          code: e.code,
          metaKey: e.metaKey,
          ctrlKey: e.ctrlKey,
          altKey: e.altKey,
          shiftKey: e.shiftKey,
        });
      }, 0);
    },
    true,
  );

  // --- the page, as the workbench names it ----------------------------------------
  let lastHref = "";
  let lastTitle = "";
  const reportPage = () => {
    const href = location.href;
    const title = document.title;
    if (href === lastHref && title === lastTitle) return;
    lastHref = href;
    lastTitle = title;
    post({ kind: "page", url: href, title });
  };
  addEventListener("popstate", reportPage);
  addEventListener("hashchange", reportPage);
  // pushState is invisible from here; a slow check while visible catches it.
  setInterval(() => document.visibilityState === "visible" && reportPage(), 1000);
  const watchTitle = () => {
    const el = document.querySelector("title");
    if (el !== null) new MutationObserver(reportPage).observe(el, { childList: true, characterData: true, subtree: true });
  };

  // --- selection ------------------------------------------------------------------
  /** The nearest heading before `node` in document order, or null. */
  const headingBefore = (node) => {
    const headings = document.querySelectorAll("h1,h2,h3,h4,h5,h6");
    let found = null;
    for (const h of headings) {
      // Headings come in document order: stop at the first one after `node`.
      if (h.compareDocumentPosition(node) & Node.DOCUMENT_POSITION_PRECEDING) break;
      if (h === node || h.contains(node)) return clip(h.textContent);
      found = h;
    }
    return found === null ? null : clip(found.textContent);
  };
  const clip = (s) => {
    const t = (s ?? "").replace(/\s+/g, " ").trim();
    return t === "" ? null : t.length > HEADING_MAX ? `${t.slice(0, HEADING_MAX - 1)}…` : t;
  };
  const rectOf = (r) => ({ x: r.left, y: r.top, width: r.width, height: r.height });

  let selTimer = 0;
  let selected = "";
  let selRect = null;
  const reportSelection = () => {
    selTimer = 0;
    const s = getSelection();
    const text = s !== null && !s.isCollapsed && s.rangeCount > 0 ? s.toString() : "";
    if (text.trim() === "") {
      selRect = null;
      drawChip();
      if (selected !== "") {
        selected = "";
        post({ kind: "selection", text: "", context: null, rect: null });
      }
      return;
    }
    // Only a selection the user just made: a page that selects text by
    // script must not take the reference over from a terminal or chat.
    if (Date.now() - lastInput > INPUT_FRESH_MS) return;
    const range = s.getRangeAt(0);
    const rects = range.getClientRects();
    const last = rects.length > 0 ? rects[rects.length - 1] : range.getBoundingClientRect();
    selected = text;
    selRect = rectOf(last);
    drawChip();
    post({
      kind: "selection",
      text: text.length > TEXT_MAX ? text.slice(0, TEXT_MAX) : text,
      context: headingBefore(range.startContainer),
      rect: selRect,
    });
  };
  function scheduleSelection() {
    if (selTimer !== 0) clearTimeout(selTimer);
    selTimer = setTimeout(reportSelection, 120);
  }
  document.addEventListener("selectionchange", scheduleSelection);
  // The pill follows its selection when the page scrolls.
  addEventListener(
    "scroll",
    () => {
      if (selected === "") return;
      const s = getSelection();
      if (s === null || s.rangeCount === 0) return;
      const rects = s.getRangeAt(0).getClientRects();
      if (rects.length > 0) {
        selRect = rectOf(rects[rects.length - 1]);
        drawChip();
      }
    },
    true,
  );

  // --- the reference pill (the workbench says what it reads) ----------------------
  let chip = null; // {text, title, enabled} or null
  let host = null;
  let button = null;
  const ensurePill = () => {
    if (host !== null && host.isConnected) return;
    host = document.createElement("chimaera-pane-ref");
    host.style.cssText = "position:fixed;z-index:2147483647;left:0;top:0;display:none;";
    const root = host.attachShadow({ mode: "closed" });
    const style = document.createElement("style");
    style.textContent =
      "button{all:initial;display:flex;align-items:center;gap:5px;box-sizing:border-box;" +
      "height:24px;padding:0 9px;border-radius:12px;border:1px solid rgba(0,0,0,.14);" +
      "background:#fff;color:#1d1f23;font:500 11.5px/1 -apple-system,system-ui,sans-serif;" +
      "box-shadow:0 2px 8px rgba(0,0,0,.14);cursor:pointer;white-space:nowrap}" +
      "button:hover{background:#f3f4f6}button:disabled{opacity:.55;cursor:default}" +
      ".at{font-weight:700;opacity:.7}" +
      "@media (prefers-color-scheme:dark){button{background:#26282d;color:#e8e9ec;" +
      "border-color:rgba(255,255,255,.16)}button:hover{background:#30333a}}";
    button = document.createElement("button");
    button.type = "button";
    button.addEventListener(
      "pointerdown",
      (e) => {
        // The press must not collapse the selection it is about.
        e.preventDefault();
        e.stopPropagation();
      },
      true,
    );
    button.addEventListener("click", (e) => {
      e.stopPropagation();
      if (e.isTrusted && chip !== null && chip.enabled) post({ kind: "reference" });
    });
    root.append(style, button);
    (document.body ?? document.documentElement).append(host);
  };
  const drawChip = () => {
    if (chip === null || selRect === null) {
      if (host !== null) host.style.display = "none";
      return;
    }
    ensurePill();
    button.replaceChildren();
    const at = document.createElement("span");
    at.className = "at";
    at.textContent = "@";
    button.append(at, document.createTextNode(` ${chip.text}`));
    button.title = chip.title;
    button.disabled = !chip.enabled;
    const w = 190;
    const x = Math.min(Math.max(selRect.x + selRect.width + 4, 4), innerWidth - w);
    const y = Math.min(Math.max(selRect.y + selRect.height + 6, 4), innerHeight - 30);
    host.style.left = `${Math.max(4, x)}px`;
    host.style.top = `${y}px`;
    host.style.display = "block";
  };

  // --- what the shell (or the parent frame) asks ----------------------------------
  /** Visible text inside a viewport rect: hit-test a grid, keep the text
   *  nodes whose boxes meet the rect, in document order. Bounded: a giant
   *  page is never walked whole. */
  const region = (rect) => {
    const nodes = new Set();
    const cols = 16;
    const rows = 16;
    for (let i = 0; i <= rows; i++) {
      for (let j = 0; j <= cols; j++) {
        const x = rect.x + (rect.width * j) / cols;
        const y = rect.y + (rect.height * i) / rows;
        for (const el of document.elementsFromPoint(x, y)) {
          if (el === host) continue;
          for (const child of el.childNodes) if (child.nodeType === Node.TEXT_NODE) nodes.add(child);
          break;
        }
      }
    }
    const meets = (r) =>
      r.right > rect.x && r.left < rect.x + rect.width && r.bottom > rect.y && r.top < rect.y + rect.height;
    const range = document.createRange();
    const parts = [...nodes]
      .sort((a, b) => (a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING ? -1 : 1))
      .filter((n) => {
        range.selectNodeContents(n);
        return [...range.getClientRects()].some(meets);
      })
      .map((n) => (n.textContent ?? "").replace(/\s+/g, " ").trim())
      .filter((t) => t !== "");
    let text = parts.join(" ");
    if (text.length > REGION_MAX) text = `${text.slice(0, REGION_MAX - 1)}…`;
    return { text, scrollX: window.scrollX, scrollY: window.scrollY, title: document.title };
  };
  const api = {
    chip(next) {
      chip =
        next !== null && typeof next === "object"
          ? { text: String(next.text ?? ""), title: String(next.title ?? ""), enabled: next.enabled === true }
          : null;
      drawChip();
      return null;
    },
    region(rect) {
      const r = {
        x: Number(rect?.x) || 0,
        y: Number(rect?.y) || 0,
        width: Math.max(0, Number(rect?.width) || 0),
        height: Math.max(0, Number(rect?.height) || 0),
      };
      return region(r);
    },
  };
  // What the shell asks a view an agent drives (never the framed copy).
  if (native !== null) {
    api.rings = readRings;
    api.cursor = drawCursor;
  }
  Object.defineProperty(globalThis, "__chimaeraPaneView", { value: Object.freeze(api) });

  if (framed) {
    addEventListener("message", (e) => {
      const cmd = e.source === window.parent ? e.data?.chimaeraPaneViewCommand : undefined;
      if (cmd === undefined || cmd === null) return;
      if (cmd.op === "chip") api.chip(cmd.chip ?? null);
    });
  }

  const ready = () => {
    watchTitle();
    reportPage();
  };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", ready, { once: true });
  else ready();
})();
