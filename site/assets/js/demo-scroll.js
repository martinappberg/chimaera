/* Brief resistance at pane edges, then ordinary page scrolling. */
(function () {
  "use strict";
  var root = document.getElementById("workspace-demo");
  if (!root) return;
  var edge = null;

  root.addEventListener("pointerleave", function () { edge = null; });
  root.addEventListener("wheel", function (event) {
    // Keep zoom, horizontal gestures, and the expanded workspace native.
    if (event.ctrlKey || event.metaKey || event.shiftKey || !event.cancelable ||
        Math.abs(event.deltaX) >= Math.abs(event.deltaY) || root.classList.contains("wb-expanded")) return;
    var pane = event.target.closest(".wb-files,.wb-preview,.wb-chat-messages,.wb-browser-content");
    if (!pane || pane.scrollHeight <= pane.clientHeight + 1) { edge = null; return; }
    var delta = event.deltaY * (event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? pane.clientHeight : 1);
    var direction = Math.sign(delta);
    var remaining = direction > 0 ? pane.scrollHeight - pane.clientHeight - pane.scrollTop : pane.scrollTop;
    if (remaining > 1) {
      edge = null;
      if (remaining >= Math.abs(delta)) return;
      // Finish the pane before handing the next part of a gesture to the page.
      pane.scrollTop = direction > 0 ? pane.scrollHeight - pane.clientHeight : 0;
      event.preventDefault();
      edge = { pane: pane, direction: direction, since: event.timeStamp, distance: 0, released: false };
      return;
    }
    if (!edge || edge.pane !== pane || edge.direction !== direction) {
      edge = { pane: pane, direction: direction, since: event.timeStamp, distance: 0, released: false };
      event.preventDefault();
      return;
    }
    edge.distance += Math.abs(delta);
    // A firmer gesture or a short continued scroll releases the page. Once
    // released, the rest of the gesture stays native, including momentum.
    if (edge.released || edge.distance >= 180 || event.timeStamp - edge.since >= 260) {
      edge.released = true;
      return;
    }
    event.preventDefault();
  }, { passive: false });
})();
