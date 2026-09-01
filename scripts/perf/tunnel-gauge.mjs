// Tunnel-load harness: simulate one chimaera window against a REMOTE daemon.
// Opens a WS per session (the pool keeps ALL of them open — parked included),
// designates one "visible", floods N others via /exec, and measures:
//   - bytes/sec arriving per socket (parked sockets receiving = tunnel waste)
//   - keystroke echo RTT on the visible terminal, idle vs under flood
// Findings + fix plan: docs/perf-remote-plan.md
// Usage: node scripts/perf/tunnel-gauge.mjs <base> <token> <flood-count> <id> <id> ...
//   PARK=1 …  parked sockets auth with {"parked": true} (the R2 attach — no
//   snapshot) AND send {"type":"park"} before the flood (the R1 frames):
//   parked bytes should then be ~0, and the script unparks one flooded
//   socket at the end to verify the resync+snapshot catch-up repaint.
const [base, token, floodCountStr, ...ids] = process.argv.slice(2);
const floodCount = Number(floodCountStr);
const usePark = process.env.PARK === "1";
const wsBase = base.replace(/^http/, "ws");

const sockets = [];
function connect(id, parked = false) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(`${wsBase}/ws/sessions/${id}`);
    ws.binaryType = "arraybuffer";
    const s = { id, ws, bytes: 0, frames: 0, ready: false, onOutput: null };
    ws.onopen = () =>
      ws.send(
        parked
          ? JSON.stringify({ type: "auth", token, parked: true })
          : JSON.stringify({ type: "auth", token, cols: 120, rows: 30 }),
      );
    ws.onmessage = (ev) => {
      if (typeof ev.data === "string") {
        const m = JSON.parse(ev.data);
        if (m.type === "ready" && !s.ready) { s.ready = true; resolve(s); }
        return;
      }
      s.bytes += ev.data.byteLength;
      s.frames += 1;
      if (s.onOutput) s.onOutput(ev.data);
    };
    ws.onerror = (e) => reject(new Error(`${id}: ${e.message ?? "ws error"}`));
    setTimeout(() => reject(new Error(`${id}: ready timeout`)), 15000);
    sockets.push(s);
  });
}

async function echoRtt(s, probes, spacingMs) {
  // Send one char, time until the next binary frame (the shell echo).
  const rtts = [];
  for (let i = 0; i < probes; i++) {
    const t0 = performance.now();
    const done = new Promise((res) => {
      s.onOutput = () => { s.onOutput = null; res(performance.now() - t0); };
    });
    s.ws.send(new TextEncoder().encode("x"));
    const rtt = await Promise.race([
      done,
      new Promise((res) => setTimeout(() => res(null), 5000)),
    ]);
    s.onOutput = null;
    if (rtt !== null) rtts.push(rtt);
    await new Promise((r) => setTimeout(r, spacingMs));
  }
  // Clear the line so the shell prompt stays sane.
  s.ws.send(new TextEncoder().encode("\x15"));
  rtts.sort((a, b) => a - b);
  if (rtts.length === 0) return { n: 0, error: "every probe timed out" };
  const pct = (p) => Math.round(rtts[Math.min(rtts.length - 1, Math.floor(rtts.length * p))] * 10) / 10;
  return { n: rtts.length, p50: pct(0.5), p95: pct(0.95), max: pct(0.999) };
}

function exec(id, command, timeout_ms) {
  // Fire-and-forget for real: the daemon's exec route responds only after
  // the command COMPLETES, so awaiting it would run the whole flood before
  // any "under-flood" measurement (the bug the first audit run shipped —
  // its under-flood RTT was measured on an idle link).
  fetch(`${base}/api/v1/sessions/${id}/exec`, {
    method: "POST",
    headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
    body: JSON.stringify({ command, timeout_ms }),
  }).catch(() => {});
}

const t0 = performance.now();
// In PARK mode every non-visible socket attaches parked (R2): no snapshot,
// no dims. The first id stays the visible control.
await Promise.all(ids.map((id, i) => connect(id, usePark && i > 0)));
console.log(`connected+ready ${sockets.length} session sockets in ${Math.round(performance.now() - t0)}ms`);

const visible = sockets[0];
const flooders = sockets.slice(1, 1 + floodCount);
const parked = sockets.slice(1);

// Let snapshots settle, zero the counters.
await new Promise((r) => setTimeout(r, 2000));
for (const s of sockets) { s.bytes = 0; s.frames = 0; }

// ---- Phase 1: idle echo RTT on the visible terminal
const idle = await echoRtt(visible, 30, 100);
console.log(`IDLE echo RTT ms: ${JSON.stringify(idle)}`);

// ---- Phase 2: flood N parked terminals, measure again
if (usePark) {
  for (const s of parked) s.ws.send(JSON.stringify({ type: "park" }));
  console.log(`sent park on ${parked.length} sockets`);
  await new Promise((r) => setTimeout(r, 500));
}
for (const s of sockets) { s.bytes = 0; s.frames = 0; }
const floodStart = performance.now();
// ~40 MB of output per flooder, fast producer.
for (const s of flooders) {
  exec(s.id, `yes "the quick brown fox jumps over the lazy dog 0123456789 abcdefghijklmnopqrstuvwxyz" | head -n 500000`, 60000);
}
console.log(`flood requested in ${floodCount} parked terminals`);
await new Promise((r) => setTimeout(r, 3000)); // let it ramp
const under = await echoRtt(visible, 30, 100);
const floodWindowMs = performance.now() - floodStart;
console.log(`UNDER-FLOOD echo RTT ms: ${JSON.stringify(under)}`);

// ---- Byte accounting over the flood window
await new Promise((r) => setTimeout(r, 2000));
const parkedBytes = parked.reduce((a, s) => a + s.bytes, 0);
const table = sockets.map((s) => ({
  id: s.id,
  role: s === visible ? "VISIBLE" : flooders.includes(s) ? "parked+flooding" : "parked idle",
  MB: Math.round(s.bytes / 1048576 * 100) / 100,
  frames: s.frames,
}));
console.table(table);
console.log(`window ${Math.round(floodWindowMs / 1000)}s: parked sockets received ${Math.round(parkedBytes / 1048576 * 10) / 10} MB over the tunnel that nobody was looking at`);
console.log(`visible socket received ${Math.round(visible.bytes / 1024)} KB in the same window`);

if (usePark) {
  // Unpark one flooded socket: the server must catch up — either the ring
  // replays, or (after an overflow) a resync + full snapshot repaints.
  const probe = flooders[0];
  probe.bytes = 0;
  let sawResync = false;
  const origHandler = probe.ws.onmessage;
  probe.ws.onmessage = (ev) => {
    if (typeof ev.data === "string" && JSON.parse(ev.data).type === "resync") sawResync = true;
    origHandler(ev);
  };
  probe.ws.send(JSON.stringify({ type: "unpark" }));
  await new Promise((r) => setTimeout(r, 3000));
  console.log(`unpark catch-up on ${probe.id}: ${Math.round(probe.bytes / 1024)} KB, resync=${sawResync}`);
}

for (const s of sockets) s.ws.close();
process.exit(0);
