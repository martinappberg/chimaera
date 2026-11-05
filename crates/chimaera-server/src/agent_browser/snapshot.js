// The agent browser's page helpers (#273): a compact DOM+ARIA outline with
// stable refs, plus what the browser_* tools need to act on a ref. Plain DOM
// only, so the same text runs in WebKit's bridge content world and in a
// Chromium isolated world; the daemon sends it as the head of an eval body.
// Defines globalThis.__snap once per world and document (version-guarded):
//   snapshot(opts)            the outline: { text, chars, refs, nodes, truncated, ms }
//   box(ref)                  where to click a ref (scrolled into view), or null
//   rect({ref|selector})      an element's viewport rect for a screenshot clip
//   inspect({ref|selector}, styles)  outerHTML, attributes, styles, box
//   find({text, selector})    for browser_wait
//   select(ref, {value|label}) set a native <select> and fire input/change
//   watch() / delta(maxMs)    what an action changed (mutations, URL, focus)
(() => {
  const V = 8;
  if (globalThis.__snap?.v === V) return;
  const st0 = (globalThis.__snapState ??= { refOf: new WeakMap(), byRef: new Map(), next: 1 });
  const refOf = st0.refOf; // element → "e12", stable across snapshots
  const byRef = st0.byRef; // "e12" → WeakRef(element)
  const INTERACTIVE = new Set(["button", "link", "textbox", "searchbox", "checkbox", "radio", "switch", "slider", "spinbutton", "combobox", "listbox", "option", "menuitem", "menuitemcheckbox", "menuitemradio", "tab", "treeitem", "gridcell", "columnheader"]);
  const LANDMARK = new Set(["navigation", "main", "banner", "contentinfo", "complementary", "region", "form", "dialog", "alertdialog", "search", "toolbar", "menu", "menubar", "tablist", "tabpanel", "tree", "grid", "table", "list", "group", "alert", "status", "log"]);
  const clip = (s, n) => (s.length > n ? s.slice(0, n - 1) + "…" : s);
  const norm = (s) => (s || "").replace(/\s+/g, " ").trim();

  function implicitRole(el) {
    const tag = el.localName;
    switch (tag) {
      case "a": case "area": return el.hasAttribute("href") ? "link" : null;
      case "button": case "summary": return "button";
      case "input": {
        const t = (el.getAttribute("type") || "text").toLowerCase();
        if (t === "hidden") return null;
        if (["button", "submit", "reset", "image", "file"].includes(t)) return "button";
        if (t === "checkbox") return el.getAttribute("role") === "switch" ? "switch" : "checkbox";
        if (t === "radio") return "radio";
        if (t === "range") return "slider";
        if (t === "number") return "spinbutton";
        if (t === "search") return "searchbox";
        return el.list ? "combobox" : "textbox";
      }
      case "textarea": return "textbox";
      case "select": return el.multiple || el.size > 1 ? "listbox" : "combobox";
      case "option": return "option";
      case "img": return el.getAttribute("alt") === "" ? null : "img";
      case "svg": return el.getAttribute("aria-label") || el.querySelector(":scope > title") ? "img" : null;
      case "canvas": return "canvas";
      case "h1": case "h2": case "h3": case "h4": case "h5": case "h6": return "heading";
      case "nav": return "navigation";
      case "main": return "main";
      case "aside": return "complementary";
      case "form": return "form";
      case "dialog": return "dialog";
      case "ul": case "ol": case "menu": return "list";
      case "li": return "listitem";
      case "table": return "table";
      case "tr": return "row";
      case "th": return "columnheader";
      case "td": return "cell";
      case "fieldset": case "details": return "group";
      case "progress": return "progressbar";
      case "hr": return "separator";
      case "iframe": return "iframe";
      case "pre": return "code";
      case "header": return el.closest("article,section,main") ? null : "banner";
      case "footer": return el.closest("article,section,main") ? null : "contentinfo";
      case "section": return el.hasAttribute("aria-label") || el.hasAttribute("aria-labelledby") ? "region" : null;
    }
    if (el.isContentEditable && (el.parentElement === null || !el.parentElement.isContentEditable)) return "textbox";
    return null;
  }

  function role(el) {
    const r = norm(el.getAttribute("role")).split(" ")[0];
    if (r && r !== "presentation" && r !== "none" && r !== "generic") return r;
    return implicitRole(el);
  }

  function textOf(el, depth = 0) {
    if (depth > 6) return "";
    let out = "";
    for (const n of el.childNodes) {
      if (n.nodeType === 3) out += n.data;
      else if (n.nodeType === 1 && !hiddenEl(n)) out += " " + (n.getAttribute("aria-label") || (n.localName === "img" ? n.getAttribute("alt") || "" : textOf(n, depth + 1))) + " ";
      if (out.length > 400) break;
    }
    return out;
  }

  function name(el, r) {
    const doc = el.ownerDocument;
    const lb = el.getAttribute("aria-labelledby");
    if (lb) {
      const t = norm(lb.split(/\s+/).map((id) => doc.getElementById(id)?.textContent || "").join(" "));
      if (t) return t;
    }
    const al = norm(el.getAttribute("aria-label"));
    if (al) return al;
    const tag = el.localName;
    if (tag === "input" || tag === "textarea" || tag === "select") {
      const t = (el.getAttribute("type") || "").toLowerCase();
      if (["submit", "button", "reset"].includes(t)) return norm(el.value);
      if (t === "file") return "Choose file";
      const labels = el.labels ? [...el.labels].map((l) => norm(l.textContent)).filter(Boolean) : [];
      if (labels.length) return labels.join(" ");
      return norm(el.getAttribute("placeholder") || el.getAttribute("title"));
    }
    if (tag === "img" || tag === "area") return norm(el.getAttribute("alt") || el.getAttribute("title"));
    if (tag === "svg") return norm(el.querySelector(":scope > title")?.textContent);
    if (tag === "iframe") return norm(el.getAttribute("title"));
    if (r === "slider" || r === "progressbar" || r === "spinbutton" || r === "textbox" || r === "searchbox" || r === "combobox" || r === "listbox") return norm(el.getAttribute("title"));
    if (r && (INTERACTIVE.has(r) || r === "clickable" || r === "heading" || r === "cell" || r === "listitem" || r === "tab" || r === "option")) {
      const t = norm(textOf(el));
      if (t) return t;
    }
    return norm(el.getAttribute("title"));
  }

  function hiddenEl(el) {
    if (el.hidden || el.getAttribute("aria-hidden") === "true" || el.inert) return true;
    const tag = el.localName;
    if (tag === "script" || tag === "style" || tag === "template" || tag === "noscript" || tag === "head") return true;
    return false;
  }

  function invisible(el, cs) {
    if (cs.display === "none" || cs.visibility === "hidden" || cs.visibility === "collapse") return true;
    if (cs.display === "contents") return false;
    const r = el.getBoundingClientRect();
    // A collapsed box that clips its children hides them all.
    if ((r.width === 0 || r.height === 0) && cs.overflow !== "visible") return true;
    // Transparent: hidden, except a native control a styled label stands in for.
    if (cs.opacity === "0" && !/^(input|select|textarea)$/.test(el.localName)) return true;
    return r.width === 0 && r.height === 0 && el.childElementCount === 0;
  }

  function refFor(el) {
    let ref = refOf.get(el);
    if (!ref) {
      ref = "e" + st0.next++;
      refOf.set(el, ref);
    }
    byRef.set(ref, new WeakRef(el));
    return ref;
  }

  function states(el, r, focused) {
    const s = [];
    if (r === "heading") s.push("level=" + (el.getAttribute("aria-level") || el.localName.slice(1)));
    const ck = el.getAttribute("aria-checked") ?? (el.localName === "input" && (el.type === "checkbox" || el.type === "radio") ? String(el.checked) : null);
    if (ck !== null && ck !== undefined) s.push(ck === "true" ? "checked" : ck === "mixed" ? "mixed" : "unchecked");
    if (el.disabled || el.getAttribute("aria-disabled") === "true") s.push("disabled");
    const ex = el.getAttribute("aria-expanded");
    if (ex !== null) s.push(ex === "true" ? "expanded" : "collapsed");
    if (el.getAttribute("aria-selected") === "true" || (el.localName === "option" && el.selected)) s.push("selected");
    const pr = el.getAttribute("aria-pressed");
    if (pr !== null) s.push(pr === "true" ? "pressed" : "not pressed");
    if (el.required || el.getAttribute("aria-required") === "true") s.push("required");
    if (el.readOnly || el.getAttribute("aria-readonly") === "true") s.push("readonly");
    if (el.getAttribute("aria-invalid") === "true") s.push("invalid");
    if (focused) s.push("focused");
    if (r === "textbox" || r === "searchbox" || r === "combobox" || r === "spinbutton" || r === "slider") {
      let v = el.isContentEditable ? el.innerText : el.localName === "select" ? el.selectedOptions?.[0]?.textContent : el.value ?? el.getAttribute("aria-valuenow");
      if (el.localName === "input" && el.type === "password") v = v ? "••••" : "";
      if (v !== undefined && v !== null) s.push("value=" + JSON.stringify(clip(norm(String(v)), 120)));
    }
    return s;
  }

  function snapshot(opts = {}) {
    const max = opts.maxChars ?? 24000;
    const viewportOnly = opts.viewportOnly ?? false;
    const lines = [];
    const named = new WeakMap(); // emitted element → its name (its own text needn't repeat)
    let chars = 0, truncated = false, nodes = 0, refs = 0;
    const vw = innerWidth, vh = innerHeight;
    const emit = (depth, s) => {
      if (truncated) return;
      const line = "  ".repeat(Math.min(depth, 12)) + "- " + s;
      if (chars + line.length + 1 > max) { truncated = true; return; }
      lines.push(line);
      chars += line.length + 1;
    };
    function walk(node, depth, win, ctx) {
      if (truncated) return;
      for (const child of node.childNodes) {
        if (truncated) return;
        if (child.nodeType === 3) {
          const t = norm(child.data);
          if (t && !(ctx && ctx.includes(clip(t, 100)))) emit(depth, "text " + JSON.stringify(clip(t, 200)));
          continue;
        }
        if (child.nodeType !== 1) continue;
        const el = child;
        nodes++;
        if (hiddenEl(el)) continue;
        const cs = (win || window).getComputedStyle(el);
        if (invisible(el, cs)) continue;
        let r = role(el);
        const focusable = el.tabIndex >= 0 && el.localName !== "iframe";
        const pointer = cs.cursor === "pointer" && (!el.parentElement || getComputedStyle(el.parentElement).cursor !== "pointer");
        let interactive = r !== null && INTERACTIVE.has(r);
        if (!interactive && (focusable || pointer) && !r && !el.querySelector("a[href],button,input,select,textarea,[role=button],[role=link],[role=checkbox],[role=tab],[role=menuitem],[contenteditable=true],[tabindex]")) { r = "clickable"; interactive = true; }
        if (viewportOnly) {
          const b = el.getBoundingClientRect();
          if (b.bottom < 0 || b.top > vh || b.right < 0 || b.left > vw) { if (!LANDMARK.has(r)) continue; }
        }
        if (r) {
          const n = name(el, r);
          if (n) named.set(el, n);
          const st = states(el, r, el === el.ownerDocument.activeElement && el !== el.ownerDocument.body);
          // An unnamed control: a hint the page left (test id, icon name).
          if (!n && interactive) {
            const hint = el.getAttribute("data-testid") || el.getAttribute("data-tooltip") || el.getAttribute("data-title") || [...el.querySelectorAll("svg[class]")].map((sv) => String(sv.getAttribute("class")).match(/(?:lucide|fa|bi|codicon|icon)-([\w-]+)/)?.[1]).find(Boolean);
            if (hint) st.unshift("hint=" + JSON.stringify(clip(hint, 40)));
          }
          const ref = interactive || r === "iframe" ? "[" + refFor(el) + "] " : "";
          if (interactive) refs++;
          emit(depth, ref + r + (n ? " " + JSON.stringify(clip(n, 100)) : "") + (st.length ? " [" + st.join(", ") + "]" : ""));
          // A control's subtree is its name; don't repeat it (but do enter
          // containers and rich text boxes, where the content is the point).
          const descend = !interactive || r === "clickable" || r === "listbox" || r === "combobox" && el.localName !== "select" || r === "textbox" && el.isContentEditable && false || r === "gridcell" || r === "treeitem" || r === "tab" && false;
          if (r === "iframe") {
            try { const d = el.contentDocument; if (d?.body) walk(d.body, depth + 1, d.defaultView, null); } catch { emit(depth + 1, "text \"(cross-origin frame)\""); }
          } else if (descend) walk(el, depth + 1, win, n || ctx);
          if (el.shadowRoot && descend) walk(el.shadowRoot, depth + 1, win, n || ctx);
        } else {
          walk(el, depth, win, ctx);
          if (el.shadowRoot) walk(el.shadowRoot, depth, win, ctx);
        }
      }
    }
    const t0 = performance.now();
    walk(document.body || document.documentElement, 0, window, null);
    const header = `page ${JSON.stringify(document.title)} ${location.pathname} viewport=${vw}x${vh} scroll=${Math.round(scrollY)}/${document.documentElement.scrollHeight}`;
    return { text: header + "\n" + lines.join("\n") + (truncated ? "\n… (truncated at " + max + " chars)" : ""), chars, refs, nodes, truncated, ms: Math.round((performance.now() - t0) * 10) / 10 };
  }

  // Where to click a ref: scrolled into view, center in viewport CSS px.
  async function box(ref) {
    const el = byRef.get(ref)?.deref();
    if (!el || !el.isConnected) return null;
    let r = el.getBoundingClientRect();
    if (r.top < 0 || r.left < 0 || r.bottom > innerHeight || r.right > innerWidth) el.scrollIntoView({ block: "center", inline: "center", behavior: "instant" });
    // Settle: an app may re-lay out after the scroll (timers, not rAF —
    // rAF never runs in a hidden view).
    for (let i = 0; i < 10; i++) {
      await new Promise((res) => setTimeout(res, 30));
      const n = el.getBoundingClientRect();
      if (Math.abs(n.top - r.top) < 1 && Math.abs(n.left - r.left) < 1) break;
      r = n;
    }
    let x = r.left + r.width / 2, y = r.top + r.height / 2;
    // In a same-origin frame: add the frame's offset.
    let w = el.ownerDocument.defaultView;
    while (w && w !== window && w.frameElement) { const fr = w.frameElement.getBoundingClientRect(); x += fr.left; y += fr.top; w = w.parent; }
    const hit = document.elementFromPoint(x, y);
    const covered = !(hit && (hit === el || el.contains(hit) || hit.contains(el) || hit.localName === "iframe"));
    return {
      x: Math.round(x), y: Math.round(y), w: Math.round(r.width), h: Math.round(r.height),
      covered, by: covered && hit ? desc(hit) : null,
      // A native <select> opens an OS menu on click: the tools refuse it.
      select: el.localName === "select",
      desc: desc(el),
    };
  }

  // One line naming an element: role "name" (or <tag>), for results.
  function desc(el) {
    if (!el || el.nodeType !== 1) return null;
    const r = role(el);
    const n = r ? name(el, r) : "";
    const base = r || "<" + el.localName + ">";
    return clip(base + (n ? " " + JSON.stringify(clip(n, 60)) : ""), 100);
  }

  function resolve(t) {
    if (t && t.ref) {
      const el = byRef.get(t.ref)?.deref();
      return el && el.isConnected ? el : null;
    }
    if (t && t.selector) {
      try { return document.querySelector(t.selector); } catch (e) { throw new Error("Invalid selector " + JSON.stringify(t.selector) + ": " + e.message); }
    }
    return null;
  }

  function viewportRect(el) {
    const r = el.getBoundingClientRect();
    let x = r.left, y = r.top;
    let w = el.ownerDocument.defaultView;
    while (w && w !== window && w.frameElement) { const fr = w.frameElement.getBoundingClientRect(); x += fr.left; y += fr.top; w = w.parent; }
    return { x, y, width: r.width, height: r.height };
  }

  // A screenshot clip: the element scrolled into view, clipped to the viewport.
  async function rect(t) {
    const el = resolve(t);
    if (!el) return null;
    let r = viewportRect(el);
    if (r.y < 0 || r.x < 0 || r.y + r.height > innerHeight || r.x + r.width > innerWidth) {
      el.scrollIntoView({ block: "nearest", inline: "nearest", behavior: "instant" });
      await new Promise((res) => setTimeout(res, 60));
      r = viewportRect(el);
    }
    const x = Math.max(0, r.x), y = Math.max(0, r.y);
    const width = Math.min(innerWidth, r.x + r.width) - x, height = Math.min(innerHeight, r.y + r.height) - y;
    if (width < 1 || height < 1) return null;
    return { x: Math.round(x), y: Math.round(y), width: Math.round(width), height: Math.round(height) };
  }

  const STYLES = ["display", "visibility", "opacity", "position", "z-index", "color", "background-color", "font-family", "font-size", "font-weight", "line-height", "width", "height", "margin", "padding", "border", "overflow", "cursor", "pointer-events", "transform"];

  function inspect(t, styles) {
    const el = resolve(t);
    if (!el) return null;
    const cs = el.ownerDocument.defaultView.getComputedStyle(el);
    const wanted = Array.isArray(styles) && styles.length ? styles.slice(0, 50) : STYLES;
    const st = {};
    for (const p of wanted) st[p] = cs.getPropertyValue(String(p));
    const attrs = {};
    for (const a of [...el.attributes].slice(0, 60)) attrs[a.name] = clip(a.value, 300);
    const r = viewportRect(el);
    const cx = r.x + r.width / 2, cy = r.y + r.height / 2;
    const hit = document.elementFromPoint(cx, cy);
    const visible = !invisible(el, cs) && r.width > 0 && r.height > 0;
    const inView = r.y + r.height > 0 && r.y < innerHeight && r.x + r.width > 0 && r.x < innerWidth;
    const covered = visible && inView && !(hit && (hit === el || el.contains(hit) || hit.contains(el)));
    const html = el.outerHTML;
    return {
      ref: refFor(el), element: desc(el), tag: el.localName,
      outerHTML: html.length > 4096 ? html.slice(0, 4095) + "…" : html,
      outerHTMLLength: html.length,
      attributes: attrs, styles: st,
      box: { x: Math.round(r.x), y: Math.round(r.y), width: Math.round(r.width), height: Math.round(r.height) },
      visible, inViewport: inView, covered, coveredBy: covered && hit ? desc(hit) : null,
      text: clip(norm(el.innerText || el.textContent || ""), 500),
    };
  }

  function find(q) {
    let el = null;
    if (q.selector) {
      el = resolve({ selector: q.selector });
      if (!el) return { found: false };
      const cs = el.ownerDocument.defaultView.getComputedStyle(el);
      if (invisible(el, cs)) return { found: false, hidden: true };
    }
    if (q.text) {
      const scope = el || document.body || document.documentElement;
      const hay = scope.innerText || scope.textContent || "";
      if (!hay.includes(q.text)) return { found: false };
    }
    return { found: true, element: el ? desc(el) : null };
  }

  function select(ref, want) {
    const el = byRef.get(ref)?.deref();
    if (!el || !el.isConnected) return { error: "gone" };
    if (el.localName !== "select") return { error: "not_select", element: desc(el) };
    if (el.disabled) return { error: "disabled", element: desc(el) };
    const opts = [...el.options];
    const match = opts.find((o) => (want.value != null && o.value === String(want.value)) || (want.label != null && norm(o.textContent) === norm(String(want.label))));
    if (!match) return { error: "no_option", options: opts.slice(0, 50).map((o) => ({ value: o.value, label: clip(norm(o.textContent), 80) })) };
    el.focus();
    el.value = match.value;
    match.selected = true;
    el.dispatchEvent(new Event("input", { bubbles: true }));
    el.dispatchEvent(new Event("change", { bubbles: true }));
    return { ok: true, value: match.value, label: clip(norm(match.textContent), 80), element: desc(el) };
  }

  function focusDesc() {
    const a = document.activeElement;
    if (!a || a === document.body || a === document.documentElement) return null;
    const r = role(a);
    let v = null;
    if (r === "textbox" || r === "searchbox" || r === "combobox" || r === "spinbutton") {
      v = a.isContentEditable ? a.innerText : a.value;
      if (a.localName === "input" && a.type === "password") v = v ? "••••" : "";
      if (v != null) v = clip(norm(String(v)), 200);
    }
    return { element: desc(a), value: v };
  }

  function openDialog() {
    for (const d of document.querySelectorAll("dialog[open], [role=dialog], [role=alertdialog]")) {
      const cs = getComputedStyle(d);
      if (!invisible(d, cs) && !hiddenEl(d)) return desc(d);
    }
    return null;
  }

  // Start counting what the next action changes (state lives per document,
  // so a new document reads as one).
  function watch() {
    const w = (globalThis.__snapWatch ??= {});
    w.obs?.disconnect();
    w.count = 0;
    w.last = performance.now();
    w.href = location.href;
    w.title = document.title;
    w.obs = new MutationObserver((ms) => { w.count += ms.length; w.last = performance.now(); });
    w.obs.observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
    return true;
  }

  // What changed since watch(): waits until the DOM is quiet for 200 ms
  // (timers, never rAF: a hidden view runs no rAF), at most maxMs.
  async function delta(maxMs = 1500) {
    const w = globalThis.__snapWatch;
    if (!w || !w.obs) {
      if (document.readyState !== "complete") {
        await new Promise((res) => { addEventListener("load", res, { once: true }); setTimeout(res, 5000); });
      }
      return { newDocument: true, url: location.href, title: document.title, focus: focusDesc(), dialog: openDialog() };
    }
    const t0 = performance.now();
    while (performance.now() - t0 < maxMs) {
      await new Promise((res) => setTimeout(res, 50));
      if (performance.now() - w.last >= 200) break;
    }
    w.obs.disconnect();
    w.obs = null;
    return { newDocument: false, mutations: w.count, urlChanged: location.href !== w.href, titleChanged: document.title !== w.title, url: location.href, title: document.title, focus: focusDesc(), dialog: openDialog() };
  }

  globalThis.__snap = { v: V, snapshot, box, rect, inspect, find, select, watch, delta };
})();
