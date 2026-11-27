// The page API: `window.chimaera` for an HTML page Chimaera shows (a preview
// pane, an embed card in an agent's answer), so a page an agent writes can
// wear the app's theme and answer back.
//
// It runs in the PAGE's own world and is the page's convenience, never a
// boundary: a page can ignore it, replace it or post the same messages
// itself. Every check lives on the trusted side — the parent window for a
// framed page (`web-ui` `shared/paneFrame.ts`), the pane bridge's isolated
// world for a native view (`pane_bridge.js`) — and a page's word is only a
// request there.
//
// The daemon appends it after `</html>` to the ticket's own page when asked
// (`?bridge=1` for a framed page, with the bridge; `?api=1` for a native
// view, whose bridge the app injects itself), the way it appends the bridge.
// So it runs after the page's own inline scripts and before
// `DOMContentLoaded`: a page reads `window.chimaera` from that event or a
// click, never at the top of an inline script (the theme variables apply
// either way, being CSS).
//
// - Theme: the host puts the app's current theme in the page's URL
//   (`chimaera-theme`: base64url JSON `{m: "light"|"dark", v: {name: css}}`)
//   and, for a framed page, posts a new one when it changes. Each value
//   becomes a CSS variable `--ch-<name>` on the root element, and the mode
//   `data-chimaera-theme` there. Variables only — no element style — so a
//   page that doesn't use them renders exactly as it did.
// - `send(text)` / `draft(text)`: a message for the agent. The host honours
//   either only right after the person pressed or typed in the page; it
//   adds where the text came from, and only a page embedded in a chat's own
//   answer may send to that chat — anywhere else both become a draft in the
//   agent's input, which the person sends or not.
(() => {
  "use strict";
  if (globalThis.chimaera !== undefined) return;
  const framed = window.parent !== window;
  const TEXT_MAX = 4000;

  const post = (msg) => {
    try {
      if (framed) window.parent.postMessage({ chimaeraPaneView: msg }, "*");
      // A native view's bridge lives in a world of its own: a DOM event,
      // with a string detail (an object would not cross worlds).
      else document.dispatchEvent(new CustomEvent("chimaera:page", { detail: JSON.stringify(msg) }));
    } catch {
      /* the host went away */
    }
  };

  const decodeTheme = (raw) => {
    if (typeof raw !== "string" || raw === "" || raw.length > 8192) return null;
    try {
      const b64 = raw.replace(/-/g, "+").replace(/_/g, "/");
      const json = new TextDecoder().decode(Uint8Array.from(atob(b64), (c) => c.charCodeAt(0)));
      const t = JSON.parse(json);
      if (t === null || typeof t !== "object" || (t.m !== "light" && t.m !== "dark")) return null;
      if (t.v === null || typeof t.v !== "object") return null;
      return t;
    } catch {
      return null;
    }
  };

  let theme = { mode: "light", vars: {} };
  const listeners = new Set();
  const NAME = /^[a-z][a-z0-9-]{0,40}$/;
  let applyTheme = (t) => {
    if (t === null) return;
    const root = document.documentElement;
    const vars = {};
    for (const [name, value] of Object.entries(t.v)) {
      if (!NAME.test(name) || typeof value !== "string" || value.length > 200) continue;
      vars[name] = value;
    }
    for (const name of Object.keys(theme.vars)) if (!(name in vars)) root.style.removeProperty(`--ch-${name}`);
    for (const [name, value] of Object.entries(vars)) root.style.setProperty(`--ch-${name}`, value);
    root.dataset.chimaeraTheme = t.m;
    theme = Object.freeze({ mode: t.m, vars: Object.freeze(vars) });
    for (const fn of listeners) {
      try {
        fn(theme);
      } catch (err) {
        console.error(err);
      }
    }
  };
  const query = new URLSearchParams(location.search);
  // An inline page (its embed says `#inline`) sits on the app's own
  // background, so the browser's own defaults — text, form controls,
  // scrollbars — follow the theme's light or dark there; any other page
  // keeps its browser defaults.
  // `chimaera-inline=<max px>`: the most height the card gives the page.
  const inlineMax = Number(query.get("chimaera-inline"));
  const inline = Number.isFinite(inlineMax) && inlineMax > 0;
  const baseApply = applyTheme;
  // Whether the page paints a background of its own (its root's or its
  // body's, which the canvas takes); read once, before we set one.
  let ownBackground = null;
  const clear = (c) => c === "" || c === "transparent" || /^rgba\(.*,\s*0\)$/.test(c);
  applyTheme = (t) => {
    baseApply(t);
    if (!inline || t === null) return;
    const root = document.documentElement;
    root.style.colorScheme = t.m;
    // Without its own, a web view paints its default canvas (a grey that
    // isn't the chat's): the app's background instead.
    if (ownBackground === null) {
      const body = document.body;
      ownBackground =
        !clear(getComputedStyle(root).backgroundColor) ||
        (body !== null && !clear(getComputedStyle(body).backgroundColor));
    }
    if (!ownBackground && typeof t.v.bg === "string") root.style.backgroundColor = t.v.bg;
  };
  applyTheme(decodeTheme(query.get("chimaera-theme")));

  // The page's own height, for an inline card that takes it: reported when
  // it changes, at most once a frame. The body's box with its margins (the
  // root's scroll height alone never drops below the frame's own, so a page
  // could grow and never shrink), or the root's scroll height when content
  // overflows the frame. A page laid out at `height: 100%` has no height of
  // its own: it gives its plot an explicit one.
  //
  // While the page fits the card's most (`inlineMax`), the root scrolls
  // never: no scrollbar to flash while the card catches up, and none to
  // narrow the page, rewrap its text and change its height — with an
  // always-shown scrollbar that loop resized the card without end.
  if (inline) {
    let sent = -1;
    let queued = false;
    const root = document.documentElement;
    const measure = () => {
      const body = document.body;
      let h = 0;
      if (body !== null) {
        const cs = getComputedStyle(body);
        h = body.getBoundingClientRect().height + parseFloat(cs.marginTop) + parseFloat(cs.marginBottom);
      }
      if (root.scrollHeight > root.clientHeight + 1) h = Math.max(h, root.scrollHeight);
      return h;
    };
    const report = () => {
      queued = false;
      const h = Math.ceil(measure());
      root.style.overflowY = h <= inlineMax ? "hidden" : "";
      // Grow at once; shrink only past a couple of pixels (sub-pixel
      // rounding must not set the card bobbing).
      if (h === sent || (h < sent && sent - h <= 2)) return;
      sent = h;
      post({ kind: "page_size", height: h });
    };
    const schedule = () => {
      if (queued) return;
      queued = true;
      requestAnimationFrame(report);
    };
    const observer = new ResizeObserver(schedule);
    observer.observe(document.documentElement);
    const watchBody = () => document.body !== null && observer.observe(document.body);
    if (document.body !== null) watchBody();
    else addEventListener("DOMContentLoaded", watchBody, { once: true });
    addEventListener("load", schedule);
    schedule();
  }
  if (framed) {
    addEventListener("message", (e) => {
      const cmd = e.source === window.parent ? e.data?.chimaeraPaneViewCommand : undefined;
      if (cmd?.op === "theme") applyTheme(decodeTheme(cmd.theme));
    });
  }

  const message = (kind) => (text) => {
    if (typeof text !== "string" || text.trim() === "") return false;
    post({ kind, text: text.length > TEXT_MAX ? text.slice(0, TEXT_MAX) : text, title: document.title });
    return true;
  };

  Object.defineProperty(globalThis, "chimaera", {
    value: Object.freeze({
      version: 1,
      /** `{mode, vars}`: the app's theme now ("light" | "dark"; vars by name, without `--ch-`). */
      get theme() {
        return theme;
      },
      /** Call `fn(theme)` when the theme changes; returns a function that stops it. */
      onTheme(fn) {
        if (typeof fn !== "function") return () => {};
        listeners.add(fn);
        return () => listeners.delete(fn);
      },
      /** Ask to send `text` to the agent: from a click or a key press only. */
      send: message("page_send"),
      /** Ask to put `text` in the agent's input for the person to send. */
      draft: message("page_draft"),
    }),
  });
})();
