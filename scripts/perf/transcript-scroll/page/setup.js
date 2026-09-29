// Page setup for the transcript-scroll harness (see README.md): opens the
// chat session named by ?scrollSession= (in workspace ?scrollWorkspace=),
// pins it to the live bottom, then samples every frame — scroll offset,
// window start, spacer, and each visible row's position — into window.__log.
// Helpers: __snap(label) records the visible rows; __tab(name) clicks a tab.
(async () => {
  const params = new URLSearchParams(location.search);
  const sessionName = params.get("scrollSession") ?? "scroll-test";
  const workspaceName = params.get("scrollWorkspace") ?? "ws";
  window.__diag = [];
  const diag = (m) => window.__diag.push(m);
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const byText = (sel, text) =>
    Array.from(document.querySelectorAll(sel)).find((el) => el.textContent.trim() === text);
  const transcript = () => document.querySelector(".chat.visible .transcript");
  // The app restores the last open tab, so "a transcript is visible" is not
  // enough: keep going until the requested session is the active tab.
  const activeTab = () => document.querySelector(".tab.active .tab-name")?.textContent.trim();
  for (let i = 0; i < 80 && (transcript() === null || activeTab() !== sessionName); i++) {
    // Rail rows open on pointer-down or Enter, not click.
    const row = byText("span.name", sessionName)?.closest(".row");
    if (row) {
      row.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
      diag("opened session");
    } else {
      const ws = byText("span, div, button, a", workspaceName);
      if (ws) {
        ws.click();
        diag("clicked ws");
      }
    }
    await sleep(250);
  }
  const t = transcript();
  if (t === null) {
    diag("no transcript");
    return;
  }
  await sleep(1500);
  // Start from the live bottom.
  t.scrollTop = t.scrollHeight;
  await sleep(500);
  window.__log = [];
  window.__scrollCost = [];
  t.addEventListener('scroll', (e) => { window.__scrollCost.push(performance.now() - e.timeStamp); }, { passive: true });
  const rowsOf = () => t.querySelectorAll(".column > [data-block-uid]");
  // Row tops miss a shift INSIDE a tall row (a figure resolving above the
  // paragraph being read): `fine` tracks the blocks within visible rows too,
  // by element identity.
  const ids = new WeakMap();
  let nextId = 1;
  const idOf = (el) => { let id = ids.get(el); if (id === undefined) { id = nextId++; ids.set(el, id); } return id; };
  const FINE = "p, h1, h2, h3, h4, li, pre, blockquote, .md-embed, .md-table, hr";
  function sample() {
    const r = t.getBoundingClientRect();
    const pos = {};
    const fine = {};
    for (const row of rowsOf()) {
      const b = row.getBoundingClientRect();
      if (b.bottom > r.top && b.top < r.bottom) {
        pos[row.dataset.blockUid] = b.top - r.top;
        for (const el of row.querySelectorAll(FINE)) {
          if (el.parentElement?.closest(".md-embed, li, blockquote") != null) continue;
          const e = el.getBoundingClientRect();
          if (e.height > 0 && e.bottom > r.top && e.top < r.bottom) fine[idOf(el)] = e.top - r.top;
        }
      }
    }
    const first = t.querySelector(".column > [data-block-index]");
    window.__log.push({
      t: performance.now(),
      fine,
      st: t.scrollTop,
      sh: t.scrollHeight,
      ch: t.clientHeight,
      pos,
      start: first ? Number(first.dataset.blockIndex) : -1,
      dir: window.__dir ?? 1,
      sp: (() => { const e = t.querySelector('.history-spacer'); return e ? (parseFloat(e.style.height || '0') + parseFloat(e.style.marginBottom || '0')) : null; })(),
      colTop: Math.round(t.querySelector('.column').getBoundingClientRect().top - t.getBoundingClientRect().top),
    });
    if (!window.__stop) requestAnimationFrame(afterFrame);
  }
  // Sample what the frame PAINTS: the app absorbs a card that grew above the
  // reader in its ResizeObserver, after layout and before paint. A sample in
  // rAF (before that layout) or in a later task (after a network reply grew a
  // card, before the next frame absorbs it) reads states never painted — a
  // jump and its correction one frame apart. So sample inside the frame's own
  // ResizeObserver pass, after the chat's (observers run in creation order,
  // and this one is created later): a dummy resized every frame guarantees a
  // callback per frame.
  const tickEl = document.createElement("div");
  tickEl.style.cssText = "position:fixed;left:0;top:0;height:1px;width:1px;pointer-events:none;opacity:0";
  document.body.appendChild(tickEl);
  let flip = false;
  new ResizeObserver(() => sample()).observe(tickEl);
  const afterFrame = () => {
    flip = !flip;
    tickEl.style.width = flip ? "2px" : "1px";
  };
  requestAnimationFrame(afterFrame);
  window.__snaps = [];
  window.__snap = (label) => {
    const tr = document.querySelector(".chat.visible .transcript");
    const r = tr.getBoundingClientRect();
    const pos = {};
    for (const row of tr.querySelectorAll(".column > [data-block-uid]")) {
      const q = row.getBoundingClientRect();
      if (q.bottom > r.top && q.top < r.bottom) pos[row.dataset.blockUid] = Math.round(q.top - r.top);
    }
    window.__snaps.push({ label, st: Math.round(tr.scrollTop), start: Number(tr.querySelector(".column > [data-block-index]").dataset.blockIndex), pos });
  };
  window.__tab = (name) => Array.from(document.querySelectorAll(".tab .tab-name")).find((e) => e.textContent.trim() === name).click();
  window.__ready = true;
})();
