#!/bin/zsh
# Tab-switch stall harness — WebKit edition. Opens the workbench in Safari (the
# same system WebKit as the app's WKWebView, without needing screen control),
# lets the in-app harness (harness.diff) cycle two terminals behind a set of
# parked documents, samples the renderer, and prints the per-switch timings.
#
#   run-safari.sh <daemon-url> <token> <workspace-id> <sessA,sessB> <file1,file2,..> <log-path> [sample-secs]
#
# The daemon must serve a web-ui/dist built WITH harness.diff applied. Timings
# land in <log-path> (inside a workspace the daemon can write); the sample in
# the current directory. See README.md.
set -e
URL=$1; T=$2; WS=$3; SESS=$4; FILES=$5; LOG=$6; SECS=${7:-45}
[ -n "$LOG" ] || { sed -n 2,12p "$0"; exit 2; }
rm -f "$LOG"
SPEC=$(python3 -c "import urllib.parse,sys; print(urllib.parse.quote(f'{sys.argv[1]}|{sys.argv[2]}|{sys.argv[3]}', safe=''))" "$SESS" "$FILES" "$LOG")
open -a Safari "$URL/#token=$T&ws=$WS&stalldrive=$SPEC"
python3 - "$SECS" <<'PYEOF'
import subprocess, time, re, sys
secs = int(sys.argv[1]); time.sleep(25)  # boot + park the documents; the switches start ~10 s in
ps = subprocess.run(["ps", "-axo", "pid,etime,command"], capture_output=True, text=True).stdout.splitlines()
pids = []
for l in ps:
    if "WebKit.WebContent" in l and "-" not in l.split()[1]:
        p = [int(x) for x in l.split()[1].split(":")]
        if p[-1] + 60 * p[-2] + (3600 * p[-3] if len(p) == 3 else 0) < 3600: pids.append(int(l.split()[0]))
args = ["top", "-l", "3", "-s", "1", "-stats", "pid,cpu"] + sum([["-pid", str(p)] for p in pids], [])
top = subprocess.run(args, capture_output=True, text=True).stdout.split("Processes:")[-1]
cpus = {int(m.group(1)): float(m.group(2)) for m in re.finditer(r"^\s*(\d+)\s+([\d.]+)", top, re.M)}
p = max(cpus, key=cpus.get); print("sampling WebContent pid", p, "cpu", cpus[p], flush=True)
f = f"tab-switch-{time.strftime('%H%M%S')}-pid{p}.sample"
subprocess.run(["sample", str(p), str(secs), "-file", f], capture_output=True, text=True); print("sample ->", f)
PYEOF
python3 - "$LOG" <<'PYEOF'
import time, os, sys
p = sys.argv[1]
for _ in range(90):
    if os.path.exists(p) and "done" in open(p).read(): break
    time.sleep(2)
print(open(p).read() if os.path.exists(p) else "NO LOG")
PYEOF
