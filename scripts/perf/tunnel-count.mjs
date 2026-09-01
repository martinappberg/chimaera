// Passive byte counter: open one WS per session id (a "window"), count for N seconds.
// Findings + fix plan: docs/perf-remote-plan.md
// Usage: node scripts/perf/tunnel-count.mjs <base> <token> <seconds> <label> <id...>
//   PARK=1 auths every socket parked (the post-R1 realistic second window —
//   without it this measures the pre-park protocol and overstates traffic).
const [base, token, secondsStr, label, ...ids] = process.argv.slice(2);
const wsBase = base.replace(/^http/, "ws");
const usePark = process.env.PARK === "1";
let total = 0;
await Promise.all(ids.map((id) => new Promise((resolve, reject) => {
  const ws = new WebSocket(`${wsBase}/ws/sessions/${id}`);
  ws.binaryType = "arraybuffer";
  ws.onopen = () =>
    ws.send(
      usePark
        ? JSON.stringify({ type: "auth", token, parked: true })
        : JSON.stringify({ type: "auth", token, cols: 120, rows: 30 }),
    );
  ws.onmessage = (ev) => {
    if (typeof ev.data === "string") { resolve(); return; }
    total += ev.data.byteLength;
  };
  ws.onerror = () => reject(new Error(id));
  setTimeout(resolve, 10000);
})));
await new Promise((r) => setTimeout(r, 1500));
total = 0; // discard snapshots; count only the streaming window
await new Promise((r) => setTimeout(r, Number(secondsStr) * 1000));
console.log(`${label}: ${Math.round(total / 1048576 * 10) / 10} MB in ${secondsStr}s across ${ids.length} sockets`);
process.exit(0);
