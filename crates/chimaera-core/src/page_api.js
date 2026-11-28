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
// - web/1 (docs/design/rich-views-plan.md, Part 2): `url(path)` (a URL the
//   page may `fetch`, by Range too: a file beside an agent's page — with
//   the key a host that shows it inside its guard puts in its URL — or one
//   within a plugin's access), `reference({text, file?, line?, crop?})` (a
//   draft with a picture, after a press), `visibility`/`onVisibility`; and
//   on a plugin's page (served under `/web/{grant}/`) `data`/`onData` (its
//   node's data), `act(action, payload)` (to its plugin), `status({state,
//   text})` (its pill) and `save(name, blob)` (into its output folder).
//   `url` and `save` go straight to the daemon under the page's own ticket;
//   the rest through the host, which checks them. A host's word arrives as
//   a `theme`/`data`/`visibility` command: posted by the parent window to a
//   framed page, a `chimaera:host` event from the bridge in a native view.
(() => {
  "use strict";
  if (globalThis.chimaera !== undefined) return;
  const framed = window.parent !== window;
  const TEXT_MAX = 4000;
  // `/web/{grant}/…`: a plugin's page; `/raw/{ticket}[/…]`: an agent's.
  const where = /^\/(web|raw)\/([^/]+)(\/.*)?$/.exec(location.pathname);
  const plugin = where !== null && where[1] === "web";
  const ticket = where !== null ? where[2] : null;

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
  // A page that wears the theme — inline, any `--ch-` variable in its own
  // `<style>` or `style` attributes, or `<meta name="chimaera"
  // content="theme">` (a linked stylesheet can't be read from the page's
  // sandbox) — gets the theme's canvas too, wherever it is shown (a pane, a
  // card): its background and the browser's light or dark defaults; and its
  // host is told, so an app view reloads for a new theme only for such a
  // page. A page that never mentions the theme (an old report) keeps the
  // browser's white exactly as it was made for, and is never reloaded.
  let follows = null;
  const wearsTheme = () => {
    if (inline) return true;
    if (document.querySelector('meta[name="chimaera"][content~="theme"]') !== null) return true;
    for (const s of document.querySelectorAll("style")) if ((s.textContent ?? "").includes("--ch-")) return true;
    for (const el of document.querySelectorAll("[style]")) {
      if (el !== document.documentElement && (el.getAttribute("style") ?? "").includes("--ch-")) return true;
    }
    return false;
  };
  applyTheme = (t) => {
    baseApply(t);
    if (t === null) return;
    if (follows === null) {
      follows = wearsTheme();
      if (follows) post({ kind: "page_themed" });
    }
    if (!follows) return;
    const root = document.documentElement;
    root.style.colorScheme = t.m;
    // Without its own, a web view paints its default canvas (white, or a
    // grey that isn't the chat's): the app's background instead.
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
  // The node's data and the page's visibility, as the host says them.
  let data = null;
  let haveData = false;
  const dataListeners = new Set();
  let visible = true;
  const visibilityListeners = new Set();
  const tell = (set, value) => {
    for (const fn of set) {
      try {
        fn(value);
      } catch (err) {
        console.error(err);
      }
    }
  };
  const command = (cmd) => {
    if (cmd === null || typeof cmd !== "object") return;
    if (cmd.op === "theme") applyTheme(decodeTheme(cmd.theme));
    else if (cmd.op === "data") {
      data = cmd.data ?? null;
      haveData = true;
      tell(dataListeners, data);
    } else if (cmd.op === "visibility" && typeof cmd.visible === "boolean" && cmd.visible !== visible) {
      visible = cmd.visible;
      tell(visibilityListeners, visible);
    }
  };
  if (framed) {
    addEventListener("message", (e) => {
      const cmd = e.source === window.parent ? e.data?.chimaeraPaneViewCommand : undefined;
      command(cmd);
    });
  } else {
    document.addEventListener("chimaera:host", (e) => {
      if (typeof e.detail !== "string") return;
      try {
        command(JSON.parse(e.detail));
      } catch {
        /* not ours */
      }
    });
  }
  // An agent page's key to its folder: in its URL (`chimaera-key`), put
  // there by a host that showed it inside its guard after asking the daemon
  // with its bearer token. A page loaded any other way has none.
  const keyParam = query.get("chimaera-key");
  const dataKey = keyParam !== null && /^k-[0-9a-f]{32}$/.test(keyParam) ? keyParam : null;

  const fail = (why) => Promise.reject(new Error(why));
  const absolute = (u) => new URL(u, location.href).href;
  // A data URL. An agent's page: a file in its own folder or below, read
  // through `/webdata/{ticket}/…` (the folder route itself stays unreadable).
  // A plugin's page: the daemon checks the path against its `[access]`.
  const url = (path) => {
    if (typeof path !== "string" || path === "" || ticket === null) return fail("chimaera.url needs a path");
    if (plugin) {
      return fetch(`/web/${ticket}/~url?path=${encodeURIComponent(path)}`).then(async (r) => {
        const body = await r.json().catch(() => ({}));
        if (!r.ok || typeof body.url !== "string") throw new Error(body.error ?? `chimaera.url: ${r.status}`);
        return absolute(body.url);
      });
    }
    const folder = `/raw/${ticket}/`;
    const base = where[3] === undefined ? `${location.origin}${folder}page` : location.href;
    let u;
    try {
      u = new URL(path, base);
    } catch {
      return fail(`chimaera.url: ${path} is not a path`);
    }
    if (u.origin !== location.origin || !u.pathname.startsWith(folder)) {
      return fail(`chimaera.url: ${path} is outside this page's folder`);
    }
    // The key a host that shows this page inside its guard was given
    // (the daemon's, before this script); a page loaded any other way
    // reads nothing.
    if (typeof dataKey !== "string") return fail("chimaera.url: this page can't read files where it is shown");
    return Promise.resolve(absolute(`/webdata/${dataKey}/${u.pathname.slice(folder.length)}`));
  };

  // A picture for `reference`: a Blob, a canvas, or a rectangle of the page
  // the host takes a picture of (a native view only).
  const CROP_INLINE_MAX = 150000;
  const asBlob = (crop) =>
    crop instanceof HTMLCanvasElement
      ? new Promise((resolve) => crop.toBlob((b) => resolve(b), "image/png"))
      : Promise.resolve(crop instanceof Blob ? crop : null);
  const rectOf = (crop) => {
    if (crop instanceof Element) {
      const r = crop.getBoundingClientRect();
      return { x: r.x, y: r.y, width: r.width, height: r.height };
    }
    if (crop !== null && typeof crop === "object" && Number.isFinite(crop.width) && Number.isFinite(crop.height)) {
      return { x: Number(crop.x) || 0, y: Number(crop.y) || 0, width: crop.width, height: crop.height };
    }
    return null;
  };
  const dataUrl = (blob) =>
    new Promise((resolve) => {
      const reader = new FileReader();
      reader.onload = () => resolve(typeof reader.result === "string" ? reader.result : null);
      reader.onerror = () => resolve(null);
      reader.readAsDataURL(blob);
    });
  const reference = async (ref) => {
    if (ref === null || typeof ref !== "object" || typeof ref.text !== "string") return false;
    const msg = {
      kind: "web_reference",
      text: ref.text.slice(0, TEXT_MAX),
      file: typeof ref.file === "string" ? ref.file : null,
      line: Number.isInteger(ref.line) ? ref.line : null,
      title: document.title,
    };
    const blob = ref.crop === undefined || ref.crop === null ? null : await asBlob(ref.crop);
    if (blob !== null) {
      // A frame carries the Blob itself; a native view's bridge only text.
      if (framed) msg.crop = blob;
      else if (blob.size <= CROP_INLINE_MAX) msg.crop = await dataUrl(blob);
      else msg.crop = rectOf(ref.crop);
    } else if (ref.crop !== undefined && ref.crop !== null) msg.crop = rectOf(ref.crop);
    post(msg);
    return true;
  };

  const save = (name, blob) => {
    if (!plugin) return fail("chimaera.save is for a plugin's page");
    if (typeof name !== "string" || name === "" || !(blob instanceof Blob)) {
      return fail("chimaera.save needs a name and a Blob");
    }
    return fetch(`/web/${ticket}/~save?name=${encodeURIComponent(name)}`, { method: "POST", body: blob }).then(
      async (r) => {
        const body = await r.json().catch(() => ({}));
        if (!r.ok || typeof body.path !== "string") throw new Error(body.error ?? `chimaera.save: ${r.status}`);
        return body.path;
      },
    );
  };

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
      /** "plugin" (a plugin's own page) or "agent" (a page an agent wrote). */
      kind: plugin ? "plugin" : "agent",
      /** A URL for a data file the page may `fetch` (Range works): a Promise. */
      url,
      /** Point the agent at something here, with a picture: a draft, after a press. */
      reference,
      /** Whether the page is shown; a hidden page stops drawing. */
      get visibility() {
        return visible;
      },
      /** Call `fn(visible)` when that changes; returns a function that stops it. */
      onVisibility(fn) {
        if (typeof fn !== "function") return () => {};
        visibilityListeners.add(fn);
        return () => visibilityListeners.delete(fn);
      },
      /** A plugin's page: its node's `data` (null until the host sends it). */
      get data() {
        return data;
      },
      /** Call `fn(data)` with the node's data now (if it came) and on each change. */
      onData(fn) {
        if (typeof fn !== "function") return () => {};
        dataListeners.add(fn);
        if (haveData) {
          try {
            fn(data);
          } catch (err) {
            console.error(err);
          }
        }
        return () => dataListeners.delete(fn);
      },
      /** A plugin's page: run `action` of its plugin (or a built-in one). */
      act(action, payload) {
        if (!plugin || typeof action !== "string" || action === "") return false;
        post({ kind: "web_act", action, payload: payload ?? null });
        return true;
      },
      /** A plugin's page: its pill — `{state: "busy"|"ok"|"warn"|"bad"|"idle", text}`. */
      status(s) {
        if (!plugin || s === null || typeof s !== "object") return false;
        post({ kind: "web_status", state: String(s.state ?? "idle"), text: String(s.text ?? "") });
        return true;
      },
      /** A plugin's page: write `blob` to its output folder: a Promise of `output:<name>`. */
      save,
    }),
  });
})();
