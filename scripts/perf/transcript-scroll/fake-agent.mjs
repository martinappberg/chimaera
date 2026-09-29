#!/usr/bin/env node
// A billing-free stream-json fake Claude for scroll testing (point
// `agents.claude.path` at a wrapper that execs this): every user turn gets a
// varied reply — 0-3 tool calls, then markdown of random length with
// paragraphs, lists, fenced code, and tables. A prompt containing "slow"
// streams its reply in small chunks over ~15 s (the live-follow case).
//
// `--embeds` makes it write the transcripts people actually scroll: it
// generates a fixture in its working directory (figures of mixed sizes and
// aspects, multi-page PDFs, markdown documents, tables) and every reply
// embeds some of them inline (`![…](fixture/figs/fig_3.png)`, a PDF page,
// a document chip), cites files as links and code spans (relative and
// absolute), and writes files the turn's gallery then shows — through a
// Write tool (a location) and through a shell command naming its output
// (a mention, confirmed by the file's time). The default mode's output is
// unchanged, so old measurements stay comparable.
import fs from "node:fs";
import path from "node:path";
import readline from "node:readline";
import zlib from "node:zlib";

const EMBEDS = process.argv.includes("--embeds");
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

// --- the embeds fixture ---------------------------------------------------------

const FIXTURE = "fixture";
const FIG_SIZES = [[1200, 800], [800, 1200], [2400, 1500], [640, 640], [1800, 600], [1000, 1400]];
const FIGS = 24;
const PDFS = 6;
const DOCS = 10;
const TABLES = 4;

let crcTable = null;
function crc32(buf) {
  if (crcTable === null) {
    crcTable = new Int32Array(256);
    for (let n = 0; n < 256; n++) {
      let c = n;
      for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
      crcTable[n] = c;
    }
  }
  let c = -1;
  for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ -1) >>> 0;
}

/** A plot-like PNG: white ground, a few colored clusters of dots. */
function png(w, h, variant) {
  const rows = Buffer.alloc((w * 3 + 1) * h, 255);
  for (let y = 0; y < h; y++) rows[y * (w * 3 + 1)] = 0;
  const colors = [[31, 119, 180], [255, 127, 14], [44, 160, 44], [214, 39, 40], [148, 103, 189]];
  let s = variant * 7919 + 1;
  const r = () => ((s = (s * 1103515245 + 12345) % 2147483648) / 2147483648);
  for (let c = 0; c < 5; c++) {
    const cx = r() * w, cy = r() * h, spread = Math.min(w, h) * (0.08 + r() * 0.12);
    for (let i = 0; i < 1500; i++) {
      const x = Math.round(cx + (r() - 0.5) * 2 * spread), y = Math.round(cy + (r() - 0.5) * 2 * spread);
      for (let dy = 0; dy < 3; dy++) for (let dx = 0; dx < 3; dx++) {
        const px = x + dx, py = y + dy;
        if (px < 0 || py < 0 || px >= w || py >= h) continue;
        const o = py * (w * 3 + 1) + 1 + px * 3;
        rows[o] = colors[c][0]; rows[o + 1] = colors[c][1]; rows[o + 2] = colors[c][2];
      }
    }
  }
  const chunk = (type, data) => {
    const len = Buffer.alloc(4); len.writeUInt32BE(data.length);
    const td = Buffer.concat([Buffer.from(type, "ascii"), data]);
    const crc = Buffer.alloc(4); crc.writeUInt32BE(crc32(td));
    return Buffer.concat([len, td, crc]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4); ihdr[8] = 8; ihdr[9] = 2;
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk("IHDR", ihdr), chunk("IDAT", zlib.deflateSync(rows)), chunk("IEND", Buffer.alloc(0))]);
}

/** A PDF of `pages` scatter-plot pages (thousands of marks: real drawing work). */
function pdf(pages, landscape, variant) {
  const [w, h] = landscape ? [792, 612] : [612, 792];
  let s = variant * 104729 + 3;
  const r = () => ((s = (s * 1103515245 + 12345) % 2147483648) / 2147483648);
  const objs = ["<< /Type /Catalog /Pages 2 0 R >>", null];
  const kids = [];
  for (let p = 0; p < pages; p++) {
    const ops = [`1 1 1 rg 0 0 ${w} ${h} re f`];
    for (let i = 0; i < 4000; i++) {
      const c = [[0.12, 0.47, 0.71], [1, 0.5, 0.05], [0.17, 0.63, 0.17], [0.84, 0.15, 0.16]][i % 4];
      ops.push(`${c[0]} ${c[1]} ${c[2]} rg ${(40 + r() * (w - 80)).toFixed(1)} ${(40 + r() * (h - 80)).toFixed(1)} 1.6 1.6 re f`);
    }
    const content = ops.join("\n");
    const pageObj = objs.length + 1;
    kids.push(`${pageObj} 0 R`);
    objs.push(`<< /Type /Page /Parent 2 0 R /MediaBox [0 0 ${w} ${h}] /Contents ${pageObj + 1} 0 R >>`);
    objs.push(`<< /Length ${content.length} >>\nstream\n${content}\nendstream`);
  }
  objs[1] = `<< /Type /Pages /Kids [${kids.join(" ")}] /Count ${pages} >>`;
  let out = "%PDF-1.4\n";
  const offsets = [];
  objs.forEach((o, i) => {
    offsets.push(out.length);
    out += `${i + 1} 0 obj\n${o}\nendobj\n`;
  });
  const xref = out.length;
  out += `xref\n0 ${objs.length + 1}\n0000000000 65535 f \n`;
  for (const o of offsets) out += `${String(o).padStart(10, "0")} 00000 n \n`;
  out += `trailer\n<< /Size ${objs.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  return Buffer.from(out, "latin1");
}

function writeFixture() {
  const dir = (sub) => {
    const d = path.join(FIXTURE, sub);
    fs.mkdirSync(d, { recursive: true });
    return d;
  };
  if (fs.existsSync(path.join(FIXTURE, "figs", `fig_${FIGS - 1}.png`))) return;
  const figs = dir("figs");
  for (let i = 0; i < FIGS; i++) {
    const [w, h] = FIG_SIZES[i % FIG_SIZES.length];
    fs.writeFileSync(path.join(figs, `fig_${i}.png`), png(w, h, i));
  }
  const reports = dir("reports");
  for (let i = 0; i < PDFS; i++) fs.writeFileSync(path.join(reports, `report_${i}.pdf`), pdf(1 + (i % 3), i % 2 === 0, i));
  const docs = dir("docs");
  for (let i = 0; i < DOCS; i++) {
    const body = [`# Note ${i}`, "", ...Array.from({ length: 6 }, (_, j) => `## Part ${j}\n\n${sentence()} ${sentence()}\n`)];
    fs.writeFileSync(path.join(docs, `note_${i}.md`), body.join("\n"));
  }
  const tables = dir("tables");
  for (let i = 0; i < TABLES; i++) {
    const rows = ["gene,log2fc,padj", ...Array.from({ length: 200 }, (_, j) => `G${j},${(rand() * 4 - 2).toFixed(3)},${rand().toExponential(3)}`)];
    fs.writeFileSync(path.join(tables, `table_${i}.csv`), rows.join("\n"));
  }
  dir("out");
}

const fig = () => `${FIXTURE}/figs/fig_${Math.floor(rand() * FIGS)}.png`;
const report = () => `${FIXTURE}/reports/report_${Math.floor(rand() * PDFS)}.pdf`;
const doc = () => `${FIXTURE}/docs/note_${Math.floor(rand() * DOCS)}.md`;
const table = () => `${FIXTURE}/tables/table_${Math.floor(rand() * TABLES)}.csv`;

/** Inline embeds, references and math woven into a reply. */
function embedsMarkdown(turn) {
  const parts = [markdown(turn)];
  const figures = Math.floor(rand() * 3);
  for (let i = 0; i < figures; i++) {
    const hint = rand() < 0.3 ? `|${pick([320, 420, 520])}` : "";
    parts.push(`${sentence()}\n\n![Figure ${turn}.${i}: ${pick(WORDS)} by ${pick(WORDS)}${hint}](${fig()})`);
  }
  if (rand() < 0.35) parts.push(`The summary page:\n\n![page ${1 + Math.floor(rand() * 2)}](${report()}#page=${1 + Math.floor(rand() * 2)})`);
  if (rand() < 0.35) parts.push(`![notes](${doc()})`);
  const abs = path.resolve(doc());
  parts.push(
    `See [the plan](${doc()}) and [${path.basename(abs)}](${abs}) — the counts are in \`${table()}\`, the figure in \`${fig()}\`, and the model fits $\\log_2 \\mathrm{FC} = \\beta_1 x$ per gene.`,
  );
  return parts.join("\n\n");
}

/** Files the turn writes (copies of fixture pieces, stamped now), so its
 *  gallery has tiles and chips: returned as [toolName, input, source]. */
function turnWrites(turn) {
  const out = [];
  if (rand() < 0.4) {
    const target = path.resolve(FIXTURE, "out", `turn_${turn}_${pick(["umap", "volcano", "qc"])}.png`);
    out.push(["Write", { file_path: target, content: "<figure>" }, fig()]);
  }
  if (rand() < 0.3) {
    const target = path.resolve(FIXTURE, "out", `turn_${turn}_report.md`);
    out.push(["Write", { file_path: target, content: "# report" }, doc()]);
  }
  if (rand() < 0.3) {
    const rel = `${FIXTURE}/out/turn_${turn}_plot.${pick(["png", "pdf"])}`;
    out.push(["Bash", { command: `python plot.py --out ${rel}` }, rel.endsWith(".pdf") ? report() : fig(), rel]);
  }
  return out;
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
  if (EMBEDS) {
    // Lazily: a session that is only restored never writes into its folder.
    writeFixture();
    for (const [name, input, source, written] of turnWrites(turn)) {
      fs.copyFileSync(source, written ?? input.file_path);
      const id = `m${++msgN}`;
      const tuid = `tu-${turn}-w${msgN}`;
      stream({ type: "message_start", message: { id } });
      emit({ type: "assistant", message: { id, content: [{ type: "tool_use", id: tuid, name, input }] } });
      const result = name === "Bash" ? `saved ${written}` : `File created successfully at: ${input.file_path}`;
      emit({ type: "user", message: { content: [{ type: "tool_result", tool_use_id: tuid, content: result, is_error: false }] } });
    }
  }
  const id = `m${++msgN}`;
  const reply = EMBEDS ? embedsMarkdown : markdown;
  const body = slow ? Array.from({ length: 12 }, () => reply(turn)).join("\n\n") : reply(turn);
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
