// Send one prompt that makes fake-agent.mjs stream a long reply slowly (~15 s).
// usage: CHIMAERA_PORT=<port> CHIMAERA_TOKEN=<token> node send-slow.mjs <sessionId> [delayMs]
const [sid, delayMs] = process.argv.slice(2);
const PORT = process.env.CHIMAERA_PORT ?? "3000";
const TOKEN = process.env.CHIMAERA_TOKEN ?? "";
await new Promise((r) => setTimeout(r, Number(delayMs ?? 0)));
const ws = new WebSocket(`ws://127.0.0.1:${PORT}/ws/chat/${sid}`);
ws.onopen = () => ws.send(JSON.stringify({ type: "auth", token: TOKEN, last_seq: 1e9 }));
ws.onmessage = (ev) => {
  const msg = JSON.parse(ev.data);
  if (msg.type === "ready") {
    ws.send(JSON.stringify({ type: "send", blocks: [{ type: "text", text: "please answer slow and long" }] }));
    setTimeout(() => process.exit(0), 500);
  }
};
