#!/usr/bin/env node
// Tab-switch stall harness, WebKit edition: opens the workbench in Safari (the
// same system WebKit as the app's WKWebView, no screen control needed) with the
// in-app driver armed (web-ui/src/lib/perf/tabSwitchDrive.ts — the UI must be
// built with CHIMAERA_PERF=1), samples the busiest WebContent process while the
// driver cycles the terminals, then prints the driver's log.
//
//   node scripts/perf/tab-switch/run-safari.mjs <daemon-url> <token> <workspace-id> \
//        <sessA,sessB> <file1,file2,..> <log-path> [sample-secs]
//
// <log-path> must be a file the daemon may write (inside a workspace). The
// sample lands in the current directory; sample-secs 0 skips sampling (the
// sampler itself slows the switches it measures — quote numbers from such a
// run). See README.md.
import { execFileSync, spawnSync } from "node:child_process";

const [base, token, ws, sessions, files, logPath, secsArg] = process.argv.slice(2);
if (!logPath) {
  console.error(
    "usage: run-safari.mjs <daemon-url> <token> <workspace-id> <sessA,sessB> <file1,..> <log-path> [sample-secs]",
  );
  process.exit(2);
}
const secs = Number(secsArg ?? 45);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const spec = encodeURIComponent(`${sessions}|${files}|${logPath}`);
execFileSync("open", ["-a", "Safari", `${base}/?stalldrive=${spec}#token=${token}&ws=${ws}`]);
console.log("opened; waiting for the driver to park the documents");
await sleep(25_000);

// The busiest WebContent over a 3 s window is the harness page (`top` truncates
// command names, so the candidates come from `ps`; `top` takes at most 20 pids).
if (secs > 0) {
  const ps = spawnSync("ps", ["-axo", "pid,etime,command"], { encoding: "utf8" }).stdout;
  const candidates = ps
    .split("\n")
    .filter((l) => l.includes("WebKit.WebContent"))
    .map((l) => {
      const [pid, etime] = l.trim().split(/\s+/);
      const parts = etime.replace("-", ":").split(":").map(Number);
      const age = parts.reduce((acc, n) => acc * 60 + n, 0);
      return { pid: Number(pid), age };
    })
    .sort((a, b) => a.age - b.age)
    .slice(0, 20);
  const top = spawnSync(
    "top",
    ["-l", "3", "-s", "1", "-stats", "pid,cpu", ...candidates.flatMap((c) => ["-pid", String(c.pid)])],
    { encoding: "utf8" },
  ).stdout;
  const rows = [...(top.split("Processes:").pop() ?? "").matchAll(/^\s*(\d+)\s+([\d.]+)/gm)]
    .map((m) => ({ pid: Number(m[1]), cpu: Number(m[2]) }))
    .sort((a, b) => b.cpu - a.cpu);
  if (rows.length === 0) {
    console.error("no WebContent process found");
    process.exit(1);
  }
  const { pid, cpu } = rows[0];
  const file = `tab-switch-${new Date().toISOString().replace(/[:.]/g, "-")}-pid${pid}.sample`;
  console.log(`sampling WebContent pid ${pid} (${cpu}% cpu) for ${secs}s -> ${file}`);
  spawnSync("sample", [String(pid), String(secs), "-file", file], { encoding: "utf8" });
} else {
  console.log("sample-secs 0: not sampling (quote timings from this run)");
}

// The driver writes its log through the daemon; read it back the same way.
const headers = { Authorization: `Bearer ${token}` };
let text = "";
for (let i = 0; i < 90; i++) {
  const res = await fetch(`${base}/api/v1/fs/file?path=${encodeURIComponent(logPath)}`, { headers });
  if (res.ok) {
    text = await res.text();
    if (text.includes("done")) break;
  }
  await sleep(2000);
}
console.log(text || "NO LOG");
