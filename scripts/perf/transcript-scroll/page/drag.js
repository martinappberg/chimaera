// Far-jump report: after a programmatic scrollbar-style jump, how many
// frames showed no rows and how long until content was under the viewport.
(() => {
  window.__stop = true;
  const L = window.__log;
  const j = L.findIndex((f) => f.st < L[0].st * 0.8);
  let blank = 0, firstFilled = null;
  for (let i = j; i < L.length; i++) {
    if (Object.keys(L[i].pos).length === 0) blank++;
    else if (firstFilled === null) firstFilled = i;
  }
  const t = document.querySelector(".chat.visible .transcript");
  const first = t.querySelector(".column > [data-block-index]");
  window.__out = JSON.stringify({ jumpFrame: j, blankFrames: blank, msToContent: firstFilled === null ? null : Math.round(L[firstFilled].t - L[j].t), st: Math.round(t.scrollTop), sh: t.scrollHeight, start: first ? Number(first.dataset.blockIndex) : null, spacer: t.querySelector(".history-spacer")?.offsetHeight });
})();
