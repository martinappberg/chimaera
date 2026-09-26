// Jump report: a frame where a row seen in the previous frame moved against
// the scroll direction (window.__dir per phase: 1 up, -1 down), or moved
// more than a fling step, or where no row survived at all ("lost" — the
// reader's position gone). Also edge hits, frame intervals, and the scroll
// handler's cost (a passive listener registered after the app's).
(() => {
  window.__stop = true;
  const L = window.__log;
  const jumps = [];
  let lost = 0, edge = 0;
  const durations = [];
  for (let i = 1; i < L.length; i++) {
    const a = L[i - 1], b = L[i];
    durations.push(b.t - a.t);
    if (b.st <= 0.5 && a.st > 0.5 && b.start > 0) edge++;
    if (b.dir < 0 && b.sh - b.st - b.ch <= 0.5 && a.sh - a.st - a.ch > 0.5) edge++;
    const common = Object.keys(b.pos).filter((k) => k in a.pos);
    if (common.length === 0) {
      if (Object.keys(a.pos).length && Object.keys(b.pos).length) { lost++; jumps.push({ i, lost: true, st: [Math.round(a.st), Math.round(b.st)], start: [a.start, b.start] }); }
      continue;
    }
    const shift = b.pos[common[0]] - a.pos[common[0]];
    const dir = b.dir ?? 1;
    if (shift * dir < -2 || Math.abs(shift) > 250) jumps.push({ i, shift: Math.round(shift), st: [Math.round(a.st), Math.round(b.st)], start: [a.start, b.start] });
  }
  durations.sort((x, y) => x - y);
  const p = (q) => Math.round(durations[Math.floor(q * (durations.length - 1))] * 10) / 10;
  const sc = (window.__scrollCost || []).slice().sort((x, y) => x - y); const q = (v) => sc.length ? Math.round(sc[Math.floor(v * (sc.length - 1))] * 100) / 100 : null;
  window.__out = JSON.stringify({ scrollCost: { n: sc.length, p50: q(0.5), p95: q(0.95), max: q(1) }, frames: L.length, jumps: jumps.length, lost, edgeHits: edge, p50: p(0.5), p95: p(0.95), p99: p(0.99), worst: Math.round(durations[durations.length - 1]), end: { st: Math.round(L[L.length - 1].st), start: L[L.length - 1].start }, events: jumps.slice(0, 8), diag: window.__diag });
})();
