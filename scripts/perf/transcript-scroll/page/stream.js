// Streaming report: frame intervals while following the live tail (phase
// __dir=9), yanks back to the bottom, and drift of a reader's rows while
// they sit still (__dir=0) as the reply grows below them.
(() => {
  window.__stop = true;
  const L = window.__log;
  const t = document.querySelector(".chat.visible .transcript");
  let drift = 0, maxDrift = 0, yanks = 0;
  const followDur = [], readDur = [];
  for (let i = 1; i < L.length; i++) {
    const a = L[i - 1], b = L[i];
    (b.dir === 9 ? followDur : readDur).push(b.t - a.t);
    const distA = a.sh - a.st - a.ch, distB = b.sh - b.st - b.ch;
    if (b.dir !== 9 && distA > 40 && distB <= 2) yanks++;
    if (b.dir !== 0) continue;
    const common = Object.keys(b.pos).filter((k) => k in a.pos);
    if (!common.length) continue;
    const shift = b.pos[common[0]] - a.pos[common[0]];
    if (Math.abs(shift) > 1) { drift++; maxDrift = Math.max(maxDrift, Math.abs(shift)); }
  }
  const pct = (arr, q) => { const s = arr.slice().sort((x, y) => x - y); return s.length ? Math.round(s[Math.floor(q * (s.length - 1))]) : null; };
  const last = L[L.length - 1];
  window.__out = JSON.stringify({ followFrames: followDur.length, followP95: pct(followDur, 0.95), followP99: pct(followDur, 0.99), followWorst: pct(followDur, 1), readFrames: readDur.length, readingDriftFrames: drift, maxDrift, yanks, finalDistFromBottom: Math.round(last.sh - last.st - last.ch) });
})();
