// Drive a chimaera chat session to an arbitrary transcript size, fast and
// billing-free, by pumping turns through a `fake-claude`-backed agent. Used to
// reproduce/regress the long-session tab-switch cost (docs/perf-plan.md).
//
// Setup (isolated daemon — see the develop skill):
//   cargo build -p chimaera-agent --bin fake-claude
//   printf '#!/bin/bash\nexec %s normal\n' "$PWD/target/debug/fake-claude" > /tmp/fake-claude-wrapper.sh
//   chmod +x /tmp/fake-claude-wrapper.sh
//   curl -X PUT -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
//     -d '{"agents.claude.path":"/tmp/fake-claude-wrapper.sh"}' http://127.0.0.1:$PORT/api/v1/settings
//   # start a claude agent in the UI, note its session id from /api/v1/sessions
//
// Run (Node 22+, native WebSocket):
//   CHIMAERA_PORT=<port> CHIMAERA_TOKEN=<token> node scripts/perf/pump-turns.mjs <sessionId> <turns>
//
// fake-claude's canned turn parks on a Bash permission ask, so the pump
// answers every permission_request with its first option.
//
// Known limitation: a LIVE ask parked from before this pump connected sits at
// seq <= the replay head, so the replay guard below never answers it and the
// session stays parked — start the pump against a fresh session (or answer
// the pending ask in the UI first) rather than resuming a parked one.
const [sid, turnsArg] = process.argv.slice(2);
const TURNS = Number(turnsArg ?? 400);
const PORT = process.env.CHIMAERA_PORT ?? "9700";
const TOKEN = process.env.CHIMAERA_TOKEN ?? "";
if (!sid || !TOKEN || !Number.isFinite(TURNS) || TURNS < 1) {
  console.error("usage: CHIMAERA_PORT=<port> CHIMAERA_TOKEN=<token> node pump-turns.mjs <sessionId> [turns]");
  process.exit(2);
}
const ws = new WebSocket(`ws://127.0.0.1:${PORT}/ws/chat/${sid}`);
let sent = 0;
let lastSeq = 0;
let replayHead = 0;
let idleTimer = null;

function sendTurn() {
  sent++;
  ws.send(JSON.stringify({ type: "send", blocks: [{ type: "text", text: `perf filler turn ${sent}: lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor.` }] }));
}

function maybeNext() {
  // fake-claude answers instantly; a short settle after the last event burst
  // means the turn (and any queued ones) finished.
  if (idleTimer) clearTimeout(idleTimer);
  idleTimer = setTimeout(() => {
    if (sent >= TURNS) {
      console.log(`done: ${sent} turns, lastSeq=${lastSeq}`);
      ws.close();
      process.exit(0);
    }
    if (sent % 50 === 0) console.log(`progress: ${sent} turns, lastSeq=${lastSeq}`);
    sendTurn();
  }, 120);
}

ws.onopen = () => ws.send(JSON.stringify({ type: "auth", token: TOKEN, last_seq: 0 }));
ws.onmessage = (ev) => {
  if (typeof ev.data !== "string") return;
  let msg;
  try { msg = JSON.parse(ev.data); } catch { return; }
  if (msg.type === "ready") {
    // Requests at or below head are history replay — don't re-answer them.
    replayHead = typeof msg.head === "number" ? msg.head : 0;
    sendTurn();
    return;
  }
  const entries =
    msg.type === "batch" && Array.isArray(msg.events) ? msg.events :
    msg.type === "ev" ? [{ seq: msg.seq, ts: msg.ts, ev: msg.ev }] : null;
  if (entries) {
    for (const e of entries) {
      if (typeof e.seq === "number" && e.seq > lastSeq) lastSeq = e.seq;
      const evt = e.ev;
      // No request-id dedupe here: fake-claude reuses request_id ("req-1")
      // for EVERY turn's ask, so a dedupe set answers turn 1 and stalls
      // forever; the seq > replayHead guard alone prevents replay re-answers.
      if (evt && evt.type === "permission_request" && evt.request_id && e.seq > replayHead) {
        const opt = Array.isArray(evt.options) && evt.options[0]?.id ? evt.options[0].id : "allow_once";
        ws.send(JSON.stringify({ type: "permission", request_id: evt.request_id, option_id: opt }));
      }
    }
    maybeNext();
  }
};
ws.onclose = () => {
  console.log(`closed at ${sent} turns, lastSeq=${lastSeq}`);
  process.exit(sent >= TURNS ? 0 : 1);
};
setTimeout(() => { console.error(`timeout at ${sent} turns, lastSeq=${lastSeq}`); process.exit(1); }, 600000);
