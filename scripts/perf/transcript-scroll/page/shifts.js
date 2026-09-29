// Shift report: frames where the text under the reader's eye (the element
// in view nearest the viewport's top, among rows AND the blocks inside them —
// setup.js \`fine\`) jumped: it moved while the offset did not, moved against
// the scroll direction (window.__dir: 1 up, -1 down), or moved well past the
// offset change (a figure growing above it). A compensating scroll write
// changes the offset without moving anything, so the offset alone is not the
// reader's scroll. Also blank frames, and scrollbar thumb moves that the
// scroll does not explain.
(() => {
  window.__stop = true;
  const L = window.__log;
  let jumps = 0, maxJump = 0, blank = 0, thumbMoves = 0, maxThumb = 0;
  const events = [];
  const thumb = (f) => (f.st / f.sh) * f.ch;
  for (let i = 1; i < L.length; i++) {
    const a = L[i - 1], b = L[i];
    if (Object.keys(b.pos).length === 0) blank++;
    const scrolled = b.st - a.st;
    let focus = null;
    for (const [id, top] of Object.entries(b.fine)) {
      if (!(id in a.fine) || top < 0) continue;
      if (focus === null || top < focus.top) focus = { top, moved: top - a.fine[id] };
    }
    if (focus !== null) {
      const moved = focus.moved;
      const dir = b.dir === 1 || b.dir === -1 ? b.dir : 0;
      const jumped =
        (Math.abs(scrolled) < 0.5 && Math.abs(moved) > 2) ||
        (dir !== 0 && moved * dir < -2) ||
        Math.abs(moved) > Math.abs(scrolled) + 40;
      if (jumped) {
        jumps++;
        maxJump = Math.max(maxJump, Math.abs(moved));
        if (events.length < 8) events.push({ i, moved: Math.round(moved), st: [Math.round(a.st), Math.round(b.st)], start: [a.start, b.start] });
      }
    }
    const t = thumb(b) - (b.st / a.sh) * a.ch;
    if (Math.abs(t) > 3) { thumbMoves++; maxThumb = Math.max(maxThumb, Math.abs(t)); }
  }
  const last = L[L.length - 1];
  window.__out = JSON.stringify({ frames: L.length, jumps, maxJump, blank, thumbMoves, maxThumb: Math.round(maxThumb), endGap: Math.round(last.sh - last.st - last.ch), events });
})();
