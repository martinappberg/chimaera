// Chromium twin of wkscroll: headless Chrome over raw CDP, the same setup /
// analyze page scripts, and compositor-driven scroll gestures
// (Input.synthesizeScrollGesture) instead of WebKit wheel events.
// usage: node cdp-scroll.mjs <url> <setup.js> <analyze.js> <scenario>
//   scenario items: up:<px>:<speed> | down:<px>:<speed> | wait:<ms> | js:<expr>
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const [url, setupPath, analyzePath, scenario] = process.argv.slice(2);
const setup = readFileSync(setupPath, "utf8");
const analyze = readFileSync(analyzePath, "utf8");
const port = 9300 + Math.floor(Math.random() * 400);
const profile = mkdtempSync(join(process.env.TMPDIR ?? tmpdir(), "cdp-"));
const chrome = spawn("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", [
  "--headless=new", `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`,
  "--window-size=1000,760", "--no-first-run", "--no-default-browser-check", "about:blank",
], { stdio: "ignore" });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let targets = null;
for (let i = 0; i < 50 && !targets; i++) {
  await sleep(200);
  try { targets = await (await fetch(`http://127.0.0.1:${port}/json`)).json(); } catch {}
}
const page = targets.find((t) => t.type === "page");
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r) => (ws.onopen = r));
let id = 0;
const pending = new Map();
ws.onmessage = (ev) => {
  const msg = JSON.parse(ev.data);
  if (msg.id && pending.has(msg.id)) { pending.get(msg.id)(msg); pending.delete(msg.id); }
};
const send = (method, params = {}) => new Promise((r) => { const n = ++id; pending.set(n, r); ws.send(JSON.stringify({ id: n, method, params })); });
const evaluate = async (expr) => (await send("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true })).result?.result?.value;

await send("Page.enable");
await send("Page.navigate", { url });
await sleep(2500);
await evaluate(setup);
for (let i = 0; i < 80 && (await evaluate("window.__ready === true")) !== true; i++) await sleep(250);
for (const item of scenario.split(",")) {
  const [kind, a, b] = item.split(":");
  if (kind === "up" || kind === "down") {
    await send("Input.synthesizeScrollGesture", {
      x: 520, y: 350, yDistance: (kind === "up" ? 1 : -1) * Number(a), speed: Number(b ?? 3000),
      gestureSourceType: "mouse", preventFling: false,
    });
  } else if (kind === "wait") {
    await sleep(Number(a));
  } else if (kind === "js") {
    await evaluate(item.slice(3));
  }
}
await sleep(500);
await evaluate(analyze);
let out = null;
for (let i = 0; i < 40 && out === null; i++) { out = await evaluate("window.__out ?? null"); if (out === null) await sleep(250); }
console.log(out);
await new Promise((r) => {
  chrome.once("exit", r);
  chrome.kill();
});
rmSync(profile, { recursive: true, force: true });
process.exit(0);
