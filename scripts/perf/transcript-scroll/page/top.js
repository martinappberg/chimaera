// End-of-history report: after flinging to the top, blank frames, jumps,
// where the first block landed, and whether the spacer fully collapsed.
(() => {
  window.__stop = true;
  const t = document.querySelector(".chat.visible .transcript");
  const spacer = t.querySelector(".history-spacer");
  const first = t.querySelector(".column > [data-block-index]");
  const later = !!t.querySelector(".history-later");
  const L = window.__log;
  let jumps = 0, lost = 0; const ev = [];
  for (let i = 1; i < L.length; i++) {
    const a = L[i-1], b = L[i];
    const common = Object.keys(b.pos).filter((k) => k in a.pos);
    if (!common.length) { if (Object.keys(a.pos).length && Object.keys(b.pos).length) { lost++; ev.push({i, lost: true, st: [Math.round(a.st), Math.round(b.st)]}); } continue; }
    const shift = b.pos[common[0]] - a.pos[common[0]];
    if (shift * (b.dir ?? 1) < -2) { jumps++; ev.push({ i, shift: Math.round(shift), st: [Math.round(a.st), Math.round(b.st)] }); }
  }
  const blank = L.filter((f) => Object.keys(f.pos).length === 0).length;
  window.__out = JSON.stringify({ blankFrames: blank, jumps, lost, st: Math.round(t.scrollTop), spacer: spacer ? spacer.offsetHeight : null, firstIndex: first ? Number(first.dataset.blockIndex) : null, firstTop: first ? Math.round(first.getBoundingClientRect().top - t.getBoundingClientRect().top) : null, laterButton: later, ev: ev.slice(0, 8) });
})();
