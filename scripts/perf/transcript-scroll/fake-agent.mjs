#!/usr/bin/env node
// A billing-free stream-json fake Claude for scroll testing (point
// `agents.claude.path` at a wrapper that execs this): every user turn gets a
// varied reply — 0-3 tool calls, then markdown of random length with
// paragraphs, lists, fenced code, and tables. A prompt containing "slow"
// streams its reply in small chunks over ~15 s (the live-follow case).
import readline from "node:readline";

const emit = (v) => process.stdout.write(JSON.stringify(v) + "\n");
const stream = (event) => emit({ type: "stream_event", event });
let seed = 7;
const rand = () => ((seed = (seed * 1103515245 + 12345) % 2147483648) / 2147483648);
const pick = (a) => a[Math.floor(rand() * a.length)];
const WORDS = "the analysis pipeline reads counts normalizes library size then fits a negative binomial model per gene before shrinking fold changes toward zero so that noisy low count genes do not dominate the ranked list and we check dispersion trends across batches".split(" ");
const sentence = () => {
  const n = 8 + Math.floor(rand() * 18);
  const w = Array.from({ length: n }, () => pick(WORDS));
  w[0] = w[0][0].toUpperCase() + w[0].slice(1);
  return w.join(" ") + ".";
};
const para = () => Array.from({ length: 1 + Math.floor(rand() * 5) }, sentence).join(" ");
function markdown(turn) {
  const parts = [`## Turn ${turn} findings`, para()];
  const blocks = Math.floor(rand() * 5);
  for (let i = 0; i < blocks; i++) {
    const k = rand();
    if (k < 0.3) parts.push(Array.from({ length: 2 + Math.floor(rand() * 5) }, () => `- ${sentence()}`).join("\n"));
    else if (k < 0.55) parts.push("```python\n" + Array.from({ length: 3 + Math.floor(rand() * 14) }, (_, j) => `x_${j} = fit(counts[:, ${j}], design)  # ${pick(WORDS)}`).join("\n") + "\n```");
    else if (k < 0.7) parts.push("| gene | log2FC | padj |\n|---|---:|---:|\n" + Array.from({ length: 2 + Math.floor(rand() * 6) }, (_, j) => `| G${j} | ${(rand() * 4 - 2).toFixed(2)} | ${rand().toExponential(2)} |`).join("\n"));
    else parts.push(para());
  }
  return parts.join("\n\n");
}

let turn = 0;
let msgN = 0;
const rl = readline.createInterface({ input: process.stdin });
rl.on("line", async (line) => {
  let frame;
  try { frame = JSON.parse(line); } catch { return; }
  if (frame.type === "control_request" && frame.request?.subtype === "initialize") {
    emit({ type: "control_response", response: { subtype: "success", request_id: frame.request_id, response: {
      commands: [{ name: "compact", description: "Compact history" }],
      remote_control_available: false, remote_control_auto_enable: false, current_permission_mode: "default",
    } } });
    return;
  }
  if (frame.type === "control_request") {
    emit({ type: "control_response", response: { subtype: "success", request_id: frame.request_id, response: {} } });
    return;
  }
  if (frame.type !== "user") return;
  const text = JSON.stringify(frame.message ?? "");
  const slow = text.includes("slow");
  turn++;
  emit({ type: "system", subtype: "init", session_id: "fake-long-1", model: "fake-model", permissionMode: "default", slash_commands: ["compact"] });
  const tools = Math.floor(rand() * 4);
  for (let t = 0; t < tools; t++) {
    const id = `m${++msgN}`;
    stream({ type: "message_start", message: { id } });
    const tuid = `tu-${turn}-${t}`;
    emit({ type: "assistant", message: { id, content: [{ type: "tool_use", id: tuid, name: pick(["Bash", "Read", "Grep"]), input: { command: `echo ${pick(WORDS)}`, file_path: `src/${pick(WORDS)}.py`, pattern: pick(WORDS) } }] } });
    emit({ type: "user", message: { content: [{ type: "tool_result", tool_use_id: tuid, content: para(), is_error: false }] } });
  }
  const id = `m${++msgN}`;
  const body = slow ? Array.from({ length: 12 }, () => markdown(turn)).join("\n\n") : markdown(turn);
  stream({ type: "message_start", message: { id } });
  stream({ type: "content_block_start", content_block: { type: "text", text: "" } });
  if (slow) {
    const chunks = body.match(/[\s\S]{1,60}/g);
    for (const c of chunks) {
      stream({ type: "content_block_delta", delta: { type: "text_delta", text: c } });
      await new Promise((r) => setTimeout(r, 15000 / chunks.length));
    }
  } else {
    stream({ type: "content_block_delta", delta: { type: "text_delta", text: body } });
  }
  emit({ type: "assistant", message: { id, content: [{ type: "text", text: body }] } });
  stream({ type: "content_block_stop" });
  emit({ type: "result", subtype: "success", is_error: false, result: "done", session_id: "fake-long-1", total_cost_usd: 0, duration_ms: 1000, usage: { input_tokens: 5, output_tokens: 50 } });
});
