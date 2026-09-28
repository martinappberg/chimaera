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
  function sample() {
    const r = t.getBoundingClientRect();
    const pos = {};
    for (const row of rowsOf()) {
      const b = row.getBoundingClientRect();
      if (b.bottom > r.top && b.top < r.bottom) pos[row.dataset.blockUid] = b.top - r.top;
    }
    const first = t.querySelector(".column > [data-block-index]");
    window.__log.push({
      t: performance.now(),
      st: t.scrollTop,
      sh: t.scrollHeight,
      ch: t.clientHeight,
      pos,
      start: first ? Number(first.dataset.blockIndex) : -1,
      dir: window.__dir ?? 1,
      sp: (() => { const e = t.querySelector('.history-spacer'); return e ? (parseFloat(e.style.height || '0') + parseFloat(e.style.marginBottom || '0')) : null; })(),
      colTop: Math.round(t.querySelector('.column').getBoundingClientRect().top - t.getBoundingClientRect().top),
    });
    if (!window.__stop) requestAnimationFrame(sample);
  }
  requestAnimationFrame(sample);
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
